//! Word lexing: quoting, parameter and command substitution, arithmetic, globs, brace
//! expansion, and history expansion.
//!
//! The functions here emit spans for the parts of a word and accumulate the word's value after
//! quote removal in a [`WordAcc`]. They recurse into the command-level parser for `$(...)`,
//! backquotes, and process substitutions.

use super::Word;
use super::parser::{ArithEnd, Bracket, Memo, Parser, Term};
use crate::token::TokenKind;

/// Unquoted characters that end a word. `(` is handled separately because inside a word it
/// starts a glob group.
pub(super) fn is_word_term(c: u8) -> bool {
    matches!(
        c,
        b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'<' | b'>' | b')'
    )
}

/// ASCII characters with a meaning in an unquoted word; anything else is copied verbatim.
fn is_special(c: u8) -> bool {
    is_word_term(c)
        || matches!(
            c,
            b'(' | b'\\'
                | b'\''
                | b'"'
                | b'$'
                | b'`'
                | b'!'
                | b'*'
                | b'?'
                | b'['
                | b'{'
                | b'}'
                | b','
                | b'.'
                | b'#'
                | b'~'
                | b'^'
                | b'='
                | b'@'
                | b'+'
        )
}

/// Accumulates what is known about a word while its parts are lexed.
pub(super) struct WordAcc {
    pub(super) start: usize,
    /// The word's text after quote removal. Parameter, command, process, arithmetic, and
    /// history expansions add nothing to it; glob characters and brace expansions are kept as
    /// written. Once [`WordAcc::expand`] has been called it is not the word's value.
    pub(super) text: String,
    /// Contains an expansion of any kind or an unquoted glob character.
    expanded: bool,
    /// [`Word::name_eq`] as recorded by the first [`WordAcc::expand`].
    name_eq: bool,
    pub(super) has_glob: bool,
    pub(super) tilde: bool,
    /// Contains quoting of any kind (relevant for here-document delimiters).
    pub(super) quoted: bool,
    /// The value of an assignment: no globbing or brace expansion, and a `}` at the end stays
    /// part of the word.
    pub(super) assignment: bool,
    collect: bool,
}

impl WordAcc {
    pub(super) fn new(start: usize) -> WordAcc {
        WordAcc {
            start,
            text: String::new(),
            expanded: false,
            name_eq: false,
            has_glob: false,
            tilde: false,
            quoted: false,
            assignment: false,
            collect: true,
        }
    }

    /// An accumulator whose text is discarded, for regions that are not words.
    pub(super) fn scratch(start: usize) -> WordAcc {
        WordAcc {
            collect: false,
            ..WordAcc::new(start)
        }
    }

    pub(super) fn push_str(&mut self, s: &str) {
        if self.collect {
            self.text.push_str(s);
        }
    }

    fn push_char(&mut self, c: char) {
        if self.collect {
            self.text.push(c);
        }
    }

    /// Marks the word as containing an expansion. The first call decides [`Word::name_eq`] from
    /// `text` so far, so call it before the expansion adds text. (A brace expansion calls it at
    /// its `}`, after its `{`, which is no name character, has been added; the result is the
    /// same.)
    pub(super) fn expand(&mut self) {
        if !self.expanded {
            self.expanded = true;
            self.name_eq = self.collect && super::is_name_eq(&self.text);
        }
    }

    fn glob(&mut self) {
        self.has_glob = true;
        self.expand();
    }

    pub(super) fn into_word(self, end: usize) -> Word {
        let name_eq = if self.expanded {
            self.name_eq
        } else {
            self.collect && super::is_name_eq(&self.text)
        };
        Word {
            start: self.start,
            end,
            literal: (!self.expanded && self.collect).then_some(self.text),
            tilde: self.tilde,
            has_glob: self.has_glob,
            name_eq,
        }
    }
}

