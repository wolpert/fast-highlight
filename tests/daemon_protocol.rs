//! End-to-end tests of `fast-highlight serve` over its standard input and output.

use fast_highlight::protocol::{
    Decoder, Frame, HighlightFields, StateUpdate, encode_frame, encode_highlight, encode_ping,
    encode_quit, encode_state,
};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_fast-highlight");
const TIMEOUT: Duration = Duration::from_secs(5);

/// A scratch directory unique to one test, under Cargo's per-target temp directory.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("daemon_protocol")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Points every directory the daemon might touch into `dir`.
fn isolate(cmd: &mut Command, dir: &std::path::Path) {
    cmd.env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env(
            "FAST_HIGHLIGHT_CONFIG_DIR",
            dir.join("config/fast-highlight"),
        )
        .env("HOME", dir);
}

/// A running daemon. Frames it writes are decoded on a background thread, so a hung daemon
/// fails a test with a timeout instead of hanging it.
struct Daemon {
    child: Child,
    stdin: Option<ChildStdin>,
    frames: Receiver<Result<Frame, String>>,
    log: PathBuf,
    /// The scratch directory, which is also the directory the daemon was started in.
    dir: PathBuf,
}

impl Daemon {
    fn spawn(name: &str, extra: &[&str]) -> Daemon {
        let dir = scratch(name);
        let log = dir.join("daemon.log");
        let mut cmd = Command::new(BIN);
        cmd.arg("serve")
            .arg("--log")
            .arg(&log)
            .args(extra)
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        isolate(&mut cmd, &dir);
        let mut child = cmd.spawn().expect("spawn daemon");
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut decoder = Decoder::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = match stdout.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                decoder.feed(&buf[..n]);
                loop {
                    match decoder.next_frame() {
                        Ok(Some(frame)) => {
                            if tx.send(Ok(frame)).is_err() {
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            let _ = tx.send(Err(e.to_string()));
                            return;
                        }
                    }
                }
            }
            if decoder.pending() > 0 {
                let _ = tx.send(Err("partial frame at end of output".to_string()));
            }
        });
        Daemon {
            child,
            stdin,
            frames: rx,
            log,
            dir,
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }

    fn recv(&self) -> Frame {
        match self.frames.recv_timeout(TIMEOUT) {
            Ok(Ok(frame)) => frame,
            Ok(Err(e)) => panic!("bad daemon output: {e}"),
            Err(e) => panic!("no frame from daemon: {e}"),
        }
    }

    fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Waits for the daemon to exit and checks it wrote nothing more to stdout or stderr.
    fn wait(mut self) -> ExitStatus {
        let deadline = Instant::now() + TIMEOUT;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("daemon did not exit");
            }
            thread::sleep(Duration::from_millis(10));
        };
        match self.frames.recv_timeout(TIMEOUT) {
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
            other => panic!("unexpected output after the last response: {other:?}"),
        }
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert_eq!(stderr, "", "daemon wrote to stderr");
        status
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn highlight(id: u64, buf: &str) -> Vec<u8> {
    encode_highlight(
        id,
        &HighlightFields {
            buffer: buf.as_bytes().to_vec(),
            opts: "u".to_string(),
            ..HighlightFields::default()
        },
    )
}

/// Parses an `R` body into `(start, end, kind)` triples, checking the line grammar.
fn result_spans(body: &[u8]) -> Vec<(usize, usize, String)> {
    let text = std::str::from_utf8(body).expect("R body is ASCII");
    assert!(text.is_empty() || text.ends_with('\n'), "{text:?}");
    text.lines()
        .map(|line| {
            let parts: Vec<_> = line.split(' ').collect();
            assert_eq!(parts.len(), 3, "bad span line {line:?}");
            let num = |s: &str| {
                assert!(s == "0" || !s.starts_with('0'), "leading zero in {line:?}");
                s.parse::<usize>().expect("number")
            };
            let kind = parts[2];
            assert!(
                !kind.is_empty() && kind.chars().all(|c| c.is_ascii_alphabetic() || c == '-'),
                "bad kind in {line:?}"
            );
            (num(parts[0]), num(parts[1]), kind.to_string())
        })
        .collect()
}

