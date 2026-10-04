//! Themes: TOML files mapping token kinds to zsh highlight specs, compiled to zsh code.
//!
//! ```toml
//! inherits = "default"        # optional: "none", or a built-in theme name
//! [meta]                      # optional, informational
//! name = "mine"
//! description = "My colours"
//! [styles]
//! command = "fg=green,bold"                        # a zsh style string
//! error = { fg = "red", bg = "#202020", bold = true } # or a table
//! comment = { fg = 244 }
//! assignment = ""                                  # unstyled
//! ```
//!
//! Style strings are validated against what zsh 5.9 accepts in `region_highlight`: `fg=` and
//! `bg=` with a color name, `default`, a number 0-255, or `#rgb`/`#rrggbb`; the attributes
//! `bold`, `underline`, and `standout`; or `none` alone.

use crate::config::{self, Config, ConfigError};
use crate::token::TokenKind;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// The zsh associative array that [`Theme::to_zsh`] defines.
pub const ZSH_STYLE_ARRAY: &str = "FASTHL_STYLES";

/// Built-in themes: name and TOML source.
const BUILTIN_THEMES: &[(&str, &str)] = &[
    ("default", include_str!("../themes/default.toml")),
    ("truecolor", include_str!("../themes/truecolor.toml")),
];

/// The eight color names zsh accepts everywhere, plus `default`.
const COLOR_NAMES: &[&str] = &[
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white", "default",
];

/// Attribute words zsh 5.9 accepts in a style.
const ATTRIBUTES: &[&str] = &["bold", "underline", "standout"];

/// A resolved theme: a zsh `region_highlight` style string (such as `fg=green,bold`) for each
/// token kind that has one. Kinds without an entry are left unstyled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub styles: BTreeMap<TokenKind, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTheme {
    inherits: Option<String>,
    #[serde(default, rename = "meta")]
    _meta: Option<RawMeta>,
    #[serde(default)]
    styles: BTreeMap<String, toml::Spanned<toml::Value>>,
}

/// `[meta]` is informational: it is parsed so that typos in it are reported, then discarded.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct RawMeta {
    name: Option<String>,
    description: Option<String>,
}

impl Theme {
    /// The built-in default theme.
    pub fn default_theme() -> Theme {
        Theme::builtin("default").expect("the default theme is built in")
    }

    /// Names of the built-in themes.
    pub fn builtin_names() -> impl Iterator<Item = &'static str> {
        BUILTIN_THEMES.iter().map(|(name, _)| *name)
    }

    /// A built-in theme by name.
    pub fn builtin(name: &str) -> Option<Theme> {
        let (_, source) = BUILTIN_THEMES.iter().find(|(n, _)| *n == name)?;
        match Theme::from_toml(source) {
            Ok(theme) => Some(theme),
            Err(e) => panic!("built-in theme `{name}` is invalid: {e}"),
        }
    }

    /// Loads the theme selected by `config` (see [`Config::theme`]).
    pub fn load(config: &Config) -> Result<Theme, ConfigError> {
        Theme::load_from(config, &config::config_dir())
    }

    /// [`Theme::load`] with an explicit config directory.
    pub fn load_from(config: &Config, config_dir: &Path) -> Result<Theme, ConfigError> {
        match &config.theme {
            None => {
                let path = config_dir.join("theme.toml");
                match config::read_optional(&path)? {
                    Some(text) => Theme::from_toml(&text).map_err(|e| e.in_file(&path)),
                    None => Ok(Theme::default_theme()),
                }
            }
            Some(name) => {
                let path = config_dir.join("themes").join(format!("{name}.toml"));
                if let Some(text) = config::read_optional(&path)? {
                    return Theme::from_toml(&text).map_err(|e| e.in_file(&path));
                }
                Theme::builtin(name).ok_or_else(|| {
                    ConfigError(format!(
                        "theme `{name}` not found: {} does not exist and there is no built-in \
                         theme of that name (built-in themes: {})",
                        path.display(),
                        builtin_list()
                    ))
                })
            }
        }
    }

    /// Parses theme TOML text, layered over the default theme.
    pub fn from_toml(text: &str) -> Result<Theme, ConfigError> {
        let raw: RawTheme = toml::from_str(text).map_err(|e| ConfigError(e.to_string()))?;
        let mut theme = match raw.inherits.as_deref().unwrap_or("default") {
            "none" => Theme {
                styles: BTreeMap::new(),
            },
            base => Theme::builtin(base).ok_or_else(|| {
                ConfigError(format!(
                    "inherits = {base:?}: expected \"none\" or a built-in theme ({})",
                    builtin_list()
                ))
            })?,
        };
        for (key, value) in &raw.styles {
            let line = text[..value.span().start].matches('\n').count() + 1;
            let Some(kind) = TokenKind::from_name(key) else {
                return Err(ConfigError(format!(
                    "line {line}: unknown token kind `{key}` in [styles]; valid kinds: {}",
                    kind_list()
                )));
            };
            let style = style_from_value(value.get_ref())
                .map_err(|e| ConfigError(format!("line {line}: styles.{key}: {e}")))?;
            if style.is_empty() {
                theme.styles.remove(&kind);
            } else {
                theme.styles.insert(kind, style);
            }
        }
        Ok(theme)
    }

    /// zsh code that defines the plugin's style table, suitable for `eval`.
    ///
    /// The output defines the global associative array [`ZSH_STYLE_ARRAY`], one kind per line
    /// sorted by name, with every style single-quoted. Kinds with an empty style are omitted.
    pub fn to_zsh(&self) -> String {
        let mut entries: Vec<(&str, &str)> = self
            .styles
            .iter()
            .filter(|(_, style)| !style.is_empty())
            .map(|(kind, style)| (kind.name(), style.as_str()))
            .collect();
        entries.sort_unstable();
        let mut out = format!("typeset -gA {ZSH_STYLE_ARRAY}\n{ZSH_STYLE_ARRAY}=(\n");
        for (name, style) in entries {
            out.push_str("  ");
            out.push_str(name);
            out.push(' ');
            out.push_str(&zsh_quote(style));
            out.push('\n');
        }
        out.push_str(")\n");
        out
    }
}

