//! Per-command specs: known subcommands and options, loaded from TOML.
//!
//! # Spec file format
//!
//! Each spec is one TOML file describing one command. The file name is irrelevant; the `name`
//! key identifies the command. Built-in specs are compiled into the binary. Files matching
//! `<config_dir>/specs/*.toml` are read in file-name order after the built-ins.
//!
//! ```toml
//! name = "git"
//! common-options = ["-h", "--help"]
//! options = ["-C=", "-c=", "-p", "--paginate", "--git-dir=", "--version"]
//!
//! [subcommands.commit]
//! options = ["-m=", "--message=", "-a", "--all", "--amend", "-S=?", "--gpg-sign=?"]
//!
//! [subcommands.remote]
//! complete = true
//!
//! [subcommands.remote.subcommands.remove]
//! aliases = ["rm"]
//! ```
//!
//! ## Top-level keys
//!
//! | Key                | Type             | Default | Meaning |
//! |--------------------|------------------|---------|---------|
//! | `name`             | string           | (required) | The command name. |
//! | `aliases`          | array of strings | `[]`    | Other command names that share this spec. |
//! | `merge`            | bool             | `false` | In a user file: merge into the existing spec of the same name instead of replacing it. |
//! | `common-options`   | array of strings | `[]`    | Options valid at the top level and at every subcommand level, at any depth. |
//! | `precommand`       | bool             | `false` | The command runs another command (`sudo`, `env`, `nice`). |
//! | `skip-assignments` | bool             | `false` | Precommands only: `NAME=value` words before the wrapped command are skipped (`env`). |
//! | `positional`       | integer          | `0`     | Precommands only: the number of plain arguments between the options and the wrapped command (`timeout DURATION cmd`). |
//!
//! The top level also accepts every level key below.
//!
//! ## Level keys
//!
//! These keys are valid at the top level and in every `[subcommands.NAME]` table. Subcommand
//! tables nest to any depth: `[subcommands.remote.subcommands.add]`.
//!
//! | Key                | Type             | Default | Meaning |
//! |--------------------|------------------|---------|---------|
//! | `options`          | array of strings | `[]`    | Options valid at this level only. |
//! | `subcommands`      | table            | `{}`    | Subcommands of this level, keyed by name. |
//! | `aliases`          | array of strings | `[]`    | Subcommand tables only: other names for this subcommand (`docker container ls` and `ps`). |
//! | `complete`         | bool             | `false` | The subcommand list is complete: an unknown word in subcommand position is an error. |
//! | `options-complete` | bool             | `false` | The option list is complete: an unknown option is an error. |
//! | `options-first`    | bool             | `false` | Options are recognised only before the first plain argument; later words are plain (`docker run IMAGE CMD -x`). |
//! | `abbreviations`    | bool             | `false` | A unique prefix of a subcommand name or alias selects that subcommand (`npm inst`). |
//!
//! Levels inherit nothing from their parent except `common-options`. Options declared in a
//! level's `options` apply only at that level, so `git -C dir` is an option before the
//! subcommand and unknown after it.
//!
//! ## Option syntax
//!
//! Each entry in `options` and `common-options` is one spelling of one option:
//!
//! - `-v`, `--verbose`, `-name`: a flag that takes no value.
//! - `-m=`, `--message=`: an option with a required value. The value is accepted attached
//!   (`--message=text`, and for single-letter options `-mtext`) or as the next word
//!   (`--message text`, `-m text`). The value word is a plain argument.
//! - `-S=?`, `--color=?`: an option with an optional value, accepted only attached
//!   (`--color=always`, `-Skey`). The next word is never consumed.
//! - `+*`, `--no-*`: a pattern. A trailing `*` matches every word that starts with the text
//!   before it. A pattern takes no separate value. This is the only way a word that does not
//!   start with `-` is an option (`cargo +nightly build`).
//!
//! Single-letter options bundle: `-av` is `-a -v` when every letter is a declared single-letter
//! option. A letter that takes a value ends the bundle; the rest of the word, or else the next
//! word, is its value (`-xzf archive.tar`).
//!
//! # Classification
//!
//! [`CommandSpec::classify_args`] walks the arguments in order, starting at the top level:
//!
//! - `--` is an option and ends option parsing; every later word is plain. At an
//!   `options-first` level, every word after the first plain argument is plain, `--` included.
//! - A word that is just `-` is plain, unless `-` is declared as an option.
//! - A word starting with `-` (or matching a pattern) is a known option, or an unknown option.
//!   Unknown options are errors only where the level has `options-complete = true`. A word of
//!   the form `-<digits>` is never an error.
//! - The first plain word at a level is the subcommand position. A known subcommand name or
//!   alias is highlighted as a subcommand and classification continues at that subcommand's
//!   level. An unknown word there is an error only where the level has subcommands and
//!   `complete = true`. A word containing an expansion is never an error.
//! - After the first plain word at a level, no further subcommand matching happens there.
//!
//! # Precommands
//!
//! For a spec with `precommand = true`, [`CommandSpec::wrapped_command`] finds the wrapped
//! command: it skips the precommand's options and their values (unknown options are assumed to
//! be flags), then `NAME=value` words when `skip-assignments = true`, then `positional` plain
//! words. The next word is the wrapped command. Option parsing stops at `--` or at the first
//! word that is not an option, as POSIX `getopt` does.
//!
//! # Loading and overriding
//!
//! A user file whose `name` matches a built-in replaces the built-in entirely. With
//! `merge = true` it is merged instead: option lists and aliases are appended (a later spelling
//! of the same option wins), keys present in the user file override, and subcommand tables are
//! merged recursively by name. Nothing can be removed by merging; replace the spec to do that.
//! A file with `merge = true` and no existing spec of that name is loaded as a new spec.
//!
//! A file that fails to parse, has an unknown key, or declares an invalid option is skipped
//! with a warning; the other files still load.

