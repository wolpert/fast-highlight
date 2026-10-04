//! Token types and highlight spans.
//!
//! The daemon never sends styles, only token types. Each [`TokenKind`] has a stable wire name
//! (see `docs/protocol.md`) that the zsh plugin maps to a style through its style table.
//!
//! # Span ordering and nesting
//!
//! A list of spans is **well nested** when, for any two spans, they are either disjoint or one
//! contains the other. Spans are applied by zsh in order, so a span that comes later wins over
//! an earlier span it overlaps. Every span list produced by this crate is therefore sorted by
//! [`sort_spans`]: ascending start, then descending end, so a containing span always precedes
//! the spans nested inside it. Empty spans (`start == end`) are never emitted.

use std::fmt;

/// The kind of a highlighted region.
///
/// The discriminant order is not part of the wire format; the wire uses [`TokenKind::name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TokenKind {
    /// Plain text with no particular meaning. Rarely emitted; unstyled text needs no span.
    Default,
    /// A syntax error, unknown command, unknown subcommand, or invalid option.
    Error,
    /// A reserved word in command position: `if`, `then`, `for`, `{`, `[[`, `!`, and so on.
    ReservedWord,
    /// A command word that names a regular alias.
    Alias,
    /// A command word that is resolved through a suffix alias (`alias -s`).
    SuffixAlias,
    /// A word anywhere on the line that names a global alias (`alias -g`).
    GlobalAlias,
    /// A command word that names a shell function, or the name in a function definition.
    Function,
    /// A command word that names a shell builtin.
    Builtin,
    /// A command word that names an external command found in `$PATH` or by explicit path.
    Command,
    /// A precommand modifier: `sudo`, `noglob`, `nocorrect`, `exec`, `command`, `builtin`,
    /// `env`, `time`, and similar wrappers.
    Precommand,
    /// A command separator or pipeline operator: `;`, `&`, `&&`, `||`, `|`, `|&`, `&!`, `&|`.
    Separator,
    /// A redirection operator including any fd prefix: `>`, `2>`, `>&2`, `<<`, `<<<`, `&>`.
    Redirection,
    /// A here-document delimiter word and the here-document body.
    Heredoc,
    /// A single-quoted string `'...'`.
    SingleQuoted,
    /// A double-quoted string `"..."`.
    DoubleQuoted,
    /// A dollar-quoted string `$'...'`.
    DollarQuoted,
    /// A backquoted command substitution `` `...` ``, delimiters included.
    Backquoted,
    /// A backslash escape, inside or outside quotes.
    Escape,
    /// A parameter expansion: `$name`, `${...}`, `$1`, `$?`, and so on.
    Parameter,
    /// The delimiters of a command substitution: `$(` and `)`.
    Substitution,
    /// The delimiters of a process substitution: `<(`, `>(`, `=(` and `)`.
    ProcessSubstitution,
    /// An arithmetic expansion or command: `$((...))` and `((...))`.
    Arithmetic,
    /// Glob characters and extended glob operators: `*`, `?`, `[...]`, `**`, `#`, `^`, `~`.
    Glob,
    /// A glob qualifier list such as `(.)` or `(om[1,3])` at the end of a word.
    GlobQualifier,
    /// A brace expansion `{a,b}` or `{1..10}`.
    BraceExpansion,
    /// A history expansion: `!!`, `!$`, `!-2`, `^old^new`, and so on.
    HistoryExpansion,
    /// A comment (only recognised when `INTERACTIVE_COMMENTS` is set).
    Comment,
    /// The `name=` or `name+=` part of an assignment, and the parentheses of an array value.
    Assignment,
    /// Subshell and group delimiters `(` and `)`, and the parentheses of a function definition.
    Grouping,
    /// An operator inside `[[ ... ]]`: `-f`, `==`, `=~`, `&&`, `!`, `<`, and so on.
    Operator,
    /// An argument that names an existing file.
    Path,
    /// An argument that names an existing directory.
    PathDirectory,
    /// An argument that is a prefix of an existing path, while the user is still typing it.
    PathPrefix,
    /// A subcommand known to the command's spec (`git commit`, `cargo build`).
    Subcommand,
    /// An option known to the command's spec (`--verbose`, `-C`).
    CmdOption,
}

