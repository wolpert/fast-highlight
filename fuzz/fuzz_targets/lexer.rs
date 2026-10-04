//! Fuzzes `syntax::parse`.
//!
//! Input: byte 0 holds option bits, the rest is the text.
//!
//! - bit 0: `interactive_comments`
//! - bit 1: `extended_glob`
//! - bit 2: `ksh_glob`
//! - bit 3: decode with `String::from_utf8_lossy` instead of `text::decode`
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

fuzz_target!(|data: &[u8]| {
    let Some((&flags, rest)) = data.split_first() else {
        return;
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
