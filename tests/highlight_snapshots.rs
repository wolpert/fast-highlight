//! Full-pipeline snapshot tests: the parser, the semantic pass, and the conversion to wire
//! offsets, run against a fixed shell state, a fake `$PATH`, and a fixture directory.
//!
//! Each case renders as a header (the buffer, plus the prebuffer, cursor, and options when they
//! are not the defaults) followed by one `start..end kind "text"` line per wire span. Offsets
//! are in characters relative to the start of `BUFFER`, exactly as the daemon sends them with
//! the `u` option. The cursor is at the end of the buffer unless the header says otherwise.

use fast_highlight::config::Config;
use fast_highlight::daemon::request_options;
use fast_highlight::highlight::{HighlightRequest, Highlighter};
use fast_highlight::paths::PathChecker;
use fast_highlight::protocol::{StateUpdate, WireSpan};
use fast_highlight::specs::SpecRegistry;
use fast_highlight::state::ShellState;
use fast_highlight::text::{RequestText, Unit};
use fast_highlight::token::{TokenKind, check_spans};
use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const EXECUTABLES: &[&str] = &[
    "git",
    "ls",
    "grep",
    "sudo",
    "cargo",
    "docker",
    "kubectl",
    "systemctl",
    "npm",
    "env",
    "nice",
    "cat",
    "diff",
    "make",
    "gitk",
];

const BUILTINS: &[&str] = &[
    "-", ".", ":", "[", "alias", "builtin", "cd", "command", "echo", "eval", "exec", "exit",
    "export", "false", "kill", "local", "noglob", "print", "printf", "pwd", "read", "set",
    "source", "test", "true", "typeset",
];

/// `$reswords` of zsh 5.9.
const RESERVED_WORDS: &[&str] = &[
    "!",
    "[[",
    "{",
    "}",
    "case",
    "coproc",
    "declare",
    "do",
    "done",
    "elif",
    "else",
    "end",
    "esac",
    "export",
    "fi",
    "float",
    "for",
    "foreach",
    "function",
    "if",
    "integer",
    "local",
    "nocorrect",
    "readonly",
    "repeat",
    "select",
    "then",
    "time",
    "typeset",
    "until",
    "while",
];

fn strings(v: &[&str]) -> Option<Vec<String>> {
    Some(v.iter().map(|s| (*s).to_string()).collect())
}

/// A highlighter with a deterministic shell state, and the fixture directory it looks at.
///
/// The directory lives under Cargo's per-target temp directory with a fixed name per test, so
/// the layout (and every path that might appear in a snapshot) is the same on every run.
struct Fixture {
    root: PathBuf,
    cwd: PathBuf,
    h: Highlighter,
}

impl Fixture {
    fn new(test: &str) -> Fixture {
        Fixture::with_user_specs(test, &[])
    }

    /// A fixture whose config directory holds the given `specs/<file>` user spec files.
    fn with_user_specs(test: &str, user_specs: &[(&str, &str)]) -> Fixture {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join("highlight_snapshots")
            .join(test);
        let _ = fs::remove_dir_all(&root);
        for exe in EXECUTABLES {
            write_file(&root.join("bin").join(exe), 0o755);
        }
        let cwd = root.join("work");
        for (rel, mode) in [
            ("README.md", 0o644),
            ("notes.txt", 0o644),
            ("build.sh", 0o755),
            ("src/main.rs", 0o644),
            ("src/lib.rs", 0o644),
            ("日本語.txt", 0o644),
            ("my file.txt", 0o644),
            ("-notes.txt", 0o644),
        ] {
            write_file(&cwd.join(rel), mode);
        }
        fs::create_dir_all(cwd.join("docs/guide")).unwrap();
        write_file(&root.join("home/.zshrc"), 0o644);
        fs::create_dir_all(root.join("home/Downloads")).unwrap();
        write_file(&root.join("proj/Cargo.toml"), 0o644);
        fs::create_dir_all(root.join("proj/src")).unwrap();
        // The config directory: empty unless the test adds user specs.
        let config_dir = root.join("config");
        fs::create_dir_all(&config_dir).unwrap();
        for (file, text) in user_specs {
            let path = config_dir.join("specs").join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }

        let mut state = ShellState::new();
        state.set_home(Some(root.join("home")));
        state.apply_update(StateUpdate {
            aliases: strings(&["ll", "gst", "sudo"]),
            global_aliases: strings(&["G", "NUL"]),
            suffix_aliases: strings(&["pdf", "md"]),
            functions: strings(&["mkcd", "gco"]),
            builtins: strings(BUILTINS),
            reserved_words: strings(RESERVED_WORDS),
            named_dirs: Some(vec![(
                "proj".to_string(),
                root.join("proj").to_string_lossy().into_owned(),
            )]),
            path: Some(root.join("bin").to_string_lossy().into_owned()),
            rehash: false,
        });
        let (specs, warnings) = SpecRegistry::load(&config_dir);
        assert!(warnings.is_empty(), "{warnings:?}");
        let config = Config::default();
        let h = Highlighter {
            paths: PathChecker::new(&config.limits),
            config,
            state,
            specs,
        };
        Fixture { root, cwd, h }
    }

