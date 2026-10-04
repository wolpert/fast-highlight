#!/usr/bin/env python3
"""Mock `fast-highlight` binary for the zsh plugin tests.

Implements the wire protocol in docs/protocol.md well enough to drive the plugin:

  mock-daemon.py serve     serve requests on stdin/stdout
  mock-daemon.py styles    print a style table for the plugin to eval

An H request is answered with one span per whitespace-separated word of `buf`. The first word
has kind `command`; later words are classified by their first character (`"` double-quoted,
`'` single-quoted, `$` parameter, `-` option, anything else `default`). Offsets are in
characters when the request carries the `u` option, else in bytes.

Environment:

  MOCK_MODE      normal (default)
                 hang        read H requests but never answer them
                 stuck       stop reading stdin after the first S request
                 crash       exit on the first H
                 crash-once  exit on the first H while MOCK_MARKER does not exist (creating it),
                             behave normally afterwards
                 slow        sleep 0.2 s before answering H
                 stale       send an extra R frame with the previous id before each answer
                 error       answer H with E
                 die         write to stderr and exit before reading anything
                 die-once    like die while MOCK_MARKER does not exist (creating it), behave
                             normally afterwards
                 partial     answer H with a header and half the body, close stdout, log
                             `partial lingered` 0.5 s later (unless killed first), and exit
                 garbage     answer H with an invalid header and stay alive
                 oversize    answer H with a header declaring a body above 16 MiB and stay
                             alive
                 badspans    answer H with the normal spans plus malformed span lines
                 flood       answer H with MOCK_SPANS (default 50000) spans `0 1 command`
  MOCK_MARKER    marker file for crash-once and die-once
  MOCK_LOG       append one line per request to this file
  MOCK_NO_STYLES when set, `styles` fails with exit status 1
  MOCK_STYLES_FAIL  when set, `styles` prints its table and then exits with status 1, as the
                 real binary does on a broken config or theme
  MOCK_START_DELAY  seconds to sleep before serving
  MOCK_STATE_DELAY  seconds to sleep before handling each S request (logged after the sleep)

List fields of S requests are checked more strictly than the protocol requires: a non-empty
value that does not end in NUL is logged as `S-badlist NAME`, since the plugin terminates every
entry. `nameddirs` is logged as `S-nameddirs name=dir ...`, sorted.
"""

import os
import select
import sys
import time

MAX_HEADER = 64
MAX_BODY = 16 * 1024 * 1024
LIST_FIELDS = ("alias", "galias", "salias", "func", "builtin", "reswords", "nameddirs")
BAD_SPANS = (
    b"0 99 command\n"  # end past the buffer
    b"0 1 nosuchkind\n"  # unknown kind
    b"2 1 command\n"  # start after end
    b"x 1 command\n"  # non-numeric start
    b"0 y command\n"  # non-numeric end
    b"1 2\n"  # no kind
    b"-1 2 command\n"  # negative start
    b"0 1 com mand\n"  # extra field
    b"\n"  # empty line
)


def log(line):
    path = os.environ.get("MOCK_LOG")
    if path:
        with open(path, "a", encoding="utf-8", errors="replace") as f:
            f.write(line + "\n")


def parse_fields(body):
    fields = {}
    order = []
    pos = 0
    while pos < len(body):
        nl = body.index(b"\n", pos)
        name, length = body[pos:nl].split(b" ")
        length = int(length)
        start = nl + 1
        value = body[start : start + length]
        if len(value) != length or body[start + length : start + length + 1] != b"\n":
            raise ValueError("bad field")
        pos = start + length + 1
        key = name.decode()
        fields[key] = value
        order.append(key)
    return fields, order


def frame(kind, rid, body=b""):
    return b"FH1 %s %d %d\n" % (kind.encode(), rid, len(body)) + body


def word_kind(index, word):
    if index == 0:
        return "command"
    return {'"': "double-quoted", "'": "single-quoted", "$": "parameter", "-": "option"}.get(
        word[:1], "default"
    )


def spans(fields):
    buf = fields.get("buf", b"")
    chars = b"u" in fields.get("opt", b"")
    text = buf.decode("utf-8", errors="replace") if chars else buf
    space = " \t\n" if chars else b" \t\n"
    out = []
    index = 0
    pos = 0
    n = len(text)
    while pos < n:
        while pos < n and text[pos : pos + 1] in space:
            pos += 1
        if pos >= n:
            break
        start = pos
        while pos < n and text[pos : pos + 1] not in space:
            pos += 1
        word = text[start:pos]
        if not chars:
            word = word.decode("utf-8", errors="replace")
        out.append(b"%d %d %s\n" % (start, pos, word_kind(index, word).encode()))
        index += 1
    return b"".join(out)


def text(fields, name, default="-"):
    if name not in fields:
        return default
    return fields[name].decode("utf-8", "replace").replace("\n", "\\n")


def linger(ppid):
    """Stay alive without reading until the parent goes away."""
    while os.getppid() == ppid:
        time.sleep(0.1)
    sys.exit(0)


class Reader:
    def __init__(self):
        self.buf = b""
        self.ppid = os.getppid()

    def fill(self):
        while True:
            ready, _, _ = select.select([0], [], [], 0.5)
            if not ready:
                if os.getppid() != self.ppid:
                    sys.exit(0)
                continue
            data = os.read(0, 65536)
            if not data:
                sys.exit(0)
            self.buf += data
            return

    def frame(self):
        while b"\n" not in self.buf:
            if len(self.buf) >= MAX_HEADER:
                sys.exit(2)
            self.fill()
        header, _, _ = self.buf.partition(b"\n")
        if len(header) + 1 > MAX_HEADER:
            sys.exit(2)
        parts = header.split(b" ")
        if len(parts) != 4 or parts[0] != b"FH1" or len(parts[1]) != 1:
            sys.exit(2)
        rid, length = int(parts[2]), int(parts[3])
        if length > MAX_BODY:
            sys.exit(2)
        need = len(header) + 1 + length
        while len(self.buf) < need:
            self.fill()
        body = self.buf[len(header) + 1 : need]
        self.buf = self.buf[need:]
        return parts[1].decode(), rid, body


