//! Command-level grammar: lists, pipelines, compound commands, redirections, and here-documents.
//!
//! The parser is a single left-to-right pass over the bytes of the input. Compound commands are
//! tracked with a per-list stack of open constructs ([`Open`]) instead of recursive descent, which
//! keeps it tolerant of partial and malformed input: a closer that matches nothing is an error,
//! an opener that is never closed is not. Recursion happens only for constructs with their own
//! lexical extent (command, process, and arithmetic substitution, backquotes, `${...}`, double
//! quotes, glob groups), and is bounded by [`MAX_DEPTH`].
//!
//! Every structural character is ASCII, so any offset at which the parser stops on such a
//! character is a UTF-8 boundary. Non-ASCII text is only ever stepped over a whole character at a
//! time (see [`Parser::bump_char`]).

use super::word::{WordAcc, is_word_term};
use super::{ParseOptions, ParseOutput, SimpleCommand, Word};
use crate::token::{Span, TokenKind, check_spans, sort_spans};

/// Nesting limit for recursive constructs. Beyond it the rest of the current construct is left
/// unhighlighted instead of descending further.
pub(super) const MAX_DEPTH: usize = 256;

/// What ends a list being parsed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Term {
    /// The end of the current limit (end of input, or the closing backquote).
    Eof,
    /// An unmatched `)`, which the caller consumes (`$(`, `<(`, `>(`, `=(`).
    Paren,
}

/// What the previous token in a list was, for error detection and command position.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Last {
    /// Start of a list, or after `;`, `&`, or a newline.
    Start,
    /// After `&&` or `||`. Another list operator may not follow, but zsh accepts a separator.
    Op,
    /// After `|` or `|&`: a command must follow.
    Pipe,
    /// After a reserved word that must be followed by a command (`if`, `then`, `do`, `{`, ...).
    Kw,
    /// After the header of a function definition, before its body. `parens` is set for the
    /// `name ()` and `()` forms, which a separator may not follow.
    FuncBody { anon: bool, parens: bool },
    /// After `always`: only the `{` of the always block may follow.
    Always,
    /// Inside a simple command.
    Cmd,
    /// Right after the end of a compound command (`fi`, `}`, `)`, `]]`, `))`, ...). Only a
    /// separator, a redirection, or a reserved word that continues an enclosing construct may
    /// follow, and `always` when the construct was a plain `{ ... }` group. `rbrace` is false
    /// after `fi`, `done`, and `end`, where zsh does not take a `}` as being in command
    /// position, so with `IGNORE_BRACES` or `IGNORE_CLOSE_BRACES` it is an ordinary word.
    Compound { always: bool, rbrace: bool },
    /// After the body of an anonymous function: the words up to the next separator are the
    /// function's arguments.
    Args,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum IfState {
    /// Between `if`/`elif` and `then`; `seen` once the condition has a command.
    Cond {
        seen: bool,
    },
    Body,
    Else,
    /// Inside the `{ ... }` of the short form `if [[ x ]] { ... }`.
    ShortBody,
    /// After the short-form body; `elif` and `else` may still follow.
    ShortDone,
    /// After `else` in the short form, waiting for `{`.
    ShortElse,
    /// Inside the short-form `else { ... }`.
    ShortElseBody,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LoopKind {
    /// `while` and `until`: a condition list, then `do`.
    While,
    /// `for`, `select`, and `repeat`: a header, then `do`, `{`, or a single command.
    For,
    /// `foreach name (words) ... end`.
    Foreach,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LoopState {
    Cond { seen: bool },
    ExpectDo,
    Body,
    ShortBody,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CaseState {
    ExpectIn,
    Pattern,
    Body,
}

/// What a `{ ... }` group is the body of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BraceKind {
    /// A plain group, which `always { ... }` may follow.
    Plain,
    /// The body of a short-form `if`, `while`, `for`, `select`, or `repeat`.
    Short,
    /// The body of a named function.
    Func,
    /// The body of an anonymous function, which arguments may follow.
    Anon,
    /// The block after `always`.
    Always,
}

/// An open compound construct awaiting its closer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Open {
    If(IfState),
    Loop(LoopKind, LoopState),
    Case {
        brace: bool,
        state: CaseState,
    },
    Brace(BraceKind),
    /// A subshell; `anon` when it is the body of an anonymous function.
    Paren {
        anon: bool,
    },
}

/// The open constructs a closer or continuation word searches the stack for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    /// `then`: an `if` or `elif` condition.
    Then,
    /// `elif` and `else`: an `if` body or a completed short-form `if`.
    Else,
    /// `fi`: any `if`.
    Fi,
    /// `do`: a `while` condition or a `for` header.
    Do,
    /// `done`: any `while`, `until`, `for`, `select`, or `repeat`.
    Done,
    /// `end`: a `foreach`.
    End,
    /// `esac`: a `case ... in`, or a `case ... {`, which zsh lets `esac` close too.
    Esac,
    /// `;;`, `;&`, `;|`: the body of a case item.
    CaseSep,
    /// `}`: a group or a `case ... {`.
    RBrace,
    /// `)`: a subshell.
    Paren,
}

const CLASS_COUNT: usize = 10;

impl Open {
    /// The classes this construct belongs to, as a bit set indexed by [`Class`].
    fn classes(self) -> u16 {
        let bit = |c: Class| 1u16 << c as u16;
        match self {
            Open::If(state) => {
                bit(Class::Fi)
                    | match state {
                        IfState::Cond { .. } => bit(Class::Then),
                        IfState::Body | IfState::ShortDone => bit(Class::Else),
                        _ => 0,
                    }
            }
            Open::Loop(LoopKind::Foreach, _) => bit(Class::End),
            Open::Loop(_, state) => {
                bit(Class::Done)
                    | match state {
                        LoopState::Cond { .. } | LoopState::ExpectDo => bit(Class::Do),
                        _ => 0,
                    }
            }
            Open::Case { brace, state } => {
                (if brace {
                    bit(Class::RBrace) | bit(Class::Esac)
                } else {
                    bit(Class::Esac)
                }) | if state == CaseState::Body {
                    bit(Class::CaseSep)
                } else {
                    0
                }
            }
            Open::Brace(_) => bit(Class::RBrace),
            Open::Paren { .. } => bit(Class::Paren),
        }
    }
}

/// The stack of open constructs in one list, with a count of the constructs in each [`Class`]
/// so that a search for a closer that matches nothing costs constant time instead of a scan of
/// the whole stack. A successful search discards everything above the match, so searches are
/// amortised constant time as well.
#[derive(Default)]
struct Stack {
    items: Vec<Open>,
    counts: [u32; CLASS_COUNT],
}

impl Stack {
    fn count(&mut self, o: Open, add: bool) {
        let bits = o.classes();
        for (i, n) in self.counts.iter_mut().enumerate() {
            if bits & (1 << i) != 0 {
                if add {
                    *n += 1;
                } else {
                    *n -= 1;
                }
            }
        }
    }

    fn last(&self) -> Option<Open> {
        self.items.last().copied()
    }

    fn push(&mut self, o: Open) {
        self.count(o, true);
        self.items.push(o);
    }

    fn pop(&mut self) -> Option<Open> {
        let o = self.items.pop()?;
        self.count(o, false);
        Some(o)
    }

    /// Replaces the innermost construct.
    fn set_last(&mut self, o: Open) {
        if self.pop().is_some() {
            self.push(o);
        }
    }