#[test]
fn ping_gets_ack() {
    let mut d = Daemon::spawn("ping", &[]);
    d.send(&encode_ping(1));
    let f = d.recv();
    assert_eq!((f.kind, f.id, f.body.len()), (b'A', 1, 0));
    d.send(&encode_ping(18446744073709551615));
    assert_eq!(d.recv().id, u64::MAX);
    d.send(&encode_quit(3));
    assert!(d.wait().success());
}

#[test]
fn quit_exits_without_response() {
    let mut d = Daemon::spawn("quit", &[]);
    d.send(&encode_quit(1));
    // Requests after Q are never answered: the daemon is gone. A write may fail with EPIPE.
    let _ = d.stdin.as_mut().unwrap().write_all(&encode_ping(2));
    assert!(d.wait().success());
}

#[test]
fn eof_exits() {
    let mut d = Daemon::spawn("eof", &[]);
    d.send(&encode_ping(1));
    assert_eq!(d.recv().kind, b'A');
    d.close_stdin();
    assert!(d.wait().success());
}

#[test]
fn eof_inside_a_frame_exits() {
    let mut d = Daemon::spawn("eof-partial", &[]);
    d.send(&encode_ping(1)[..5]);
    d.close_stdin();
    assert!(d.wait().success());
}

#[test]
fn framing_error_exits() {
    let mut d = Daemon::spawn("framing", &[]);
    d.send(&encode_ping(1));
    assert_eq!(d.recv().kind, b'A');
    d.send(b"FH1 P 01 0\n");
    let status = d.wait();
    assert!(!status.success());
}

#[test]
fn framing_error_is_logged() {
    let mut d = Daemon::spawn("framing-log", &[]);
    d.send(b"hello\n");
    let log = d.log.clone();
    assert!(!d.wait().success());
    let text = std::fs::read_to_string(log).unwrap_or_default();
    assert!(text.contains("framing error"), "log: {text}");
}

#[test]
fn invalid_requests_get_error_and_daemon_continues() {
    let mut d = Daemon::spawn("invalid", &[]);
    d.send(&encode_frame(b'X', 5, b"body"));
    let f = d.recv();
    assert_eq!((f.kind, f.id), (b'E', 5));
    d.send(&encode_frame(b'H', 6, b"buf 3\nab\n"));
    let f = d.recv();
    assert_eq!((f.kind, f.id), (b'E', 6));
    assert!(std::str::from_utf8(&f.body).is_ok());
    d.send(&encode_ping(7));
    assert_eq!((d.recv().kind, 7), (b'A', 7));
    d.send(&encode_quit(8));
    assert!(d.wait().success());
}

/// A highlight and a state update are answered with `R` and `A`, and the daemon keeps serving.
#[test]
fn highlight_gets_a_response_and_daemon_survives() {
    let mut d = Daemon::spawn("highlight", &[]);
    d.send(&highlight(1, "ls ~ | grep 日本"));
    let f = d.recv();
    assert_eq!((f.kind, f.id), (b'R', 1));
    let spans = result_spans(&f.body);
    let len = "ls ~ | grep 日本".chars().count();
    assert!(
        spans.iter().all(|(s, e, _)| s < e && *e <= len),
        "{spans:?}"
    );
    assert!(
        spans.contains(&(5, 6, "separator".to_string())),
        "{spans:?}"
    );
    let update = StateUpdate {
        path: Some("/usr/bin:/bin".into()),
        ..StateUpdate::default()
    };
    d.send(&encode_state(2, &update));
    let f = d.recv();
    assert_eq!((f.kind, f.id), (b'A', 2));
    d.send(&encode_ping(3));
    assert_eq!(d.recv().kind, b'A');
    d.send(&encode_quit(4));
    assert!(d.wait().success());
}

