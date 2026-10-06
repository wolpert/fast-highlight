#!/usr/bin/env python3
"""Regenerates fuzz/seeds/<target>/ from the parser snapshot inputs and protocol examples.

Run from anywhere: python3 fuzz/make_seeds.py

Seeds follow the input layout documented at the top of each fuzz target. Existing seed
directories are replaced.
"""

import re
import shutil
import struct
from pathlib import Path

FUZZ = Path(__file__).resolve().parent
ROOT = FUZZ.parent
SEEDS = FUZZ / "seeds"

# Extra lines for the semantic pass: specs, precommands, aliases, paths in the scratch cwd.
HIGHLIGHT_EXTRA = [
    "git commit -m 'msg' --amend",
    "sudo -u root ls -la dir/sub",
    "env FOO=1 nice -n 5 grep -r x .",
    "docker run --rm -it image sh -c 'ls'",
    "ll ~/docs ~proj/file.txt G",
    "./run.sh file.txt a\\ b dir/su",
    "myfunc | cat é.md > out && nonexistent_cmd",
    "cd dir; dir/sub/x.rs",
    "kubectl get pods -n kube-system",
    "systemctl --user restart foo.service",
    "timeout 5 command -v ls",
    "cargo build --release --features x",
    "npm run test -- --watch",
]

# Lines for the options of the extra option byte, with that byte's bits (see
# fuzz_targets/lexer.rs).
OPTION_EXTRA = [
    ("ignore-braces", "{echo a}; { echo {a,b} a} }; exec {fd}>x", 1),
    ("ignore-close-braces", "{echo a}; { echo a } }; { if :; then :; fi }", 2),
    ("rc-quotes", "echo 'it''s' '''' $'a''b' 'open''", 4),
    ("ksh-arrays", "echo $a[1] $PWD:h ${a[1]} \"$a[2]\"", 8),
    ("posix-identifiers", "é=1 echo $#a $+a $é", 16),
    ("sh-glob", "echo a(b) *(.) (a|b) <1-5> x=a(b) [[ a == (a|b) ]]", 32),
    ("brace-ccl", "echo {abc} {} x{ba}y {a-z", 64),
    ("no-short-loops", "for x in a; echo $x; repeat 2 ls; for x (a) { ls; }", 128),
    ("all-options", "for x (a) echo {ab}'c''d' $a[1] (x) }", 255),
]

# Parse options after the first three, in the bit order of the extra option byte.
MORE_OPTIONS = [
    "ignore_braces",
    "ignore_close_braces",
    "rc_quotes",
    "ksh_arrays",
    "posix_identifiers",
    "sh_glob",
    "brace_ccl",
    "no_short_loops",
]


def rust_unescape(s):
    """Undoes Rust's `{:?}` escaping of a string body (without the quotes)."""
    out = []
    i = 0
    simple = {"n": "\n", "t": "\t", "r": "\r", "0": "\0", "\\": "\\", '"': '"', "'": "'"}
    while i < len(s):
        c = s[i]
        if c != "\\":
            out.append(c)
            i += 1
            continue
        nxt = s[i + 1]
        if nxt in simple:
            out.append(simple[nxt])
            i += 2
        elif nxt == "u":
            end = s.index("}", i)
            out.append(chr(int(s[i + 3 : end], 16)))
            i = end + 1
        else:
            raise ValueError(f"unknown escape \\{nxt} in {s!r}")
    return "".join(out)


def snapshot_inputs():
    """Yields (name, text, option bits, extra option bits) for every snapshot input."""
    for snap in sorted((ROOT / "tests" / "snapshots").glob("*.snap")):
        body = snap.read_text(encoding="utf-8")
        m = re.search(r'^input: "(.*)"$', body, re.M)
        if not m:
            continue
        bits = 0
        more = 0
        o = re.search(r"^options: ParseOptions \{(.*)\}$", body, re.M)
        if o:
            for bit, name in enumerate(["interactive_comments", "extended_glob", "ksh_glob"]):
                if re.search(rf"\b{name}: true", o.group(1)):
                    bits |= 1 << bit
            for bit, name in enumerate(MORE_OPTIONS):
                if re.search(rf"\b{name}: true", o.group(1)):
                    more |= 1 << bit
        name = snap.stem.split("__", 1)[-1]
        yield name, rust_unescape(m.group(1)), bits, more


def frame(kind, ident, body=b""):
    return b"FH1 %s %d %d\n" % (kind, ident, len(body)) + body


def field(name, value):
    return b"%s %d\n" % (name, len(value)) + value + b"\n"