impl Parser<'_> {
    pub(super) fn lex_word(&mut self) -> Word {
        self.lex_word_with(!self.spans_only)
    }

    /// Lexes one word; without `literal` its [`Word::literal`] is always `None`.
    pub(super) fn lex_word_with(&mut self, literal: bool) -> Word {
        let acc = self.lex_word_acc(literal);
        acc.into_word(self.pos)
    }

    /// Lexes one word at the cursor, collecting its text when `literal` is set. Always consumes
    /// at least one character.
    pub(super) fn lex_word_acc(&mut self, literal: bool) -> WordAcc {
        let mut acc = if literal {
            WordAcc::new(self.pos)
        } else {
            WordAcc::scratch(self.pos)
        };
        self.lex_word_into(&mut acc);
        if self.pos == acc.start && self.pos < self.end {
            let s = self.pos;
            self.bump_char();
            acc.push_str(&self.src[s..self.pos]);
        }
        acc
    }

    /// Lexes word parts until an unquoted terminator.
    pub(super) fn lex_word_into(&mut self, acc: &mut WordAcc) {
        let mut braces: Vec<(usize, bool)> = Vec::new();
        while let Some(c) = self.peek() {
            match c {
                b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b')' => break,
                // zsh ends a word before a `}` that closes no `{` of the word and is followed by
                // a terminator (`{echo hi}`), except in an assignment value (`x=a}`).
                b'}' if !acc.assignment
                    && braces.is_empty()
                    && self.pos > acc.start
                    && self.byte(self.pos + 1).is_none_or(is_word_term) =>
                {
                    break;
                }
                b'<' | b'>' => {
                    if self.peek_at(1) == Some(b'(') {
                        self.substitution(acc, TokenKind::ProcessSubstitution);
                    } else if let Some(e) = self.numeric_range_at(self.pos) {
                        self.glob_span(acc, self.pos, e);
                    } else {
                        break;
                    }
                }
                b'(' => {
                    if self.empty_parens_ahead() {
                        break;
                    }
                    self.glob_group(acc, self.pos, false);
                }
                _ => self.unquoted_part(acc, &mut braces),
            }
        }
    }

    /// One part of an unquoted word, starting at a character that is not a word terminator.
    fn unquoted_part(&mut self, acc: &mut WordAcc, braces: &mut Vec<(usize, bool)>) {
        let start = self.pos;
        let first = start == acc.start;
        let c = self.b[start];
        let next = self.peek_at(1);
        let ext = self.opts.extended_glob;
        match c {
            b'\\' => self.escape(acc),
            b'\'' => self.single_quoted(acc),
            b'"' => self.double_quoted(acc, start),
            b'$' => self.dollar(acc, false),
            b'`' => self.backquote(acc),
            b'@' | b'+' | b'!' | b'*' | b'?' if self.opts.ksh_glob && next == Some(b'(') => {
                self.pos += 1;
                self.glob_group(acc, start, true);
            }
            b'!' => {
                if !self.history(acc, false) {
                    self.literal_run(acc);
                }
            }
            b'*' => {
                let mut e = start + 1;
                if self.byte(e) == Some(b'*') {
                    e += 1;
                    if self.byte(e) == Some(b'*') {
                        e += 1;
                    }
                    if self.byte(e) == Some(b'/') {
                        e += 1;
                    }
                }
                self.glob_span(acc, start, e);
            }
            b'?' => self.glob_span(acc, start, start + 1),
            b'[' => match self.bracket_end(start) {
                Some(e) => self.glob_span(acc, start, e),
                None => self.literal_run(acc),
            },
            b'{' => {
                braces.push((start, false));
                self.literal_run(acc);
            }
            b',' => {
                if let Some(top) = braces.last_mut() {
                    top.1 = true;
                }
                self.literal_run(acc);
            }
            b'.' if next == Some(b'.') => {
                if let Some(top) = braces.last_mut() {
                    top.1 = true;
                }
                self.pos += 2;
                acc.push_str("..");
            }
            b'}' => {
                if let Some((open, sep)) = braces.pop()
                    && sep
                    && !acc.assignment
                {
                    self.push(open, start + 1, TokenKind::BraceExpansion);
                    acc.expand();
                }
                self.literal_run(acc);
            }
            b'#' if ext && !first => {
                let e = if next == Some(b'#') {
                    start + 2
                } else {
                    start + 1
                };
                self.glob_span(acc, start, e);
            }
            b'~' if first => {
                acc.tilde = true;
                self.literal_run(acc);
            }
            b'~' if ext => self.glob_span(acc, start, start + 1),
            b'^' if ext && (first || self.b[start - 1] == b'/') => {
                self.glob_span(acc, start, start + 1)
            }
            b'=' if first && next == Some(b'(') => {
                self.substitution(acc, TokenKind::ProcessSubstitution)
            }
            _ => self.literal_run(acc),
        }
    }

    /// Copies the current character and any following ordinary characters into the word.
    fn literal_run(&mut self, acc: &mut WordAcc) {
        let s = self.pos;
        self.bump_char();
        while let Some(c) = self.peek() {
            if c < 0x80 && is_special(c) {
                break;
            }
            // Stops only at ASCII bytes or the limit, so always on a character boundary.
            self.pos += 1;
        }
        acc.push_str(&self.src[s..self.pos]);
    }

    fn glob_span(&mut self, acc: &mut WordAcc, start: usize, end: usize) {
        let end = end.min(self.end);
        if !acc.assignment {
            self.push(start, end, TokenKind::Glob);
            acc.glob();
        }
        acc.push_str(&self.src[start..end]);
        self.pos = end;
    }

    /// `(`, optional blanks, `)`: ends a word so a function definition can be recognised.
    fn empty_parens_ahead(&self) -> bool {
        let mut j = self.pos + 1;
        while matches!(self.byte(j), Some(b' ' | b'\t')) {
            j += 1;
        }
        self.byte(j) == Some(b')')
    }

    /// A parenthesised glob group at the cursor; `open` is where the group starts (the `@` of
    /// a ksh `@(...)`, otherwise the `(`). A final group without `|` is a glob qualifier list.
    fn glob_group(&mut self, acc: &mut WordAcc, open: usize, ksh: bool) {
        let paren = self.pos;
        self.pos += 1;
        let glob = !acc.assignment;
        if glob {
            acc.glob();
        }
        acc.push_str(&self.src[open..self.pos]);
        if glob && !ksh && self.opts.extended_glob && self.peek() == Some(b'#') {
            // Globbing flags `(#i)`, or the explicit qualifier form `(#q...)`.
            let qual = self.peek_at(1) == Some(b'q');
            while let Some(c) = self.peek() {
                if c == b')' {
                    self.pos += 1;
                    break;
                }
                if matches!(c, b' ' | b'\t' | b'\n') {
                    break;
                }
                self.bump_char();
            }
            let kind = if qual {
                TokenKind::GlobQualifier
            } else {
                TokenKind::Glob
            };
            self.push(open, self.pos, kind);
            return;
        }
        if !self.enter() {
            if glob {
                self.push(open, paren + 1, TokenKind::Glob);
            }
            return;
        }
        let before = self.span_count();
        let mut has_bar = false;
        let mut braces = Vec::new();
        let closed = loop {
            let Some(c) = self.peek() else { break false };
            match c {
                b')' => {
                    self.pos += 1;
                    break true;
                }
                b'|' => {
                    if glob {
                        self.push(self.pos, self.pos + 1, TokenKind::Glob);
                    }
                    self.pos += 1;
                    acc.push_str("|");
                    has_bar = true;
                }
                b' ' | b'\t' => {
                    acc.push_str(&self.src[self.pos..self.pos + 1]);
                    self.pos += 1;
                }
                b'\n' | b';' | b'&' => break false,
                b'<' | b'>' => {
                    if self.peek_at(1) == Some(b'(') {
                        self.substitution(acc, TokenKind::ProcessSubstitution);
                    } else if let Some(e) = self.numeric_range_at(self.pos) {
                        self.glob_span(acc, self.pos, e);
                    } else {
                        break false;
                    }
                }
                b'(' => self.glob_group(acc, self.pos, false),
                _ => self.unquoted_part(acc, &mut braces),
            }
        };
        self.leave();
        if !glob {
            if closed {
                acc.push_str(")");
            }
        } else if closed {
            acc.push_str(")");
            let at_end = self.peek().is_none_or(is_word_term);
            if !ksh && at_end && !has_bar {
                self.truncate_spans(before);
                self.push(open, self.pos, TokenKind::GlobQualifier);
            } else {
                self.push(open, paren + 1, TokenKind::Glob);
                self.push(self.pos - 1, self.pos, TokenKind::Glob);
            }
        } else {
            self.push(open, paren + 1, TokenKind::Glob);
        }
    }

    /// The end of a bracket expression `[...]` starting at `i`, or `None` when it is not closed
    /// within the word.
    ///
    /// A failed scan is remembered: a scan from a later `[` that this one stepped over as an
    /// ordinary character sees the same characters from the next offset on, so it fails too.
    /// The `[`s inside a character class or after a backslash are not in that list, because a
    /// scan starting there can find a `]` this one skipped.
    pub(super) fn bracket_end(&mut self, i: usize) -> Option<usize> {
        let (from, to, limit) = self.bracket_fail;
        if i > from && i < to && limit == self.end {
            // Lookups mostly move forward through the list, so resume from the last one.
            let list = &self.bracket_fail_at;
            let mut c = self.bracket_fail_cursor.min(list.len());
            if c > 0 && list[c - 1] >= i {
                c = list.partition_point(|&p| p < i);
            } else {
                while list.get(c).is_some_and(|&p| p < i) {
                    c += 1;
                }
            }
            self.bracket_fail_cursor = c;
            if list.get(c) == Some(&i) {
                return None;
            }
        }
        let mut j = i + 1;
        if matches!(self.byte(j), Some(b'!' | b'^')) {
            j += 1;
        }
        if self.byte(j) == Some(b']') {
            j += 1;
        }
        let mut stepped = std::mem::take(&mut self.bracket_scratch);
        stepped.clear();
        // A class search starting before this offset is known to find no closing `:]`.
        let mut class_fail = 0;
        while let Some(c) = self.byte(j) {
            match c {
                b']' => {
                    self.bracket_scratch = stepped;
                    return Some(j + 1);
                }
                b'[' if self.byte(j + 1) == Some(b':') => {
                    // A character class such as `[:alpha:]`.
                    let mut k = j + 2;
                    let mut closed = false;
                    if k > class_fail {
                        while let Some(d) = self.byte(k) {
                            if d == b':' && self.byte(k + 1) == Some(b']') {
                                closed = true;
                                break;
                            }
                            if is_word_term(d) {
                                break;
                            }
                            k += 1;
                        }
                        if !closed {
                            class_fail = k;
                        }
                    }
                    if closed {
                        j = k + 2;
                    } else {
                        stepped.push(j);
                        j += 2;
                    }
                }
                b'\\' => j += 2,
                _ if is_word_term(c) || c == b'(' => break,
                b'[' => {
                    stepped.push(j);
                    j += 1;
                }
                _ => j += 1,
            }
        }
        self.bracket_fail = (i, j, self.end);
        self.bracket_fail_cursor = 0;
        self.bracket_scratch = std::mem::replace(&mut self.bracket_fail_at, stepped);
        None
    }

    /// `<N-M>` numeric range glob at `i`: its end offset.
    pub(super) fn numeric_range_at(&self, i: usize) -> Option<usize> {
        if self.byte(i) != Some(b'<') {
            return None;
        }
        let mut j = i + 1;
        while matches!(self.byte(j), Some(b'0'..=b'9')) {
            j += 1;
        }
        if self.byte(j) != Some(b'-') {
            return None;
        }
        j += 1;
        while matches!(self.byte(j), Some(b'0'..=b'9')) {
            j += 1;
        }
        (self.byte(j) == Some(b'>')).then_some(j + 1)
    }

    /// The end of a parameter name starting at `i`: a letter or `_`, then letters, digits, and
    /// `_`. Non-ASCII alphanumerics are accepted, as zsh does with `MULTIBYTE`.
    pub(super) fn name_end(&self, i: usize) -> Option<usize> {
        let mut j = i;
        while j < self.end {
            let c = self.b[j];
            if c.is_ascii_alphabetic() || c == b'_' || (c.is_ascii_digit() && j > i) {
                j += 1;
            } else if c >= 0x80 {
                let ch = self.src.get(j..self.end)?.chars().next()?;
                if !ch.is_alphanumeric() {
                    break;
                }
                j += ch.len_utf8();
            } else {
                break;
            }
        }
        (j > i).then_some(j)
    }

    // ---------------------------------------------------------------------------------------
    // Quoting
    // ---------------------------------------------------------------------------------------

    fn escape(&mut self, acc: &mut WordAcc) {
        let s = self.pos;
        self.pos += 1;
        match self.peek() {
            None => {}
            Some(b'\n') => self.pos += 1,
            Some(_) => {
                let cs = self.pos;
                self.bump_char();
                acc.push_str(&self.src[cs..self.pos]);
                acc.quoted = true;
            }
        }
        self.push(s, self.pos, TokenKind::Escape);
    }

    fn single_quoted(&mut self, acc: &mut WordAcc) {
        let s = self.pos;
        self.pos += 1;
        let close = self.b[self.pos..self.end]
            .iter()
            .position(|&c| c == b'\'')
            .map(|n| self.pos + n);
        let content_end = close.unwrap_or(self.end);
        acc.push_str(&self.src[self.pos..content_end]);
        acc.quoted = true;
        self.pos = close.map_or(self.end, |c| c + 1);
        self.push(s, self.pos, TokenKind::SingleQuoted);
    }

    /// `"..."` with the opening quote at the cursor; the span starts at `span_start` (which is
    /// the `$` of `$"..."`).
    fn double_quoted(&mut self, acc: &mut WordAcc, span_start: usize) {
        self.pos += 1;
        acc.quoted = true;
        if self.enter() {
            self.dq_body(acc, false);
            self.leave();
        }
        if self.peek() == Some(b'"') {
            self.pos += 1;
        }
        self.push(span_start, self.pos, TokenKind::DoubleQuoted);
    }

    /// The inside of a double-quoted string, or (with `heredoc`) of an unquoted here-document
    /// body, where `"` is an ordinary character.
    pub(super) fn dq_body(&mut self, acc: &mut WordAcc, heredoc: bool) {
        while let Some(c) = self.peek() {
            match c {
                b'"' if !heredoc => return,
                b'\\' => {
                    let n = self.peek_at(1);
                    let escapes = matches!(n, Some(b'$' | b'`' | b'\\' | b'\n'))
                        || (!heredoc && matches!(n, Some(b'"' | b'!')));
                    let s = self.pos;
                    if escapes {
                        self.pos += 2;
                        self.push(s, self.pos, TokenKind::Escape);
                        if n != Some(b'\n') {
                            acc.push_str(&self.src[s + 1..self.pos]);
                        }
                    } else {
                        self.pos += 1;
                        acc.push_str("\\");
                    }
                }
                b'$' => self.dollar(acc, true),
                b'`' => self.backquote(acc),
                b'!' if !heredoc && self.history(acc, true) => {}
                _ => {
                    let s = self.pos;
                    self.bump_char();
                    while let Some(c) = self.peek() {
                        if matches!(c, b'"' | b'\\' | b'$' | b'`' | b'!') {
                            break;
                        }
                        self.pos += 1;
                    }
                    acc.push_str(&self.src[s..self.pos]);
                }
            }
        }
    }

    fn dollar_quoted(&mut self, acc: &mut WordAcc) {
        let s = self.pos;
        self.pos += 2;
        acc.quoted = true;
        while let Some(c) = self.peek() {
            match c {
                b'\'' => {
                    self.pos += 1;
                    break;
                }
                b'\\' => {
                    let e = self.pos;
                    self.dollar_escape(acc);
                    self.push(e, self.pos, TokenKind::Escape);
                }
                _ => {
                    let r = self.pos;
                    self.bump_char();
                    while let Some(c) = self.peek() {
                        if c == b'\'' || c == b'\\' {
                            break;
                        }
                        self.pos += 1;
                    }
                    acc.push_str(&self.src[r..self.pos]);
                }
            }
        }
        self.push(s, self.pos, TokenKind::DollarQuoted);
    }

    /// One escape sequence inside `$'...'`, with the backslash at the cursor.
    fn dollar_escape(&mut self, acc: &mut WordAcc) {
        self.pos += 1;
        let Some(c) = self.peek() else { return };
        let simple = match c {
            b'n' => Some('\n'),
            b't' => Some('\t'),
            b'r' => Some('\r'),
            b'a' => Some('\x07'),
            b'b' => Some('\x08'),
            b'e' | b'E' => Some('\x1b'),
            b'f' => Some('\x0c'),
            b'v' => Some('\x0b'),
            b'\\' | b'\'' | b'"' | b'?' => Some(c as char),
            _ => None,
        };
        if let Some(ch) = simple {
            self.pos += 1;
            acc.push_char(ch);
            return;
        }
        match c {
            b'x' | b'u' | b'U' => {
                self.pos += 1;
                let max = match c {
                    b'x' => 2,
                    b'u' => 4,
                    _ => 8,
                };
                match self.read_digits(16, max) {
                    Some(v) => acc.push_char(char::from_u32(v).unwrap_or('\u{fffd}')),
                    None => {
                        acc.push_char('\\');
                        acc.push_char(c as char);
                    }
                }
            }
            b'0'..=b'7' => {
                let v = self.read_digits(8, 3).unwrap_or(0);
                acc.push_char(char::from_u32(v).unwrap_or('\u{fffd}'));
            }
            b'c' => {
                self.pos += 1;
                if let Some(n) = self.peek() {
                    let cs = self.pos;
                    self.bump_char();
                    if n.is_ascii() {
                        acc.push_char(char::from(n & 0x1f));
                    } else {
                        acc.push_str(&self.src[cs..self.pos]);
                    }
                }
            }
            _ => {
                let cs = self.pos;
                self.bump_char();
                acc.push_char('\\');
                acc.push_str(&self.src[cs..self.pos]);
            }
        }
    }

    /// Consumes up to `max` digits in `radix`; `None` when there are none.
    fn read_digits(&mut self, radix: u32, max: usize) -> Option<u32> {
        let mut v: u32 = 0;
        let mut n = 0;
        while n < max {
            let Some(d) = self.peek().and_then(|c| char::from(c).to_digit(radix)) else {
                break;
            };
            v = v.wrapping_mul(radix).wrapping_add(d);
            self.pos += 1;
            n += 1;
        }
        (n > 0).then_some(v)
    }

    fn backquote(&mut self, acc: &mut WordAcc) {
        let s = self.pos;
        acc.expand();
        let mut i = s + 1;
        let mut close = None;
        while i < self.end {
            match self.b[i] {
                b'\\' => i += 2,
                b'`' => {
                    close = Some(i);
                    break;
                }
                _ => i += 1,
            }
        }
        let inner_end = close.unwrap_or(self.end);
        let after = close.map_or(self.end, |c| c + 1);
        self.push(s, after, TokenKind::Backquoted);
        if self.enter() {
            let saved = self.end;
            self.end = inner_end;
            self.pos = s + 1;
            self.parse_list(Term::Eof);
            self.end = saved;
            self.leave();
        }
        self.pos = after;
    }

    // ---------------------------------------------------------------------------------------
    // Expansions
    // ---------------------------------------------------------------------------------------

    /// A `$` at the cursor. `quoted` is true inside double quotes or a here-document body.
    /// Always consumes at least the `$`.
    fn dollar(&mut self, acc: &mut WordAcc, quoted: bool) {
        let s = self.pos;
        match self.peek_at(1) {
            Some(b'(') => {
                if self.peek_at(2) == Some(b'(') {
                    let arith = match self.find_arith_close(s + 3) {
                        ArithEnd::Closed(i) => Some((i, i + 2)),
                        ArithEnd::Unclosed => Some((self.end, self.end)),
                        ArithEnd::NotArith => None,
                    };
                    if let Some((content_end, end)) = arith {
                        self.arith(s, s + 3, content_end, end);
                        acc.expand();
                        return;
                    }
                }
                self.substitution(acc, TokenKind::Substitution);
            }
            Some(b'{') => self.brace_param(acc, quoted),
            Some(b'\'') if !quoted => self.dollar_quoted(acc),
            Some(b'"') if !quoted => {
                self.pos += 1;
                self.double_quoted(acc, s);
            }
            Some(b'[') => self.old_arith(acc),
            _ => {
                if !self.simple_param(acc) {
                    self.pos += 1;
                    acc.push_str("$");
                }
            }
        }
    }

    /// `$(`, `<(`, `>(`, or `=(` at the cursor: delimiters of `kind` around a nested list.
    fn substitution(&mut self, acc: &mut WordAcc, kind: TokenKind) {
        let s = self.pos;
        self.pos += 2;
        self.push(s, self.pos, kind);
        acc.expand();
        if self.enter() {
            self.parse_list(Term::Paren);
            self.leave();
        }
        if self.peek() == Some(b')') {
            self.push(self.pos, self.pos + 1, kind);
            self.pos += 1;
        }
    }

    /// Scans `((...))` content from `i`, just after the second `(`, for the closing `))`.
    pub(super) fn find_arith_close(&mut self, i: usize) -> ArithEnd {
        match self.matching(Bracket::Arith, i - 1) {
            Ok(j) => match self.byte(j + 1) {
                Some(b')') => ArithEnd::Closed(j),
                None => ArithEnd::Unclosed,
                Some(_) => ArithEnd::NotArith,
            },
            Err(_) => ArithEnd::Unclosed,
        }
    }

    /// The closer matching the opener of `kind` at `open` within the current limit (`Ok`), or
    /// where the scan for it stopped (`Err`). Memoised in [`Parser::memos`].
    pub(super) fn matching(&mut self, kind: Bracket, open: usize) -> Result<usize, usize> {
        match self.memos[kind as usize].get(open) {
            Some(Ok(close)) => {
                return if close < self.end {
                    Ok(close)
                } else {
                    Err(self.end)
                };
            }
            Some(Err(stop)) if self.end <= stop => return Err(self.end),
            Some(Err(stop)) if kind == Bracket::Subscript && self.b[stop] == b'\n' => {
                return Err(stop);
            }
            _ => {}
        }
        let (opener, closer) = match kind {
            Bracket::Arith => (b'(', b')'),
            Bracket::Subscript | Bracket::OldArith => (b'[', b']'),
        };
        let mut memo: Memo = std::mem::take(&mut self.memos[kind as usize]);
        memo.begin(open, self.b.len());
        let mut j = open + 1;
        let result = loop {
            let Some(c) = self.byte(j) else {
                break Err(self.end);
            };
            match c {
                _ if c == opener => memo.opened(j),
                _ if c == closer => {
                    if memo.closed(j) {
                        break Ok(j);
                    }
                }
                b'\n' if kind == Bracket::Subscript => break Err(j),
                b'\\' if kind != Bracket::OldArith => j += 1,
                b'\'' | b'"' if kind == Bracket::Arith => {
                    j += 1;
                    while j < self.end && self.b[j] != c {
                        j += 1;
                    }
                }
                _ => {}
            }
            j += 1;
        };
        if let Err(stop) = result {
            memo.stopped(stop);
        }
        self.memos[kind as usize] = memo;
        result
    }

    /// An arithmetic expansion or command: one Arithmetic span with expansions nested inside.
    pub(super) fn arith(
        &mut self,
        span_start: usize,
        content_start: usize,
        content_end: usize,
        span_end: usize,
    ) {
        let span_end = span_end.min(self.end);
        self.push(span_start, span_end, TokenKind::Arithmetic);
        self.lex_region(content_start, content_end);
        self.pos = span_end;
    }

    /// `$[...]`, the old arithmetic expansion syntax.
    fn old_arith(&mut self, acc: &mut WordAcc) {
        let s = self.pos;
        let (content_end, end) = match self.matching(Bracket::OldArith, s + 1) {
            Ok(c) => (c, c + 1),
            Err(_) => (self.end, self.end),
        };
        self.arith(s, s + 2, content_end, end);
        acc.expand();
    }

    /// Highlights expansions and quoting inside `[start, end)` without moving the cursor. Used
    /// for arithmetic and subscripts, whose other characters carry no spans.
    pub(super) fn lex_region(&mut self, start: usize, end: usize) {
        let end = end.min(self.end);
        if start >= end {
            return;
        }
        let (saved_pos, saved_end) = (self.pos, self.end);
        self.pos = start;
        self.end = end;
        if self.enter() {
            let mut acc = WordAcc::scratch(start);
            while let Some(c) = self.peek() {
                match c {
                    b'$' => self.dollar(&mut acc, false),
                    b'`' => self.backquote(&mut acc),
                    b'\'' => self.single_quoted(&mut acc),
                    b'"' => self.double_quoted(&mut acc, self.pos),
                    b'\\' => {
                        self.pos += 1;
                        self.bump_char();
                    }
                    _ => {
                        self.bump_char();
                        while self
                            .peek()
                            .is_some_and(|c| !matches!(c, b'$' | b'`' | b'\'' | b'"' | b'\\'))
                        {
                            self.pos += 1;
                        }
                    }
                }
            }
            self.leave();
        }
        self.pos = saved_pos;
        self.end = saved_end;
    }

    /// An unbraced parameter: `$name`, `$1`, `$?`, `$#name`, `$=name`, with subscripts
    /// (`$arr[1]`) and modifiers (`$file:t`). Returns false, consuming nothing, when the `$` is
    /// literal.
    fn simple_param(&mut self, acc: &mut WordAcc) -> bool {
        let s = self.pos;
        let mut i = s + 1;
        while matches!(self.byte(i), Some(b'^' | b'=' | b'~')) {
            i += 1;
        }
        let mut end = None;
        if matches!(self.byte(i), Some(b'#' | b'+')) {
            end = self
                .name_end(i + 1)
                .or((self.byte(i) == Some(b'#')).then_some(i + 1));
        }
        if end.is_none() {
            end = self.name_end(i).or_else(|| match self.byte(i) {
                Some(b'0'..=b'9') => {
                    let mut j = i;
                    while matches!(self.byte(j), Some(b'0'..=b'9')) {
                        j += 1;
                    }
                    Some(j)
                }
                Some(b'?' | b'#' | b'$' | b'@' | b'*' | b'-' | b'!') => Some(i + 1),
                _ => None,
            });
        }
        let Some(mut e) = end else { return false };
        while self.byte(e) == Some(b'[') {
            e = self.subscript(e);
        }
        while self.byte(e) == Some(b':') {
            match self.modifier_end(e + 1, false) {
                Some(m) => e = m,
                None => break,
            }
        }
        self.push(s, e, TokenKind::Parameter);
        self.pos = e;
        acc.expand();
        true
    }

    /// A subscript `[...]` at `open` (which may nest and contain expansions); returns the
    /// offset after the closing `]`, or where the subscript is cut off.
    fn subscript(&mut self, open: usize) -> usize {
        let close = self.matching(Bracket::Subscript, open);
        let inner_end = match close {
            Ok(c) | Err(c) => c,
        };
        self.lex_region(open + 1, inner_end);
        close.map_or(inner_end, |c| c + 1)
    }

    /// A modifier after `:` at `i` (`h`, `t2`, `s/a/b/`, ...): its end. History expansion also
    /// accepts word designators and `p`.
    fn modifier_end(&self, i: usize, history: bool) -> Option<usize> {
        let digits = |mut j: usize| {
            while matches!(self.byte(j), Some(b'0'..=b'9')) {
                j += 1;
            }
            j
        };
        match self.byte(i)? {
            b'a' | b'A' | b'c' | b'e' | b'l' | b'P' | b'q' | b'Q' | b'r' | b'u' | b'x' | b'&' => {
                Some(i + 1)
            }
            b'h' | b't' => Some(digits(i + 1)),
            b's' => Some(self.delimited_end(i + 1)),
            b'g' => match self.byte(i + 1) {
                Some(b's') => Some(self.delimited_end(i + 2)),
                Some(b'&') => Some(i + 2),
                _ => None,
            },
            b'p' if history => Some(i + 1),
            b'0'..=b'9' if history => {
                let j = digits(i);
                Some(if self.byte(j) == Some(b'-') {
                    digits(j + 1)
                } else {
                    j
                })
            }
            b'-' if history => Some(digits(i + 1)),
            b'^' | b'$' | b'*' | b'%' if history => Some(i + 1),
            _ => None,
        }
    }

    /// `/old/new/` after an `s` modifier at `j`: the end of the third delimiter, or where the
    /// text is cut off by whitespace or the end.
    fn delimited_end(&self, j: usize) -> usize {
        let Some(d) = self.byte(j) else { return j };
        if !d.is_ascii() || d.is_ascii_whitespace() || is_word_term(d) {
            return j;
        }
        let mut k = j + 1;
        let mut seen = 0;
        while let Some(c) = self.byte(k) {
            if c.is_ascii_whitespace() {
                break;
            }
            k += 1;
            if c == d {
                seen += 1;
                if seen == 2 {
                    break;
                }
            } else if c == b'\\' && self.byte(k).is_some_and(|n| n.is_ascii()) {
                k += 1;
            }
        }
        k.min(self.end)
    }

    /// `${...}` at the cursor, with flags, operators, and nested expansions.
    fn brace_param(&mut self, acc: &mut WordAcc, quoted: bool) {
        let s = self.pos;
        self.pos += 2;
        acc.expand();
        if self.enter() {
            if self.peek() == Some(b'(') {
                self.param_flags();
            }
            let mut scratch = WordAcc::scratch(s);
            let mut depth = 0usize;
            while let Some(c) = self.peek() {
                match c {
                    b'}' => {
                        self.pos += 1;
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                    }
                    b'{' => {
                        depth += 1;
                        self.pos += 1;
                    }
                    b'\\' => {
                        let e = self.pos;
                        self.pos += 1;
                        self.bump_char();
                        self.push(e, self.pos, TokenKind::Escape);
                    }
                    b'\'' if !quoted => self.single_quoted(&mut scratch),
                    b'"' => self.double_quoted(&mut scratch, self.pos),
                    b'$' => self.dollar(&mut scratch, quoted),
                    b'`' => self.backquote(&mut scratch),
                    _ => self.bump_char(),
                }
            }
            self.leave();
        }
        self.push(s, self.pos, TokenKind::Parameter);
    }

    /// Parameter expansion flags `(...)` right after `${`, including delimited arguments such
    /// as `(j:,:)` and `(l:10::0:)`.
    fn param_flags(&mut self) {
        self.pos += 1;
        while let Some(c) = self.peek() {
            match c {
                b')' => {
                    self.pos += 1;
                    return;
                }
                b'j' | b's' | b'l' | b'r' | b'Z' | b'_' | b'I' | b'g' => {
                    self.pos += 1;
                    let Some(open) = self.peek() else { return };
                    if !open.is_ascii() || open.is_ascii_whitespace() || open == b')' {
                        continue;
                    }
                    let close = match open {
                        b'(' => b')',
                        b'[' => b']',
                        b'{' => b'}',
                        b'<' => b'>',
                        _ => open,
                    };
                    let max_args = if matches!(c, b'l' | b'r') { 3 } else { 1 };
                    let mut args = 0;
                    while args < max_args && self.peek() == Some(open) {
                        self.pos += 1;
                        while self.peek().is_some_and(|d| d != close) {
                            self.bump_char();
                        }
                        if self.peek() == Some(close) {
                            self.pos += 1;
                        }
                        args += 1;
                    }
                }
                _ => self.bump_char(),
            }
        }
    }

    /// History expansion at a `!`. Returns false, consuming nothing, when the `!` is literal.
    fn history(&mut self, acc: &mut WordAcc, in_dq: bool) -> bool {
        let s = self.pos;
        let Some(n) = self.peek_at(1) else {
            return false;
        };
        if matches!(n, b' ' | b'\t' | b'\n' | b'=' | b'(') || (in_dq && n == b'"') {
            return false;
        }
        let digits = |mut j: usize| {
            while matches!(self.byte(j), Some(b'0'..=b'9')) {
                j += 1;
            }
            j
        };
        let mut i = s + 1;
        match n {
            b'!' | b'$' | b'^' | b'*' | b'#' => i += 1,
            b'-' => {
                if !matches!(self.byte(i + 1), Some(b'0'..=b'9')) {
                    return false;
                }
                i = digits(i + 1);
            }
            b'0'..=b'9' => i = digits(i),
            b'?' | b'{' => {
                let close = if n == b'?' { b'?' } else { b'}' };
                i += 1;
                while let Some(c) = self.byte(i) {
                    if c == b'\n' {
                        break;
                    }
                    i += 1;
                    if c == close {
                        break;
                    }
                }
            }
            b':' => {}
            _ => {
                let from = i;
                while let Some(c) = self.byte(i) {
                    if is_word_term(c)
                        || matches!(c, b'(' | b':' | b'"' | b'\'' | b'`' | b'$' | b'\\')
                    {
                        break;
                    }
                    i += 1;
                }
                if i == from {
                    return false;
                }
            }
        }
        while self.byte(i) == Some(b':') {
            match self.modifier_end(i + 1, true) {
                Some(e) => i = e,
                None => break,
            }
        }
        self.push(s, i, TokenKind::HistoryExpansion);
        self.pos = i;
        acc.expand();
        true
    }
}
