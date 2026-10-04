//! Request text decoding and offset conversion.
//!
//! The wire carries `PREBUFFER` and `BUFFER` as raw bytes and expresses offsets in one of two
//! units ([`Unit`]): characters, as zsh counts them with `MULTIBYTE` set, or bytes. The highlighter
//! works on a Rust `String` in byte offsets. [`RequestText`] bridges the two.
//!
//! zsh counts each byte of an invalid UTF-8 sequence as one character, so decoding replaces each
//! such byte with its own U+FFFD (unlike `String::from_utf8_lossy`, which replaces a whole
//! invalid sequence with one). Character counts of the decoded text therefore match zsh, and the
//! positions of those replacement characters are recorded so byte offsets in the original input
//! can be recovered.

use crate::protocol::WireSpan;
use crate::token::Span;

/// The unit of offsets on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// Characters (the request's `u` option). An invalid input byte is one character.
    Chars,
    /// Bytes of the original, undecoded input.
    Bytes,
}

impl Unit {
    /// [`Unit::Chars`] when `char_offsets` is true, else [`Unit::Bytes`].
    pub fn from_char_offsets(char_offsets: bool) -> Unit {
        if char_offsets {
            Unit::Chars
        } else {
            Unit::Bytes
        }
    }
}

const REPLACEMENT: char = '\u{FFFD}';

/// Decodes `bytes` as UTF-8, replacing each byte of an invalid sequence with one U+FFFD.
pub fn decode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    decode_into(bytes, &mut out, &mut Vec::new());
    out
}

/// Appends the decoding of `bytes` to `out`, recording in `invalid` the offset in `out` of each
/// replacement character that stands for one invalid input byte.
fn decode_into(bytes: &[u8], out: &mut String, invalid: &mut Vec<usize>) {
    for chunk in bytes.utf8_chunks() {
        out.push_str(chunk.valid());
        for _ in chunk.invalid() {
            invalid.push(out.len());
            out.push(REPLACEMENT);
        }
    }
}

/// The decoded text of one highlight request: `PREBUFFER` followed by `BUFFER`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestText {
    text: String,
    buffer_start: usize,
    /// Ascending offsets in `text` of replacement characters that each stand for one invalid
    /// input byte. A U+FFFD that was valid UTF-8 in the input is not listed.
    invalid: Vec<usize>,
}

impl RequestText {
    /// Decodes a request's text. The two parts are decoded separately, as zsh holds them in
    /// separate parameters, so a sequence split across them is invalid in both.
    pub fn new(prebuffer: &[u8], buffer: &[u8]) -> RequestText {
        let mut text = String::with_capacity(prebuffer.len() + buffer.len());
        let mut invalid = Vec::new();
        decode_into(prebuffer, &mut text, &mut invalid);
        let buffer_start = text.len();
        decode_into(buffer, &mut text, &mut invalid);
        RequestText {
            text,
            buffer_start,
            invalid,
        }
    }

    /// The decoded `PREBUFFER` followed by the decoded `BUFFER`.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Byte offset in [`RequestText::as_str`] where the decoded `BUFFER` begins.
    pub fn buffer_start(&self) -> usize {
        self.buffer_start
    }

    /// Index into `invalid` of the first entry at or after `offset`.
    fn invalid_from(&self, offset: usize) -> usize {
        self.invalid.partition_point(|&p| p < offset)
    }

    /// Converts a cursor, given relative to the start of `BUFFER` in `unit`, into a byte offset
    /// in the decoded text. `None` means the end of the text. A cursor past the end is clamped
    /// to the end; a byte cursor inside a multibyte character is moved back to its start.
    pub fn cursor_offset(&self, cursor: Option<usize>, unit: Unit) -> usize {
        let Some(cursor) = cursor else {
            return self.text.len();
        };
        let buffer = &self.text[self.buffer_start..];
        match unit {
            Unit::Chars => buffer
                .char_indices()
                .nth(cursor)
                .map_or(self.text.len(), |(i, _)| self.buffer_start + i),
            Unit::Bytes => {
                let mut next_invalid = self.invalid_from(self.buffer_start);
                let mut units = 0;
                for (i, c) in buffer.char_indices() {
                    let pos = self.buffer_start + i;
                    let width = self.byte_width(pos, c, &mut next_invalid);
                    if units + width > cursor {
                        return pos;
                    }
                    units += width;
                }
                self.text.len()
            }
        }
    }