    /// Wire spans (character units) for one request, after checking the byte-offset spans
    /// satisfy the span invariants.
    fn wire(&mut self, pre: &str, buf: &str, cursor: Option<usize>, opts: &str) -> Vec<WireSpan> {
        self.wire_in(pre, buf, cursor, opts, Unit::Chars)
    }

    fn wire_in(
        &mut self,
        pre: &str,
        buf: &str,
        cursor: Option<usize>,
        opts: &str,
        unit: Unit,
    ) -> Vec<WireSpan> {
        let text = RequestText::new(pre.as_bytes(), buf.as_bytes());
        let req = HighlightRequest {
            text: text.as_str(),
            buffer_start: text.buffer_start(),
            cursor: text.cursor_offset(cursor, unit),
            cwd: &self.cwd,
            opts: request_options(opts),
        };
        let spans = self.h.highlight(&req);
        check_spans(text.as_str(), &spans).expect("spans satisfy the invariants");
        text.wire_spans(&spans, unit)
    }

    /// Renders one case.
    fn render(&mut self, pre: &str, buf: &str, cursor: Option<usize>, opts: &str) -> String {
        let spans = self.wire(pre, buf, cursor, opts);
        let mut out = String::new();
        if !pre.is_empty() {
            let _ = writeln!(out, "prebuffer: {pre:?}");
        }
        let _ = writeln!(out, "input: {buf:?}");
        if let Some(c) = cursor {
            let _ = writeln!(out, "cursor: {c}");
        }
        if !opts.is_empty() {
            let _ = writeln!(out, "opts: {opts:?}");
        }
        let chars: Vec<char> = buf.chars().collect();
        for s in &spans {
            assert!(s.start < s.end && s.end <= chars.len(), "{s:?} in {buf:?}");
            let text: String = chars[s.start..s.end].iter().collect();
            let _ = writeln!(out, "  {}..{} {} {text:?}", s.start, s.end, s.kind);
        }
        self.redact(&out)
    }

    /// Replaces the fixture's absolute paths with `$CWD` and `$ROOT`.
    fn redact(&self, text: &str) -> String {
        text.replace(&*self.cwd.to_string_lossy(), "$CWD")
            .replace(&*self.root.to_string_lossy(), "$ROOT")
    }

