//! Error-tolerant zsh lexer and parser.
//!
//! [`parse`] is purely syntactic: it knows zsh grammar and the fixed list of reserved words, but
//! nothing about aliases, functions, `$PATH`, the filesystem, or command specs. It produces
//! syntactic spans plus a structural summary ([`SimpleCommand`]s and extra path-candidate words)
//! that the semantic pass in `crate::highlight` uses to classify commands, arguments, and paths.
//!
//! The parser never panics, on any input. Input is usually partial (the user is still typing),
//! so a construct that is unclosed at end of input is not an error: an unterminated quote is
//! still highlighted as a string. Only constructs that can no longer be completed are reported
//! as [`TokenKind::Error`](crate::token::TokenKind::Error) spans: a stray `fi` or `)`, a `|` or
//! `&&` with no command before it, a redirection operator followed by a separator, and so on.

use crate::token::Span;

mod parser;
#[cfg(test)]
mod tests;
mod word;

/// Shell options that change how the input is lexed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ParseOptions {
    /// `INTERACTIVE_COMMENTS`: a word starting with `#` begins a comment to end of line.
    pub interactive_comments: bool,
    /// `EXTENDED_GLOB`: `#`, `##`, `~`, and `^` are glob operators.
    pub extended_glob: bool,
    /// `KSH_GLOB`: `@(...)`, `*(...)`, `+(...)`, `?(...)`, `!(...)` are glob patterns.
    pub ksh_glob: bool,
}

/// One shell word as it appears in the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    /// Byte offset of the first byte of the word.
    pub start: usize,
    /// Byte offset one past the last byte of the word.
    pub end: usize,
    /// The word's value after quote removal, when the word contains no expansion of any kind
    /// (no parameter, command, process, or arithmetic substitution, no history expansion, no
    /// brace expansion, and no unquoted glob characters). `None` otherwise.
    ///
    /// A leading unquoted `~` is kept verbatim in the literal; see [`Word::tilde`].
    pub literal: Option<String>,
    /// The word starts with an unquoted `~`, so tilde expansion applies.
    pub tilde: bool,
    /// The word contains unquoted glob characters.
    pub has_glob: bool,
}

/// A simple command: the command word followed by its arguments.
///
/// Assignments before the command (`FOO=1 cmd`) and redirections (`> file`) are not included
/// in `words`. A simple command is only emitted when it has a command word, and never for a
/// reserved word in command position (`if`, `{`, `[[`, ...): those get syntactic spans only.
/// Words of a precommand chain are included unchanged (`sudo -u root git status` yields five
/// words); the semantic pass resolves precommands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimpleCommand {
    /// `words[0]` is the command word; the rest are its arguments in order.
    pub words: Vec<Word>,
}

/// The result of parsing one input string.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParseOutput {
    /// Syntactic spans, sorted with [`crate::token::sort_spans`] and well nested.
    pub spans: Vec<Span>,
    /// Every simple command in the input, including those nested in substitutions, subshells,
    /// groups, and compound commands, in order of their command word's start offset.
    pub commands: Vec<SimpleCommand>,
    /// Words outside simple commands that should be checked as paths: redirection targets,
    /// operands inside `[[ ... ]]`, `for` loop list items, and the subject of `case`.
    pub path_words: Vec<Word>,
}

/// Parses `input` and returns its syntactic spans and command structure. Never panics.
pub fn parse(input: &str, opts: &ParseOptions) -> ParseOutput {
    let mut parser = parser::Parser::new(input, *opts);
    parser.run();
    parser.finish()
}