    /// Finds the innermost construct in `class` and discards everything above it, so the
    /// match ends up on top. Returns `Some(true)` when it already was on top, `Some(false)` when
    /// constructs had to be discarded (an error), and `None` when nothing matches.
    fn find(&mut self, class: Class) -> Option<bool> {
        if self.counts[class as usize] == 0 {
            return None;
        }
        let bit = 1u16 << class as u16;
        let idx = self.items.iter().rposition(|o| o.classes() & bit != 0)?;
        let top = idx + 1 == self.items.len();
        while self.items.len() > idx + 1 {
            self.pop();
        }
        Some(top)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kw {
    If,
    Then,
    Elif,
    Else,
    Fi,
    Case,
    Esac,
    For,
    While,
    Do,
    Done,
    Repeat,
    Function,
    Coproc,
    Foreach,
    End,
    LBrace,
    RBrace,
    Bang,
    CondOpen,
    CondClose,
}

fn keyword(s: &str) -> Option<Kw> {
    Some(match s {
        "if" => Kw::If,
        "then" => Kw::Then,
        "elif" => Kw::Elif,
        "else" => Kw::Else,
        "fi" => Kw::Fi,
        "case" => Kw::Case,
        "esac" => Kw::Esac,
        "for" | "select" => Kw::For,
        "while" | "until" => Kw::While,
        "do" => Kw::Do,
        "done" => Kw::Done,
        "repeat" => Kw::Repeat,
        "function" => Kw::Function,
        "coproc" => Kw::Coproc,
        "foreach" => Kw::Foreach,
        "end" => Kw::End,
        "{" => Kw::LBrace,
        "}" => Kw::RBrace,
        "!" => Kw::Bang,
        "[[" => Kw::CondOpen,
        "]]" => Kw::CondClose,
        _ => return None,
    })
}

impl Kw {
    /// Reserved words that begin a command which `time` may precede.
    fn begins_compound(self) -> bool {
        matches!(
            self,
            Kw::If
                | Kw::Case
                | Kw::For
                | Kw::While
                | Kw::Repeat
                | Kw::Foreach
                | Kw::LBrace
                | Kw::Bang
                | Kw::CondOpen
        )
    }

    /// Reserved words that may directly follow the end of a compound command.
    fn continues_compound(self) -> bool {
        matches!(
            self,
            Kw::Then
                | Kw::Elif
                | Kw::Else
                | Kw::Fi
                | Kw::Esac
                | Kw::Do
                | Kw::Done
                | Kw::End
                | Kw::LBrace
                | Kw::RBrace
                | Kw::CondClose
        )
    }
}

/// Builtins whose `name=value` arguments are assignments.
fn is_declaration(word: &str) -> bool {
    matches!(
        word,
        "local" | "typeset" | "declare" | "export" | "readonly" | "integer" | "float" | "private"
    )
}

/// Precommand modifiers that may precede a declaration builtin (`builtin local x=1`).
fn is_precommand(word: &str) -> bool {
    matches!(
        word,
        "builtin" | "command" | "noglob" | "nocorrect" | "exec" | "-"
    )
}

/// `[[ ... ]]` operators that are recognised as whole unquoted words.
const COND_OPERATORS: &[&str] = &[
    "-a", "-b", "-c", "-d", "-e", "-f", "-g", "-h", "-k", "-n", "-o", "-p", "-r", "-s", "-t", "-u",
    "-v", "-w", "-x", "-z", "-A", "-C", "-G", "-L", "-N", "-O", "-S", "-nt", "-ot", "-ef", "-eq",
    "-ne", "-lt", "-le", "-gt", "-ge", "=", "==", "!=", "=~",
];

/// The simple command being assembled.
#[derive(Default)]
struct Cmd {
    words: Vec<Word>,
    /// Assignments were seen before the command word.
    assign: bool,
    /// Redirections were seen before the command word.
    redir: bool,
    /// The command is a declaration builtin, so `name=value` arguments are assignments.
    decl: bool,
    /// The declaration builtin is reached through a precommand (`builtin export`), so it is an
    /// ordinary builtin and its `name=value` arguments are globbed like any other word.
    decl_globbed: bool,
    /// Every word so far is a precommand modifier (`builtin`, `command`, ...).
    chain: bool,
    /// Words are lexed but no command is recorded (after `^old^new`, and after a short-form
    /// body rejected by `NO_SHORT_LOOPS`).
    suppressed: bool,
}

impl Cmd {
    /// Assignments or redirections were seen before the command word.
    fn prefix(&self) -> bool {
        self.assign || self.redir
    }

    /// The command so far is the single word `time`, which may precede a compound command.
    fn is_time(&self) -> bool {
        !self.prefix() && self.words.len() == 1 && self.words[0].literal.as_deref() == Some("time")
    }
}

struct Frame {
    stack: Stack,
    last: Last,
    cmd: Cmd,
    term: Term,
}

impl Frame {
    fn new(term: Term) -> Frame {
        Frame {
            stack: Stack::default(),
            last: Last::Start,
            cmd: Cmd::default(),
            term,
        }
    }

    fn case_pattern_expected(&self) -> bool {
        matches!(
            self.stack.last(),
            Some(Open::Case {
                state: CaseState::Pattern | CaseState::ExpectIn,
                ..
            })
        )
    }
}

/// A here-document whose body starts after the next newline.
struct PendingHeredoc {
    delim: String,
    strip_tabs: bool,
    expand: bool,
}

/// How a redirection operator relates to its target.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Redir {
    /// A file target follows.
    File,
    /// The operator ends in `&` (`>&`, `<&`); a target of digits is an fd, not a path.
    DupWord,
    /// The fd or `-` is part of the operator (`2>&1`, `>&-`); no target follows.
    Dup,
    Heredoc {
        strip_tabs: bool,
    },
    HereString,
}

/// Result of scanning for the end of `((...))` or `$((...))`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ArithEnd {
    /// The offset of the first of the two closing parentheses.
    Closed(usize),
    /// Input ends inside the expression.
    Unclosed,
    /// The parentheses close separately, so this is a nested subshell, not arithmetic.
    NotArith,
}

pub(super) fn utf8_len(lead: u8) -> usize {
    match lead {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xff => 4,
        _ => 1,
    }
}

pub(super) struct Parser<'a> {
    pub(super) src: &'a str,
    pub(super) b: &'a [u8],
    pub(super) pos: usize,
    /// Exclusive limit of the region being parsed; narrowed inside backquotes and arithmetic.
    pub(super) end: usize,
    pub(super) opts: ParseOptions,
    depth: usize,
    spans: Vec<Span>,
    commands: Vec<SimpleCommand>,
    pub(super) path_words: Vec<Word>,
    heredocs: Vec<PendingHeredoc>,
    /// Only spans are wanted: no commands, path words, or word literals are built, except the
    /// literals the grammar itself depends on.
    pub(super) spans_only: bool,
    /// The range and limit of the last failed `[` scan; see [`Parser::bracket_end`].
    pub(super) bracket_fail: (usize, usize, usize),
    /// The `[`s inside `bracket_fail` from which a scan is known to fail, in order.
    pub(super) bracket_fail_at: Vec<usize>,
    /// Where the last lookup in `bracket_fail_at` ended.
    pub(super) bracket_fail_cursor: usize,
    /// Spare buffer for the next failed scan's list.
    pub(super) bracket_scratch: Vec<usize>,
    /// Matching closers found by bracket scans, indexed by [`Bracket`].
    pub(super) memos: [Memo; 3],
}

/// Bracket pairs whose closer is found by a memoised scan.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Bracket {
    /// `(` and `)` of arithmetic, skipping quoted text and escaped characters.
    Arith,
    /// `[` and `]` of a subscript, skipping escaped characters and stopping at a newline.
    Subscript,
    /// `[` and `]` of the old arithmetic expansion `$[...]`.
    OldArith,
}

/// Remembers, for each opening bracket a scan has passed over, where its matching closer is.
///
/// Scans for the end of `((...))`, `$((...))`, `$[...]`, and subscripts can be nested inside each
/// other's range (`$(( $(( $(( ...`), and rescanning from each would be quadratic. A scan that reaches
/// an opening bracket as a token of its own sees exactly the bytes and quoting a scan starting
/// at that bracket would see, so it records the match for every opener it passes, and a later
/// scan from one of them is answered from the table. Each entry is 0 (unknown), the closer's
/// offset plus one, or [`Memo::OPEN`] with the offset where the scan gave up. The table is
/// allocated on first use and only for inputs whose offsets fit.
#[derive(Default)]
pub(super) struct Memo {
    table: Vec<u32>,
    /// Openers whose closer has not been seen yet during the current scan.
    stack: Vec<usize>,
}

impl Memo {
    const OPEN: u32 = 1 << 31;

    /// The recorded result for the opener at `open`: `Ok(close)`, `Err(stop)` when the scan
    /// stopped at `stop` without finding the closer, or `None` when unknown.
    pub(super) fn get(&self, open: usize) -> Option<Result<usize, usize>> {
        let v = *self.table.get(open)?;
        if v == 0 {
            None
        } else if v & Memo::OPEN != 0 {
            Some(Err((v & !Memo::OPEN) as usize))
        } else {
            Some(Ok(v as usize - 1))
        }
    }

    /// Starts a scan from the opener at `open` in an input of `len` bytes.
    pub(super) fn begin(&mut self, open: usize, len: usize) {
        if self.table.is_empty() && len < Memo::OPEN as usize - 1 {
            self.table = vec![0; len + 1];
        }
        self.stack.clear();
        self.stack.push(open);
    }

    pub(super) fn opened(&mut self, open: usize) {
        self.stack.push(open);
    }

    /// Records a closer at `close`; returns true when it matches the scan's first opener.
    pub(super) fn closed(&mut self, close: usize) -> bool {
        if let Some(open) = self.stack.pop()
            && let Some(slot) = self.table.get_mut(open)
        {
            *slot = close as u32 + 1;
        }
        self.stack.is_empty()
    }