def serve():
    mode = os.environ.get("MOCK_MODE", "normal")
    delay = os.environ.get("MOCK_START_DELAY")
    if delay:
        time.sleep(float(delay))
    if mode == "die-once":
        marker = os.environ["MOCK_MARKER"]
        if not os.path.exists(marker):
            open(marker, "w").close()
            mode = "die"
    if mode == "die":
        sys.stderr.write("mock-daemon: dying at startup\n")
        sys.exit(1)
    # Extra arguments (such as --parent PID) are logged and otherwise ignored.
    log("ARGS %s" % " ".join(sys.argv[1:]))
    reader = Reader()
    while True:
        kind, rid, body = reader.frame()
        if kind == "Q":
            log("Q %d" % rid)
            sys.exit(0)
        if kind == "P":
            log("P %d" % rid)
            os.write(1, frame("A", rid))
            continue
        try:
            fields, order = parse_fields(body)
        except (ValueError, IndexError):
            log("%s %d malformed" % (kind, rid))
            os.write(1, frame("E", rid, b"malformed request"))
            continue
        if kind == "S":
            state_delay = os.environ.get("MOCK_STATE_DELAY")
            if state_delay:
                time.sleep(float(state_delay))
            log("S %d %s" % (rid, ",".join(order)))
            if "func" in fields:
                names = [n.decode() for n in fields["func"].split(b"\0") if n]
                log("S-func %s" % " ".join(sorted(names)))
            if "alias" in fields:
                names = [n.decode() for n in fields["alias"].split(b"\0") if n]
                log("S-alias %s" % " ".join(sorted(names)))
            for name in LIST_FIELDS:
                value = fields.get(name, b"")
                if value and not value.endswith(b"\0"):
                    log("S-badlist %s" % name)
            if "nameddirs" in fields:
                entries = [e.decode() for e in fields["nameddirs"].split(b"\0")[:-1]]
                pairs = ["%s=%s" % (entries[i], entries[i + 1]) for i in range(0, len(entries) - 1, 2)]
                if len(entries) % 2:
                    pairs.append("odd-count")
                log("S-nameddirs %s" % " ".join(sorted(pairs)))
            os.write(1, frame("A", rid))
            if mode == "stuck":
                while os.getppid() == reader.ppid:
                    time.sleep(0.2)
                sys.exit(0)
            continue
        if kind != "H":
            os.write(1, frame("E", rid, b"unknown request type"))
            continue
        log(
            "H %d buf=%s cur=%s cwd=%s opt=%s pre=%s"
            % (
                rid,
                text(fields, "buf", ""),
                text(fields, "cur"),
                text(fields, "cwd"),
                text(fields, "opt", ""),
                text(fields, "pre"),
            )
        )
        if mode == "hang":
            continue
        if mode == "crash":
            sys.exit(3)
        if mode == "crash-once":
            marker = os.environ["MOCK_MARKER"]
            if not os.path.exists(marker):
                open(marker, "w").close()
                sys.exit(3)
        if mode == "slow":
            time.sleep(0.2)
        if mode == "error":
            os.write(1, frame("E", rid, b"mock error"))
            continue
        if mode == "stale" and rid > 0:
            os.write(1, frame("R", rid - 1, b"0 1 command\n"))
        if mode == "partial":
            data = frame("R", rid, spans(fields))
            header_len = data.index(b"\n") + 1
            os.write(1, data[: header_len + (len(data) - header_len) // 2])
            os.close(1)
            time.sleep(0.5)
            log("partial lingered")
            sys.exit(0)
        if mode == "garbage":
            os.write(1, b"XYZ not a frame\n")
            linger(reader.ppid)
        if mode == "oversize":
            os.write(1, b"FH1 R %d %d\n" % (rid, MAX_BODY + 1))
            linger(reader.ppid)
        if mode == "badspans":
            os.write(1, frame("R", rid, spans(fields) + BAD_SPANS))
            continue
        if mode == "flood":
            count = int(os.environ.get("MOCK_SPANS", "50000"))
            os.write(1, frame("R", rid, b"0 1 command\n" * count))
            continue
        os.write(1, frame("R", rid, spans(fields)))


STYLES = """typeset -gA FASTHL_STYLES
FASTHL_STYLES=(
  command 'fg=green,bold'
  double-quoted 'fg=yellow'
  single-quoted 'fg=yellow'
  parameter 'fg=cyan'
  option 'fg=blue'
  default ''
)
"""


def main():
    if len(sys.argv) >= 2 and sys.argv[1] == "serve":
        try:
            serve()
        except (BrokenPipeError, KeyboardInterrupt):
            sys.exit(0)
    elif len(sys.argv) >= 2 and sys.argv[1] == "styles":
        if os.environ.get("MOCK_NO_STYLES"):
            sys.exit(1)
        sys.stdout.write(STYLES)
        if os.environ.get("MOCK_STYLES_FAIL"):
            sys.exit(1)
    else:
        sys.stderr.write("usage: mock-daemon.py serve|styles\n")
        sys.exit(2)


if __name__ == "__main__":
    main()