    /// The number of original input bytes the character `c` at decoded offset `pos` came from.
    /// `next_invalid` indexes the first entry of `self.invalid` not before `pos` and is advanced
    /// past `pos` when it matches.
    fn byte_width(&self, pos: usize, c: char, next_invalid: &mut usize) -> usize {
        if self.invalid.get(*next_invalid) == Some(&pos) {
            *next_invalid += 1;
            1
        } else {
            c.len_utf8()
        }
    }

    /// Converts spans in byte offsets of the decoded text into wire spans relative to the start
    /// of `BUFFER` in `unit`.
    ///
    /// Spans ending at or before the buffer start are dropped, spans starting before it are
    /// clipped to it, and spans are clamped to the end of the text. Offsets that fall inside a
    /// character are moved back to its start, and spans that become empty are dropped. The
    /// output keeps the input order, so sorted, well-nested input yields sorted, well-nested
    /// output.
    ///
    /// The cost is one pass over the buffer text plus a sort of the span boundaries.
    pub fn wire_spans(&self, spans: &[Span], unit: Unit) -> Vec<WireSpan> {
        self.wire_spans_prefix(spans, unit, usize::MAX)
    }

    /// [`RequestText::wire_spans`] for at most the first `max` spans that are not clipped away,
    /// so the cost of a long span list is bounded by `max`. For spans on character boundaries
    /// (as the highlighter emits) the result is the first `max` entries of
    /// [`RequestText::wire_spans`]; a prefix of a sorted, well-nested list is still both.
    pub fn wire_spans_prefix(&self, spans: &[Span], unit: Unit, max: usize) -> Vec<WireSpan> {
        let bs = self.buffer_start;
        let len = self.text.len();
        let clipped: Vec<Span> = spans
            .iter()
            .map(|s| Span::new(s.start.max(bs), s.end.min(len), s.kind))
            .filter(|s| s.start < s.end)
            .take(max)
            .collect();

        let identity = match unit {
            Unit::Chars => self.text.as_bytes()[bs..].is_ascii(),
            Unit::Bytes => self.invalid_from(bs) == self.invalid.len(),
        };
        if identity && clipped.iter().all(|s| self.on_boundaries(s)) {
            return clipped
                .iter()
                .map(|s| WireSpan {
                    start: s.start - bs,
                    end: s.end - bs,
                    kind: s.kind,
                })
                .collect();
        }

        // Boundary `2 * i` is the start of span `i`, `2 * i + 1` its end.
        let mut bounds: Vec<(usize, usize)> = clipped
            .iter()
            .enumerate()
            .flat_map(|(i, s)| [(s.start, 2 * i), (s.end, 2 * i + 1)])
            .collect();
        bounds.sort_unstable();
        let mut mapped = vec![0usize; bounds.len()];
        let mut pos = bs;
        let mut units = 0;
        let mut next_invalid = self.invalid_from(bs);
        for &(offset, slot) in &bounds {
            while pos < offset {
                let Some(c) = self.text[pos..].chars().next() else {
                    break;
                };
                if pos + c.len_utf8() > offset {
                    break;
                }
                units += match unit {
                    Unit::Chars => 1,
                    Unit::Bytes => self.byte_width(pos, c, &mut next_invalid),
                };
                pos += c.len_utf8();
            }
            mapped[slot] = units;
        }
        clipped
            .iter()
            .enumerate()
            .map(|(i, s)| WireSpan {
                start: mapped[2 * i],
                end: mapped[2 * i + 1],
                kind: s.kind,
            })
            .filter(|w| w.start < w.end)
            .collect()
    }

    fn on_boundaries(&self, s: &Span) -> bool {
        self.text.is_char_boundary(s.start) && self.text.is_char_boundary(s.end)
    }
}

