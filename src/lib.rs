//! fast-highlight: a syntax highlighter for the zsh line editor.
//!
//! Module map:
//!
//! - [`token`]: token kinds and spans, the contract every other module shares.
//! - [`syntax`]: the error-tolerant zsh lexer and parser (purely syntactic).
//! - [`state`]: the daemon's copy of the shell's command namespace and the `$PATH` scan.
//! - [`paths`]: tilde expansion and cached filesystem checks for path arguments.
//! - [`specs`]: per-command subcommand and option specs loaded from TOML.
//! - [`highlight`]: the semantic pass that turns a parse into the final span list.
//! - [`config`] and [`theme`]: TOML configuration and the theme compiled to zsh styles.
//! - [`protocol`]: the framed wire protocol described in `docs/protocol.md`.
//! - [`daemon`]: the `serve` loop.

pub mod config;
pub mod daemon;
pub mod highlight;
pub mod paths;
pub mod protocol;
pub mod specs;
pub mod state;
pub mod syntax;
pub mod theme;
pub mod token;