fn builtin_list() -> String {
    Theme::builtin_names().collect::<Vec<_>>().join(", ")
}

fn kind_list() -> String {
    let mut names: Vec<&str> = TokenKind::ALL.iter().map(|k| k.name()).collect();
    names.sort_unstable();
    names.join(", ")
}

/// Single-quotes `s` for zsh, writing each `'` as `'\''`.
fn zsh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Converts a `[styles]` value to a normalised zsh style string. An empty result means unstyled.
fn style_from_value(value: &toml::Value) -> Result<String, String> {
    match value {
        toml::Value::String(spec) => normalize_spec(spec),
        toml::Value::Table(table) => style_from_table(table),
        other => Err(format!(
            "expected a style string or a table, found {}",
            other.type_str()
        )),
    }
}

/// Validates a zsh style string such as `fg=green,bold` and returns it with the whitespace
/// around each comma-separated item removed.
fn normalize_spec(spec: &str) -> Result<String, String> {
    if spec.trim().is_empty() {
        return Ok(String::new());
    }
    let items: Vec<&str> = spec.split(',').map(str::trim).collect();
    let mut seen: Vec<&str> = Vec::new();
    for &item in &items {
        let key = match item.split_once('=') {
            Some(("fg" | "bg", color)) => {
                validate_color(color)?;
                &item[..2]
            }
            Some(_) => {
                return Err(format!(
                    "unknown style item `{item}`; {}",
                    valid_items_hint()
                ));
            }
            None if item.is_empty() => return Err(format!("empty item in `{spec}`")),
            None if item == "none" => {
                if items.len() > 1 {
                    return Err(format!(
                        "`none` cannot be combined with other items in `{spec}`"
                    ));
                }
                item
            }
            None if ATTRIBUTES.contains(&item) => item,
            None => {
                return Err(format!(
                    "unknown attribute `{item}`; {}{}",
                    valid_items_hint(),
                    unsupported_note(item)
                ));
            }
        };
        if seen.contains(&key) {
            return Err(format!("`{key}` given more than once in `{spec}`"));
        }
        seen.push(key);
    }
    Ok(items.join(","))
}

