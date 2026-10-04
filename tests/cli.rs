//! Command-line behaviour that needs a real process: output to a closed pipe.

use std::io::Read;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_fast-highlight");

/// Runs the binary with `args`, its standard output a pipe whose read end is already closed, so
/// every write to it fails with EPIPE (or raises SIGPIPE). Returns the exit status and what it
/// wrote to standard error.
fn run_into_closed_pipe(args: &[&str]) -> (std::process::ExitStatus, String) {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cli");
    std::fs::create_dir_all(&dir).unwrap();
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let mut child = Command::new(BIN)
        .args(args)
        .env("FAST_HIGHLIGHT_CONFIG_DIR", dir.join("config"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .stdin(Stdio::null())
        .stdout(writer)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    (child.wait().unwrap(), stderr)
}

#[test]
fn closed_stdout_ends_quietly() {
    for args in [
        &["check-config"][..],
        &["styles"],
        &["help"],
        &["--version"],
        &["plugin-path"],
        &["highlight", "ls", "-la"],
        &["highlight", "--format", "ansi", "ls"],
    ] {
        let (status, stderr) = run_into_closed_pipe(args);
        assert!(!stderr.contains("panicked"), "{args:?}: {stderr}");
        assert!(
            status.success() || status.signal() == Some(libc::SIGPIPE),
            "{args:?}: {status:?}, stderr: {stderr}"
        );
    }
}