    /// Ends a scan that stopped at `stop` with openers still unmatched.
    pub(super) fn stopped(&mut self, stop: usize) {
        for &open in &self.stack {
            if let Some(slot) = self.table.get_mut(open) {
                *slot = stop as u32 | Memo::OPEN;
            }
        }
        self.stack.clear();
    }
}

impl<'a> Parser<'a> {
    pub(super) fn new(src: &'a str, opts: ParseOptions, spans_only: bool) -> Parser<'a> {
        Parser {
            src,
            b: src.as_bytes(),
            pos: 0,
            end: src.len(),
            opts,
            depth: 0,
            // Typical input has a span every four to eight bytes.
            spans: Vec::with_capacity((src.len() / 4).min(1 << 14)),
            commands: Vec::new(),
            path_words: Vec::new(),
            heredocs: Vec::new(),
            spans_only,
            bracket_fail: (0, 0, 0),
            bracket_fail_at: Vec::new(),
            bracket_fail_cursor: 0,
            bracket_scratch: Vec::new(),
            memos: Default::default(),
        }
    }

    // ---------------------------------------------------------------------------------------
    // Cursor helpers
    // ---------------------------------------------------------------------------------------

    pub(super) fn byte(&self, i: usize) -> Option<u8> {
        if i < self.end { Some(self.b[i]) } else { None }
    }

    pub(super) fn peek(&self) -> Option<u8> {
        self.byte(self.pos)
    }

    pub(super) fn peek_at(&self, off: usize) -> Option<u8> {
        self.byte(self.pos + off)
    }

    /// Advances over one whole character.
    pub(super) fn bump_char(&mut self) {
        if self.pos < self.end {
            self.pos = (self.pos + utf8_len(self.b[self.pos])).min(self.end);
        }
    }

    pub(super) fn push(&mut self, start: usize, end: usize, kind: TokenKind) {
        if start < end {
            self.spans.push(Span::new(start, end, kind));
        }
    }

    pub(super) fn span_count(&self) -> usize {
        self.spans.len()
    }

    pub(super) fn truncate_spans(&mut self, len: usize) {
        self.spans.truncate(len);
    }

    /// Enters one level of recursion. At the limit, skips the rest of the current region and
    /// returns false.
    pub(super) fn enter(&mut self) -> bool {
        if self.depth >= MAX_DEPTH {
            self.pos = self.end;
            false
        } else {
            self.depth += 1;
            true
        }
    }

    pub(super) fn leave(&mut self) {
        self.depth -= 1;
    }

    /// The word at `i` when it consists only of plain characters (no quoting or expansion) and
    /// is followed by a word terminator or the end of the region.
    fn plain_word_at(&self, i: usize) -> Option<&'a str> {
        let mut j = i;
        while j < self.end {
            let c = self.b[j];
            if is_word_term(c) || c == b'(' {
                break;
            }
            if matches!(c, b'\'' | b'"' | b'\\' | b'$' | b'`') {
                return None;
            }
            j += 1;
        }
        let src: &'a str = self.src;
        (j > i).then(|| &src[i..j])
    }

    /// True when the characters at `i` start a word terminator, or `i` is at the end.
    fn term_at(&self, i: usize) -> bool {
        self.byte(i).is_none_or(|c| is_word_term(c) || c == b'(')
    }

    /// Skips spaces, tabs, and line continuations.
    pub(super) fn skip_blanks(&mut self) {
        while let Some(c) = self.peek() {
            match c {
                b' ' | b'\t' => self.pos += 1,
                b'\\' if self.peek_at(1) == Some(b'\n') => {
                    self.push(self.pos, self.pos + 2, TokenKind::Escape);
                    self.pos += 2;
                }
                _ => break,
            }
        }
    }

    /// Consumes a newline and any here-document bodies that start after it.
    fn newline(&mut self) {
        self.pos += 1;
        if !self.heredocs.is_empty() {
            self.read_heredoc_bodies();
        }
    }

    fn comment(&mut self) {
        let start = self.pos;
        let end = self.b[start..self.end]
            .iter()
            .position(|&c| c == b'\n')
            .map_or(self.end, |n| start + n);
        self.push(start, end, TokenKind::Comment);
        self.pos = end;
    }

    // ---------------------------------------------------------------------------------------
    // Entry points
    // ---------------------------------------------------------------------------------------

    pub(super) fn run(&mut self) {
        let mut f = Frame::new(Term::Eof);
        if self.b.first() == Some(&b'^') {
            // `^old^new`: quick history substitution, only at the very start of the input.
            let mut e = 1;
            while e < self.end && !matches!(self.b[e], b' ' | b'\t' | b'\n') {
                e += 1;
            }
            self.push(0, e, TokenKind::HistoryExpansion);
            self.pos = e;
            f.cmd.suppressed = true;
            f.cmd.chain = true;
            f.last = Last::Cmd;
        }
        self.list_loop(&mut f);
        self.end_cmd(&mut f);
    }

    /// Parses a nested list (the inside of `$(...)`, backquotes, or a process substitution).
    pub(super) fn parse_list(&mut self, term: Term) {
        let mut f = Frame::new(term);
        self.list_loop(&mut f);
        self.end_cmd(&mut f);
    }

    pub(super) fn finish(mut self) -> ParseOutput {
        sort_spans(&mut self.spans);
        let check = check_spans(self.src, &self.spans);
        debug_assert!(check.is_ok(), "invalid spans for {:?}: {check:?}", self.src);
        if check.is_err() {
            repair_spans(self.src, &mut self.spans);
        }
        self.commands
            .sort_by_key(|c| c.words.first().map_or(0, |w| w.start));
        self.path_words.sort_by_key(|w| w.start);
        ParseOutput {
            spans: self.spans,
            commands: self.commands,
            path_words: self.path_words,
        }
    }

    // ---------------------------------------------------------------------------------------
    // Lists and commands
    // ---------------------------------------------------------------------------------------

    fn list_loop(&mut self, f: &mut Frame) {
        let mut guard = (usize::MAX, 0u8);
        loop {
            self.skip_blanks();
            let Some(c) = self.peek() else { return };
            // Every branch below consumes input or changes state; this catches a branch that
            // does neither, so a bug degrades highlighting instead of hanging the daemon.
            if guard.0 == self.pos {
                guard.1 += 1;
                if guard.1 > 3 {
                    debug_assert!(false, "no progress at {} in {:?}", self.pos, self.src);
                    self.bump_char();
                    continue;
                }
            } else {
                guard = (self.pos, 0);
            }
            if self.short_body_ahead(f, c) {
                self.short_body_error(f);
                continue;
            }
            match c {
                b'\n' => {
                    self.end_cmd(f);
                    self.newline();
                    if !matches!(
                        f.last,
                        Last::Op | Last::Pipe | Last::Kw | Last::FuncBody { .. } | Last::Always
                    ) {
                        f.last = Last::Start;
                    }
                }
                b'#' if self.opts.interactive_comments => self.comment(),
                _ if f.case_pattern_expected() && self.case_pattern_char(c) => self.case_pattern(f),
                b';' => self.semicolon(f),
                b'&' if self.peek_at(1) == Some(b'>') => self.redirection(f),
                b'&' => self.ampersand(f),
                b'|' => self.pipe(f),
                b'<' | b'>' if self.peek_at(1) == Some(b'(') => self.word_token(f),
                b'<' if self.numeric_range_at(self.pos).is_some() => self.word_token(f),
                b'<' | b'>' => self.redirection(f),
                b'(' => self.open_paren(f),
                b')' => {
                    if self.close_paren(f) {
                        return;
                    }
                }
                b'0'..=b'9' if self.fd_redirect_ahead() => self.redirection(f),
                b'{' if self.brace_fd_ahead() => self.redirection(f),
                _ => self.word_token(f),
            }
        }
    }

    fn end_cmd(&mut self, f: &mut Frame) {
        let mut cmd = std::mem::take(&mut f.cmd);
        if self.spans_only {
            // Keep the buffer for the next command instead of reallocating it.
            cmd.words.clear();
            f.cmd.words = cmd.words;
        } else if !cmd.words.is_empty() && !cmd.suppressed {
            self.commands.push(SimpleCommand { words: cmd.words });
        }
    }

    /// Whether a word that may continue a precommand chain needs its literal: always, except
    /// in a spans-only parse of a plain word that names no declaration builtin, precommand, or
    /// `time`, whose literal the grammar never looks at.
    fn literal_needed(&self, plain: Option<&str>) -> bool {
        !self.spans_only
            || plain.is_none_or(|p| is_declaration(p) || is_precommand(p) || p == "time")
    }

