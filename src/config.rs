//! Configuration loaded from `config.toml` in the config directory.
//!
//! ```toml
//! theme = "default"            # optional
//! [limits]
//! lex-only-bytes = 10240
//! hard-cap-bytes = 262144
//! max-path-checks = 64
//! path-cache-ttl-ms = 1000
//! [log]
//! timing = false
//! file = "~/some/path.log"     # `~` and `~/` are expanded
//! ```
//!
//! Every key is optional; unknown keys are errors.

use serde::Deserialize;
use std::ffi::{CStr, OsString};
use std::path::{Path, PathBuf};

/// Size and cost limits.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
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
    config_dir_from(|key| std::env::var_os(key))
}

/// [`config_dir`] with the environment supplied by `env`.
///
/// Empty variables count as unset. `XDG_CONFIG_HOME` is ignored unless it is absolute, as the
/// XDG base directory specification requires.
pub fn config_dir_from(env: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    let var = |key: &str| env(key).filter(|v| !v.is_empty());
    if let Some(dir) = var("FAST_HIGHLIGHT_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(xdg) = var("XDG_CONFIG_HOME").map(PathBuf::from)
        && xdg.is_absolute()
    {
        return xdg.join("fast-highlight");
    }
    home_dir(&env).join(".config").join("fast-highlight")
}

/// `$HOME`, else the home directory in the password database, else `/`.
fn home_dir(env: &impl Fn(&str) -> Option<OsString>) -> PathBuf {
    env("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(passwd_home)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The current user's home directory from the password database.
fn passwd_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;

    let mut buf = vec![0 as libc::c_char; 4096];
    loop {
        // SAFETY: `pwd` and `result` are valid out-pointers, `buf` is a writable buffer of the
        // stated length, and the strings read from `pwd` point into `buf`, which outlives them.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                &mut pwd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            )
        };
        if rc == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
            return None;
        }
        let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
        let dir = std::ffi::OsStr::from_bytes(dir.to_bytes());
        return (!dir.is_empty()).then(|| PathBuf::from(dir));
    }
}

#[derive(Debug)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

impl ConfigError {
    /// Prefixes the message with the file it came from.
    pub(crate) fn in_file(self, path: &Path) -> ConfigError {
        ConfigError(format!("{}: {}", path.display(), self.0))
    }
}