    /// Renders every case, separated by blank lines.
    fn cases(&mut self, cases: &[Case]) -> String {
        cases
            .iter()
            .map(|c| self.render(c.pre, c.buf, c.cursor, c.opts))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn write_file(path: &Path, mode: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"#!/bin/sh\n").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// One request: the buffer, optional prebuffer, cursor (characters into the buffer; `None` is
/// the end), and option letters.
#[derive(Clone, Copy)]
struct Case {
    pre: &'static str,
    buf: &'static str,
    cursor: Option<usize>,
    opts: &'static str,
}

/// The buffer with the cursor at its end.
const fn at_end(buf: &'static str) -> Case {
    Case {
        pre: "",
        buf,
        cursor: None,
        opts: "",
    }
}

/// The buffer with the cursor at offset 0, which is the end of no word, so nothing is treated
/// as still being typed.
const fn idle(buf: &'static str) -> Case {
    Case {
        cursor: Some(0),
        ..at_end(buf)
    }
}

const fn with_opts(buf: &'static str, opts: &'static str) -> Case {
    Case {
        opts,
        ..at_end(buf)
    }
}

const fn continued(pre: &'static str, buf: &'static str) -> Case {
    Case { pre, ..at_end(buf) }
}

fn snap(test: &str, cases: &[Case]) {
    let mut f = Fixture::new(test);
    insta::assert_snapshot!(test, f.cases(cases));
}

#[test]
fn command_classes() {
    snap(
        "command_classes",
        &[
            idle("ll -a"),
            idle("README.md"),
            idle("G"),
            idle("echo hi G"),
            idle("mkcd docs"),
            idle("echo hi"),
            idle("if true; then ls; fi"),
            idle("ls -la"),
            idle("./build.sh"),
            idle("./notes.txt"),
            idle("nosuchcmd arg"),
            // Quoting does not hide an unknown command.
            idle("'nosuchcmd' arg"),
            idle("\\ll"),
            idle("'ls' src"),
            idle("$EDITOR notes.txt"),
            idle("FOO=1 BAR=$x make all"),
        ],
    );
}

#[test]
fn precommands() {
    snap(
        "precommands",
        &[
            idle("sudo -u root git status"),
            idle("sudo -E env FOO=1 nice -n 5 make"),
            idle("noglob ls *.txt"),
            idle("command -v git"),
            idle("builtin echo x"),
            idle("exec -a name ls"),
            idle("nocorrect ls"),
            idle("time ls"),
            idle("sudo nosuchcmd"),
            at_end("sudo"),
            at_end("sudo -u"),
        ],
    );
}

#[test]
fn typing_at_cursor() {
    snap(
        "typing_at_cursor",
        &[
            // Command word prefixes: no span while typing, error otherwise.
            at_end("gi"),
            idle("gi"),
            at_end("gix"),
            at_end("mk"),
            at_end("./bui"),
            at_end("./zz"),
            // Spec subcommand and option prefixes while typing.
            at_end("systemctl stat"),
            idle("systemctl stat"),
            at_end("systemctl statx"),
            at_end("docker container l"),
            at_end("git remote ad"),
            at_end("npm st"),
            // The cursor in the middle of the line, at the end of the first argument.
            Case {
                cursor: Some(14),
                ..at_end("systemctl stat nginx")
            },
            Case {
                cursor: Some(6),
                ..at_end("ls not src")
            },
        ],
    );
}

#[test]
fn spec_git_cargo() {
    snap(
        "spec_git_cargo",
        &[
            idle("git -C src --no-pager log --oneline -n 5"),
            idle("git commit -am msg --amend"),
            idle("git commit --bogus notes.txt"),
            idle("git remote add origin url"),
            idle("git remote rm origin"),
            idle("git remote frobnicate"),
            idle("git stash push -m wip"),
            idle("git frobnicate"),
            idle("git -c user.name=x status -sb"),
            idle("cargo +nightly build --release -p fast-highlight"),
            idle("cargo test -- --nocapture"),
            idle("cargo run --bin x -- --help"),
            idle("cargo frobnicate"),
        ],
    );
}

#[test]
fn spec_docker_kubectl() {
    snap(
        "spec_docker_kubectl",
        &[
            idle("docker run --rm -it -e A=1 ubuntu bash -l"),
            idle("docker container ls -a"),
            idle("docker container frob"),
            idle("docker ps -q"),
            idle("docker compose up -d"),
            idle("kubectl get pods -n kube-system -o wide"),
            idle("kubectl config use-context prod"),
            idle("kubectl config frob"),
            idle("kubectl rollout restart deployment/web"),
        ],
    );
}

#[test]
fn spec_systemctl_npm() {
    snap(
        "spec_systemctl_npm",
        &[
            idle("systemctl --user restart pipewire"),
            idle("systemctl status nginx --no-pager"),
            idle("systemctl frob nginx"),
            idle("sudo systemctl stop nginx"),
            idle("npm install --save-dev typescript"),
            idle("npm i -g npm"),
            idle("npm inst lodash"),
            idle("npm run build"),
            idle("npm frob"),
        ],
    );
}

#[test]
fn paths() {
    snap(
        "paths",
        &[
            idle("cat notes.txt src src/ src/main.rs missing.txt"),
            at_end("cat not"),
            at_end("cat src/ma"),
            at_end("cat src/zz"),
            idle("cat not"),
            idle("ls ~ ~/ ~/.zshrc ~/Downloads ~/nope"),
            at_end("ls ~/Dow"),
            idle("ls ~proj ~proj/Cargo.toml ~proj/src ~proj/nope"),
            at_end("ls ~proj/Car"),
            idle("ls 'my file.txt' my\\ file.txt \"my file.txt\""),
            idle("ls '~'/.zshrc"),
            idle("ls -- -notes.txt"),
            idle("cat docs/guide/ ./docs ../work/notes.txt"),
        ],
    );
}

/// A quoted `~` is literal: `'~'/.zshrc` names `./~/.zshrc`, which does not exist, even though
/// `~/.zshrc` does.
#[test]
fn quoted_tilde_is_not_expanded() {
    let mut f = Fixture::new("quoted_tilde_is_not_expanded");
    let kinds = |spans: Vec<WireSpan>| spans.into_iter().map(|s| s.kind).collect::<Vec<_>>();
    assert_eq!(
        kinds(f.wire("", "ls ~/.zshrc", Some(0), "u")),
        vec![TokenKind::Command, TokenKind::Path]
    );
    assert_eq!(
        kinds(f.wire("", "ls '~'/.zshrc", Some(0), "u")),
        vec![TokenKind::Command, TokenKind::SingleQuoted]
    );
}

/// After `--`, a word starting with `-` is an operand and is checked as a path; before it, the
/// word is an option.
#[test]
fn dash_operand_after_double_dash_is_a_path() {
    let mut f = Fixture::new("dash_operand_after_double_dash_is_a_path");
    let triples = |spans: Vec<WireSpan>| -> Vec<(usize, usize, TokenKind)> {
        spans
            .into_iter()
            .map(|s| (s.start, s.end, s.kind))
            .collect()
    };
    assert_eq!(
        triples(f.wire("", "ls -- -notes.txt", Some(0), "u")),
        vec![(0, 2, TokenKind::Command), (6, 16, TokenKind::Path)]
    );
    assert_eq!(
        triples(f.wire("", "ls -notes.txt", Some(0), "u")),
        vec![(0, 2, TokenKind::Command)]
    );
}

/// The size thresholds with the default config: above `lex-only-bytes` (10 KiB) only syntax is
/// highlighted, and above `hard-cap-bytes` (64 KiB) nothing is.
#[test]
fn default_size_thresholds() {
    let mut f = Fixture::new("default_size_thresholds");
    let limits = Config::default().limits;
    let buffer_of = |len: usize| {
        let head = "cat notes.txt ; : ";
        format!("{head}{}", "x".repeat(len - head.len()))
    };
    let semantic = |k: TokenKind| {
        matches!(
            k,
            TokenKind::Command | TokenKind::Path | TokenKind::PathPrefix
        )
    };

    let at_threshold = f.wire("", &buffer_of(limits.lex_only_bytes), Some(0), "u");
    assert!(
        at_threshold.iter().any(|s| s.kind == TokenKind::Command),
        "{at_threshold:?}"
    );
    assert!(at_threshold.iter().any(|s| s.kind == TokenKind::Path));

    let lex_only = f.wire("", &buffer_of(limits.lex_only_bytes + 1), Some(0), "u");
    assert!(!lex_only.iter().any(|s| semantic(s.kind)), "{lex_only:?}");
    assert!(
        lex_only.iter().any(|s| s.kind == TokenKind::Separator),
        "syntax is still highlighted: {lex_only:?}"
    );

    let at_cap = f.wire("", &buffer_of(limits.hard_cap_bytes), Some(0), "u");
    assert!(!at_cap.is_empty());
    assert!(!at_cap.iter().any(|s| semantic(s.kind)));
    let over_cap = f.wire("", &buffer_of(limits.hard_cap_bytes + 1), Some(0), "u");
    assert_eq!(over_cap, vec![]);
}

/// Absolute paths. The offsets depend on where the fixture lives, so they are checked against
/// the path length instead of being snapshotted.
#[test]
fn absolute_paths() {
    let mut f = Fixture::new("absolute_paths");
    let cwd = f.cwd.to_string_lossy().into_owned();
    let buf = format!("cat {cwd}/notes.txt {cwd}/src {cwd}/nope");
    let spans = f.wire("", &buf, Some(0), "u");
    let file_end = 4 + cwd.chars().count() + "/notes.txt".len();
    let dir_start = file_end + 1;
    let dir_end = dir_start + cwd.chars().count() + "/src".len();
    let got: Vec<_> = spans.iter().map(|s| (s.start, s.end, s.kind)).collect();
    assert_eq!(
        got,
        vec![
            (0, 3, TokenKind::Command),
            (4, file_end, TokenKind::Path),
            (dir_start, dir_end, TokenKind::PathDirectory),
        ]
    );
}

/// A user spec that makes `git commit` options complete: an unknown option is an error, except
/// a prefix of a known option while it is being typed.
#[test]
fn user_spec_options_complete() {
    let spec = "name = \"git\"\nmerge = true\n[subcommands.commit]\noptions-complete = true\n";
    let mut f = Fixture::with_user_specs("user_spec_options_complete", &[("git.toml", spec)]);
    let out = f.cases(&[
        at_end("git commit --am"),
        idle("git commit --am"),
        at_end("git commit --amx"),
        at_end("git commit --amend --bogus"),
        Case {
            cursor: Some(15),
            ..at_end("git commit --am notes.txt")
        },
        at_end("git commit --no-ver"),
    ]);
    insta::assert_snapshot!("user_spec_options_complete", out);
}

#[test]
fn expansions_and_quoting() {
    snap(
        "expansions_and_quoting",
        &[
            idle("echo 'single' \"double $HOME ${PATH:h} $(pwd)\" $'tab\\t'"),
            idle("echo ${(j:,:)array[@]} ${#var} ${var:-default} $1 $? $$"),
            idle("echo $(ls `pwd` | grep x) $((1 + 2))"),
            idle("diff <(ls src) >(cat) =(ls)"),
            idle("ls *.txt src/**/*.rs file?.md [ab]*"),
            idle("ls *(.) *(/om[1,3])"),
            with_opts("ls ^*.txt *.rs~main.rs (#i)readme", "e"),
            idle("echo {a,b,c} {1..10}"),
            idle("echo !! !$ !-2:p"),
            idle("arr=(one two) x+=1 y=$HOME echo"),
            with_opts("ls # a comment", "c"),
            idle("ls # not a comment"),
            idle("[[ -f notes.txt && $x == y* ]]"),
            idle("(( i++ )) && { ls; } || ( cd src )"),
            idle("for f in *.txt src; do cat $f; done"),
            idle("case $x in a|b) ls;; *) echo;; esac"),
            idle("f() { echo; }"),
        ],
    );
}

#[test]
fn redirections_and_heredocs() {
    snap(
        "redirections_and_heredocs",
        &[
            idle("ls > out.txt 2>&1 >> notes.txt < notes.txt"),
            idle("ls &> /dev/null 2>/dev/null >| out"),
            idle("cat <<< \"here string\""),
            idle("cat <<EOF\nline $HOME\nEOF"),
            idle("cat <<-'EOF' > out\n\tliteral $x\n\tEOF\nls"),
            at_end("cat <<EOF\nunterminated"),
            idle("exec 3<> notes.txt 4>&-"),
        ],
    );
}

#[test]
fn partial_input() {
    snap(
        "partial_input",
        &[
            at_end("echo \"unterminated $HOME"),
            at_end("echo 'unterminated"),
            at_end("echo $(ls"),
            at_end("echo ${HO"),
            at_end("echo $((1 +"),
            at_end("if true; then"),
            at_end("for x in"),
            at_end("ls |"),
            at_end("ls &&"),
            at_end("ls >"),
            at_end("git "),
            at_end("case x in"),
        ],
    );
}

#[test]
fn syntax_errors() {
    snap(
        "syntax_errors",
        &[
            idle("fi"),
            idle("ls; fi"),
            idle("| grep foo"),
            idle("ls > ;"),
            idle("ls && || ls"),
            idle("ls )"),
            idle("then ls"),
            idle("done"),
            idle("ls ;; ls"),
            idle("echo }"),
        ],
    );
}

#[test]
fn multi_line_with_prebuffer() {
    snap(
        "multi_line_with_prebuffer",
        &[
            // A string opened in PREBUFFER continues into BUFFER.
            continued("echo \"first line\n", "second $HOME\" notes.txt"),
            // A control structure across lines: only the BUFFER part is reported.
            continued("if true\nthen\n", "ls src\nfi"),
            continued("for f in *.txt; do\n", "cat $f"),
            // A backslash continuation: the command word is in PREBUFFER.
            continued("git commit \\\n", "--amend notes.txt"),
            // A here-document body in BUFFER.
            continued("cat <<EOF\n", "body $x\nEOF"),
            // A stray closer in BUFFER after a complete PREBUFFER construct.
            continued("if true; then ls\n", "fi; fi"),
            // A multi-line BUFFER without PREBUFFER.
            at_end("ls src\ngit status\ngi"),
        ],
    );
}

#[test]
fn multibyte_and_wide() {
    snap(
        "multibyte_and_wide",
        &[
            idle("echo \"héllo 世界\" 日本語.txt $名前 🎉"),
            at_end("cat 日本"),
            idle("ls 日本語.txt | grep 'ü' > 出力.txt"),
            idle("日本 arg"),
            continued("echo \"日本\n", "語\" 🎉 notes.txt"),
            Case {
                cursor: Some(6),
                ..at_end("cat 日本 x")
            },
        ],
    );
}

/// Character-unit wire offsets checked explicitly, in both units, for wide and multibyte text.
#[test]
fn multibyte_wire_offsets() {
    let mut f = Fixture::new("multibyte_wire_offsets");
    let triples = |spans: Vec<WireSpan>| -> Vec<(usize, usize, TokenKind)> {
        spans
            .into_iter()
            .map(|s| (s.start, s.end, s.kind))
            .collect()
    };
    use TokenKind as K;

    // `echo "世界" 日本語.txt 🎉`: 世界 is 2 chars (6 bytes), 日本語.txt 7 chars (13 bytes).
    let buf = "echo \"世界\" 日本語.txt 🎉";
    assert_eq!(
        triples(f.wire("", buf, Some(0), "u")),
        vec![
            (0, 4, K::Builtin),
            (5, 9, K::DoubleQuoted),
            (10, 17, K::Path),
        ]
    );
    assert_eq!(
        triples(f.wire_in("", buf, Some(0), "", Unit::Bytes)),
        vec![
            (0, 4, K::Builtin),
            (5, 13, K::DoubleQuoted),
            (14, 27, K::Path),
        ]
    );

    // The cursor in characters: `cat 日本` with the cursor after 本 (char 6) is a path prefix
    // of 日本語.txt; in bytes the same position is 10.
    let buf = "cat 日本 x";
    assert_eq!(
        triples(f.wire("", buf, Some(6), "u")),
        vec![(0, 3, K::Command), (4, 6, K::PathPrefix)]
    );
    assert_eq!(
        triples(f.wire_in("", buf, Some(10), "", Unit::Bytes)),
        vec![(0, 3, K::Command), (4, 10, K::PathPrefix)]
    );
    // A byte cursor inside 本 rounds down to its start, which is no word end.
    assert_eq!(
        triples(f.wire_in("", buf, Some(9), "", Unit::Bytes)),
        vec![(0, 3, K::Command)]
    );

    // PREBUFFER with wide characters does not shift BUFFER offsets; the string continued from
    // PREBUFFER is clipped to the start of BUFFER.
    let pre = "echo \"日本\n";
    let buf = "語\" 🎉 notes.txt";
    assert_eq!(
        triples(f.wire(pre, buf, Some(0), "u")),
        vec![(0, 2, K::DoubleQuoted), (5, 14, K::Path)]
    );
    assert_eq!(
        triples(f.wire_in(pre, buf, Some(0), "", Unit::Bytes)),
        vec![(0, 4, K::DoubleQuoted), (10, 19, K::Path)]
    );

    // A command word made of wide characters is an unknown command.
    assert_eq!(
        triples(f.wire("", "日本 arg", Some(0), "u")),
        vec![(0, 2, K::Error)]
    );
}