    /// Records a word outside simple commands that should be checked as a path.
    fn path_word(&mut self, w: Word) {
        if !self.spans_only {
            self.path_words.push(w);
        }
    }

    /// Applies the effect of a new command starting in the current list: it satisfies an
    /// `if`/`while` condition, is the body of a short-form `for`, or completes a short `if`.
    fn note_command_start(&mut self, f: &mut Frame) {
        loop {
            match f.stack.last() {
                Some(Open::If(IfState::Cond { seen: false })) => {
                    f.stack.set_last(Open::If(IfState::Cond { seen: true }));
                }
                Some(Open::Loop(LoopKind::While, LoopState::Cond { seen: false })) => {
                    f.stack
                        .set_last(Open::Loop(LoopKind::While, LoopState::Cond { seen: true }));
                }
                Some(Open::Loop(LoopKind::For, LoopState::ExpectDo))
                | Some(Open::If(IfState::ShortDone)) => {
                    f.stack.pop();
                    continue;
                }
                _ => {}
            }
            return;
        }
    }

    /// True when, with `NO_SHORT_LOOPS`, the token at the cursor (starting with `c`) begins the
    /// short-form body of a `for`, `select`, or `repeat` header, which needs `do` or `{`. A
    /// prefix of `do` at the end of the input is partial input, not a body.
    fn short_body_ahead(&self, f: &Frame, c: u8) -> bool {
        if !self.opts.no_short_loops
            || f.stack.last() != Some(Open::Loop(LoopKind::For, LoopState::ExpectDo))
            || matches!(c, b'\n' | b';' | b'&' | b'|' | b')')
            || (c == b'#' && self.opts.interactive_comments)
            || self.lbrace_at(self.pos)
        {
            return false;
        }
        match self.plain_word_at(self.pos) {
            Some("do") => false,
            Some(p) => !("do".starts_with(p) && self.pos + p.len() == self.b.len()),
            None => true,
        }
    }

    /// Marks the word that starts a short-form body as an error, as zsh rejects it, and drops
    /// the loop. The words after it are its arguments, and no command is recorded, so the
    /// error is the word's only span.
    fn short_body_error(&mut self, f: &mut Frame) {
        f.stack.pop();
        self.error_word();
        f.cmd.suppressed = true;
        f.last = Last::Cmd;
    }

    /// Completes short-form `if`s that cannot be continued by what follows.
    fn settle(f: &mut Frame) {
        while f.stack.last() == Some(Open::If(IfState::ShortDone)) {
            f.stack.pop();
        }
    }

    /// True when a `}` at `i` is a word of its own: unquoted and followed by a word terminator
    /// or the end. Without `IGNORE_CLOSE_BRACES` (and `IGNORE_BRACES`) such a `}` closes a group
    /// wherever it appears; with either, it is never significant where this is asked.
    fn rbrace_at(&self, i: usize) -> bool {
        !self.opts.close_braces_ignored()
            && self.byte(i) == Some(b'}')
            && self.byte(i + 1).is_none_or(is_word_term)
    }

    /// True when the word at `i` starts with a `{` that is a reserved word in command position:
    /// zsh splits a `{` off the start of a word (`{echo hi}` is a group), but with
    /// `IGNORE_BRACES` only a whole `{` word counts.
    fn lbrace_at(&self, i: usize) -> bool {
        match self.opts.ignore_braces {
            true => self.plain_word_at(i) == Some("{"),
            false => self.byte(i) == Some(b'{'),
        }
    }

    /// Consumes the word at the cursor and marks all of it as an error.
    fn error_word(&mut self) {
        let start = self.pos;
        let before = self.span_count();
        let w = self.lex_word();
        self.truncate_spans(before);
        self.push(start, w.end, TokenKind::Error);
    }

    fn word_token(&mut self, f: &mut Frame) {
        let start = self.pos;
        let plain = self.plain_word_at(start);
        let kw = plain.and_then(keyword);
        // `{fd}>` has already been taken as a redirection.
        let brace = self.lbrace_at(start);
        // With IGNORE_BRACES or IGNORE_CLOSE_BRACES, a `}` is significant only in command
        // position.
        let close_ignored = self.opts.close_braces_ignored();
        match f.last {
            Last::Compound { always, rbrace } => {
                if always && plain == Some("always") {
                    self.push(start, start + 6, TokenKind::ReservedWord);
                    self.pos = start + 6;
                    f.last = Last::Always;
                } else if brace {
                    self.keyword(f, Kw::LBrace, start, 1);
                } else if let Some(kw) = kw
                    && kw.continues_compound()
                    && (kw != Kw::RBrace || rbrace || !close_ignored)
                {
                    self.keyword(f, kw, start, plain.map_or(0, str::len));
                } else {
                    // A word right after `fi`, `}`, `)`, `]]` ... is a syntax error.
                    self.error_word();
                }
                return;
            }
            Last::Always => {
                if brace {
                    self.keyword(f, Kw::LBrace, start, 1);
                } else {
                    self.error_word();
                }
                return;
            }
            Last::Args if kw != Some(Kw::RBrace) || close_ignored => {
                // Arguments of an anonymous function: plain words, even reserved ones.
                let w = self.lex_word();
                self.path_word(w);
                return;
            }
            _ => {}
        }
        if kw == Some(Kw::RBrace) && !close_ignored {
            // Without IGNORE_CLOSE_BRACES a lone `}` is significant anywhere in a command.
            self.end_cmd(f);
            self.keyword(f, Kw::RBrace, start, 1);
            return;
        }
        if f.cmd.is_time() && (brace || kw.is_some_and(Kw::begins_compound)) {
            // `time` is a reserved word in zsh and may precede a compound command.
            self.end_cmd(f);
        }
        let cmd_pos = f.cmd.words.is_empty() && !f.cmd.suppressed;
        if cmd_pos {
            let reserved = if brace {
                Some((Kw::LBrace, 1))
            } else {
                kw.map(|k| (k, plain.map_or(0, str::len)))
            };
            if let Some((rkw, len)) = reserved {
                if !f.cmd.assign {
                    if rkw == Kw::Bang && (f.last == Last::Pipe || f.cmd.redir) {
                        // zsh accepts `!` only at the start of a pipeline, before redirections.
                        self.push(start, start + 1, TokenKind::Error);
                        self.pos = start + 1;
                        f.last = Last::Kw;
                        return;
                    }
                    // Redirections may precede a compound command (`> f { ... }`).
                    self.end_cmd(f);
                    self.keyword(f, rkw, start, len);
                    return;
                }
                if kw.is_some() {
                    // zsh rejects a reserved word after an assignment (`x=1 if`).
                    self.error_word();
                    f.cmd.words.push(Word {
                        start,
                        end: self.pos,
                        literal: plain.map(str::to_string),
                        tilde: false,
                        has_glob: false,
                        name_eq: false,
                        equals: false,
                    });
                    f.last = Last::Cmd;
                    return;
                }
            }
            if self.assignment_ahead(start).is_some() {
                if !f.cmd.prefix() {
                    self.note_command_start(f);
                }
                self.assignment(f, false);
                return;
            }
            if !f.cmd.prefix() {
                self.note_command_start(f);
            }
            let w = self.lex_word_with(self.literal_needed(plain));
            if let Some((open, close)) = self.funcdef_parens() {
                // `name () body`: a function definition, not a command.
                self.push(w.start, w.end, TokenKind::Function);
                self.push(open, open + 1, TokenKind::Grouping);
                self.push(close, close + 1, TokenKind::Grouping);
                self.pos = close + 1;
                f.cmd = Cmd::default();
                f.last = Last::FuncBody {
                    anon: false,
                    parens: true,
                };
                return;
            }
            f.cmd.decl = w.literal.as_deref().is_some_and(is_declaration);
            f.cmd.chain = w.literal.as_deref().is_some_and(is_precommand);
            f.cmd.words.push(w);
        } else {
            if f.cmd.decl && self.assignment_ahead(start).is_some() {
                self.assignment(f, true);
                return;
            }
            // Only a word of a precommand chain needs its literal to recognise a declaration.
            let w =
                self.lex_word_with(!self.spans_only || f.cmd.chain && self.literal_needed(plain));
            if !f.cmd.decl && f.cmd.chain && w.literal.as_deref().is_some_and(is_declaration) {
                f.cmd.decl = true;
                f.cmd.decl_globbed = true;
            }
            f.cmd.chain = f.cmd.chain && w.literal.as_deref().is_some_and(is_precommand);
            f.cmd.words.push(w);
        }
        f.last = Last::Cmd;
    }