#[test]
fn hard_cap_returns_empty_result() {
    // The default hard cap is 64 KiB; the engine is never consulted above it.
    let mut d = Daemon::spawn("hard-cap", &[]);
    let at_cap = format!("echo '{}'", "x".repeat(64 * 1024 - 7));
    d.send(&highlight(1, &at_cap));
    let f = d.recv();
    assert_eq!((f.kind, f.id), (b'R', 1));
    assert!(!f.body.is_empty(), "lex-only spans at the cap");
    let over_cap = format!("{at_cap} ");
    d.send(&highlight(2, &over_cap));
    let f = d.recv();
    assert_eq!((f.kind, f.id, f.body.len()), (b'R', 2, 0));
    d.send(&encode_quit(3));
    assert!(d.wait().success());
}

#[test]
fn result_is_cut_to_max_spans() {
    // 1500 commands and separators: the default limit of 500 spans applies.
    let mut d = Daemon::spawn("max-spans", &[]);
    let buf = ": ;".repeat(1500);
    d.send(&highlight(1, &buf));
    let f = d.recv();
    let spans = result_spans(&f.body);
    assert_eq!(spans.len(), 500);
    d.send(&encode_quit(2));
    assert!(d.wait().success());
}

/// The daemon leaves the directory it was started in (so that file system can be unmounted)
/// but keeps resolving relative paths against it until a request names another.
#[cfg(target_os = "linux")]
#[test]
fn daemon_runs_in_root_but_remembers_its_start_directory() {
    let mut d = Daemon::spawn("chdir", &[]);
    std::fs::write(d.dir.join("notes.txt"), "").unwrap();
    d.send(&encode_ping(1));
    assert_eq!(d.recv().kind, b'A');
    let cwd = std::fs::read_link(format!("/proc/{}/cwd", d.child.id())).unwrap();
    assert_eq!(cwd, PathBuf::from("/"));
    d.send(&highlight(2, "cat notes.txt"));
    let spans = result_spans(&d.recv().body);
    assert!(spans.contains(&(4, 13, "path".to_string())), "{spans:?}");
    d.send(&encode_quit(3));
    assert!(d.wait().success());
}

#[test]
fn relative_log_path_is_in_the_start_directory() {
    let dir = scratch("relative-log");
    let mut cmd = Command::new(BIN);
    cmd.args(["serve", "--timing", "--log", "rel.log"])
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    isolate(&mut cmd, &dir);
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(&encode_ping(1)).unwrap();
    stdin.write_all(&encode_quit(2)).unwrap();
    drop(stdin);
    assert!(child.wait().unwrap().success());
    let log = std::fs::read_to_string(dir.join("rel.log")).unwrap();
    assert!(log.contains("id=1 type=P"), "{log}");
}

#[test]
fn malformed_named_dirs_still_ack_and_apply_the_rest() {
    let mut d = Daemon::spawn("nameddirs", &[]);
    let body = b"alias 5\nzzfh\0\nnameddirs 2\nx\0\n";
    d.send(&encode_frame(b'S', 1, body));
    assert_eq!(d.recv().kind, b'A');
    d.send(&highlight(2, "zzfh x"));
    let spans = result_spans(&d.recv().body);
    assert!(spans.contains(&(0, 4, "alias".to_string())), "{spans:?}");
    d.send(&encode_quit(3));
    let log = d.log.clone();
    assert!(d.wait().success());
    let text = std::fs::read_to_string(log).unwrap();
    assert!(text.contains("nameddirs"), "log: {text}");
}