impl TokenKind {
    /// Every token kind, in declaration order.
    pub const ALL: &'static [TokenKind] = &[
        TokenKind::Default,
        TokenKind::Error,
        TokenKind::ReservedWord,
        TokenKind::Alias,
        TokenKind::SuffixAlias,
        TokenKind::GlobalAlias,
        TokenKind::Function,
        TokenKind::Builtin,
        TokenKind::Command,
        TokenKind::Precommand,
        TokenKind::Separator,
        TokenKind::Redirection,
        TokenKind::Heredoc,
        TokenKind::SingleQuoted,
        TokenKind::DoubleQuoted,
        TokenKind::DollarQuoted,
        TokenKind::Backquoted,
        TokenKind::Escape,
        TokenKind::Parameter,
        TokenKind::Substitution,
        TokenKind::ProcessSubstitution,
        TokenKind::Arithmetic,
        TokenKind::Glob,
        TokenKind::GlobQualifier,
        TokenKind::BraceExpansion,
        TokenKind::HistoryExpansion,
        TokenKind::Comment,
        TokenKind::Assignment,
        TokenKind::Grouping,
        TokenKind::Operator,
        TokenKind::Path,
        TokenKind::PathDirectory,
        TokenKind::PathPrefix,
        TokenKind::Subcommand,
        TokenKind::CmdOption,
    ];

    /// The stable wire name of this kind. Theme files and the zsh style table use these names.
    pub const fn name(self) -> &'static str {
        match self {
            TokenKind::Default => "default",
            TokenKind::Error => "error",
            TokenKind::ReservedWord => "reserved-word",
            TokenKind::Alias => "alias",
            TokenKind::SuffixAlias => "suffix-alias",
            TokenKind::GlobalAlias => "global-alias",
            TokenKind::Function => "function",
            TokenKind::Builtin => "builtin",
            TokenKind::Command => "command",
            TokenKind::Precommand => "precommand",
            TokenKind::Separator => "separator",
            TokenKind::Redirection => "redirection",
            TokenKind::Heredoc => "heredoc",
            TokenKind::SingleQuoted => "single-quoted",
            TokenKind::DoubleQuoted => "double-quoted",
            TokenKind::DollarQuoted => "dollar-quoted",
            TokenKind::Backquoted => "backquoted",
            TokenKind::Escape => "escape",
            TokenKind::Parameter => "parameter",
            TokenKind::Substitution => "substitution",
            TokenKind::ProcessSubstitution => "process-substitution",
            TokenKind::Arithmetic => "arithmetic",
            TokenKind::Glob => "glob",
            TokenKind::GlobQualifier => "glob-qualifier",
            TokenKind::BraceExpansion => "brace-expansion",
            TokenKind::HistoryExpansion => "history-expansion",
            TokenKind::Comment => "comment",
            TokenKind::Assignment => "assignment",
            TokenKind::Grouping => "grouping",
            TokenKind::Operator => "operator",
            TokenKind::Path => "path",
            TokenKind::PathDirectory => "path-directory",
            TokenKind::PathPrefix => "path-prefix",
            TokenKind::Subcommand => "subcommand",
            TokenKind::CmdOption => "option",
        }
    }

    /// Looks up a kind by its wire name.
    pub fn from_name(name: &str) -> Option<TokenKind> {
        TokenKind::ALL.iter().copied().find(|k| k.name() == name)
    }
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A highlighted region of the input.
///
/// Offsets are **byte** offsets into the input string, half-open (`start..end`), and always lie
/// on UTF-8 character boundaries. Conversion to the character offsets that `region_highlight`
/// uses happens only at the protocol boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub kind: TokenKind,
}

impl Span {
    pub const fn new(start: usize, end: usize, kind: TokenKind) -> Span {
        Span { start, end, kind }
    }

    pub const fn len(&self) -> usize {
        self.end - self.start
    }

    pub const fn is_empty(&self) -> bool {
        self.start >= self.end
    }

    /// True when `self` fully contains `other`.
    pub const fn contains(&self, other: &Span) -> bool {
        self.start <= other.start && other.end <= self.end
    }
}

/// Sorts spans into canonical order: ascending start, then descending end. The sort is stable,
/// so among spans with identical ranges the one pushed later stays later (and wins in zsh).
/// Empty spans are removed.
pub fn sort_spans(spans: &mut Vec<Span>) {
    spans.retain(|s| !s.is_empty());
    spans.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
}

/// Checks the invariants every emitted span list must satisfy against an input of `len` bytes:
/// non-empty, in bounds, on character boundaries of `text`, sorted canonically, and well nested.
/// Returns a description of the first violation.
pub fn check_spans(text: &str, spans: &[Span]) -> Result<(), String> {
    let mut stack: Vec<Span> = Vec::new();
    let mut prev: Option<Span> = None;
    for s in spans {
        if s.is_empty() {
            return Err(format!("empty span {s:?}"));
        }
        if s.end > text.len() {
            return Err(format!("span {s:?} out of bounds (len {})", text.len()));
        }
        if !text.is_char_boundary(s.start) || !text.is_char_boundary(s.end) {
            return Err(format!("span {s:?} not on a char boundary"));
        }
        if let Some(p) = prev
            && (s.start < p.start || (s.start == p.start && s.end > p.end))
        {
            return Err(format!("span {s:?} out of order after {p:?}"));
        }
        while let Some(top) = stack.last() {
            if top.end <= s.start {
                stack.pop();
            } else {
                break;
            }
        }
        if let Some(top) = stack.last()
            && !top.contains(s)
        {
            return Err(format!("span {s:?} partially overlaps {top:?}"));
        }
        stack.push(*s);
        prev = Some(*s);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for k in TokenKind::ALL {
            assert!(seen.insert(k.name()), "duplicate name {}", k.name());
            assert_eq!(TokenKind::from_name(k.name()), Some(*k));
        }
        assert_eq!(TokenKind::from_name("nope"), None);
    }

    #[test]
    fn sort_puts_containers_first() {
        let mut v = vec![
            Span::new(2, 4, TokenKind::Parameter),
            Span::new(0, 6, TokenKind::DoubleQuoted),
            Span::new(3, 3, TokenKind::Error),
        ];
        sort_spans(&mut v);
        assert_eq!(
            v,
            vec![
                Span::new(0, 6, TokenKind::DoubleQuoted),
                Span::new(2, 4, TokenKind::Parameter)
            ]
        );
        assert!(check_spans("\"a$bc\"", &v).is_ok());
    }

    #[test]
    fn check_rejects_partial_overlap() {
        let v = vec![
            Span::new(0, 3, TokenKind::Glob),
            Span::new(2, 5, TokenKind::Glob),
        ];
        assert!(check_spans("abcdef", &v).is_err());
    }

    #[test]
    fn check_rejects_non_boundary() {
        let v = vec![Span::new(0, 1, TokenKind::Glob)];
        assert!(check_spans("é", &v).is_err());
    }
}
