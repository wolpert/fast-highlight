//! Fuzzes the full semantic pass, from request bytes to wire spans, the way the daemon runs it.
//!
//! Input: a 5-byte header, then the text.
//!
//! - byte 0, option bits: 0 `interactive_comments`, 1 `extended_glob`, 2 `ksh_glob`,
//!   3 `auto_cd`, 4 cursor in characters (else bytes), 5 use tight limits (lex-only above 64
//!   bytes, 2 filesystem checks per request) instead of the defaults, 6 `no_equals`, 7 a sixth
//!   header byte follows
//! - bytes 1-2, little endian: where the text splits into `PREBUFFER` and `BUFFER`, taken modulo
//!   the text length plus one
//! - bytes 3-4, little endian: the cursor; `0xffff` means no cursor (end of buffer)
//! - byte 5, with bit 7 of byte 0 only: more parse option bits, 0 `ignore_braces`,
//!   1 `ignore_close_braces`, 2 `rc_quotes`, 3 `ksh_arrays`, 4 `posix_identifiers`,
//!   5 `sh_glob`, 6 `brace_ccl`, 7 `no_short_loops`
//!
//! Asserts that highlighting never panics, that the cursor offset and the spans are valid for
//! the decoded text, and that the wire spans in both units are non-empty, within the buffer's
//! length in that unit, sorted, and well nested.
//!
//! The highlighter runs in a scratch directory under the system temp dir holding a few files,
//! directories, and executables, with `HOME` and `FAST_HIGHLIGHT_CONFIG_DIR` pointing into it so
//! no real configuration is read. The directory is not removed when the process exits.

#![no_main]

use fast_highlight::config::{Config, Limits};
use fast_highlight::daemon::new_highlighter;
use fast_highlight::highlight::{HighlightRequest, Highlighter, RequestOptions};
use fast_highlight::protocol::StateUpdate;
use fast_highlight::syntax::ParseOptions;
use fast_highlight::text::{RequestText, Unit, check_wire_spans};
use fast_highlight::token::check_spans;
use libfuzzer_sys::fuzz_target;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

struct Env {
    cwd: PathBuf,
    default: Mutex<Highlighter>,
    tight: Mutex<Highlighter>,
}

static ENV: LazyLock<Env> = LazyLock::new(setup);

fn write_executable(path: &Path) {
    fs::write(path, "#!/bin/sh\n").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn setup() -> Env {
    let root = std::env::temp_dir().join(format!("fast-highlight-fuzz-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let cwd = root.join("work");
    let home = root.join("home");
    let config = root.join("config");
    let bin = root.join("bin");
    for dir in [
        &cwd,
        &home,
        &config,
        &bin,
        &cwd.join("dir/sub"),
        &home.join("docs"),
    ] {
        fs::create_dir_all(dir).unwrap();
    }
    for file in ["file.txt", "a b", "dir/sub/x.rs", "é.md"] {
        fs::write(cwd.join(file), "").unwrap();
    }
    write_executable(&cwd.join("run.sh"));
    for name in ["ls", "cat", "grep", "git", "sudo", "env", "docker", "echo"] {
        write_executable(&bin.join(name));
    }

    // SAFETY: runs once, before any highlighter exists, and libFuzzer drives this target from a
    // single thread, so nothing reads the environment concurrently.
    unsafe {
        std::env::set_var("FAST_HIGHLIGHT_CONFIG_DIR", &config);
        std::env::set_var("HOME", &home);
        std::env::set_var("PATH", &bin);
    }

    let state = || StateUpdate {
        aliases: Some(vec!["ll".into(), "g".into()]),
        global_aliases: Some(vec!["G".into()]),
        suffix_aliases: Some(vec!["txt".into()]),
        functions: Some(vec!["myfunc".into()]),
        named_dirs: Some(vec![("proj".into(), cwd.to_string_lossy().into_owned())]),
        ..StateUpdate::default()
    };
    let build = |config: Config| {
        let (mut h, _warnings) = new_highlighter(&config);
        h.state.apply_update(state());
        Mutex::new(h)
    };
    let tight = Config {
        limits: Limits {
            lex_only_bytes: 64,
            max_path_checks: 2,
            ..Limits::default()
        },
        ..Config::default()
    };
    Env {
        default: build(Config::default()),
        tight: build(tight),
        cwd,
    }
}

/// The parse options of the extra option byte (see the input layout above).
fn more_options(opts: ParseOptions, bits: u8) -> ParseOptions {
    ParseOptions {
        ignore_braces: bits & 1 != 0,
        ignore_close_braces: bits & 2 != 0,
        rc_quotes: bits & 4 != 0,
        ksh_arrays: bits & 8 != 0,
        posix_identifiers: bits & 16 != 0,
        sh_glob: bits & 32 != 0,
        brace_ccl: bits & 64 != 0,
        no_short_loops: bits & 128 != 0,
        ..opts
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((header, mut rest)) = data.split_first_chunk::<5>() else {
        return;
    };
    let flags = header[0];
    let mut more = 0;
    if flags & 128 != 0 {
        let Some((&m, text)) = rest.split_first() else {
            return;
        };
        (more, rest) = (m, text);
    }
    let split = usize::from(u16::from_le_bytes([header[1], header[2]])) % (rest.len() + 1);
    let cursor = match u16::from_le_bytes([header[3], header[4]]) {
        u16::MAX => None,
        c => Some(usize::from(c)),
    };
    let (prebuffer, buffer) = rest.split_at(split);
    let parse = ParseOptions {
        interactive_comments: flags & 1 != 0,
        extended_glob: flags & 2 != 0,
        ksh_glob: flags & 4 != 0,
        ..ParseOptions::default()
    };
    let opts = RequestOptions {
        parse: more_options(parse, more),
        auto_cd: flags & 8 != 0,
        no_equals: flags & 64 != 0,
    };
    let cursor_unit = Unit::from_char_offsets(flags & 16 != 0);
    let env = &*ENV;
    let highlighter = if flags & 32 != 0 {
        &env.tight
    } else {
        &env.default
    };

    let text = RequestText::new(prebuffer, buffer);
    let s = text.as_str();
    let bs = text.buffer_start();
    assert!(
        bs <= s.len() && s.is_char_boundary(bs),
        "bad buffer start {bs}"
    );
    let cursor = text.cursor_offset(cursor, cursor_unit);
    assert!(
        cursor >= bs && cursor <= s.len() && s.is_char_boundary(cursor),
        "bad cursor offset {cursor} (buffer start {bs}, len {})",
        s.len()
    );

    let req = HighlightRequest {
        text: s,
        buffer_start: bs,
        cursor,
        cwd: &env.cwd,
        opts,
    };
    let spans = highlighter.lock().unwrap().highlight(&req);
    if let Err(e) = check_spans(s, &spans) {
        panic!("check_spans failed: {e}\ntext: {s:?}\nspans: {spans:?}");
    }

    for (unit, len) in [
        (Unit::Bytes, buffer.len()),
        (Unit::Chars, s[bs..].chars().count()),
    ] {
        let wire = text.wire_spans(&spans, unit);
        if let Err(e) = check_wire_spans(&wire, len) {
            panic!(
                "{unit:?} wire spans invalid: {e}\ntext: {s:?}\nspans: {spans:?}\nwire: {wire:?}"
            );
        }
    }
});