    /// When a `(`, optional blanks, and `)` follow, returns their offsets without consuming.
    fn funcdef_parens(&self) -> Option<(usize, usize)> {
        let mut i = self.pos;
        while matches!(self.byte(i), Some(b' ' | b'\t')) {
            i += 1;
        }
        if self.byte(i) != Some(b'(') {
            return None;
        }
        let open = i;
        i += 1;
        while matches!(self.byte(i), Some(b' ' | b'\t')) {
            i += 1;
        }
        (self.byte(i) == Some(b')')).then_some((open, i))
    }

    fn keyword(&mut self, f: &mut Frame, kw: Kw, start: usize, len: usize) {
        let end = start + len;
        self.pos = end;
        let rw = TokenKind::ReservedWord;
        let err = TokenKind::Error;
        let ok = |top: Option<bool>| if top == Some(true) { rw } else { err };
        match kw {
            Kw::If => {
                self.note_command_start(f);
                f.stack.push(Open::If(IfState::Cond { seen: false }));
                self.push(start, end, rw);
                f.last = Last::Kw;
            }
            Kw::While => {
                self.note_command_start(f);
                f.stack
                    .push(Open::Loop(LoopKind::While, LoopState::Cond { seen: false }));
                self.push(start, end, rw);
                f.last = Last::Kw;
            }
            Kw::Then => {
                Self::settle(f);
                // zsh accepts an empty condition (`if then`).
                let top = f.stack.find(Class::Then);
                if top.is_some() {
                    f.stack.set_last(Open::If(IfState::Body));
                }
                self.push(start, end, ok(top));
                f.last = Last::Kw;
            }
            Kw::Elif | Kw::Else => {
                let top = f.stack.find(Class::Else);
                if top.is_some()
                    && let Some(Open::If(state)) = f.stack.last()
                {
                    f.stack.set_last(Open::If(match (kw, state) {
                        (Kw::Elif, _) => IfState::Cond { seen: false },
                        (_, IfState::ShortDone) => IfState::ShortElse,
                        _ => IfState::Else,
                    }));
                }
                self.push(start, end, ok(top));
                f.last = Last::Kw;
            }
            Kw::Fi => {
                Self::settle(f);
                let mut top = f.stack.find(Class::Fi);
                if top.is_some()
                    && let Some(Open::If(state)) = f.stack.pop()
                    && !matches!(state, IfState::Body | IfState::Else)
                {
                    top = Some(false);
                }
                self.push(start, end, ok(top));
                f.last = Last::Compound {
                    always: false,
                    rbrace: false,
                };
            }
            Kw::For | Kw::Repeat | Kw::Foreach => {
                self.note_command_start(f);
                self.push(start, end, rw);
                match kw {
                    Kw::For => self.for_header(),
                    Kw::Repeat => {
                        self.skip_blanks();
                        if !self.term_at(self.pos) {
                            self.lex_word();
                        }
                    }
                    _ => self.foreach_header(),
                }
                let state = if kw == Kw::Foreach {
                    LoopState::Body
                } else {
                    LoopState::ExpectDo
                };
                let kind = if kw == Kw::Foreach {
                    LoopKind::Foreach
                } else {
                    LoopKind::For
                };
                f.stack.push(Open::Loop(kind, state));
                f.last = Last::Kw;
            }
            Kw::Do => {
                Self::settle(f);
                // zsh accepts an empty condition (`while do`).
                let top = f.stack.find(Class::Do);
                if top.is_some()
                    && let Some(Open::Loop(kind, _)) = f.stack.last()
                {
                    f.stack.set_last(Open::Loop(kind, LoopState::Body));
                }
                self.push(start, end, ok(top));
                f.last = Last::Kw;
            }
            Kw::Done | Kw::End => {
                Self::settle(f);
                let class = if kw == Kw::End {
                    Class::End
                } else {
                    Class::Done
                };
                let mut top = f.stack.find(class);
                if top.is_some()
                    && let Some(Open::Loop(_, state)) = f.stack.pop()
                    && state != LoopState::Body
                {
                    top = Some(false);
                }
                self.push(start, end, ok(top));
                f.last = Last::Compound {
                    always: false,
                    rbrace: false,
                };
            }
            Kw::Case => {
                self.note_command_start(f);
                self.push(start, end, rw);
                let open = self.case_header();
                f.stack.push(open);
                f.last = Last::Kw;
            }
            Kw::Esac => {
                Self::settle(f);
                let top = f.stack.find(Class::Esac);
                if top.is_some() {
                    f.stack.pop();
                }
                self.push(start, end, ok(top));
                f.last = Last::Compound {
                    always: false,
                    rbrace: true,
                };
            }
            Kw::LBrace => {
                let kind = match f.last {
                    Last::FuncBody { anon: true, .. } => BraceKind::Anon,
                    Last::FuncBody { anon: false, .. } => BraceKind::Func,
                    Last::Always => BraceKind::Always,
                    _ => match f.stack.last() {
                        Some(Open::If(IfState::Cond { seen: true }))
                            if matches!(f.last, Last::Compound { .. }) =>
                        {
                            f.stack.set_last(Open::If(IfState::ShortBody));
                            BraceKind::Short
                        }
                        Some(Open::If(IfState::ShortElse)) => {
                            f.stack.set_last(Open::If(IfState::ShortElseBody));
                            BraceKind::Short
                        }
                        Some(Open::Loop(
                            kind @ LoopKind::While,
                            LoopState::Cond { seen: true },
                        ))
                        | Some(Open::Loop(kind @ LoopKind::For, LoopState::ExpectDo)) => {
                            f.stack.set_last(Open::Loop(kind, LoopState::ShortBody));
                            BraceKind::Short
                        }
                        _ => BraceKind::Plain,
                    },
                };
                let kind_span =
                    if kind == BraceKind::Plain && matches!(f.last, Last::Compound { .. }) {
                        err
                    } else {
                        rw
                    };
                if kind != BraceKind::Short {
                    self.note_command_start(f);
                }
                f.stack.push(Open::Brace(kind));
                self.push(start, end, kind_span);
                f.last = Last::Kw;
            }
            Kw::RBrace => {
                Self::settle(f);
                let top = f.stack.find(Class::RBrace);
                let mut last = Last::Compound {
                    always: false,
                    rbrace: true,
                };
                if top.is_some() {
                    match f.stack.pop() {
                        Some(Open::Brace(BraceKind::Short)) => match f.stack.last() {
                            Some(Open::If(IfState::ShortBody)) => {
                                f.stack.set_last(Open::If(IfState::ShortDone));
                            }
                            Some(Open::If(IfState::ShortElseBody))
                            | Some(Open::Loop(_, LoopState::ShortBody)) => {
                                f.stack.pop();
                            }
                            _ => {}
                        },
                        Some(Open::Brace(BraceKind::Plain)) => {
                            last = Last::Compound {
                                always: true,
                                rbrace: true,
                            };
                        }
                        Some(Open::Brace(BraceKind::Anon)) => last = Last::Args,
                        _ => {}
                    }
                }
                self.push(start, end, ok(top));
                f.last = last;
            }
            Kw::Bang | Kw::Coproc => {
                self.note_command_start(f);
                self.push(start, end, rw);
                f.last = Last::Kw;
            }
            Kw::Function => {
                self.note_command_start(f);
                self.push(start, end, rw);
                let named = self.function_header();
                f.last = Last::FuncBody {
                    anon: !named,
                    parens: false,
                };
            }
            Kw::CondOpen => {
                self.note_command_start(f);
                self.push(start, end, rw);
                f.last = Last::Cmd;
                self.cond(f);
            }
            Kw::CondClose => {
                self.push(start, end, err);
                f.last = Last::Start;
            }
        }
    }

    fn open_paren(&mut self, f: &mut Frame) {
        if f.cmd.is_time() || (f.cmd.words.is_empty() && f.cmd.redir && !f.cmd.assign) {
            // `time ( ... )`, and redirections before a subshell (`> f ( ... )`).
            self.end_cmd(f);
        }
        let cmd_pos = f.cmd.words.is_empty() && !f.cmd.prefix() && !f.cmd.suppressed;
        if !cmd_pos || matches!(f.last, Last::Compound { .. } | Last::Always | Last::Args) {
            self.word_token(f);
            return;
        }
        let anon = matches!(f.last, Last::FuncBody { anon: true, .. });
        self.note_command_start(f);
        if self.peek_at(1) == Some(b'(') && self.arith_command() {
            f.last = Last::Compound {
                always: false,
                rbrace: true,
            };
            return;
        }
        if self.peek_at(1) == Some(b')') {
            // `() body`: an anonymous function.
            self.push(self.pos, self.pos + 2, TokenKind::Grouping);
            self.pos += 2;
            f.last = Last::FuncBody {
                anon: true,
                parens: true,
            };
            return;
        }
        self.push(self.pos, self.pos + 1, TokenKind::Grouping);
        self.pos += 1;
        f.stack.push(Open::Paren { anon });
        f.last = Last::Kw;
    }