/// Checks the invariants of a wire span list for a buffer of `len` units (the counterpart of
/// [`crate::token::check_spans`] after conversion): non-empty, in bounds, sorted by ascending
/// start then descending end, and well nested. Returns a description of the first violation.
pub fn check_wire_spans(spans: &[WireSpan], len: usize) -> Result<(), String> {
    let mut stack: Vec<&WireSpan> = Vec::new();
    let mut prev: Option<&WireSpan> = None;
    for s in spans {
        if s.start >= s.end {
            return Err(format!("empty wire span {s:?}"));
        }
        if s.end > len {
            return Err(format!("wire span {s:?} out of bounds (len {len})"));
        }
        if let Some(p) = prev
            && (s.start < p.start || (s.start == p.start && s.end > p.end))
        {
            return Err(format!("wire span {s:?} out of order after {p:?}"));
        }
        while stack.last().is_some_and(|top| top.end <= s.start) {
            stack.pop();
        }
        if let Some(top) = stack.last()
            && s.end > top.end
        {
            return Err(format!("wire span {s:?} partially overlaps {top:?}"));
        }
        stack.push(s);
        prev = Some(s);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::TokenKind::{self, *};

    fn w(start: usize, end: usize, kind: TokenKind) -> WireSpan {
        WireSpan { start, end, kind }
    }

    fn s(start: usize, end: usize, kind: TokenKind) -> Span {
        Span::new(start, end, kind)
    }

    /// Spans over the whole text, one per char, in byte offsets.
    fn per_char_spans(text: &str) -> Vec<Span> {
        text.char_indices()
            .map(|(i, c)| s(i, i + c.len_utf8(), Default))
            .collect()
    }

    #[test]
    fn decode_valid_utf8_unchanged() {
        assert_eq!(decode("ls ~/日本 🎉 é".as_bytes()), "ls ~/日本 🎉 é");
        assert_eq!(decode(b""), "");
    }

    #[test]
    fn decode_replaces_each_invalid_byte() {
        // Lone continuation byte, lone lead byte, truncated 3-byte sequence, overlong encoding,
        // and a surrogate: each byte is one replacement character.
        assert_eq!(decode(b"a\x80b"), "a\u{FFFD}b");
        assert_eq!(decode(b"\xff"), "\u{FFFD}");
        assert_eq!(decode(b"\xe2\x82"), "\u{FFFD}\u{FFFD}");
        assert_eq!(decode(b"\xe2\x82x"), "\u{FFFD}\u{FFFD}x");
        assert_eq!(decode(b"\xc0\x80"), "\u{FFFD}\u{FFFD}");
        assert_eq!(decode(b"\xed\xa0\x80"), "\u{FFFD}\u{FFFD}\u{FFFD}");
        // Truncated 4-byte emoji at the end.
        assert_eq!(decode(b"\xf0\x9f\x8e").chars().count(), 3);
        // Contrast with the standard lossy decoder, which would merge these.
        assert_eq!(String::from_utf8_lossy(b"\xe2\x82").chars().count(), 1);
    }

    #[test]
    fn halves_decoded_separately() {
        // "€" is E2 82 AC; split across PREBUFFER and BUFFER it is invalid on both sides.
        let t = RequestText::new(b"\xe2\x82", b"\xac");
        assert_eq!(t.as_str(), "\u{FFFD}\u{FFFD}\u{FFFD}");
        assert_eq!(t.buffer_start(), 6);
    }

    #[test]
    fn cursor_in_chars() {
        let t = RequestText::new(b"x\n", "日本 🎉a".as_bytes());
        let bs = t.buffer_start();
        assert_eq!(bs, 2);
        assert_eq!(t.cursor_offset(Some(0), Unit::Chars), bs);
        assert_eq!(t.cursor_offset(Some(1), Unit::Chars), bs + 3);
        assert_eq!(t.cursor_offset(Some(3), Unit::Chars), bs + 7);
        assert_eq!(t.cursor_offset(Some(4), Unit::Chars), bs + 11);
        assert_eq!(t.cursor_offset(Some(5), Unit::Chars), bs + 12);
        assert_eq!(t.cursor_offset(Some(6), Unit::Chars), bs + 12);
        assert_eq!(t.cursor_offset(Some(usize::MAX), Unit::Chars), bs + 12);
        assert_eq!(t.cursor_offset(None, Unit::Chars), bs + 12);
    }

    #[test]
    fn cursor_in_bytes() {
        let t = RequestText::new(b"", "é\u{1F389}a".as_bytes());
        assert_eq!(t.cursor_offset(Some(0), Unit::Bytes), 0);
        assert_eq!(
            t.cursor_offset(Some(1), Unit::Bytes),
            0,
            "inside é rounds down"
        );
        assert_eq!(t.cursor_offset(Some(2), Unit::Bytes), 2);
        assert_eq!(
            t.cursor_offset(Some(5), Unit::Bytes),
            2,
            "inside emoji rounds down"
        );
        assert_eq!(t.cursor_offset(Some(6), Unit::Bytes), 6);
        assert_eq!(t.cursor_offset(Some(7), Unit::Bytes), 7);
        assert_eq!(t.cursor_offset(Some(100), Unit::Bytes), 7);
    }

    #[test]
    fn cursor_over_invalid_bytes() {
        // a FF b: decoded "a" U+FFFD "b" (5 bytes), original 3 bytes.
        let t = RequestText::new(b"\xfe", b"a\xffb");
        let bs = t.buffer_start();
        assert_eq!(bs, 3);
        assert_eq!(t.cursor_offset(Some(1), Unit::Bytes), bs + 1);
        assert_eq!(t.cursor_offset(Some(2), Unit::Bytes), bs + 4);
        assert_eq!(t.cursor_offset(Some(3), Unit::Bytes), bs + 5);
        assert_eq!(t.cursor_offset(Some(2), Unit::Chars), bs + 4);
        // A genuine U+FFFD in the input is three bytes.
        let t = RequestText::new(b"", "\u{FFFD}b".as_bytes());
        assert_eq!(t.cursor_offset(Some(1), Unit::Bytes), 0);
        assert_eq!(t.cursor_offset(Some(3), Unit::Bytes), 3);
        assert_eq!(t.cursor_offset(Some(1), Unit::Chars), 3);
    }

    #[test]
    fn ascii_spans_pass_through() {
        let t = RequestText::new(b"", b"ls ~");
        let spans = [s(0, 2, Command), s(3, 4, PathDirectory)];
        let want = vec![w(0, 2, Command), w(3, 4, PathDirectory)];
        assert_eq!(t.wire_spans(&spans, Unit::Chars), want);
        assert_eq!(t.wire_spans(&spans, Unit::Bytes), want);
    }

    #[test]
    fn multibyte_spans_in_both_units() {
        // echo "日本🎉" é
        let text = "echo \"日本🎉\" é";
        let t = RequestText::new(b"", text.as_bytes());
        let q_end = text.find("\" ").unwrap() + 1;
        let e_start = text.find('é').unwrap();
        let spans = [
            s(0, 4, Builtin),
            s(5, q_end, DoubleQuoted),
            s(e_start, e_start + 2, Path),
        ];
        assert_eq!(
            t.wire_spans(&spans, Unit::Chars),
            vec![w(0, 4, Builtin), w(5, 10, DoubleQuoted), w(11, 12, Path)]
        );
        assert_eq!(
            t.wire_spans(&spans, Unit::Bytes),
            vec![w(0, 4, Builtin), w(5, 17, DoubleQuoted), w(18, 20, Path)]
        );
    }

    #[test]
    fn invalid_bytes_map_back_to_original_offsets() {
        // Original: 'a' FF 'é' E2 82 'b' (7 bytes, 6 chars).
        let raw = b"a\xff\xc3\xa9\xe2\x82b";
        let t = RequestText::new(b"", raw);
        assert_eq!(t.as_str().chars().count(), 6);
        let spans = per_char_spans(t.as_str());
        let chars: Vec<_> = t.wire_spans(&spans, Unit::Chars);
        assert_eq!(
            chars.iter().map(|w| (w.start, w.end)).collect::<Vec<_>>(),
            vec![(0, 1), (1, 2), (2, 3), (3, 4), (4, 5), (5, 6)]
        );
        let bytes: Vec<_> = t.wire_spans(&spans, Unit::Bytes);
        assert_eq!(
            bytes.iter().map(|w| (w.start, w.end)).collect::<Vec<_>>(),
            vec![(0, 1), (1, 2), (2, 4), (4, 5), (5, 6), (6, 7)]
        );
        let whole = t.wire_spans(&[s(0, t.as_str().len(), Error)], Unit::Bytes);
        assert_eq!(whole, vec![w(0, raw.len(), Error)]);
    }

    #[test]
    fn invalid_bytes_with_ascii_chars_mode() {
        // Byte mode fast path must not apply when invalid bytes are present; chars mode never
        // takes the fast path when the decoded buffer has non-ASCII text.
        let t = RequestText::new(b"", b"\xffls");
        let spans = [s(3, 5, Command)];
        assert_eq!(t.wire_spans(&spans, Unit::Chars), vec![w(1, 3, Command)]);
        assert_eq!(t.wire_spans(&spans, Unit::Bytes), vec![w(1, 3, Command)]);
    }

    #[test]
    fn prebuffer_clipping() {
        // PREBUFFER `echo "a` + newline, BUFFER `b" 日`: the string spans both.
        let pre = "echo \"a\n";
        let buf = "b\" 日";
        let t = RequestText::new(pre.as_bytes(), buf.as_bytes());
        let bs = t.buffer_start();
        let spans = [
            s(0, 4, Builtin),                   // entirely in PREBUFFER: dropped
            s(4, bs, Default),                  // ends exactly at buffer start: dropped
            s(5, bs + 2, DoubleQuoted),         // clipped to the buffer start
            s(6, bs + 1, Escape),               // nested, clipped
            s(bs + 3, bs + 6, PathPrefix),      // inside the buffer
            s(bs + 3, bs + 100, GlobQualifier), // runs past the end: clamped
        ];
        assert_eq!(
            t.wire_spans(&spans, Unit::Chars),
            vec![
                w(0, 2, DoubleQuoted),
                w(0, 1, Escape),
                w(3, 4, PathPrefix),
                w(3, 4, GlobQualifier),
            ]
        );
        assert_eq!(
            t.wire_spans(&spans, Unit::Bytes),
            vec![
                w(0, 2, DoubleQuoted),
                w(0, 1, Escape),
                w(3, 6, PathPrefix),
                w(3, 6, GlobQualifier),
            ]
        );
    }

    #[test]
    fn prebuffer_with_multibyte_does_not_shift_buffer_offsets() {
        let pre = "日本語 🎉 \\\n";
        let t = RequestText::new(pre.as_bytes(), "ab".as_bytes());
        let bs = t.buffer_start();
        let spans = [s(0, bs + 1, Default), s(bs + 1, bs + 2, Glob)];
        let want = vec![w(0, 1, Default), w(1, 2, Glob)];
        assert_eq!(t.wire_spans(&spans, Unit::Chars), want);
        assert_eq!(t.wire_spans(&spans, Unit::Bytes), want);
    }

    #[test]
    fn nesting_and_order_preserved() {
        // $(echo "日$x") with nested spans, sorted canonically.
        let text = "$(echo \"日$x\")";
        let t = RequestText::new(b"", text.as_bytes());
        let len = text.len();
        let spans = [
            s(0, len, Default),
            s(0, 2, Substitution),
            s(2, 6, Builtin),
            s(7, len - 1, DoubleQuoted),
            s(11, 13, Parameter),
            s(len - 1, len, Substitution),
        ];
        let out = t.wire_spans(&spans, Unit::Chars);
        assert_eq!(
            out,
            vec![
                w(0, 13, Default),
                w(0, 2, Substitution),
                w(2, 6, Builtin),
                w(7, 12, DoubleQuoted),
                w(9, 11, Parameter),
                w(12, 13, Substitution),
            ]
        );
        // Output stays canonically sorted and well nested in char space too.
        let as_spans: Vec<Span> = out.iter().map(|w| s(w.start, w.end, w.kind)).collect();
        let ascii: String = "x".repeat(13);
        assert_eq!(crate::token::check_spans(&ascii, &as_spans), Ok(()));
    }

    #[test]
    fn misaligned_offsets_round_down_and_empty_spans_drop() {
        let t = RequestText::new(b"", "a日b".as_bytes());
        // Start inside 日 rounds to its start; an end inside 日 rounds down too, so a span
        // entirely inside it becomes empty and is dropped.
        let spans = [s(2, 5, Glob), s(2, 3, Error), s(0, 2, Escape)];
        assert_eq!(
            t.wire_spans(&spans, Unit::Chars),
            vec![w(1, 3, Glob), w(0, 1, Escape)]
        );
        // Offsets past the end and fully empty spans are tolerated.
        assert_eq!(
            t.wire_spans(&[s(9, 12, Glob), s(1, 1, Glob)], Unit::Chars),
            vec![]
        );
    }

    #[test]
    fn empty_input() {
        let t = RequestText::new(b"", b"");
        assert_eq!(t.as_str(), "");
        assert_eq!(t.cursor_offset(Some(3), Unit::Chars), 0);
        assert_eq!(t.cursor_offset(Some(3), Unit::Bytes), 0);
        assert_eq!(t.wire_spans(&[s(0, 1, Error)], Unit::Chars), vec![]);
    }

    #[test]
    fn unsorted_input_maps_each_span() {
        // The conversion does not depend on input order; it maps every span independently.
        let t = RequestText::new(b"", "日a日b".as_bytes());
        let spans = [s(7, 8, Glob), s(0, 3, Error), s(3, 7, Path)];
        assert_eq!(
            t.wire_spans(&spans, Unit::Chars),
            vec![w(3, 4, Glob), w(0, 1, Error), w(1, 3, Path)]
        );
    }

    #[test]
    fn large_input_is_linear() {
        // 100k wide chars with a span per char; a quadratic conversion would take far too long.
        let text = "日".repeat(100_000);
        let t = RequestText::new(b"", text.as_bytes());
        let spans = per_char_spans(&text);
        let out = t.wire_spans(&spans, Unit::Chars);
        assert_eq!(out.len(), 100_000);
        assert_eq!(out[99_999], w(99_999, 100_000, Default));
        let out = t.wire_spans(&spans, Unit::Bytes);
        assert_eq!(out[99_999], w(299_997, 300_000, Default));
    }

    #[test]
    fn combining_marks_and_zwj_count_as_separate_chars() {
        // zsh counts code points: `e` + U+0301 is two characters, and the ZWJ family emoji is
        // three (man, ZWJ, woman), whatever a terminal draws.
        let text = "echo e\u{301} \u{1F468}\u{200D}\u{1F469} $HOME";
        let t = RequestText::new(b"", text.as_bytes());
        let parse = crate::syntax::parse(text, &crate::syntax::ParseOptions::default());
        let param = parse
            .spans
            .iter()
            .find(|s| s.kind == Parameter)
            .expect("a parameter span");
        assert_eq!(&text[param.start..param.end], "$HOME");
        let wire = t.wire_spans(std::slice::from_ref(param), Unit::Chars);
        assert_eq!(wire, vec![w(12, 17, Parameter)]);
        let wire = t.wire_spans(std::slice::from_ref(param), Unit::Bytes);
        assert_eq!(wire, vec![w(21, 26, Parameter)]);
        // The cursor in characters lands on the same code points.
        assert_eq!(t.cursor_offset(Some(12), Unit::Chars), param.start);
        assert_eq!(t.cursor_offset(Some(7), Unit::Chars), "echo e\u{301}".len());
    }

    #[test]
    fn wire_span_checks() {
        assert_eq!(check_wire_spans(&[], 0), Ok(()));
        let good = [
            w(0, 6, DoubleQuoted),
            w(1, 3, Parameter),
            w(3, 6, Parameter),
        ];
        assert_eq!(check_wire_spans(&good, 6), Ok(()));
        assert_eq!(
            check_wire_spans(&good[..2], 6),
            Ok(()),
            "prefixes stay valid"
        );
        for (bad, len) in [
            (vec![w(1, 1, Glob)], 5),
            (vec![w(0, 6, Glob)], 5),
            (vec![w(2, 3, Glob), w(1, 3, Glob)], 5),
            (vec![w(0, 2, Glob), w(0, 3, Glob)], 5),
            (vec![w(0, 3, Glob), w(2, 4, Glob)], 5),
        ] {
            assert!(check_wire_spans(&bad, len).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn wire_span_prefix_skips_clipped_spans() {
        let pre = "**\n";
        let buf = "a日 b c";
        let t = RequestText::new(pre.as_bytes(), buf.as_bytes());
        let bs = t.buffer_start();
        let spans = [
            s(0, 1, Glob),
            s(1, 2, Glob),
            s(0, bs + 4, Default),
            s(bs, bs + 1, Escape),
            s(bs + 5, bs + 6, Path),
            s(bs + 7, bs + 8, Path),
        ];
        let all = t.wire_spans(&spans, Unit::Chars);
        assert_eq!(all.len(), 4);
        for max in 0..=5 {
            let prefix = t.wire_spans_prefix(&spans, Unit::Chars, max);
            assert_eq!(prefix, all[..max.min(all.len())], "max {max}");
        }
    }

    #[test]
    fn unit_from_flag() {
        assert_eq!(Unit::from_char_offsets(true), Unit::Chars);
        assert_eq!(Unit::from_char_offsets(false), Unit::Bytes);
    }
}