def protocol_streams():
    """Yields (name, stream bytes): valid frames, then malformed ones."""
    ping = frame(b"P", 1)
    hl = frame(
        b"H",
        2,
        field(b"buf", b"ls -l | grep x")
        + field(b"pre", b"for f in *; do\n")
        + field(b"cur", b"3")
        + field(b"cwd", b"/tmp")
        + field(b"opt", b"uce"),
    )
    hl_utf8 = frame(b"H", 3, field(b"buf", "echo 日本 ".encode() + b"\xff\xfe"))
    state = frame(
        b"S",
        4,
        field(b"alias", b"ll\0la\0")
        + field(b"func", b"f\0")
        + field(b"nameddirs", b"proj\0/home/u/proj\0")
        + field(b"path", b"/usr/bin:/bin"),
    )
    quit_ = frame(b"Q", 18446744073709551615)
    yield "ping", ping
    yield "highlight", hl
    yield "highlight-utf8", hl_utf8
    yield "state", state
    yield "quit-max-id", quit_
    yield "sequence", ping + hl + state + hl_utf8 + quit_
    yield "partial", hl + hl[:20]
    yield "unknown-type", frame(b"X", 5, b"abc") + ping
    yield "bad-fields", frame(b"H", 6, b"buf 99\nshort\n") + frame(b"S", 7, field(b"nameddirs", b"odd\0"))
    yield "bad-cur", frame(b"H", 8, field(b"buf", b"x") + field(b"cur", b"01"))
    yield "bad-magic", b"FH2 P 1 0\n"
    yield "leading-zero", b"FH1 P 01 0\n"
    yield "id-overflow", b"FH1 P 18446744073709551616 0\n"
    yield "too-long", frame(b"P", 9, b"") + b"FH1 P 1 " + b"9" * 60 + b"\n"
    yield "body-too-big", b"FH1 H 1 16777217\n"
    yield "state-rehash", frame(b"S", 10, field(b"path", b"/bin") + field(b"rehash", b""))


def write(target, name, data):
    d = SEEDS / target
    d.mkdir(parents=True, exist_ok=True)
    (d / name).write_bytes(data)


def main():
    shutil.rmtree(SEEDS, ignore_errors=True)
    inputs = list(snapshot_inputs())
    inputs += [(name, text, 0, more) for name, text, more in OPTION_EXTRA]
    for name, text, bits, more in inputs:
        raw = text.encode("utf-8")
        # The extra option byte, present only when one of its options is set.
        lexer_bits, highlight_bits, extra = bits, bits, b""
        if more:
            lexer_bits, highlight_bits, extra = bits | 16, bits | 128, bytes([more])
        write("lexer", name, bytes([lexer_bits]) + extra + raw)
        # Whole text as BUFFER, no cursor.
        header = bytes([highlight_bits]) + struct.pack("<HH", 0, 0xFFFF) + extra
        write("highlight", name, header + raw)
        # Multi-line text: everything up to the last line as PREBUFFER, cursor at 2 chars.
        nl = raw.rfind(b"\n")
        if nl >= 0:
            header = bytes([highlight_bits | 16]) + struct.pack("<HH", nl + 1, 2) + extra
            write("highlight", name + "-prebuffer", header + raw)
    write("lexer", "invalid-utf8", b"\x00echo \xff\xc3 \xe6\x97 x")
    write("lexer", "invalid-utf8-lossy", b"\x08echo \xff\xc3 \xe6\x97 x")
    for i, text in enumerate(HIGHLIGHT_EXTRA):
        write("highlight", f"semantic-{i:02}", bytes([8]) + struct.pack("<HH", 0, 0xFFFF) + text.encode())
    write("highlight", "tight-limits", bytes([32]) + struct.pack("<HH", 0, 5) + (" ".join(HIGHLIGHT_EXTRA)).encode())
    write("highlight", "invalid-utf8", bytes([16]) + struct.pack("<HH", 3, 4) + b"a\xff\xe6echo \xe6\x97\xa5 \xff x")
    for i, (name, stream) in enumerate(protocol_streams()):
        write("protocol", name, bytes([0, i]) + stream)
    # Round-trip mode: arbitrary bytes become a request list.
    write("protocol", "roundtrip-a", b"\x01" + bytes(range(64)))
    write("protocol", "roundtrip-b", b"\x03" + b"\xff\x00\x10ls -l\x00\x02\x05cwd\x01\x01u" * 4)
    for target in sorted(p.name for p in SEEDS.iterdir()):
        print(f"{target}: {len(list((SEEDS / target).iterdir()))} seeds")


if __name__ == "__main__":
    main()
