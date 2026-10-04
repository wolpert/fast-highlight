//! Per-command specs: known subcommands and options, loaded from TOML.

use crate::token::TokenKind;
use std::path::Path;

/// The spec for one command or subcommand.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandSpec {
    // Fields are defined by the specs module.
}

/// One argument word as the classifier sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgInput<'a> {
    /// The word's literal value after quote removal, or `None` when it contains expansions.
    pub literal: Option<&'a str>,
}

/// All loaded specs, keyed by command name.
#[derive(Debug, Clone, Default)]
pub struct SpecRegistry {}

impl SpecRegistry {
    /// The specs compiled into the binary.
    pub fn builtin() -> SpecRegistry {
        todo!("specs agent")
    }

    /// Built-in specs overlaid with `<config_dir>/specs/*.toml`. Returns warnings for files that
    /// failed to load; a bad file never prevents the others from loading.
    pub fn load(config_dir: &Path) -> (SpecRegistry, Vec<String>) {
        let _ = config_dir;
        todo!("specs agent")
    }

    pub fn get(&self, command: &str) -> Option<&CommandSpec> {
        let _ = command;
        todo!("specs agent")
    }
}

impl CommandSpec {
    /// True when this spec describes a precommand (`sudo`, `env`, `nice`, ...).
    pub fn is_precommand(&self) -> bool {
        todo!("specs agent")
    }

    /// For a precommand spec, the index into `args` of the wrapped command word, after skipping
    /// the precommand's own options, option arguments, and (for `env`) `NAME=value` words.
    /// `None` when no wrapped command is present yet.
    pub fn wrapped_command(&self, args: &[ArgInput<'_>]) -> Option<usize> {
        let _ = args;
        todo!("specs agent")
    }

    /// Classifies each argument of a non-precommand: `Some(Subcommand)`, `Some(CmdOption)`,
    /// `Some(Error)` (unknown subcommand or option where the spec is complete), or `None` for a
    /// plain argument. The result has the same length as `args`.
    pub fn classify_args(&self, args: &[ArgInput<'_>]) -> Vec<Option<TokenKind>> {
        let _ = args;
        todo!("specs agent")
    }
}