/// The latency target, measured by the daemon itself: 300 requests for a buffer of about 200
/// characters, sent one at a time as the plugin does, with the per-request times taken from
/// the `--timing` log. Release builds only; debug builds are far slower.
#[cfg(not(debug_assertions))]
#[test]
fn latency_for_a_200_char_buffer() {
    let mut d = Daemon::spawn("latency", &["--timing"]);
    for file in ["notes.txt", "Cargo.toml", "src/main.rs"] {
        let path = d.dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }
    // Let the startup PATH scan finish between requests, as it does in a shell.
    d.send(&encode_ping(1));
    assert_eq!(d.recv().kind, b'A');
    let mut buf = String::new();
    while buf.chars().count() < 200 {
        buf.push_str("git commit -m \"$msg\" notes.txt && ls -la src/ | grep -v '^d' > out; ");
    }
    buf.truncate(200);
    let n = 300;
    for id in 2..2 + n {
        let fields = HighlightFields {
            buffer: buf.as_bytes().to_vec(),
            cwd: (id == 2).then(|| d.dir.to_string_lossy().as_bytes().to_vec()),
            opts: "u".to_string(),
            ..HighlightFields::default()
        };
        d.send(&encode_highlight(id, &fields));
        assert_eq!(d.recv().kind, b'R');
    }
    d.send(&encode_quit(n + 2));
    let log = d.log.clone();
    assert!(d.wait().success());
    let text = std::fs::read_to_string(log).unwrap();
    let mut times: Vec<u64> = text
        .lines()
        .filter(|line| line.contains(" type=H "))
        .map(|line| {
            let us = line.rsplit("us=").next().unwrap();
            us.trim().parse().unwrap()
        })
        .collect();
    assert_eq!(times.len(), n as usize, "{text}");
    times.sort_unstable();
    let median = times[times.len() / 2];
    let p99 = times[times.len() * 99 / 100];
    assert!(median < 1000, "median {median} us, p99 {p99} us");
    assert!(p99 < 2000, "median {median} us, p99 {p99} us");
}

#[test]
fn responses_keep_request_order() {
    let mut d = Daemon::spawn("order", &[]);
    let mut batch = Vec::new();
    for id in 1..=20 {
        if id % 3 == 0 {
            batch.extend(encode_frame(b'Z', id, b""));
        } else {
            batch.extend(encode_ping(id));
        }
    }
    d.send(&batch);
    for id in 1..=20 {
        let f = d.recv();
        assert_eq!(f.id, id);
        assert_eq!(f.kind, if id % 3 == 0 { b'E' } else { b'A' });
    }
    d.close_stdin();
    assert!(d.wait().success());
}

#[test]
fn timing_lines_are_logged() {
    let mut d = Daemon::spawn("timing", &["--timing"]);
    d.send(&encode_ping(41));
    assert_eq!(d.recv().id, 41);
    d.send(&encode_quit(42));
    let log = d.log.clone();
    assert!(d.wait().success());
    let text = std::fs::read_to_string(log).unwrap();
    assert!(
        text.contains("id=41 type=P bytes=0 spans=0 us="),
        "log: {text}"
    );
}

#[test]
fn highlight_returns_spans() {
    let mut d = Daemon::spawn("spans", &[]);
    let dir = scratch("spans-cwd");
    let fields = HighlightFields {
        buffer: "echo \"日本\" $HOME".as_bytes().to_vec(),
        cwd: Some(dir.to_string_lossy().as_bytes().to_vec()),
        opts: "u".to_string(),
        ..HighlightFields::default()
    };
    d.send(&encode_highlight(1, &fields));
    let f = d.recv();
    assert_eq!((f.kind, f.id), (b'R', 1));
    let spans = result_spans(&f.body);
    assert!(
        spans.contains(&(5, 9, "double-quoted".to_string())),
        "{spans:?}"
    );
    assert!(
        spans.contains(&(10, 15, "parameter".to_string())),
        "{spans:?}"
    );
    // Sorted by start ascending, then end descending.
    for pair in spans.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        assert!(a.0 < b.0 || (a.0 == b.0 && a.1 >= b.1), "{spans:?}");
    }
    d.send(&encode_quit(2));
    assert!(d.wait().success());
}

#[test]
fn prebuffer_spans_are_clipped() {
    let mut d = Daemon::spawn("prebuffer", &[]);
    let fields = HighlightFields {
        prebuffer: b"echo \"abc\n".to_vec(),
        buffer: "日\" x".as_bytes().to_vec(),
        opts: "u".to_string(),
        ..HighlightFields::default()
    };
    d.send(&encode_highlight(1, &fields));
    let f = d.recv();
    assert_eq!(f.kind, b'R');
    let spans = result_spans(&f.body);
    assert!(
        spans.contains(&(0, 2, "double-quoted".to_string())),
        "{spans:?}"
    );
    assert!(spans.iter().all(|(_, e, _)| *e <= 4), "{spans:?}");
    d.send(&encode_quit(2));
    assert!(d.wait().success());
}

