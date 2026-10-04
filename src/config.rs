//! Configuration loaded from `config.toml` in the config directory.

use std::path::PathBuf;

/// Size and cost limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Above this many bytes of text, skip command and filesystem lookups and lex only.
    pub lex_only_bytes: usize,
    /// Above this many bytes of text, return no highlighting at all.
    pub hard_cap_bytes: usize,
    /// Maximum number of filesystem checks per highlight request.
    pub max_path_checks: usize,
    /// How long a cached filesystem check result stays valid, in milliseconds.
    pub path_cache_ttl_ms: u64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            lex_only_bytes: 10 * 1024,
            hard_cap_bytes: 256 * 1024,
            max_path_checks: 64,
            path_cache_ttl_ms: 1000,
        }
    }
}

/// Logging options.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LogConfig {
    /// Write one line per request with its processing time.
    pub timing: bool,
    /// Log file. `None` means `$XDG_STATE_HOME/fast-highlight/fast-highlight.log`.
    pub file: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Config {
    pub limits: Limits,
    pub log: LogConfig,
    /// Theme name: `themes/<name>.toml` in the config directory, or a built-in theme name.
    /// `None` means `theme.toml` in the config directory if it exists, else the default theme.
    pub theme: Option<String>,
}

/// The config directory: `$FAST_HIGHLIGHT_CONFIG_DIR`, else
/// `${XDG_CONFIG_HOME:-$HOME/.config}/fast-highlight`.
pub fn config_dir() -> PathBuf {
    todo!("config agent")
}

#[derive(Debug)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Loads `config.toml` from [`config_dir`]. A missing file yields the defaults.
    pub fn load() -> Result<Config, ConfigError> {
        todo!("config agent")
    }

    /// Parses config TOML text.
    pub fn from_toml(text: &str) -> Result<Config, ConfigError> {
        let _ = text;
        todo!("config agent")
    }
}
