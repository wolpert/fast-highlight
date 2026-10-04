//! Runs the zpty-driven plugin suite in `tests/zsh/` against the built binary.
//!
//! The suite starts interactive zsh sessions in a pseudo-terminal, types into them, and checks
//! `region_highlight`, daemon restarts after crashes and hangs, cleanup on exit, and that the
//! plugin never writes to the terminal. Failure-mode tests use `tests/fixtures/mock-daemon.py`;
//! the rest use the real binary. The test is skipped when `zsh` or `python3` is not installed.

use std::path::Path;
use std::process::Command;

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn zsh_plugin_suite() {
    if !have("zsh") || !have("python3") {
        eprintln!("skipping: zsh and python3 are required");
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("zsh")
        .arg(root.join("tests/zsh/run.zsh"))
        .env("FASTHL_TEST_BIN", env!("CARGO_BIN_EXE_fast-highlight"))
        .current_dir(root)
        .output()
        .expect("run tests/zsh/run.zsh");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "zsh plugin suite failed\n{stdout}\n{stderr}"
    );
}