mod file;

#[cfg(test)]
mod tests;

use crate::token::TokenKind;
use file::SpecFile;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, OnceLock};

/// The spec for one command or subcommand.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandSpec {
    name: String,
    precommand: bool,
    skip_assignments: bool,
    positional: usize,
    complete: bool,
    options_complete: bool,
    options_first: bool,
    abbreviations: bool,
    options: OptionTable,
    subcommands: Vec<CommandSpec>,
    /// Subcommand names and aliases, mapped to indexes into `subcommands`.
    subcommand_names: HashMap<String, usize>,
}

/// Whether an option takes a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arity {
    Flag,
    Required,
    Optional,
}

/// The options valid at one level.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct OptionTable {
    /// Every declared spelling, including single-letter ones.
    exact: HashMap<String, Arity>,
    /// Single-dash single-letter options, for bundles like `-av`.
    short: HashMap<char, Arity>,
    /// `PREFIX*` patterns.
    prefixes: Vec<String>,
}

/// The result of matching one word against an [`OptionTable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptionMatch {
    /// A known option whose value, if any, is inside the word.
    Known,
    /// A known option whose value is the next word.
    KnownTakesNext,
    Unknown,
}

impl OptionTable {
    /// True when `word` is option-shaped at this level: it starts with `-` (other than a bare
    /// `-`, unless declared) or matches a pattern.
    fn is_option_word(&self, word: &str) -> bool {
        if word == "-" {
            return self.exact.contains_key(word);
        }
        word.starts_with('-') || self.prefixes.iter().any(|p| word.starts_with(p.as_str()))
    }

    fn match_word(&self, word: &str) -> OptionMatch {
        if let Some(&arity) = self.exact.get(word) {
            return if arity == Arity::Required {
                OptionMatch::KnownTakesNext
            } else {
                OptionMatch::Known
            };
        }
        if self.prefixes.iter().any(|p| word.starts_with(p.as_str())) {
            return OptionMatch::Known;
        }
        if let Some((name, _)) = word.split_once('=')
            && matches!(
                self.exact.get(name),
                Some(Arity::Required | Arity::Optional)
            )
        {
            return OptionMatch::Known;
        }
        if let Some(bundle) = word.strip_prefix('-')
            && !bundle.starts_with('-')
            && !bundle.is_empty()
        {
            for (i, c) in bundle.char_indices() {
                match self.short.get(&c) {
                    Some(Arity::Flag) => {}
                    Some(Arity::Optional) => return OptionMatch::Known,
                    Some(Arity::Required) => {
                        return if i + c.len_utf8() < bundle.len() {
                            OptionMatch::Known
                        } else {
                            OptionMatch::KnownTakesNext
                        };
                    }
                    None => return OptionMatch::Unknown,
                }
            }
            return OptionMatch::Known;
        }
        OptionMatch::Unknown
    }
}

/// One argument word as the classifier sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgInput<'a> {
    /// The word's literal value after quote removal, or `None` when it contains expansions.
    pub literal: Option<&'a str>,
}

/// All loaded specs, keyed by command name. Cloning is cheap: the table is shared.
#[derive(Debug, Clone, Default)]
pub struct SpecRegistry {
    specs: Arc<HashMap<String, Arc<CommandSpec>>>,
}

