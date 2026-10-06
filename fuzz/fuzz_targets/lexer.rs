//! Fuzzes `syntax::parse`.
//!
//! Input: byte 0 holds option bits, the rest is the text.
//!
//! - bit 0: `interactive_comments`
//! - bit 1: `extended_glob`
//! - bit 2: `ksh_glob`
//! - bit 3: decode with `String::from_utf8_lossy` instead of `text::decode`
//! - bit 4: byte 1 holds more option bits, and the text starts at byte 2: 0 `ignore_braces`,
//!   1 `ignore_close_braces`, 2 `rc_quotes`, 3 `ksh_arrays`, 4 `posix_identifiers`,
//!   5 `sh_glob`, 6 `brace_ccl`, 7 `no_short_loops`
//!
//! Asserts that parsing never panics, that the spans satisfy `token::check_spans`, and that every
//! word in `commands` and `path_words` is non-empty, in bounds, and on character boundaries.
//! Commands must be non-empty and ordered by the start of their command word.

#![no_main]

use fast_highlight::syntax::{ParseOptions, Word, parse};
use fast_highlight::text;
use fast_highlight::token::check_spans;
use libfuzzer_sys::fuzz_target;

fn check_word(input: &str, w: &Word, what: &str) {
    assert!(w.start < w.end, "{what} word {w:?} is empty");
    assert!(
        w.end <= input.len(),
        "{what} word {w:?} out of bounds (len {})",
        input.len()
    );
    assert!(
        input.is_char_boundary(w.start) && input.is_char_boundary(w.end),
        "{what} word {w:?} not on a char boundary"
    );
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
    let Some((&flags, mut rest)) = data.split_first() else {
        return;
    };
    let mut opts = ParseOptions {
        interactive_comments: flags & 1 != 0,
        extended_glob: flags & 2 != 0,
        ksh_glob: flags & 4 != 0,
        ..ParseOptions::default()
    };
    if flags & 16 != 0 {
        let Some((&more, text)) = rest.split_first() else {
            return;
        };
        opts = more_options(opts, more);
        rest = text;
    }
    let input = if flags & 8 != 0 {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        text::decode(rest)
    };

    let out = parse(&input, &opts);

    if let Err(e) = check_spans(&input, &out.spans) {
        panic!(
            "check_spans failed: {e}\ninput: {input:?}\nspans: {:?}",
            out.spans
        );
    }
    let mut prev_start = 0;
    for cmd in &out.commands {
        let first = cmd.words.first().expect("simple command without words");
        assert!(
            first.start >= prev_start,
            "commands out of order: command word at {} after one at {prev_start}",
            first.start
        );
        prev_start = first.start;
        for w in &cmd.words {
            check_word(&input, w, "command");
        }
    }
    for w in &out.path_words {
        check_word(&input, w, "path");
    }
});