    /// Handles `)` in a list. Returns true when it terminates the list (left unconsumed).
    fn close_paren(&mut self, f: &mut Frame) -> bool {
        self.end_cmd(f);
        Self::settle(f);
        let start = self.pos;
        match f.stack.find(Class::Paren) {
            Some(top) => {
                let anon = f.stack.pop() == Some(Open::Paren { anon: true });
                let kind = if top {
                    TokenKind::Grouping
                } else {
                    TokenKind::Error
                };
                self.push(start, start + 1, kind);
                self.pos += 1;
                f.last = if anon {
                    Last::Args
                } else {
                    Last::Compound {
                        always: false,
                        rbrace: true,
                    }
                };
                false
            }
            None if f.term == Term::Paren => true,
            None => {
                self.push(start, start + 1, TokenKind::Error);
                self.pos += 1;
                f.last = Last::Start;
                false
            }
        }
    }

    fn semicolon(&mut self, f: &mut Frame) {
        self.end_cmd(f);
        let start = self.pos;
        if matches!(self.peek_at(1), Some(b';' | b'&' | b'|')) {
            // `;;`, `;&`, `;|` end a case item.
            Self::settle(f);
            let top = f.stack.find(Class::CaseSep);
            if top.is_some()
                && let Some(Open::Case { brace, .. }) = f.stack.last()
            {
                f.stack.set_last(Open::Case {
                    brace,
                    state: CaseState::Pattern,
                });
            }
            let kind = if top == Some(true) {
                TokenKind::Separator
            } else {
                TokenKind::Error
            };
            self.push(start, start + 2, kind);
            self.pos += 2;
        } else {
            // zsh accepts a separator after `&&` and `||` (`a && ;`), but not after a pipe, after
            // `always`, or between `name ()` and the function body.
            let kind = match f.last {
                Last::Pipe | Last::Always | Last::FuncBody { parens: true, .. } => TokenKind::Error,
                _ => TokenKind::Separator,
            };
            self.push(start, start + 1, kind);
            self.pos += 1;
        }
        f.last = Last::Start;
    }

    /// The kind for a list operator, which needs a command before it.
    fn list_op_kind(f: &Frame) -> TokenKind {
        if matches!(
            f.last,
            Last::Start | Last::Op | Last::Pipe | Last::Kw | Last::FuncBody { .. } | Last::Always
        ) {
            TokenKind::Error
        } else {
            TokenKind::Separator
        }
    }

    fn ampersand(&mut self, f: &mut Frame) {
        self.end_cmd(f);
        let kind = Self::list_op_kind(f);
        let start = self.pos;
        let (len, last) = match self.peek_at(1) {
            Some(b'&') => (2, Last::Op),
            Some(b'!' | b'|') => (2, Last::Start),
            _ => (1, Last::Start),
        };
        self.push(start, start + len, kind);
        self.pos += len;
        f.last = last;
    }

    fn pipe(&mut self, f: &mut Frame) {
        self.end_cmd(f);
        let kind = Self::list_op_kind(f);
        let start = self.pos;
        let (len, last) = match self.peek_at(1) {
            Some(b'|') => (2, Last::Op),
            Some(b'&') => (2, Last::Pipe),
            _ => (1, Last::Pipe),
        };
        self.push(start, start + len, kind);
        self.pos += len;
        f.last = last;
    }

    // ---------------------------------------------------------------------------------------
    // Compound command headers
    // ---------------------------------------------------------------------------------------

    /// `for`/`select` header: loop variables, then `in words`, `(words)`, or `((...))`. Stops
    /// before `do`, `{`, a separator, or a newline.
    fn for_header(&mut self) {
        self.skip_blanks();
        if self.peek() == Some(b'(') && self.peek_at(1) == Some(b'(') && self.arith_command() {
            return;
        }
        loop {
            self.skip_blanks();
            let Some(c) = self.peek() else { return };
            if c == b'(' {
                self.paren_word_list(true);
                return;
            }
            if c == b'\n' && self.in_on_later_line() {
                // `for x` newline `in a b`: zsh looks for `in` across newlines.
                loop {
                    match self.peek() {
                        Some(b'\n') => self.newline(),
                        Some(b'#') if self.opts.interactive_comments => self.comment(),
                        Some(b' ' | b'\t') => self.pos += 1,
                        _ => break,
                    }
                }
                continue;
            }
            if is_word_term(c) {
                return;
            }
            match self.plain_word_at(self.pos) {
                Some("in") => {
                    self.push(self.pos, self.pos + 2, TokenKind::ReservedWord);
                    self.pos += 2;
                    self.line_word_list();
                    return;
                }
                Some("do" | "{") => return,
                _ => {
                    self.lex_word();
                }
            }
        }
    }

    /// True when, from a newline at the cursor, only blank lines (and comments) come before a
    /// line whose first word is `in`.
    fn in_on_later_line(&self) -> bool {
        let mut i = self.pos;
        loop {
            match self.byte(i) {
                Some(b' ' | b'\t' | b'\n') => i += 1,
                Some(b'#') if self.opts.interactive_comments => {
                    while self.byte(i).is_some_and(|c| c != b'\n') {
                        i += 1;
                    }
                }
                Some(_) => return self.plain_word_at(i) == Some("in"),
                None => return false,
            }
        }
    }

    /// `foreach name (words)`.
    fn foreach_header(&mut self) {
        loop {
            self.skip_blanks();
            let Some(c) = self.peek() else { return };
            if c == b'(' {
                self.paren_word_list(true);
                return;
            }
            if is_word_term(c) {
                return;
            }
            self.lex_word();
        }
    }

    /// Words up to the end of the line or a separator, recorded as path candidates.
    fn line_word_list(&mut self) {
        loop {
            self.skip_blanks();
            let Some(c) = self.peek() else { return };
            if c == b'#' && self.opts.interactive_comments {
                return;
            }
            let word_start = !is_word_term(c)
                || (matches!(c, b'<' | b'>') && self.peek_at(1) == Some(b'('))
                || (c == b'<' && self.numeric_range_at(self.pos).is_some());
            if !word_start || self.rbrace_at(self.pos) {
                return;
            }
            let w = self.lex_word();
            self.path_word(w);
        }
    }

    /// `( words )` after `for name` or `foreach name`, with Grouping parentheses.
    fn paren_word_list(&mut self, path: bool) {
        self.push(self.pos, self.pos + 1, TokenKind::Grouping);
        self.pos += 1;
        self.element_list(path, false);
        if self.peek() == Some(b')') {
            self.push(self.pos, self.pos + 1, TokenKind::Grouping);
            self.pos += 1;
        }
    }

    /// Words up to an unmatched `)`, across newlines. With `assoc`, `[key]=value` elements of
    /// an associative array assignment get an Assignment span on `[key]=`.
    pub(super) fn element_list(&mut self, path: bool, assoc: bool) {
        loop {
            self.skip_blanks();
            let Some(c) = self.peek() else { return };
            match c {
                b'\n' => self.newline(),
                b'#' if self.opts.interactive_comments => self.comment(),
                b')' | b';' | b'&' | b'|' => return,
                b'<' | b'>'
                    if self.peek_at(1) != Some(b'(')
                        && self.numeric_range_at(self.pos).is_none() =>
                {
                    return;
                }
                b'}' if self.rbrace_at(self.pos) => return,
                _ => {
                    if assoc
                        && c == b'['
                        && let Some(eq) = self.assoc_key_ahead(self.pos)
                    {
                        let start = self.pos;
                        self.push(start, eq, TokenKind::Assignment);
                        self.pos = eq;
                        // Like a scalar assignment, the value of `[key]=value` is not globbed.
                        let mut acc = WordAcc::scratch(start);
                        acc.assignment = true;
                        self.lex_word_into(&mut acc);
                        continue;
                    }
                    let w = self.lex_word();
                    if path {
                        self.path_word(w);
                    }
                }
            }
        }
    }