/// The built-in spec files, embedded at compile time.
const BUILTIN_FILES: &[(&str, &str)] = &[
    ("git.toml", include_str!("../../specs/git.toml")),
    ("cargo.toml", include_str!("../../specs/cargo.toml")),
    ("docker.toml", include_str!("../../specs/docker.toml")),
    ("kubectl.toml", include_str!("../../specs/kubectl.toml")),
    ("systemctl.toml", include_str!("../../specs/systemctl.toml")),
    ("npm.toml", include_str!("../../specs/npm.toml")),
    ("sudo.toml", include_str!("../../specs/sudo.toml")),
    ("doas.toml", include_str!("../../specs/doas.toml")),
    ("env.toml", include_str!("../../specs/env.toml")),
    ("nice.toml", include_str!("../../specs/nice.toml")),
    ("nohup.toml", include_str!("../../specs/nohup.toml")),
    ("time.toml", include_str!("../../specs/time.toml")),
    ("timeout.toml", include_str!("../../specs/timeout.toml")),
    ("stdbuf.toml", include_str!("../../specs/stdbuf.toml")),
    ("ionice.toml", include_str!("../../specs/ionice.toml")),
    ("chrt.toml", include_str!("../../specs/chrt.toml")),
    ("taskset.toml", include_str!("../../specs/taskset.toml")),
    ("noglob.toml", include_str!("../../specs/noglob.toml")),
    ("nocorrect.toml", include_str!("../../specs/nocorrect.toml")),
    ("exec.toml", include_str!("../../specs/exec.toml")),
    ("command.toml", include_str!("../../specs/command.toml")),
    ("builtin.toml", include_str!("../../specs/builtin.toml")),
    ("dash.toml", include_str!("../../specs/dash.toml")),
];

/// The parsed built-ins, kept in source form so user files can merge into them.
struct Builtins {
    files: Vec<SpecFile>,
    registry: SpecRegistry,
    warnings: Vec<String>,
}

fn builtins() -> &'static Builtins {
    static BUILTINS: OnceLock<Builtins> = OnceLock::new();
    BUILTINS.get_or_init(|| {
        let mut files = Vec::with_capacity(BUILTIN_FILES.len());
        let mut warnings = Vec::new();
        for (file_name, text) in BUILTIN_FILES {
            match SpecFile::parse(text).and_then(|f| f.compile().map(|_| f)) {
                Ok(f) => files.push(f),
                Err(e) => warnings.push(format!("built-in spec {file_name}: {e}")),
            }
        }
        let by_name = files.iter().map(|f| (f.name.clone(), f.clone())).collect();
        let registry = SpecRegistry::from_files(&by_name, &mut warnings);
        Builtins {
            files,
            registry,
            warnings,
        }
    })
}

impl SpecRegistry {
    /// The specs compiled into the binary. Parsed once per process; later calls are cheap.
    pub fn builtin() -> SpecRegistry {
        builtins().registry.clone()
    }

    /// Built-in specs overlaid with `<config_dir>/specs/*.toml`. Returns warnings for files that
    /// failed to load; a bad file never prevents the others from loading.
    pub fn load(config_dir: &Path) -> (SpecRegistry, Vec<String>) {
        let builtins = builtins();
        let mut warnings = builtins.warnings.clone();
        let dir = config_dir.join("specs");
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return (builtins.registry.clone(), warnings);
            }
            Err(e) => {
                warnings.push(format!("{}: {e}", dir.display()));
                return (builtins.registry.clone(), warnings);
            }
        };
        let mut paths: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "toml") && p.is_file())
            .collect();
        paths.sort();

        let mut by_name: BTreeMap<String, SpecFile> = builtins
            .files
            .iter()
            .map(|f| (f.name.clone(), f.clone()))
            .collect();
        let mut changed = false;
        for path in paths {
            let parsed = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|text| SpecFile::parse(&text))
                .and_then(|f| f.compile().map(|_| f));
            let user = match parsed {
                Ok(f) => f,
                Err(e) => {
                    warnings.push(format!("{}: {e}", path.display()));
                    continue;
                }
            };
            changed = true;
            match by_name.get_mut(&user.name) {
                Some(existing) if user.merge => existing.merge_from(user),
                _ => {
                    by_name.insert(user.name.clone(), user);
                }
            }
        }
        if !changed {
            return (builtins.registry.clone(), warnings);
        }
        let registry = SpecRegistry::from_files(&by_name, &mut warnings);
        (registry, warnings)
    }

    /// Compiles source specs into a registry. Command names take precedence over aliases, and
    /// among aliases the first in name order wins.
    fn from_files(files: &BTreeMap<String, SpecFile>, warnings: &mut Vec<String>) -> SpecRegistry {
        let mut specs = HashMap::with_capacity(files.len() * 2);
        let mut compiled = Vec::with_capacity(files.len());
        for (name, f) in files {
            match f.compile() {
                Ok(spec) => {
                    let spec = Arc::new(spec);
                    specs.insert(name.clone(), Arc::clone(&spec));
                    compiled.push((f, spec));
                }
                Err(e) => warnings.push(format!("spec {name:?}: {e}")),
            }
        }
        for (f, spec) in compiled {
            for alias in f.aliases() {
                specs
                    .entry(alias.clone())
                    .or_insert_with(|| Arc::clone(&spec));
            }
        }
        SpecRegistry {
            specs: Arc::new(specs),
        }
    }

    pub fn get(&self, command: &str) -> Option<&CommandSpec> {
        self.specs.get(command).map(Arc::as_ref)
    }
}

