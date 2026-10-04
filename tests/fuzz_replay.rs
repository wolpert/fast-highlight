//! Replays every seed in `fuzz/seeds/{lexer,highlight,protocol}` through the invariants the
//! cargo-fuzz targets assert, so `cargo test` catches a regression on a seed without a nightly
//! toolchain.
//!
//! Each replay decodes its input exactly as the matching target under `fuzz/fuzz_targets/` does
//! (see the comment at the top of each target). Two differences, both because a test process
//! runs tests on several threads and has no `arbitrary` dependency:
//!
//! - The highlight replay builds its highlighter from an explicit state instead of setting
//!   `HOME`, `PATH`, and `FAST_HIGHLIGHT_CONFIG_DIR` in the environment.
//! - Protocol seeds in round-trip mode (odd first byte) need `arbitrary` to become requests;
//!   their bytes are replayed in stream mode instead, which checks the decoder on them all the
//!   same.

use fast_highlight::config::{Config, Limits};
use fast_highlight::highlight::{HighlightRequest, Highlighter, RequestOptions};
use fast_highlight::paths::PathChecker;
use fast_highlight::protocol::{Decoder, FramingError, Request, StateUpdate, encode_request};
use fast_highlight::specs::SpecRegistry;
use fast_highlight::state::ShellState;
use fast_highlight::syntax::{ParseOptions, Word, parse};
use fast_highlight::text::{self, RequestText, Unit, check_wire_spans};
use fast_highlight::token::check_spans;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The seed files of one target, sorted by name, with their contents.
fn seeds(target: &str) -> Vec<(String, Vec<u8>)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fuzz/seeds")
        .join(target);
    let mut out: Vec<_> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, fs::read(&p).unwrap())
        })
        .collect();
    out.sort();
    assert!(!out.is_empty(), "no seeds in {}", dir.display());
    out
}

fn check_word(input: &str, w: &Word, what: &str) -> Result<(), String> {
    if w.start >= w.end {
        return Err(format!("{what} word {w:?} is empty"));
    }
    if w.end > input.len() {
        return Err(format!(
            "{what} word {w:?} out of bounds (len {})",
            input.len()
        ));
    }
    if !input.is_char_boundary(w.start) || !input.is_char_boundary(w.end) {
        return Err(format!("{what} word {w:?} not on a char boundary"));
    }
    Ok(())
}

/// `fuzz_targets/lexer.rs`.
fn replay_lexer(data: &[u8]) -> Result<(), String> {
    let Some((&flags, rest)) = data.split_first() else {
        return Ok(());
    };
    let opts = ParseOptions {
        interactive_comments: flags & 1 != 0,
        extended_glob: flags & 2 != 0,
        ksh_glob: flags & 4 != 0,
    };
    let input = if flags & 8 != 0 {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        text::decode(rest)
    };
    let out = parse(&input, &opts);
    check_spans(&input, &out.spans).map_err(|e| format!("check_spans: {e}"))?;
    let mut prev_start = 0;
    for cmd in &out.commands {
        let first = cmd.words.first().ok_or("simple command without words")?;
        if first.start < prev_start {
            return Err(format!(
                "commands out of order: command word at {} after one at {prev_start}",
                first.start
            ));
        }
        prev_start = first.start;
        for w in &cmd.words {
            check_word(&input, w, "command")?;
        }
    }
    for w in &out.path_words {
        check_word(&input, w, "path")?;
    }
    Ok(())
}

#[test]
fn lexer_seeds() {
    for (name, data) in seeds("lexer") {
        if let Err(e) = replay_lexer(&data) {
            panic!("lexer seed {name}: {e}");
        }
    }
}

