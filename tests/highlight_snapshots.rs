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
use fast_highlight::specs::{ArgInput, SpecRegistry, Tail};
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
    "pip",
    "pip3",
    "uv",
    "go",
    "rustup",
    "ip",
    "journalctl",
    "ssh",
    "tar",
    "xargs",
    "rm",
    "mv",
    "podman",
    "gh",
    "apt",
    "dnf",
    "brew",
    "dnf5",
    "doas",
    "chrt",
    "taskset",
    "ionice",
    "env",
    "nice",
    "timeout",
    "cat",
    "diff",
    "make",
    "gitk",
    "date",
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
fn spec_git_refresh() {
    snap(
        "spec_git_refresh",
        &[
            idle("git backfill --sparse"),
            idle("git last-modified"),
            idle("git repo info --all"),
            idle("git refs verify --strict"),
            idle("git stash export --print"),
            idle("git stash import refs/stash"),
            idle("git update-index --refresh"),
            idle("git diff-tree -r HEAD"),
            idle("git credential capability"),
            idle("git commit-graph write --reachable"),
            idle("git diff-pairs --raw -z"),
            // A pre-verb option that takes a value: `d` is its value, `write` the subcommand.
            idle("git multi-pack-index --object-dir d write"),
            idle("git commit-graph --object-dir d write"),
            // `--abbrev` takes an optional attached value only, so `-r` is still an option;
            // `-O` takes the next word as its value even when that word looks like an option, so
            // `-r` is not styled.
            idle("git diff-tree --abbrev -r HEAD"),
            idle("git diff-tree -O -r HEAD"),
            // `stash` is the only level here with a complete subcommand list, so only
            // `git stash frob` is an error. `repo` and `refs` are not complete, so an unknown word
            // gets no span. `backfill` has no subcommands and its option list is not complete.
            idle("git repo frob"),
            idle("git refs frob"),
            idle("git stash frob"),
            idle("git backfill --bogus"),
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
fn spec_docker_swarm_service() {
    snap(
        "spec_docker_swarm_service",
        &[
            idle("docker service create --replicas 3 nginx"),
            idle("docker service create --replicas 3 --name web nginx echo -n"),
            idle("docker service create -t nginx sleep -t 5"),
            idle("docker swarm init --advertise-addr x"),
            idle("docker stack deploy -c f.yml s"),
            idle("docker plugin ls --no-trunc"),
            // None of `swarm`, `stack`, and `plugin` has a complete subcommand list, so an
            // unknown word gets no span.
            idle("docker swarm frob"),
            idle("docker stack frob"),
            idle("docker plugin frob"),
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
fn spec_npm_systemctl_refresh() {
    snap(
        "spec_npm_systemctl_refresh",
        &[
            idle("npm trust list"),
            idle("npm trust github --file release.yml --repo o/r --allow-publish"),
            idle("npm trust gitlab --project g/p --env prod"),
            idle("npm trust revoke --id abc123"),
            // `trust` does not have a complete subcommand list, so no error is expected.
            idle("npm trust frob"),
            idle("npm undeprecate pkg"),
            idle("npm undeprecate pkg@1.2.3 --otp 123456"),
            idle("systemctl sleep"),
            idle("systemctl sleep --no-block"),
            // Not a verb in systemd 259, so with a complete verb list it is an error.
            idle("systemctl enqueue-marked-jobs"),
            idle("systemctl reload-or-restart --marked"),
        ],
    );
}

#[test]
fn spec_pip_uv() {
    snap(
        "spec_pip_uv",
        &[
            idle("pip --timeout 30 install -r requirements.txt --no-deps"),
            idle("pip3 --proxy http://proxy:3128 list --outdated"),
            idle("pip frobnicate"),
            idle("pip --python python3.12 -v show pytest"),
            idle("uv --directory src run --with pytest pytest -x"),
            idle("uv --color never add --dev pytest"),
            idle("uv --project proj pip install -r requirements.txt"),
            // A value option before the subcommand of a nested group.
            idle("uv tool --directory src ls"),
            idle("uv pip ls"),
            idle("uv pip frobnicate"),
            idle("uv tool run --from ruff ruff check -x"),
            idle("uv frobnicate"),
        ],
    );
}

#[test]
fn spec_go_rustup() {
    snap(
        "spec_go_rustup",
        &[
            idle("go -C src build -o out -tags netgo ./cmd"),
            idle("go test -run TestFoo -count=1 ./..."),
            idle("go run -race main.go -v"),
            idle("go mod tidy -v"),
            // `-C` may follow a command group, before its subcommand.
            idle("go mod -C src tidy"),
            idle("go mod frobnicate"),
            idle("go frobnicate"),
            idle("rustup +nightly toolchain list -v"),
            // Option parsing stops at the toolchain, so `cargo build --release` is plain.
            idle("rustup run --install nightly cargo build --release"),
            idle("rustup toolchain help"),
            idle("rustup target add --toolchain nightly wasm32-unknown-unknown"),
            idle("rustup toolchain frobnicate"),
            idle("rustup frobnicate"),
        ],
    );
}

#[test]
fn spec_ip_journalctl() {
    snap(
        "spec_ip_journalctl",
        &[
            idle("ip a"),
            idle("ip r"),
            idle("ip -br -c a"),
            idle("ip -n ns1 a"),
            // Only proves that `sh` (a prefix of `show`) is not a false error.
            idle("ip addr sh"),
            idle("ip l"),
            idle("ip li"),
            idle("ip n"),
            idle("ip -4 a"),
            idle("ip -s -s a"),
            idle("ip -color=always a"),
            idle("ip -f inet6 route show"),
            idle("ip frobnicate"),
            idle("journalctl -u nginx -n 50 --no-pager"),
            idle("journalctl -xeu nginx -p err --since today"),
            // The value of `-u` looks like an option and is still the value, so `-n` stays an option.
            idle("journalctl -u -n"),
            idle("journalctl -n50 --since=today"),
            idle("journalctl --bogus"),
        ],
    );
}

#[test]
fn spec_make_ssh_tar() {
    snap(
        "spec_make_ssh_tar",
        &[
            idle("make -C src -j4 all"),
            // `-j` takes an optional attached value, so `-k` is an option of its own.
            idle("make -j4 -k"),
            // `make` reads options between targets; `ssh` stops at the destination.
            idle("make all -k"),
            idle("ssh host -l"),
            idle("make -f Makefile.alt --just-print install"),
            idle("make --bogus all"),
            idle("ssh -p 2222 -i key.pem -o StrictHostKeyChecking=no user@host uptime -l"),
            idle("ssh -L 8080:localhost:80 -J jump host"),
            idle("ssh -Z host"),
            // The value of `-o` looks like an option and is still the value.
            idle("ssh -o -v host"),
            idle("ssh -p2222 host"),
            idle("tar -C src -czf out.tgz --exclude=node_modules ."),
            idle("tar -xvf out.tgz --strip-components 1"),
            idle("tar --bogus -tf out.tgz"),
            // The value of `-f` looks like an option and is still the value, so `-x` is not styled.
            idle("tar -f -x"),
        ],
    );
}

#[test]
fn spec_xargs_rm() {
    snap(
        "spec_xargs_rm",
        &[
            idle("xargs -0 -n1 rm -f"),
            idle("xargs -I {} mv {} {}.bak"),
            idle("xargs --max-args 1 rm"),
            idle("xargs -r -P 4 -d '\\n' rm -rf"),
            // `-i`, `--replace`, and `-I` differ in whether the value is attached or the next word.
            idle("xargs -i rm"),
            idle("xargs --replace rm"),
            idle("xargs -I{} rm"),
            idle("xargs -n1 -- rm"),
            // The first word that is not an option is the command, so `-0` after it is rm's.
            idle("xargs rm -0"),
            idle("xargs rm"),
            idle("xargs sudo rm -f"),
            idle("rm -rf no-such-dir --bogus"),
            idle("rm -v -- -notes.txt"),
        ],
    );
}

#[test]
fn spec_precommands_scheduling() {
    snap(
        "spec_precommands_scheduling",
        &[
            idle("doas -u root ls"),
            idle("chrt -f 10 ls"),
            idle("taskset -c 0 ls"),
            idle("ionice -c 3 ls"),
        ],
    );
}

#[test]
fn spec_wrapped_and_modes() {
    snap(
        "spec_wrapped_and_modes",
        &[
            // `kubectl exec` wraps the command after `--`; the wrapped command keeps its spec.
            // `ls` has none, so `-l` gets no span, as in `ls -la` at the top level; `rm` has one.
            idle("kubectl exec mypod -- ls -l"),
            idle("kubectl exec mypod -- rm -f notes.txt"),
            idle("kubectl exec -it -n ns mypod -- ls"),
            idle("kubectl --context prod exec mypod -- ls"),
            // The value of `-c` is the first `--`.
            idle("kubectl exec -c -- mypod -- ls"),
            // No `--`: no wrapped command, so `ls` is a plain argument.
            idle("kubectl exec mypod ls"),
            idle("kubectl exec mypod --"),
            idle("kubectl get pods -- ls"),
            // The wrapped command is looked up locally, so a command only in the pod is an error.
            idle("kubectl exec mypod -- nosuchcmd"),
            idle("kubectl exec mypod -- sudo -e notes.txt"),
            idle("kubectl exec pod -- sudo -e ~/.zshrc"),
            // `sudo -e` takes files: operands are checked as paths, and a missing one is no error.
            idle("sudo -e ~/.zshrc ~proj/Cargo.toml"),
            idle("sudo -e notes.txt docs nosuch.txt"),
            idle("sudo --edit notes.txt"),
            idle("sudo -Eu root -e notes.txt"),
            // In path mode an option value is a plain word and is path-checked too.
            idle("sudo -e -u docs notes.txt"),
            // Operands after the first file, and words after `--`, are not options.
            idle("sudo -e notes.txt -n -- -notes.txt"),
            idle("sudo -e"),
            // `e` is the value of `-u`: `ls` is the command.
            idle("sudo -ue ls"),
            idle("sudo ls"),
            idle("sudo -u root ls"),
            idle("timeout 5 ls"),
            // Typing.
            at_end("sudo -e not"),
            at_end("sudo -e "),
            at_end("sudo -e zzz"),
            at_end("sudo -e -u r"),
            at_end("kubectl exec mypod -- l"),
            at_end("kubectl exec mypod -- lsx"),
            at_end("kubectl exec mypod --"),
            at_end("kubectl exec mypod -- "),
        ],
    );
}

/// The literal acceptance case of `sudo -e`, against the host's `/etc/hosts`. Skipped where
/// that file does not exist.
#[test]
fn sudo_edit_etc_hosts() {
    if !Path::new("/etc/hosts").is_file() {
        eprintln!("skipped: /etc/hosts is not a file");
        return;
    }
    let mut f = Fixture::new("sudo_edit_etc_hosts");
    let got: Vec<_> = f
        .wire("", "sudo -e /etc/hosts", Some(0), "")
        .iter()
        .map(|s| (s.start, s.end, s.kind))
        .collect();
    assert_eq!(
        got,
        vec![
            (0, 4, TokenKind::Precommand),
            (5, 7, TokenKind::CmdOption),
            (8, 18, TokenKind::Path),
        ]
    );
}

/// A function shadowing a built-in precommand takes its place: the next word is a plain
/// argument, not a wrapped command.
#[test]
fn function_shadows_spec_precommand() {
    let mut f = Fixture::new("function_shadows_spec_precommand");
    f.h.state.apply_update(StateUpdate {
        functions: strings(&["mkcd", "gco", "nice", "noglob"]),
        ..Default::default()
    });
    let mut kinds = |buf: &str| -> Vec<(usize, usize, TokenKind)> {
        f.wire("", buf, Some(0), "")
            .iter()
            .map(|s| (s.start, s.end, s.kind))
            .collect()
    };
    assert_eq!(kinds("noglob ls"), vec![(0, 6, TokenKind::Function)]);
    assert_eq!(
        kinds("nice nosuchcmd arg"),
        vec![(0, 4, TokenKind::Function)]
    );
    assert_eq!(
        kinds("nice -n 5 ls"),
        vec![(0, 4, TokenKind::Function), (5, 7, TokenKind::CmdOption)]
    );
}

/// `builtin` accepts only builtins and `command` only external commands; `command -v` and `-V`
/// look names up. `time` and `echo` exist on PATH here, and a function `nice` shadows the
/// precommand except directly after `command`.
#[test]
fn wrapper_word_restrictions() {
    let mut f = Fixture::new("wrapper_word_restrictions");
    for exe in ["time", "echo"] {
        write_file(&f.root.join("bin").join(exe), 0o755);
    }
    f.h.state.apply_update(StateUpdate {
        functions: strings(&["mkcd", "gco", "nice"]),
        rehash: true,
        ..Default::default()
    });
    let cases = &[
        idle("builtin echo x"),
        idle("builtin ls"),
        idle("builtin mkcd"),
        idle("builtin time ls"),
        idle("builtin --"),
        idle("builtin git status notes.txt"),
        idle("builtin command ls"),
        idle("builtin builtin echo"),
        idle("builtin - ls"),
        idle("command ls"),
        idle("command mkcd"),
        idle("command ll"),
        idle("command cd"),
        idle("command typeset"),
        idle("command noglob ls"),
        idle("command builtin echo"),
        idle("command echo hi"),
        idle("command nice -n 5 ls"),
        idle("nice -n 5 ls"),
        idle("command time ls"),
        idle("command -x ls"),
        idle("command -- -v ls"),
        idle("command -pv mkcd ll cd time ls nosuchcmd"),
        idle("command -Vp git"),
        idle("command -v -- ls"),
        idle("command -v"),
        idle("builtin command -v mkcd"),
        idle("noglob command mkcd"),
        idle("command kubectl exec mypod -- mkcd"),
        idle("command $cmd notes.txt"),
        Case {
            opts: "a",
            ..idle("command docs")
        },
        at_end("command gi"),
        at_end("command mk"),
        at_end("builtin ec"),
        at_end("builtin gi"),
        at_end("command -v mk"),
        at_end("command ./bu"),
    ];
    insta::assert_snapshot!("wrapper_word_restrictions", f.cases(cases));
}

/// A user spec with a value-taking path-mode option:`--file x` consumes `x` as its value, and
/// the remaining words are path operands rather than a wrapped command.
#[test]
fn user_spec_value_taking_path_mode_option() {
    let spec =
        "name = \"w\"\nprecommand = true\noptions = [\"-v\"]\npath-mode-options = [\"--file=\"]\n";
    let mut f = Fixture::with_user_specs(
        "user_spec_value_taking_path_mode_option",
        &[("w.toml", spec)],
    );
    let w = f.h.specs.get("w").expect("user spec loaded");
    let tail = |line: &str| {
        let words: Vec<ArgInput<'_>> = line
            .split_whitespace()
            .map(|w| ArgInput {
                literal: Some(w),
                name_eq: false,
            })
            .collect();
        w.tail(&words)
    };
    assert_eq!(tail("--file x f"), Tail::Paths);
    assert_eq!(tail("-v --file=x f"), Tail::Paths);
    assert_eq!(tail("--file x"), Tail::Paths);
    assert_eq!(tail("-v ls"), Tail::Command(1));

    let mut kinds = |buf: &str| -> Vec<(usize, usize, TokenKind)> {
        f.wire("", buf, Some(0), "")
            .iter()
            .map(|s| (s.start, s.end, s.kind))
            .collect()
    };
    // `x` is the value of `--file` (missing, so no span); `notes.txt` and `docs` are operands.
    assert_eq!(
        kinds("w --file x notes.txt docs"),
        vec![
            (0, 1, TokenKind::Precommand),
            (2, 8, TokenKind::CmdOption),
            (11, 20, TokenKind::Path),
            (21, 25, TokenKind::PathDirectory),
        ]
    );
    // The value is the next word even when it looks like an option: `-v` is not `-v` here.
    assert_eq!(
        kinds("w --file -v notes.txt"),
        vec![
            (0, 1, TokenKind::Precommand),
            (2, 8, TokenKind::CmdOption),
            (12, 21, TokenKind::Path),
        ]
    );
    // `ls` is an operand here, not a wrapped command: a missing path, so no span.
    assert_eq!(
        kinds("w --file=x ls"),
        vec![(0, 1, TokenKind::Precommand), (2, 10, TokenKind::CmdOption)]
    );
    // Without the path-mode option, `ls` is the wrapped command.
    assert_eq!(
        kinds("w -v ls"),
        vec![
            (0, 1, TokenKind::Precommand),
            (2, 4, TokenKind::CmdOption),
            (5, 7, TokenKind::Command),
        ]
    );
}

#[test]
fn spec_option_abbreviations() {
    snap(
        "spec_option_abbreviations",
        &[
            // A unique prefix is the full option with its arity: `--dir` takes `src`.
            idle("tar --dir src --exclude-b -czf out.tgz ."),
            // The value of `--dir` looks like an option and is still the value.
            idle("tar --dir -x"),
            // `--fil` is `--file` or `--files-from`: ambiguous, so unknown (no span).
            idle("tar --fil out.tgz -x"),
            // An exact spelling wins over a longer option it is a prefix of.
            idle("tar --exclude-caches --exclude x -c"),
            idle("tar --strip=1 -xf out.tgz"),
            idle("xargs --max-a 1 --nu rm --rec"),
            // An ambiguous prefix is skipped as a flag; `rm` is still the command.
            idle("xargs --ver rm"),
            idle("timeout --sig KILL 5 ls"),
            idle("timeout --ver 5 ls"),
            idle("nice --adj 5 ls"),
            idle("env --defa ls"),
        ],
    );
}

#[test]
fn user_spec_option_abbreviations() {
    let spec = "name = \"tar\"\nmerge = true\noptions-complete = true\n";
    let mut f = Fixture::with_user_specs("user_spec_option_abbreviations", &[("tar.toml", spec)]);
    let out = f.cases(&[
        // Ambiguous and unmatched prefixes are errors where the option list is complete;
        // `--verb` is ambiguous too (`--verbose`, `--verbatim-files-from`).
        idle("tar --ver -x"),
        idle("tar --verb -x"),
        idle("tar --qqq -x"),
        idle("tar --verbo -x"),
        // At the cursor, an ambiguous prefix may still be typed into an option: no span.
        at_end("tar -x --ver"),
        at_end("tar -x --qqq"),
        at_end("tar -x --verbo"),
        Case {
            cursor: Some(12),
            ..at_end("tar -x --ver --qqq")
        },
    ]);
    insta::assert_snapshot!("user_spec_option_abbreviations", out);
}

#[test]
fn spec_assignments_with_expansions() {
    snap(
        "spec_assignments_with_expansions",
        &[
            // A value with an expansion is skipped like a literal one; the next word is the
            // wrapped command.
            idle("env FOO=$x ls"),
            idle("env -i FOO=\"$(date)\" BAR=1 ls"),
            idle("sudo -E env FOO=\"$(date)\" BAR=1 nice -n 5 make"),
            idle("env FOO=$x nosuchcmd"),
            idle("env $'FOO'=1 ls"),
            idle("env FOO=<(date) ls"),
            // Typing: the word after the assignment is the command word being typed (`l` is a
            // prefix of `ls`, so no span yet; `lsx` is a prefix of nothing, so an error), and a
            // trailing space leaves no wrapped command.
            at_end("env FOO=$x l"),
            at_end("env FOO=$x lsx"),
            at_end("env FOO=$x "),
            // Not `NAME=value`: an expansion in the name, a bare expansion, and a precommand
            // without `skip-assignments`. The word is the wrapped command.
            idle("env FOO$x=1 ls"),
            idle("env $cmd"),
            idle("nice FOO=$x ls"),
        ],
    );
}

/// `NAME=value` words with an expansion in the value are skipped like literal ones: `expanded`
/// highlights exactly as the same buffer with the expansion `from` replaced by plain text `to` of
/// the same length, apart from the spans inside the replaced region. Checked idle and with the
/// cursor at the end (typing). `wrapped` is the kind of the last word, the wrapped command, or
/// `None` when it has no span (a command prefix being typed, or no word yet).
#[test]
fn assignment_with_expansion_matches_literal_assignment() {
    let mut f = Fixture::new("assignment_with_expansion_matches_literal_assignment");
    let cmd = Some(TokenKind::Command);
    let err = Some(TokenKind::Error);
    for (expanded, from, to, cursor, wrapped) in [
        ("env FOO=$x ls", "$x", "xy", Some(0), cmd),
        ("env FOO=$x ls", "$x", "xy", None, cmd),
        ("env FOO=$x nosuchcmd", "$x", "xy", Some(0), err),
        ("env FOO=$x nosuchcmd", "$x", "xy", None, err),
        ("env FOO=$x l", "$x", "xy", None, None),
        ("env FOO=$x ", "$x", "xy", None, None),
        ("env FOO=\"$x\" ls", "$x", "xy", Some(0), cmd),
        ("env -u A FOO=${x}y BAR=1 ls", "${x}", "abcd", Some(0), cmd),
        (
            "sudo -E env FOO=\"$(date)\" BAR=1 nice -n 5 make",
            "$(date)",
            "abcdefg",
            Some(0),
            cmd,
        ),
    ] {
        assert_eq!(from.len(), to.len());
        let at = expanded.find(from).unwrap();
        let replaced = at..at + from.len();
        let literal = expanded.replacen(from, to, 1);
        // Spans entirely inside the replaced region differ; any span reaching outside it (the
        // quotes around a replaced value, for one) must match.
        let outside = |spans: Vec<WireSpan>| -> Vec<(usize, usize, TokenKind)> {
            spans
                .into_iter()
                .filter(|s| !(replaced.start <= s.start && s.end <= replaced.end))
                .map(|s| (s.start, s.end, s.kind))
                .collect()
        };
        let want = outside(f.wire("", &literal, cursor, ""));
        assert_eq!(
            outside(f.wire("", expanded, cursor, "")),
            want,
            "{expanded:?} cursor {cursor:?}"
        );
        let last = expanded.rsplit(' ').next().unwrap();
        let at = expanded.len() - last.len();
        match wrapped {
            Some(kind) => assert!(
                want.contains(&(at, expanded.len(), kind)),
                "{literal:?} cursor {cursor:?}: {want:?}"
            ),
            None => assert!(
                want.iter().all(|&(start, _, _)| start < at),
                "{literal:?} cursor {cursor:?}: {want:?}"
            ),
        }
    }
}

#[test]
fn spec_podman_gh() {
    snap(
        "spec_podman_gh",
        &[
            idle("podman --connection remote ps -a"),
            idle("podman --root /var/tmp/store run --rm -it -e A=1 fedora ls -l"),
            idle("podman container frobnicate"),
            idle("podman frobnicate"),
            idle("gh pr -R owner/repo list --state open"),
            // `-R` is inherited from the group, so it is declared on the group and each subcommand.
            idle("gh issue -R owner/repo list"),
            idle("gh issue list -R owner/repo"),
            idle("gh run -R o/r view 12"),
            idle("gh pr frobnicate"),
            idle("gh issue view 12 --web"),
            idle("gh frobnicate"),
        ],
    );
}

#[test]
fn spec_apt_dnf_brew() {
    snap(
        "spec_apt_dnf_brew",
        &[
            idle("apt -o Dpkg::Options::=--force-confold install -y curl"),
            idle("apt -t bookworm-backports install --no-install-recommends vim"),
            idle("apt auto-remove -y"),
            idle("apt frobnicate"),
            idle("dnf --releasever 40 install -y nginx"),
            idle("dnf5 in -y nginx"),
            idle("dnf up"),
            idle("dnf grp list"),
            // The subcommands of a group declare no options (a documented limitation of the
            // spec), so `--with-optional` gets no span.
            idle("dnf group install --with-optional tools"),
            idle("dnf group frobnicate"),
            idle("dnf frobnicate"),
            idle("brew install --cask --appdir /Applications firefox"),
            idle("brew services restart --all"),
            idle("brew services frobnicate"),
            idle("brew frobnicate"),
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