/// Reads a file, mapping "not found" to `None`.
pub(crate) fn read_optional(path: &Path) -> Result<Option<String>, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ConfigError(format!("{}: cannot read: {e}", path.display()))),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawConfig {
    theme: Option<String>,
    #[serde(default)]
    limits: Limits,
    #[serde(default)]
    log: RawLog,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawLog {
    #[serde(default)]
    timing: bool,
    file: Option<String>,
}

impl Config {
    /// Loads `config.toml` from [`config_dir`]. A missing file yields the defaults.
    pub fn load() -> Result<Config, ConfigError> {
        Config::load_from(&config_dir().join("config.toml"))
    }

    /// Loads a config file. A missing file yields the defaults; every error names the file.
    pub fn load_from(path: &Path) -> Result<Config, ConfigError> {
        match read_optional(path)? {
            None => Ok(Config::default()),
            Some(text) => Config::from_toml(&text).map_err(|e| e.in_file(path)),
        }
    }

    /// Parses config TOML text.
    pub fn from_toml(text: &str) -> Result<Config, ConfigError> {
        Config::from_toml_with_env(text, |key| std::env::var_os(key))
    }

    /// [`Config::from_toml`] with the environment (used for `~` expansion) supplied by `env`.
    pub fn from_toml_with_env(
        text: &str,
        env: impl Fn(&str) -> Option<OsString>,
    ) -> Result<Config, ConfigError> {
        let raw: RawConfig = toml::from_str(text).map_err(|e| ConfigError(e.to_string()))?;
        if let Some(name) = &raw.theme {
            validate_theme_name(name)?;
        }
        validate_limits(&raw.limits)?;
        let file = match raw.log.file {
            None => None,
            Some(file) => Some(expand_log_path(&file, &env)?),
        };
        Ok(Config {
            limits: raw.limits,
            log: LogConfig {
                timing: raw.log.timing,
                file,
            },
            theme: raw.theme,
        })
    }
}

fn validate_theme_name(name: &str) -> Result<(), ConfigError> {
    if name.is_empty() || name.starts_with('.') || name.contains(['/', '\0']) {
        return Err(ConfigError(format!(
            "theme = {name:?}: a theme name must be non-empty, must not start with `.`, and must \
             not contain `/`"
        )));
    }
    Ok(())
}

fn validate_limits(limits: &Limits) -> Result<(), ConfigError> {
    if limits.hard_cap_bytes == 0 {
        return Err(ConfigError(
            "limits.hard-cap-bytes must be greater than 0".into(),
        ));
    }
    if limits.hard_cap_bytes < limits.lex_only_bytes {
        return Err(ConfigError(format!(
            "limits.hard-cap-bytes ({}) must be at least limits.lex-only-bytes ({})",
            limits.hard_cap_bytes, limits.lex_only_bytes
        )));
    }
    Ok(())
}

/// Expands a leading `~` or `~/` and requires the result to be absolute.
fn expand_log_path(
    file: &str,
    env: &impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf, ConfigError> {
    let path = if file == "~" {
        home_dir(env)
    } else if let Some(rest) = file.strip_prefix("~/") {
        home_dir(env).join(rest)
    } else if file.starts_with('~') {
        return Err(ConfigError(format!(
            "log.file = {file:?}: only `~` and `~/` are expanded, not `~user`"
        )));
    } else {
        PathBuf::from(file)
    };
    if !path.is_absolute() {
        return Err(ConfigError(format!(
            "log.file = {file:?}: the path must be absolute or start with `~/`"
        )));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn parse(text: &str) -> Result<Config, ConfigError> {
        Config::from_toml_with_env(text, env_of(&[("HOME", "/home/u")]))
    }

    fn parse_err(text: &str) -> String {
        parse(text).expect_err("expected an error").to_string()
    }

    #[test]
    fn empty_text_gives_defaults() {
        assert_eq!(parse("").unwrap(), Config::default());
        assert_eq!(parse("[limits]\n[log]\n").unwrap(), Config::default());
    }

    #[test]
    fn defaults_match_documented_values() {
        let l = Limits::default();
        assert_eq!(
            (
                l.lex_only_bytes,
                l.hard_cap_bytes,
                l.max_path_checks,
                l.path_cache_ttl_ms
            ),
            (10240, 262144, 64, 1000)
        );
        let c = Config::default();
        assert_eq!(c.theme, None);
        assert_eq!(
            c.log,
            LogConfig {
                timing: false,
                file: None
            }
        );
    }

    #[test]
    fn full_config_parses() {
        let c = parse(
            r#"
theme = "truecolor"
[limits]
lex-only-bytes = 100
hard-cap-bytes = 200
max-path-checks = 0
path-cache-ttl-ms = 0
[log]
timing = true
file = "~/logs/fh.log"
"#,
        )
        .unwrap();
        assert_eq!(c.theme.as_deref(), Some("truecolor"));
        assert_eq!(
            c.limits,
            Limits {
                lex_only_bytes: 100,
                hard_cap_bytes: 200,
                max_path_checks: 0,
                path_cache_ttl_ms: 0
            }
        );
        assert!(c.log.timing);
        assert_eq!(c.log.file, Some(PathBuf::from("/home/u/logs/fh.log")));
    }

    #[test]
    fn partial_limits_keep_other_defaults() {
        let c = parse("[limits]\nmax-path-checks = 8\n").unwrap();
        assert_eq!(
            c.limits,
            Limits {
                max_path_checks: 8,
                ..Limits::default()
            }
        );
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(parse_err("colour = 1").contains("colour"));
        assert!(parse_err("[limits]\nlex_only_bytes = 1").contains("lex_only_bytes"));
        assert!(parse_err("[log]\nlevel = 1").contains("level"));
        assert!(parse_err("[extra]").contains("extra"));
    }

    #[test]
    fn wrong_types_are_rejected() {
        assert!(parse_err("[limits]\nmax-path-checks = -1").contains("max-path-checks"));
        assert!(parse_err("[limits]\nmax-path-checks = \"8\"").contains("max-path-checks"));
        assert!(parse_err("theme = 3").contains("theme"));
        assert!(parse_err("[log]\ntiming = \"yes\"").contains("timing"));
    }

    #[test]
    fn syntax_errors_are_reported() {
        assert!(parse_err("theme = ").contains("line 1"));
    }

    #[test]
    fn hard_cap_must_cover_lex_only() {
        let e = parse_err("[limits]\nlex-only-bytes = 500\nhard-cap-bytes = 499\n");
        assert!(
            e.contains("hard-cap-bytes (499)") && e.contains("lex-only-bytes (500)"),
            "{e}"
        );
        // Equal values are allowed.
        assert!(parse("[limits]\nlex-only-bytes = 500\nhard-cap-bytes = 500\n").is_ok());
        // The default lex-only threshold applies when only the cap is set.
        assert!(parse_err("[limits]\nhard-cap-bytes = 100\n").contains("(10240)"));
    }

    #[test]
    fn hard_cap_must_be_nonzero() {
        let e = parse_err("[limits]\nlex-only-bytes = 0\nhard-cap-bytes = 0\n");
        assert!(e.contains("hard-cap-bytes must be greater than 0"), "{e}");
        assert!(parse("[limits]\nlex-only-bytes = 0\n").is_ok());
    }

    #[test]
    fn bad_theme_names_are_rejected() {
        for name in ["", ".hidden", "../x", "a/b"] {
            let e = parse_err(&format!("theme = {name:?}"));
            assert!(e.contains("theme name"), "{name}: {e}");
        }
        assert!(parse("theme = \"my-theme.v2\"").is_ok());
    }

    #[test]
    fn log_file_tilde_expansion() {
        let file = |text: &str| parse(text).unwrap().log.file.unwrap();
        assert_eq!(file("[log]\nfile = \"~\""), PathBuf::from("/home/u"));
        assert_eq!(
            file("[log]\nfile = \"~/a/b.log\""),
            PathBuf::from("/home/u/a/b.log")
        );
        assert_eq!(
            file("[log]\nfile = \"/var/log/x\""),
            PathBuf::from("/var/log/x")
        );
        assert!(parse_err("[log]\nfile = \"~bob/x\"").contains("~user"));
        assert!(parse_err("[log]\nfile = \"rel/x\"").contains("absolute"));
    }

    #[test]
    fn log_file_tilde_without_home_uses_passwd() {
        let c = Config::from_toml_with_env("[log]\nfile = \"~/x\"", |_| None).unwrap();
        let path = c.log.file.unwrap();
        assert!(path.is_absolute() && path.ends_with("x"), "{path:?}");
    }

    #[test]
    fn config_dir_override_wins() {
        let env = env_of(&[
            ("FAST_HIGHLIGHT_CONFIG_DIR", "/etc/fh"),
            ("XDG_CONFIG_HOME", "/xdg"),
            ("HOME", "/home/u"),
        ]);
        assert_eq!(config_dir_from(env), PathBuf::from("/etc/fh"));
    }

    #[test]
    fn config_dir_uses_absolute_xdg() {
        let env = env_of(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/u")]);
        assert_eq!(config_dir_from(env), PathBuf::from("/xdg/fast-highlight"));
    }

    #[test]
    fn config_dir_ignores_relative_or_empty_xdg() {
        for xdg in ["relative/dir", ""] {
            let env = env_of(&[("XDG_CONFIG_HOME", xdg), ("HOME", "/home/u")]);
            assert_eq!(
                config_dir_from(env),
                PathBuf::from("/home/u/.config/fast-highlight")
            );
        }
    }

    #[test]
    fn config_dir_ignores_empty_override() {
        let env = env_of(&[("FAST_HIGHLIGHT_CONFIG_DIR", ""), ("HOME", "/home/u")]);
        assert_eq!(
            config_dir_from(env),
            PathBuf::from("/home/u/.config/fast-highlight")
        );
    }

    #[test]
    fn config_dir_without_home_is_absolute() {
        let dir = config_dir_from(|_| None);
        assert!(
            dir.is_absolute() && dir.ends_with(".config/fast-highlight"),
            "{dir:?}"
        );
    }

    #[test]
    fn load_from_missing_file_gives_defaults() {
        let dir = test_dir("missing");
        assert_eq!(
            Config::load_from(&dir.join("config.toml")).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn load_from_reports_the_path() {
        let dir = test_dir("bad");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[limits]\nbogus = 1\n").unwrap();
        let e = Config::load_from(&path).unwrap_err().to_string();
        assert!(e.starts_with(&format!("{}: ", path.display())), "{e}");
        assert!(e.contains("bogus"), "{e}");

        std::fs::write(&path, "[limits]\nhard-cap-bytes = 1\n").unwrap();
        let e = Config::load_from(&path).unwrap_err().to_string();
        assert!(e.starts_with(&format!("{}: ", path.display())), "{e}");
    }

    #[test]
    fn load_from_unreadable_path_is_an_error() {
        let dir = test_dir("isdir");
        let e = Config::load_from(&dir).unwrap_err().to_string();
        assert!(e.contains("cannot read"), "{e}");
    }

    /// A fresh, empty directory under the system temp dir, unique per process and test name.
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("fast-highlight-config-test-{}", std::process::id()))
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