fn style_from_table(table: &toml::Table) -> Result<String, String> {
    let mut colors = Vec::new();
    let mut attrs = Vec::new();
    for (key, value) in table {
        match key.as_str() {
            "fg" | "bg" => colors.push((key.as_str(), color_from_value(value)?)),
            attr if ATTRIBUTES.contains(&attr) => match value {
                toml::Value::Boolean(true) => attrs.push(attr),
                toml::Value::Boolean(false) => {}
                other => {
                    return Err(format!(
                        "`{key}` must be true or false, found {}",
                        other.type_str()
                    ));
                }
            },
            _ => {
                return Err(format!(
                    "unknown key `{key}`; valid keys: fg, bg, {}{}",
                    ATTRIBUTES.join(", "),
                    unsupported_note(key)
                ));
            }
        }
    }
    // Fixed output order: fg, bg, then attributes in `ATTRIBUTES` order.
    colors.sort_by_key(|(key, _)| *key != "fg");
    attrs.sort_by_key(|attr| ATTRIBUTES.iter().position(|a| a == attr));
    let mut items: Vec<String> = colors
        .into_iter()
        .map(|(k, c)| format!("{k}={c}"))
        .collect();
    items.extend(attrs.into_iter().map(String::from));
    Ok(items.join(","))
}

fn color_from_value(value: &toml::Value) -> Result<String, String> {
    match value {
        toml::Value::String(color) => validate_color(color).map(|()| color.clone()),
        toml::Value::Integer(n) if (0..=255).contains(n) => Ok(n.to_string()),
        toml::Value::Integer(n) => Err(format!("color number {n} is out of range 0-255")),
        other => Err(format!(
            "a color must be a string or a number, found {}",
            other.type_str()
        )),
    }
}

fn validate_color(color: &str) -> Result<(), String> {
    let ok = if let Some(hex) = color.strip_prefix('#') {
        matches!(hex.len(), 3 | 6) && hex.bytes().all(|b| b.is_ascii_hexdigit())
    } else if !color.is_empty() && color.bytes().all(|b| b.is_ascii_digit()) {
        color.len() <= 3 && color.parse::<u16>().is_ok_and(|n| n <= 255)
    } else {
        COLOR_NAMES.contains(&color)
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid color `{color}`; expected one of {}, a number 0-255, or #rrggbb",
            COLOR_NAMES.join(", ")
        ))
    }
}

fn valid_items_hint() -> String {
    format!(
        "valid items: fg=<color>, bg=<color>, {}, none",
        ATTRIBUTES.join(", ")
    )
}