    /// `case word in` or `case word {`; returns the construct to push.
    fn case_header(&mut self) -> Open {
        self.skip_blanks();
        if (!self.term_at(self.pos) || self.peek() == Some(b'(')) && !self.rbrace_at(self.pos) {
            let w = self.lex_word();
            self.path_word(w);
        }
        self.skip_blanks();
        match self.plain_word_at(self.pos) {
            Some("in") => {
                self.push(self.pos, self.pos + 2, TokenKind::ReservedWord);
                self.pos += 2;
                Open::Case {
                    brace: false,
                    state: CaseState::Pattern,
                }
            }
            Some("{") => {
                self.push(self.pos, self.pos + 1, TokenKind::ReservedWord);
                self.pos += 1;
                Open::Case {
                    brace: true,
                    state: CaseState::Pattern,
                }
            }
            _ => Open::Case {
                brace: false,
                state: CaseState::ExpectIn,
            },
        }
    }

    /// Characters that start (part of) a case pattern rather than a list operator.
    fn case_pattern_char(&self, c: u8) -> bool {
        match c {
            b';' | b'&' => false,
            b'<' | b'>' => c == b'<' && self.numeric_range_at(self.pos).is_some(),
            _ => true,
        }
    }

    /// One case item pattern: `[(] pat [| pat]... )`, or `esac`/`}` closing the case.
    fn case_pattern(&mut self, f: &mut Frame) {
        let start = self.pos;
        let plain = self.plain_word_at(start);
        if let Some(Open::Case {
            state: CaseState::ExpectIn,
            brace,
        }) = f.stack.last()
        {
            let brace = brace || plain == Some("{");
            f.stack.set_last(Open::Case {
                brace,
                state: CaseState::Pattern,
            });
            match plain {
                Some("in") => {
                    self.push(start, start + 2, TokenKind::ReservedWord);
                    self.pos += 2;
                    return;
                }
                Some("{") => {
                    self.push(start, start + 1, TokenKind::ReservedWord);
                    self.pos += 1;
                    return;
                }
                _ => {}
            }
        }
        match plain {
            Some("esac") => {
                self.keyword(f, Kw::Esac, start, 4);
                return;
            }
            Some("}") if !self.opts.close_braces_ignored() => {
                self.keyword(f, Kw::RBrace, start, 1);
                return;
            }
            _ => {}
        }
        if self.peek() == Some(b'(') {
            self.push(start, start + 1, TokenKind::Grouping);
            self.pos += 1;
        }
        loop {
            self.skip_blanks();
            let Some(c) = self.peek() else { return };
            match c {
                b')' => {
                    self.push(self.pos, self.pos + 1, TokenKind::Grouping);
                    self.pos += 1;
                    if let Some(Open::Case { brace, .. }) = f.stack.last() {
                        f.stack.set_last(Open::Case {
                            brace,
                            state: CaseState::Body,
                        });
                    }
                    f.last = Last::Kw;
                    return;
                }
                b'|' => {
                    self.push(self.pos, self.pos + 1, TokenKind::Separator);
                    self.pos += 1;
                }
                b'\n' | b';' | b'&' => return,
                b'}' if self.rbrace_at(self.pos) => return,
                b'<' | b'>' if !self.case_pattern_char(c) => return,
                _ => {
                    self.lex_word();
                }
            }
        }
    }

    /// `function name... [()]`; stops before the body. Returns false for an anonymous function
    /// (no name).
    fn function_header(&mut self) -> bool {
        let mut named = false;
        loop {
            self.skip_blanks();
            let Some(c) = self.peek() else { return named };
            if c == b'(' {
                if let Some((open, close)) = self.funcdef_parens() {
                    self.push(open, open + 1, TokenKind::Grouping);
                    self.push(close, close + 1, TokenKind::Grouping);
                    self.pos = close + 1;
                }
                return named;
            }
            if is_word_term(c) || self.plain_word_at(self.pos) == Some("{") {
                return named;
            }
            let w = self.lex_word();
            self.push(w.start, w.end, TokenKind::Function);
            named = true;
        }
    }

    /// The inside of `[[ ... ]]`, after `[[`.
    fn cond(&mut self, f: &mut Frame) {
        let mut parens = 0usize;
        let mut expect_pattern = false;
        loop {
            self.skip_blanks();
            let Some(c) = self.peek() else { return };
            let start = self.pos;
            match c {
                b'\n' => {
                    self.newline();
                    continue;
                }
                b'#' if self.opts.interactive_comments => {
                    self.comment();
                    continue;
                }
                b']' if self.peek_at(1) == Some(b']') && self.term_at(start + 2) => {
                    self.push(start, start + 2, TokenKind::ReservedWord);
                    self.pos += 2;
                    f.last = Last::Compound {
                        always: false,
                        rbrace: true,
                    };
                    return;
                }
                b'&' | b'|' if self.peek_at(1) == Some(c) => {
                    self.push(start, start + 2, TokenKind::Operator);
                    self.pos += 2;
                    expect_pattern = false;
                    continue;
                }
                b';' | b'&' | b'|' => return,
                b'(' if !expect_pattern => {
                    self.push(start, start + 1, TokenKind::Operator);
                    self.pos += 1;
                    parens += 1;
                    continue;
                }
                b')' => {
                    if parens == 0 {
                        return;
                    }
                    self.push(start, start + 1, TokenKind::Operator);
                    self.pos += 1;
                    parens -= 1;
                    expect_pattern = false;
                    continue;
                }
                b'<' | b'>'
                    if self.peek_at(1) != Some(b'(') && self.numeric_range_at(start).is_none() =>
                {
                    self.push(start, start + 1, TokenKind::Operator);
                    self.pos += 1;
                    expect_pattern = false;
                    continue;
                }
                b'!' if self.term_at(start + 1) => {
                    self.push(start, start + 1, TokenKind::Operator);
                    self.pos += 1;
                    continue;
                }
                _ => {}
            }
            if let Some(op) = self.plain_word_at(start)
                && op.len() <= 3
                && COND_OPERATORS.contains(&op)
            {
                self.push(start, start + op.len(), TokenKind::Operator);
                self.pos += op.len();
                expect_pattern = matches!(op, "=" | "==" | "!=" | "=~");
                continue;
            }
            let w = self.lex_word();
            self.path_word(w);
            expect_pattern = false;
            if self.peek() == Some(b'}') {
                // zsh splits `}` off the end of `a}`, and a `}` token is not valid here.
                self.push(self.pos, self.pos + 1, TokenKind::Error);
                self.pos += 1;
            }
        }
    }

    /// `((...))` at the cursor. Returns false (consuming nothing) when it is a nested subshell.
    fn arith_command(&mut self) -> bool {
        let start = self.pos;
        match self.find_arith_close(start + 2) {
            ArithEnd::NotArith => false,
            ArithEnd::Closed(i) => {
                self.arith(start, start + 2, i, i + 2);
                true
            }
            ArithEnd::Unclosed => {
                self.arith(start, start + 2, self.end, self.end);
                true
            }
        }
    }

    // ---------------------------------------------------------------------------------------
    // Assignments
    // ---------------------------------------------------------------------------------------

    /// If `name=`, `name+=`, `name[sub]=`, or `name[sub]+=` starts at `i`, the offset just past
    /// the `=`.
    pub(super) fn assignment_ahead(&self, i: usize) -> Option<usize> {
        let mut j = self.name_end(i)?;
        if self.byte(j) == Some(b'[') {
            j = self.raw_subscript_end(j)?;
        }
        if self.byte(j) == Some(b'+') {
            j += 1;
        }
        (self.byte(j) == Some(b'=')).then_some(j + 1)
    }

    /// `[key]=` or `[key]+=` at `i`: the offset just past the `=`.
    fn assoc_key_ahead(&self, i: usize) -> Option<usize> {
        let mut j = self.raw_subscript_end(i)?;
        if self.byte(j) == Some(b'+') {
            j += 1;
        }
        (self.byte(j) == Some(b'=')).then_some(j + 1)
    }

