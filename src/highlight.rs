//! The semantic pass: combines a parse with shell state, specs, and the filesystem.

use crate::config::Config;
use crate::paths::PathChecker;
use crate::specs::SpecRegistry;
use crate::state::ShellState;
use crate::syntax::ParseOptions;
use crate::token::Span;
use std::path::Path;

/// Request-level options that are not purely syntactic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RequestOptions {
    pub parse: ParseOptions,
    /// `AUTO_CD`: a directory in command position is valid.
    pub auto_cd: bool,
}

/// One highlight request in byte offsets.
#[derive(Debug, Clone, Copy)]
pub struct HighlightRequest<'a> {
    /// `PREBUFFER` followed by `BUFFER`.
    pub text: &'a str,
    /// Byte offset in `text` where `BUFFER` begins.
    pub buffer_start: usize,
    /// Cursor as a byte offset in `text`.
    pub cursor: usize,
    /// The shell's current directory.
    pub cwd: &'a Path,
    pub opts: RequestOptions,
}

pub struct Highlighter {
    pub config: Config,
    pub state: ShellState,
    pub paths: PathChecker,
    pub specs: SpecRegistry,
}

impl Highlighter {
    /// Returns the final spans for `req.text` in byte offsets, sorted and well nested. Spans may
    /// cover the `PREBUFFER` part; the caller clips them to the buffer.
    pub fn highlight(&mut self, req: &HighlightRequest<'_>) -> Vec<Span> {
        let _ = req;
        todo!("highlight agent")
    }
}
