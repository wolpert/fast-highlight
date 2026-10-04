//! The daemon's copy of the shell's command namespace, plus its own `$PATH` scan.

use crate::protocol::StateUpdate;

/// What a command word resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandClass {
    Alias,
    SuffixAlias,
    GlobalAlias,
    Function,
    Builtin,
    ReservedWord,
    External,
    Unknown,
}

#[derive(Debug, Default)]
pub struct ShellState {}

impl ShellState {
    pub fn new() -> ShellState {
        todo!("state agent")
    }

    pub fn apply_update(&mut self, update: StateUpdate) {
        let _ = update;
        todo!("state agent")
    }
}