    /// The offset after the `]` matching the `[` at `i`, without crossing a word terminator.
    fn raw_subscript_end(&self, i: usize) -> Option<usize> {
        let mut depth = 0usize;
        let mut j = i;
        while let Some(c) = self.byte(j) {
            match c {
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(j + 1);
                    }
                }
                b'\\' => j += 1,
                b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'<' | b'>' => return None,
                _ => {}
            }
            j += 1;
        }
        None
    }

    /// An assignment at the cursor (checked with [`Self::assignment_ahead`]). With `as_arg` it
    /// is an argument of a declaration builtin and becomes a word of the command.
    fn assignment(&mut self, f: &mut Frame, as_arg: bool) {
        let start = self.pos;
        let Some(eq) = self.assignment_ahead(start) else {
            return;
        };
        if let Some(name_end) = self.name_end(start)
            && self.byte(name_end) == Some(b'[')
        {
            // Highlight expansions inside the subscript.
            let close = self.raw_subscript_end(name_end).unwrap_or(eq);
            self.lex_region(name_end + 1, close.saturating_sub(1).max(name_end + 1));
        }
        self.push(start, eq, TokenKind::Assignment);
        self.pos = eq;
        let mut acc = WordAcc::new(start);
        acc.push_str(&self.src[start..eq]);
        // A scalar value is not globbed and gets no brace expansion, and `~` after the `=` or a
        // `:` is a tilde expansion; none of that applies to an argument of a declaration builtin
        // reached through a precommand, which is an ordinary word.
        acc.assignment = !(as_arg && f.cmd.decl_globbed);
        if self.peek() == Some(b'(') {
            self.push(self.pos, self.pos + 1, TokenKind::Assignment);
            self.pos += 1;
            acc.expand();
            self.element_list(false, true);
            if self.peek() == Some(b')') {
                self.push(self.pos, self.pos + 1, TokenKind::Assignment);
                self.pos += 1;
            }
        } else {
            self.lex_word_into(&mut acc);
        }
        if as_arg {
            let w = acc.into_word(self.pos);
            f.cmd.words.push(w);
        } else {
            f.cmd.assign = true;
        }
        f.last = Last::Cmd;
    }

    // ---------------------------------------------------------------------------------------
    // Redirections and here-documents
    // ---------------------------------------------------------------------------------------

    /// Digits directly followed by `<` or `>` (but not a process substitution).
    fn fd_redirect_ahead(&self) -> bool {
        let mut i = self.pos;
        while matches!(self.byte(i), Some(b'0'..=b'9')) {
            i += 1;
        }
        matches!(self.byte(i), Some(b'<' | b'>')) && self.byte(i + 1) != Some(b'(')
    }

    /// `{name}` directly followed by `<` or `>`, unless `IGNORE_BRACES` is set.
    fn brace_fd_ahead(&self) -> bool {
        if self.opts.ignore_braces {
            return false;
        }
        let Some(e) = self.name_end(self.pos + 1) else {
            return false;
        };
        self.byte(e) == Some(b'}')
            && matches!(self.byte(e + 1), Some(b'<' | b'>'))
            && self.byte(e + 2) != Some(b'(')
    }

    /// Parses the operator at `i` (after any fd prefix): its end offset and how it takes a
    /// target.
    fn redirect_op(&self, i: usize) -> (usize, Redir) {
        let at = |k: usize| self.byte(i + k);
        let dup_target = |j: usize| -> usize {
            let mut k = j;
            while matches!(self.byte(k), Some(b'0'..=b'9')) {
                k += 1;
            }
            if k == j && matches!(self.byte(j), Some(b'-' | b'p')) {
                k += 1;
            }
            k - j
        };
        match at(0) {
            Some(b'<') => match at(1) {
                Some(b'<') => match at(2) {
                    Some(b'<') => (i + 3, Redir::HereString),
                    Some(b'-') => (i + 3, Redir::Heredoc { strip_tabs: true }),
                    _ => (i + 2, Redir::Heredoc { strip_tabs: false }),
                },
                Some(b'>') => (i + 2, Redir::File),
                Some(b'&') => {
                    let n = dup_target(i + 2);
                    if n > 0 {
                        (i + 2 + n, Redir::Dup)
                    } else {
                        (i + 2, Redir::DupWord)
                    }
                }
                _ => (i + 1, Redir::File),
            },
            Some(b'>') => {
                let mut j = i + 1;
                if self.byte(j) == Some(b'>') {
                    j += 1;
                }
                let mut kind = Redir::File;
                if self.byte(j) == Some(b'&') {
                    j += 1;
                    let n = dup_target(j);
                    if n > 0 {
                        return (j + n, Redir::Dup);
                    }
                    kind = Redir::DupWord;
                }
                if matches!(self.byte(j), Some(b'|' | b'!')) {
                    j += 1;
                    kind = Redir::File;
                }
                (j, kind)
            }
            Some(b'&') => {
                // `&>`, `&>>`, with an optional clobber `|` or `!`.
                let mut j = i + 2;
                if self.byte(j) == Some(b'>') {
                    j += 1;
                }
                if matches!(self.byte(j), Some(b'|' | b'!')) {
                    j += 1;
                }
                (j, Redir::File)
            }
            _ => (i + 1, Redir::File),
        }
    }

    fn redirection(&mut self, f: &mut Frame) {
        let start = self.pos;
        let mut i = start;
        if self.b[i].is_ascii_digit() {
            while matches!(self.byte(i), Some(b'0'..=b'9')) {
                i += 1;
            }
        } else if self.b[i] == b'{' {
            i = self.name_end(i + 1).map_or(i, |e| e + 1);
        }
        let (op_end, redir) = self.redirect_op(i);
        let op_end = op_end.min(self.end);
        match f.last {
            // `always` must directly follow the `}` of the group.
            Last::Compound { rbrace, .. } => {
                f.last = Last::Compound {
                    always: false,
                    rbrace,
                }
            }
            Last::Args => {}
            _ => {
                if f.cmd.words.is_empty() && !f.cmd.prefix() {
                    self.note_command_start(f);
                }
                f.last = Last::Cmd;
            }
        }
        if f.cmd.words.is_empty() {
            f.cmd.redir = true;
        }
        self.pos = op_end;
        if redir == Redir::Dup {
            self.push(start, op_end, TokenKind::Redirection);
            return;
        }
        self.skip_blanks();
        let missing = match self.peek() {
            None => {
                // Partial input: the target has not been typed yet.
                self.push(start, op_end, TokenKind::Redirection);
                return;
            }
            Some(b'\n' | b';' | b'&' | b'|' | b')' | b'(') => true,
            Some(b'<' | b'>') => {
                self.peek_at(1) != Some(b'(') && self.numeric_range_at(self.pos).is_none()
            }
            Some(b'}') => self.rbrace_at(self.pos),
            Some(b'#') => self.opts.interactive_comments,
            Some(_) => false,
        };
        if missing {
            self.push(start, op_end, TokenKind::Error);
            return;
        }
        self.push(start, op_end, TokenKind::Redirection);
        match redir {
            Redir::Heredoc { strip_tabs } => {
                let before = self.span_count();
                let acc = self.lex_word_acc(true);
                self.truncate_spans(before);
                self.push(acc.start, self.pos, TokenKind::Heredoc);
                if !acc.text.is_empty() {
                    self.heredocs.push(PendingHeredoc {
                        delim: acc.text,
                        strip_tabs,
                        expand: !acc.quoted,
                    });
                }
            }
            Redir::HereString => {
                self.lex_word();
            }
            Redir::File | Redir::DupWord => {
                let w = self.lex_word();
                let fd_like = w.literal.as_deref().is_some_and(|l| {
                    l == "-" || (redir == Redir::DupWord && l.bytes().all(|c| c.is_ascii_digit()))
                });
                if !fd_like {
                    self.path_word(w);
                }
            }
            Redir::Dup => {}
        }
    }

    /// Reads the bodies of pending here-documents, starting at the beginning of a line.
    fn read_heredoc_bodies(&mut self) {
        let pending = std::mem::take(&mut self.heredocs);
        for h in pending {
            let body_start = self.pos;
            let mut content_end = self.end;
            let mut span_end = self.end;
            while self.pos < self.end {
                let line_start = self.pos;
                let line_end = self.b[line_start..self.end]
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(self.end, |n| line_start + n);
                let mut line = &self.src[line_start..line_end];
                if h.strip_tabs {
                    line = line.trim_start_matches('\t');
                }
                self.pos = (line_end + 1).min(self.end);
                if line == h.delim {
                    content_end = line_start;
                    span_end = line_end;
                    break;
                }
            }
            let after = self.pos;
            if h.expand && body_start < content_end {
                let saved_end = self.end;
                self.end = content_end;
                self.pos = body_start;
                let mut acc = WordAcc::scratch(body_start);
                self.dq_body(&mut acc, true);
                self.end = saved_end;
            }
            self.pos = after;
            self.push(body_start, span_end, TokenKind::Heredoc);
        }
    }
}

/// Drops spans that violate the output invariants. Only reached if the parser has a bug.
fn repair_spans(text: &str, spans: &mut Vec<Span>) {
    spans.retain(|s| {
        s.end <= text.len() && text.is_char_boundary(s.start) && text.is_char_boundary(s.end)
    });
    sort_spans(spans);
    let mut kept: Vec<Span> = Vec::with_capacity(spans.len());
    let mut stack: Vec<Span> = Vec::new();
    for s in spans.iter() {
        while stack.last().is_some_and(|top| top.end <= s.start) {
            stack.pop();
        }
        if stack.last().is_none_or(|top| top.contains(s)) {
            stack.push(*s);
            kept.push(*s);
        }
    }
    *spans = kept;
}