impl CommandSpec {
    /// The command or subcommand name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// True when this spec describes a precommand (`sudo`, `env`, `nice`, ...).
    pub fn is_precommand(&self) -> bool {
        self.precommand
    }

    /// The subcommand selected by `word` at this level: an exact name or alias, or with
    /// `abbreviations` a prefix of exactly one name or alias.
    fn subcommand(&self, word: &str) -> Option<&CommandSpec> {
        if let Some(&i) = self.subcommand_names.get(word) {
            return Some(&self.subcommands[i]);
        }
        if !self.abbreviations || word.is_empty() {
            return None;
        }
        let mut found = None;
        for (name, &i) in &self.subcommand_names {
            if name.starts_with(word) {
                if found.is_some() {
                    return None;
                }
                found = Some(i);
            }
        }
        found.map(|i| &self.subcommands[i])
    }

    /// For a precommand spec, the index into `args` of the wrapped command word, after skipping
    /// the precommand's own options, option arguments, and (for `env`) `NAME=value` words.
    /// `None` when no wrapped command is present yet.
    pub fn wrapped_command(&self, args: &[ArgInput<'_>]) -> Option<usize> {
        let mut positional = self.positional;
        let mut in_options = true;
        let mut i = 0;
        while i < args.len() {
            let Some(word) = args[i].literal else {
                if positional == 0 {
                    return Some(i);
                }
                positional -= 1;
                in_options = false;
                i += 1;
                continue;
            };
            if in_options {
                if word == "--" {
                    in_options = false;
                    i += 1;
                    continue;
                }
                if self.options.is_option_word(word) {
                    let takes_next = self.options.match_word(word) == OptionMatch::KnownTakesNext;
                    i += if takes_next { 2 } else { 1 };
                    continue;
                }
            }
            in_options = false;
            if self.skip_assignments && positional == self.positional && is_assignment(word) {
                i += 1;
            } else if positional > 0 {
                positional -= 1;
                i += 1;
            } else {
                return Some(i);
            }
        }
        None
    }

    /// Classifies each argument of a non-precommand: `Some(Subcommand)`, `Some(CmdOption)`,
    /// `Some(Error)` (unknown subcommand or option where the spec is complete), or `None` for a
    /// plain argument. The result has the same length as `args`.
    ///
    /// For a precommand, pass only the arguments before [`CommandSpec::wrapped_command`] to
    /// classify the precommand's own options.
    pub fn classify_args(&self, args: &[ArgInput<'_>]) -> Vec<Option<TokenKind>> {
        let mut out = vec![None; args.len()];
        let mut level = self;
        let mut positional_seen = false;
        let mut i = 0;
        while i < args.len() {
            let Some(word) = args[i].literal else {
                positional_seen = true;
                i += 1;
                continue;
            };
            if level.options_first && positional_seen {
                break;
            }
            if word == "--" {
                out[i] = Some(TokenKind::CmdOption);
                break;
            }
            if level.options.is_option_word(word) {
                match level.options.match_word(word) {
                    OptionMatch::Known => out[i] = Some(TokenKind::CmdOption),
                    OptionMatch::KnownTakesNext => {
                        out[i] = Some(TokenKind::CmdOption);
                        i += 1;
                    }
                    OptionMatch::Unknown => {
                        if level.options_complete && !is_negative_number(word) {
                            out[i] = Some(TokenKind::Error);
                        }
                    }
                }
                i += 1;
                continue;
            }
            if !positional_seen && !level.subcommands.is_empty() && word != "-" {
                if let Some(sub) = level.subcommand(word) {
                    out[i] = Some(TokenKind::Subcommand);
                    level = sub;
                    i += 1;
                    continue;
                }
                if level.complete {
                    out[i] = Some(TokenKind::Error);
                }
            }
            positional_seen = true;
            i += 1;
        }
        out
    }
}

/// `NAME=value` with `NAME` a shell identifier.
fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `-` followed by one or more ASCII digits.
fn is_negative_number(word: &str) -> bool {
    word.strip_prefix('-')
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}