fn process_gone(pid: libc::pid_t) -> bool {
    // A reparented daemon that exited may linger briefly as a zombie until init reaps it;
    // count that as gone.
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        && let Some(state) = stat
            .rsplit(')')
            .next()
            .and_then(|s| s.split_whitespace().next())
        && state == "Z"
    {
        return true;
    }
    // SAFETY: kill with signal 0 only checks for the process's existence.
    unsafe { libc::kill(pid, 0) != 0 }
}

#[test]
fn exits_when_parent_dies() {
    let dir = scratch("parent-death");
    // The daemon's stdin is the test's pipe (via fd 3, because an asynchronous list in a
    // non-interactive shell gets /dev/null as stdin), so it never sees EOF; only the death of
    // its parent shell can stop it.
    let script = r#"exec 3<&0; "$0" serve --log "$1" <&3 >/dev/null 2>&1 & echo $!; sleep 0.3"#;
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(script)
        .arg(BIN)
        .arg(dir.join("daemon.log"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    isolate(&mut cmd, &dir);
    let mut sh = cmd.spawn().expect("spawn sh");
    let keep_stdin_open = sh.stdin.take();
    let mut out = String::new();
    sh.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    assert!(sh.wait().unwrap().success());
    let pid: libc::pid_t = out.trim().parse().expect("daemon pid");
    assert!(pid > 0);

    let deadline = Instant::now() + Duration::from_millis(2500);
    while !process_gone(pid) {
        if Instant::now() > deadline {
            // SAFETY: plain kill of the stray daemon.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            panic!("daemon {pid} still running after its parent exited");
        }
        thread::sleep(Duration::from_millis(50));
    }
    // The daemon stopped because of the parent check, not because its input closed.
    let log = std::fs::read_to_string(dir.join("daemon.log")).unwrap_or_default();
    assert!(log.contains("parent process exited"), "log: {log}");
    drop(keep_stdin_open);
}

/// Spawns a `sleep` to stand in for the shell `--parent` names. The daemon's real parent is the
/// test process, which outlives it, so only the `--parent` check can stop the daemon.
fn stand_in_shell() -> Child {
    Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep")
}

fn assert_exits_for_parent(d: Daemon) {
    let log = d.log.clone();
    // The parent check runs at least once per second.
    let status = d.wait();
    assert!(!status.success(), "{status:?}");
    let text = std::fs::read_to_string(log).unwrap_or_default();
    assert!(text.contains("parent process exited"), "log: {text}");
}

#[test]
fn exits_when_watched_parent_dies() {
    let mut shell = stand_in_shell();
    let pid = shell.id().to_string();
    let mut d = Daemon::spawn("watched-parent", &["--parent", &pid]);
    d.send(&encode_ping(1));
    assert_eq!(d.recv().kind, b'A');
    shell.kill().unwrap();
    // Reap it: a zombie still exists as far as kill(pid, 0) is concerned.
    shell.wait().unwrap();
    assert_exits_for_parent(d);
}

#[test]
fn exits_when_watched_parent_died_before_startup() {
    // The race `--parent` exists for: the shell is already gone when the daemon starts.
    let mut shell = stand_in_shell();
    let pid = shell.id().to_string();
    shell.kill().unwrap();
    shell.wait().unwrap();
    let d = Daemon::spawn("watched-parent-early", &["--parent", &pid]);
    assert_exits_for_parent(d);
}

#[test]
fn survives_sigint() {
    let mut d = Daemon::spawn("sigint", &[]);
    d.send(&encode_ping(1));
    assert_eq!(d.recv().kind, b'A');
    // SAFETY: sending SIGINT to our own child.
    unsafe { libc::kill(d.child.id() as libc::pid_t, libc::SIGINT) };
    thread::sleep(Duration::from_millis(100));
    d.send(&encode_ping(2));
    assert_eq!(d.recv().id, 2);
    d.send(&encode_quit(3));
    assert!(d.wait().success());
}
