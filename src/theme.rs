//! Themes: TOML files mapping token kinds to zsh highlight specs, compiled to zsh code.

use crate::config::{Config, ConfigError};
use crate::token::TokenKind;
use std::collections::BTreeMap;

/// A resolved theme: a zsh `region_highlight` style string (such as `fg=green,bold`) for each
/// token kind that has one. Kinds without an entry are left unstyled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub styles: BTreeMap<TokenKind, String>,
}

impl Theme {
    /// The built-in default theme.
    pub fn default_theme() -> Theme {
        todo!("theme agent")
    }

    /// Loads the theme selected by `config` (see [`Config::theme`]).
    pub fn load(config: &Config) -> Result<Theme, ConfigError> {
        let _ = config;
        todo!("theme agent")
    }

    /// Parses theme TOML text, layered over the default theme.
    pub fn from_toml(text: &str) -> Result<Theme, ConfigError> {
        let _ = text;
        todo!("theme agent")
    }

    /// zsh code that defines the plugin's style table, suitable for `eval`.
    pub fn to_zsh(&self) -> String {
        todo!("theme agent")
    }
}