fn unsupported_note(word: &str) -> &'static str {
    match word {
        "italic" | "faint" => " (zsh 5.9 does not support italic or faint)",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn style(theme: &Theme, kind: TokenKind) -> Option<&str> {
        theme.styles.get(&kind).map(String::as_str)
    }

    fn parse_err(text: &str) -> String {
        Theme::from_toml(text)
            .expect_err("expected an error")
            .to_string()
    }

    fn only(text: &str) -> Theme {
        Theme::from_toml(&format!("inherits = \"none\"\n{text}")).unwrap()
    }

    fn command_style(value: &str) -> Result<String, String> {
        Theme::from_toml(&format!(
            "inherits = \"none\"\n[styles]\ncommand = {value}\n"
        ))
        .map(|t| style(&t, TokenKind::Command).unwrap_or("").to_owned())
        .map_err(|e| e.to_string())
    }

    #[test]
    fn default_theme_covers_every_kind_deliberately() {
        let raw: RawTheme = toml::from_str(include_str!("../themes/default.toml")).unwrap();
        for kind in TokenKind::ALL {
            assert!(
                raw.styles.contains_key(kind.name()),
                "default.toml lacks `{kind}`"
            );
        }
        let theme = Theme::default_theme();
        let unstyled: Vec<TokenKind> = TokenKind::ALL
            .iter()
            .copied()
            .filter(|k| !theme.styles.contains_key(k))
            .collect();
        assert_eq!(unstyled, vec![TokenKind::Default, TokenKind::Assignment]);
        assert_eq!(style(&theme, TokenKind::Command), Some("fg=green"));
        assert_eq!(style(&theme, TokenKind::Error), Some("fg=red,bold"));
        assert_eq!(style(&theme, TokenKind::Comment), Some("fg=8"));
    }

    #[test]
    fn builtin_themes_parse() {
        let names: Vec<&str> = Theme::builtin_names().collect();
        assert!(names.contains(&"default") && names.contains(&"truecolor"));
        for name in names {
            let theme = Theme::builtin(name).unwrap();
            assert!(!theme.styles.is_empty(), "{name}");
            for style in theme.styles.values() {
                assert_eq!(
                    normalize_spec(style).as_deref(),
                    Ok(style.as_str()),
                    "{name}"
                );
            }
        }
        assert_eq!(Theme::builtin("nope"), None);
    }

    #[test]
    fn truecolor_theme_uses_hex_colors() {
        let theme = Theme::builtin("truecolor").unwrap();
        assert_eq!(style(&theme, TokenKind::Error), Some("fg=#f05f5f,bold"));
        assert_eq!(style(&theme, TokenKind::Path), Some("underline"));
        assert!(theme.styles.values().filter(|s| s.contains('#')).count() > 20);
    }

    #[test]
    fn empty_text_is_the_default_theme() {
        assert_eq!(Theme::from_toml("").unwrap(), Theme::default_theme());
        assert_eq!(
            Theme::from_toml("[styles]\n").unwrap(),
            Theme::default_theme()
        );
    }

    #[test]
    fn styles_layer_over_default() {
        let theme = Theme::from_toml("[styles]\ncommand = \"fg=blue\"\nerror = \"\"\n").unwrap();
        let mut expected = Theme::default_theme();
        expected.styles.insert(TokenKind::Command, "fg=blue".into());
        expected.styles.remove(&TokenKind::Error);
        assert_eq!(theme, expected);
    }

    #[test]
    fn inherits_none_starts_empty() {
        let theme = only("[styles]\noption = \"bold\"\n");
        assert_eq!(theme.styles.len(), 1);
        assert_eq!(style(&theme, TokenKind::CmdOption), Some("bold"));
    }

    #[test]
    fn inherits_a_builtin_theme() {
        let theme = Theme::from_toml("inherits = \"truecolor\"\n[styles]\nglob = \"fg=1\"\n");
        let mut expected = Theme::builtin("truecolor").unwrap();
        expected.styles.insert(TokenKind::Glob, "fg=1".into());
        assert_eq!(theme.unwrap(), expected);
    }

    #[test]
    fn unknown_base_is_rejected() {
        let e = parse_err("inherits = \"solarized\"");
        assert!(
            e.contains("\"solarized\"") && e.contains("default, truecolor"),
            "{e}"
        );
    }

    #[test]
    fn meta_is_accepted_and_checked() {
        assert!(Theme::from_toml("[meta]\nname = \"x\"\ndescription = \"y\"\n").is_ok());
        assert!(parse_err("[meta]\nauthor = \"x\"\n").contains("author"));
    }

    #[test]
    fn unknown_top_level_keys_are_rejected() {
        assert!(parse_err("[colors]\n").contains("colors"));
    }

    #[test]
    fn unknown_kind_lists_valid_kinds() {
        let e = parse_err("[styles]\n\ncomand = \"fg=green\"\n");
        assert!(e.contains("line 3") && e.contains("`comand`"), "{e}");
        assert!(
            e.contains("reserved-word") && e.contains("path-prefix"),
            "{e}"
        );
    }

    #[test]
    fn string_specs() {
        assert_eq!(command_style("\"fg=green,bold\"").unwrap(), "fg=green,bold");
        assert_eq!(
            command_style("\" fg=green , bg=black ,underline\"").unwrap(),
            "fg=green,bg=black,underline"
        );
        assert_eq!(command_style("\"standout\"").unwrap(), "standout");
        assert_eq!(command_style("\"none\"").unwrap(), "none");
        assert_eq!(command_style("\"  \"").unwrap(), "");
    }

    #[test]
    fn all_color_forms() {
        for color in [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ] {
            assert!(command_style(&format!("\"fg={color}\"")).is_ok(), "{color}");
        }
        for color in ["default", "0", "8", "255", "#000", "#ABCdef", "#a0b1c2"] {
            assert_eq!(
                command_style(&format!("\"bg={color}\"")).unwrap(),
                format!("bg={color}")
            );
        }
        for color in [
            "256", "-1", "1000", "0255", "#12", "#1234", "#gggggg", "Red", "bl", "", "#",
        ] {
            let e = command_style(&format!("\"fg={color}\"")).unwrap_err();
            assert!(
                e.contains("invalid color") && e.contains("styles.command"),
                "{color}: {e}"
            );
        }
    }

    #[test]
    fn bad_string_specs() {
        let cases = [
            ("\"bold,,underline\"", "empty item"),
            ("\"fg=red,\"", "empty item"),
            ("\"none,bold\"", "`none` cannot be combined"),
            ("\"blink\"", "unknown attribute `blink`"),
            ("\"italic\"", "does not support italic"),
            ("\"color=red\"", "unknown style item"),
            ("\"fg=red,fg=blue\"", "`fg` given more than once"),
            ("\"bold,bold\"", "`bold` given more than once"),
        ];
        for (value, needle) in cases {
            let e = command_style(value).unwrap_err();
            assert!(e.contains(needle), "{value}: {e}");
        }
    }

    #[test]
    fn table_specs() {
        assert_eq!(
            command_style("{ standout = true, underline = true, bold = true, bg = \"blue\", fg = \"#102030\" }")
                .unwrap(),
            "fg=#102030,bg=blue,bold,underline,standout"
        );
        assert_eq!(
            command_style("{ fg = 208, bold = false }").unwrap(),
            "fg=208"
        );
        assert_eq!(command_style("{ bg = 0 }").unwrap(), "bg=0");
        assert_eq!(command_style("{}").unwrap(), "");
    }

    #[test]
    fn bad_table_specs() {
        let cases = [
            ("{ fg = 256 }", "out of range"),
            ("{ fg = -1 }", "out of range"),
            ("{ fg = \"grren\" }", "invalid color `grren`"),
            ("{ fg = true }", "must be a string or a number"),
            ("{ bold = \"yes\" }", "must be true or false"),
            ("{ italic = true }", "does not support italic"),
            ("{ blink = true }", "unknown key `blink`"),
        ];
        for (value, needle) in cases {
            let e = command_style(value).unwrap_err();
            assert!(e.contains(needle), "{value}: {e}");
        }
        assert!(command_style("3").unwrap_err().contains("found integer"));
        assert!(
            command_style("[\"fg=red\"]")
                .unwrap_err()
                .contains("found array")
        );
    }

    #[test]
    fn to_zsh_format() {
        let theme =
            only("[styles]\noption = \"fg=cyan\"\ncommand = { fg = \"green\" }\nalias = \"\"\n");
        assert_eq!(
            theme.to_zsh(),
            "typeset -gA FASTHL_STYLES\nFASTHL_STYLES=(\n  command 'fg=green'\n  option 'fg=cyan'\n)\n"
        );
    }

    #[test]
    fn to_zsh_sorts_by_name_and_skips_empty() {
        let mut styles = BTreeMap::new();
        styles.insert(TokenKind::Path, "underline".to_owned());
        styles.insert(TokenKind::Alias, "bold".to_owned());
        styles.insert(TokenKind::CmdOption, String::new());
        styles.insert(TokenKind::Error, "fg=red".to_owned());
        let out = Theme { styles }.to_zsh();
        let keys: Vec<&str> = out
            .lines()
            .filter_map(|l| l.strip_prefix("  ")?.split(' ').next())
            .collect();
        assert_eq!(keys, ["alias", "error", "path"]);
    }

    #[test]
    fn zsh_quote_escapes_single_quotes() {
        assert_eq!(zsh_quote("fg=red"), "'fg=red'");
        assert_eq!(zsh_quote("a'b"), r"'a'\''b'");
        assert_eq!(zsh_quote("''"), r"''\'''\'''");
        assert_eq!(zsh_quote(""), "''");
    }

    /// Runs `script` with `zsh -f`, or returns `None` when zsh is not installed.
    fn run_zsh(script: &str) -> Option<String> {
        let out = match std::process::Command::new("zsh")
            .arg("-f")
            .arg("-c")
            .arg(script)
            .output()
        {
            Ok(out) => out,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("zsh not installed; skipping");
                return None;
            }
            Err(e) => panic!("cannot run zsh: {e}"),
        };
        assert!(
            out.status.success(),
            "zsh failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stderr.is_empty(),
            "zsh stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Some(String::from_utf8(out.stdout).unwrap())
    }

    /// Prints every `key=value` entry of the style table, NUL-terminated.
    const DUMP: &str =
        r#"for k in ${(k)FASTHL_STYLES}; do print -rN -- "$k=${FASTHL_STYLES[$k]}"; done"#;

    /// The entries printed by [`DUMP`], sorted (zsh's own ordering depends on the locale).
    fn parse_dump(out: &str) -> Vec<String> {
        let mut entries: Vec<String> = out.split_terminator('\0').map(String::from).collect();
        entries.sort();
        entries
    }

    fn expected_dump(theme: &Theme) -> Vec<String> {
        let mut entries: Vec<String> = theme
            .styles
            .iter()
            .map(|(k, s)| format!("{}={s}", k.name()))
            .collect();
        entries.sort();
        entries
    }

    #[test]
    fn to_zsh_is_valid_zsh_for_builtin_themes() {
        for name in Theme::builtin_names() {
            let theme = Theme::builtin(name).unwrap();
            let script = format!("{}print -r -- ${{FASTHL_STYLES[command]}}", theme.to_zsh());
            let Some(out) = run_zsh(&script) else { return };
            assert_eq!(out.trim_end(), theme.styles[&TokenKind::Command], "{name}");

            // Eval inside a function: the table must still be global and complete.
            let script = format!("f() {{ eval {}; }}; f; {DUMP}", zsh_quote(&theme.to_zsh()));
            assert_eq!(
                parse_dump(&run_zsh(&script).unwrap()),
                expected_dump(&theme),
                "{name}"
            );
        }
    }

    #[test]
    fn to_zsh_quoting_survives_zsh() {
        let mut styles = BTreeMap::new();
        styles.insert(
            TokenKind::Command,
            "it's $(touch /nonexistent/x) `id` \"q\" \\ ;".to_owned(),
        );
        styles.insert(TokenKind::Error, "a\nb'".to_owned());
        styles.insert(TokenKind::Glob, "'".to_owned());
        let theme = Theme { styles };
        let Some(out) = run_zsh(&format!("{}{DUMP}", theme.to_zsh())) else {
            return;
        };
        assert_eq!(parse_dump(&out), expected_dump(&theme));
    }

    #[test]
    fn to_zsh_replaces_previous_table() {
        let full = Theme::default_theme().to_zsh();
        let small = only("[styles]\nglob = \"bold\"\n");
        let Some(out) = run_zsh(&format!("{full}{}{DUMP}", small.to_zsh())) else {
            return;
        };
        assert_eq!(parse_dump(&out), ["glob=bold"]);
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("fast-highlight-theme-test-{}", std::process::id()))
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn with_theme(name: Option<&str>) -> Config {
        Config {
            theme: name.map(String::from),
            ..Config::default()
        }
    }

    #[test]
    fn load_without_files_uses_default() {
        let dir = test_dir("empty");
        assert_eq!(
            Theme::load_from(&with_theme(None), &dir).unwrap(),
            Theme::default_theme()
        );
    }

    #[test]
    fn load_theme_toml_when_unnamed() {
        let dir = test_dir("unnamed");
        std::fs::write(dir.join("theme.toml"), "[styles]\ncommand = \"fg=1\"\n").unwrap();
        let theme = Theme::load_from(&with_theme(None), &dir).unwrap();
        assert_eq!(style(&theme, TokenKind::Command), Some("fg=1"));
        assert_eq!(style(&theme, TokenKind::Error), Some("fg=red,bold"));
    }

    #[test]
    fn load_named_user_theme_before_builtin() {
        let dir = test_dir("named");
        std::fs::create_dir(dir.join("themes")).unwrap();
        std::fs::write(dir.join("theme.toml"), "[styles]\ncommand = \"fg=1\"\n").unwrap();
        std::fs::write(
            dir.join("themes/truecolor.toml"),
            "[styles]\ncommand = \"fg=2\"\n",
        )
        .unwrap();
        let theme = Theme::load_from(&with_theme(Some("truecolor")), &dir).unwrap();
        assert_eq!(style(&theme, TokenKind::Command), Some("fg=2"));
    }

    #[test]
    fn load_named_builtin_when_no_file() {
        let dir = test_dir("builtin");
        let theme = Theme::load_from(&with_theme(Some("truecolor")), &dir).unwrap();
        assert_eq!(theme, Theme::builtin("truecolor").unwrap());
    }

    #[test]
    fn load_unknown_name_is_an_error() {
        let dir = test_dir("unknown");
        let e = Theme::load_from(&with_theme(Some("nope")), &dir)
            .unwrap_err()
            .to_string();
        assert!(e.contains("theme `nope` not found"), "{e}");
        assert!(
            e.contains(&dir.join("themes/nope.toml").display().to_string()),
            "{e}"
        );
        assert!(e.contains("default, truecolor"), "{e}");
    }

    #[test]
    fn load_errors_name_the_file() {
        let dir = test_dir("bad");
        let path = dir.join("theme.toml");
        std::fs::write(&path, "[styles]\ncommand = \"fg=grren\"\n").unwrap();
        let e = Theme::load_from(&with_theme(None), &dir)
            .unwrap_err()
            .to_string();
        assert!(
            e.starts_with(&format!("{}: line 2: styles.command: ", path.display())),
            "{e}"
        );
    }
}