fn write_executable(path: &Path) {
    fs::write(path, "#!/bin/sh\n").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// The scratch directory and highlighters of `fuzz_targets/highlight.rs`.
struct HighlightEnv {
    cwd: PathBuf,
    default: Highlighter,
    tight: Highlighter,
}

fn highlight_env() -> HighlightEnv {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fuzz_replay");
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

    let build = |config_values: Config| {
        let mut state = ShellState::new();
        state.set_home(Some(home.clone()));
        state.apply_update(StateUpdate {
            aliases: Some(vec!["ll".into(), "g".into()]),
            global_aliases: Some(vec!["G".into()]),
            suffix_aliases: Some(vec!["txt".into()]),
            functions: Some(vec!["myfunc".into()]),
            named_dirs: Some(vec![("proj".into(), cwd.to_string_lossy().into_owned())]),
            path: Some(bin.to_string_lossy().into_owned()),
            ..StateUpdate::default()
        });
        let (specs, _warnings) = SpecRegistry::load(&config);
        Highlighter {
            paths: PathChecker::new(&config_values.limits),
            config: config_values,
            state,
            specs,
        }
    };
    let tight = Config {
        limits: Limits {
            lex_only_bytes: 64,
            max_path_checks: 2,
            ..Limits::default()
        },
        ..Config::default()
    };
    HighlightEnv {
        default: build(Config::default()),
        tight: build(tight),
        cwd,
    }
}

/// `fuzz_targets/highlight.rs`.
fn replay_highlight(env: &mut HighlightEnv, data: &[u8]) -> Result<(), String> {
    let Some((header, rest)) = data.split_first_chunk::<5>() else {
        return Ok(());
    };
    let flags = header[0];
    let split = usize::from(u16::from_le_bytes([header[1], header[2]])) % (rest.len() + 1);
    let cursor = match u16::from_le_bytes([header[3], header[4]]) {
        u16::MAX => None,
        c => Some(usize::from(c)),
    };
    let (prebuffer, buffer) = rest.split_at(split);
    let opts = RequestOptions {
        parse: ParseOptions {
            interactive_comments: flags & 1 != 0,
            extended_glob: flags & 2 != 0,
            ksh_glob: flags & 4 != 0,
        },
        auto_cd: flags & 8 != 0,
    };
    let cursor_unit = Unit::from_char_offsets(flags & 16 != 0);
    let highlighter = if flags & 32 != 0 {
        &mut env.tight
    } else {
        &mut env.default
    };

    let text = RequestText::new(prebuffer, buffer);
    let s = text.as_str();
    let bs = text.buffer_start();
    if bs > s.len() || !s.is_char_boundary(bs) {
        return Err(format!("bad buffer start {bs}"));
    }
    let cursor = text.cursor_offset(cursor, cursor_unit);
    if cursor < bs || cursor > s.len() || !s.is_char_boundary(cursor) {
        return Err(format!(
            "bad cursor offset {cursor} (buffer start {bs}, len {})",
            s.len()
        ));
    }
    let req = HighlightRequest {
        text: s,
        buffer_start: bs,
        cursor,
        cwd: &env.cwd,
        opts,
    };
    let spans = highlighter.highlight(&req);
    check_spans(s, &spans).map_err(|e| format!("check_spans: {e}\nspans: {spans:?}"))?;
    for (unit, len) in [
        (Unit::Bytes, buffer.len()),
        (Unit::Chars, s[bs..].chars().count()),
    ] {
        let wire = text.wire_spans(&spans, unit);
        check_wire_spans(&wire, len)
            .map_err(|e| format!("{unit:?} wire spans: {e}\nwire: {wire:?}"))?;
    }
    Ok(())
}

#[test]
fn highlight_seeds() {
    let mut env = highlight_env();
    for (name, data) in seeds("highlight") {
        if let Err(e) = replay_highlight(&mut env, &data) {
            panic!("highlight seed {name}: {e}");
        }
    }
}

type Outcome = Result<Request, FramingError>;

/// The chunk-size generator of `fuzz_targets/protocol.rs`.
struct Chunks(u32);

impl Chunks {
    fn new(seed: u8) -> Chunks {
        Chunks(0x9e37_79b9 ^ (u32::from(seed) << 8 | u32::from(seed)))
    }

    fn next_len(&mut self) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        if self.0 & 0x300 == 0 {
            1 + (self.0 as usize >> 10) % 64
        } else {
            1 + (self.0 as usize >> 10) % 8
        }
    }
}

/// Pulls every available request out of `d`, stopping after a framing error, which must be
/// sticky. Returns true when an error ended the stream.
fn drain(d: &mut Decoder, out: &mut Vec<Outcome>) -> Result<bool, String> {
    loop {
        match d.next_request() {
            Ok(Some(r)) => out.push(Ok(r)),
            Ok(None) => return Ok(false),
            Err(e) => {
                if d.next_request() != Err(e.clone()) {
                    return Err("framing error not sticky".into());
                }
                out.push(Err(e));
                return Ok(true);
            }
        }
    }
}

type Decoded = (Vec<Outcome>, Option<usize>);

fn decode(bytes: &[u8], mut lens: impl FnMut() -> usize) -> Result<Decoded, String> {
    let mut d = Decoder::new();
    let mut out = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let (chunk, tail) = rest.split_at(lens().min(rest.len()));
        rest = tail;
        d.feed(chunk);
        if drain(&mut d, &mut out)? {
            return Ok((out, None));
        }
    }
    if drain(&mut d, &mut out)? {
        return Ok((out, None));
    }
    Ok((out, Some(d.pending())))
}

fn without_warnings(r: Request) -> Request {
    match r {
        Request::State { id, update, .. } => Request::State {
            id,
            update,
            warnings: Vec::new(),
        },
        other => other,
    }
}

/// Stream mode of `fuzz_targets/protocol.rs`.
fn replay_stream(seed: u8, bytes: &[u8]) -> Result<(), String> {
    let whole = decode(bytes, || usize::MAX)?;
    let mut chunks = Chunks::new(seed);
    if decode(bytes, || chunks.next_len())? != whole {
        return Err("chunked decoding differs from whole decoding".into());
    }
    if decode(bytes, || 1)? != whole {
        return Err("byte-at-a-time decoding differs from whole decoding".into());
    }
    for r in whole.0.iter().flatten() {
        let Some(frame) = encode_request(r) else {
            continue;
        };
        let mut d = Decoder::new();
        d.feed(&frame);
        let again = d
            .next_request()
            .map_err(|e| format!("re-encoded frame: {e}"))?
            .ok_or("re-encoded frame is incomplete")?;
        if d.pending() != 0 {
            return Err("re-encoded frame has trailing bytes".into());
        }
        if without_warnings(again) != without_warnings(r.clone()) {
            return Err(format!("request did not survive re-encoding: {r:?}"));
        }
    }
    Ok(())
}

#[test]
fn protocol_seeds() {
    for (name, data) in seeds("protocol") {
        let Some((&mode, rest)) = data.split_first() else {
            continue;
        };
        let result = if mode & 1 == 0 {
            match rest.split_first() {
                Some((&seed, stream)) => replay_stream(seed, stream),
                None => Ok(()),
            }
        } else {
            replay_stream(mode, rest)
        };
        if let Err(e) = result {
            panic!("protocol seed {name}: {e}");
        }
    }
}
