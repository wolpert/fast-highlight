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
//! | `precommand`       | bool             | `false` | The command runs another command (`sudo`, `env`, `nice`). The top level wraps with `positional = 0` unless it sets `positional` or `wrapped-after`. |
//! | `skip-assignments` | bool             | `false` | When the top level wraps: `NAME=value` words before the wrapped command are skipped (`env`), also when the value contains an expansion (`FOO=$x`). |
//!
//! The top level also accepts every level key below.
//!
//! ## Level keys
//!
//! These keys are valid at the top level and in every `[subcommands.NAME]` table. Subcommand
//! tables nest to any depth: `[subcommands.remote.subcommands.add]`.
//!
//! | Key                    | Type             | Default | Meaning |
//! |------------------------|------------------|---------|---------|
//! | `options`              | array of strings | `[]`    | Options valid at this level only. |
//! | `subcommands`          | table            | `{}`    | Subcommands of this level, keyed by name. |
//! | `aliases`              | array of strings | `[]`    | Subcommand tables only: other names for this subcommand (`docker container ls` and `ps`). |
//! | `complete`             | bool             | `false` | The subcommand list is complete: an unknown word in subcommand position is an error. |
//! | `options-complete`     | bool             | `false` | The option list is complete: an unknown option is an error. |
//! | `options-first`        | bool             | `false` | Options are recognised only before the first plain argument; later words are plain (`docker run IMAGE CMD -x`). |
//! | `abbreviations`        | bool             | `false` | A unique prefix of a subcommand name or alias selects that subcommand (`npm inst`). For options, see `option-abbreviations`. |
//! | `option-abbreviations` | bool             | `false` | A unique prefix of a declared long option selects that option (`--verb` for `--verbose`), as GNU `getopt_long` does. For subcommands, see `abbreviations`. |
//! | `positional`           | integer          | unset   | The level wraps a command: the number of plain arguments between the options and the wrapped command (`timeout DURATION cmd`). |
//! | `wrapped-after`        | string           | unset   | The level wraps the command after the first `--` (`kubectl exec POD -- cmd`). `"--"` is the only accepted value. |
//! | `path-mode-options`    | array of strings | `[]`    | Options of this level that make the remaining words files rather than a command (`sudo -e FILE`). |
//!
//! A level sets at most one of `positional` and `wrapped-after`. See
//! [Wrapped commands and path mode](#wrapped-commands-and-path-mode).
//!
//! Levels inherit nothing from their parent except `common-options` and
//! `option-abbreviations`: a level that leaves `option-abbreviations` unset uses its parent's
//! setting, and an explicit value at a level applies to it and to its descendants that leave it
//! unset. Options declared in a level's `options` apply only at that level, so `git -C dir` is
//! an option before the subcommand and unknown after it.
//!
//! ## Option syntax
//!
//! Each entry in `options`, `common-options`, and `path-mode-options` is one spelling of one
//! option:
//!
//! - `-v`, `--verbose`, `-name`: a flag that takes no value.
//! - `-m=`, `--message=`: an option with a required value. The value is accepted attached
//!   (`--message=text`, and for single-letter options `-mtext`) or as the next word
//!   (`--message text`, `-m text`). The value word is a plain argument.
//! - `-S=?`, `--color=?`: an option with an optional value, accepted only attached
//!   (`--color=always`, `-Skey`). The next word is never consumed.
//! - `+*`, `--no-*`: a pattern. A trailing `*` matches every word that starts with the text
//!   before it. A pattern takes no separate value. This is the only way a word that does not
//!   start with `-` is an option (`cargo +nightly build`). `path-mode-options` takes no
//!   patterns.
//!
//! Single-letter options bundle: `-av` is `-a -v` when every letter is a declared single-letter
//! option. A letter that takes a value ends the bundle; the rest of the word, or else the next
//! word, is its value (`-xzf archive.tar`).
//!
//! ## Long-option abbreviations
//!
//! With `option-abbreviations = true`, a word starting with `--` that is not a declared spelling
//! is matched by its stem, the text before the first `=`. The stem selects an option when it is
//! a proper prefix of exactly one declared spelling that starts with `--`, and the word then
//! takes that option's arity: `--out file` consumes `file` for `--output=`, `--out=file` is
//! complete, and `--verb=1` is unknown for the flag `--verbose`.
//!
//! - Only long options abbreviate; a single-dash word never does.
//! - Only declared spellings count, and patterns are never candidates. A spec that sets the key
//!   should declare every long option of the command, or a prefix the command finds ambiguous
//!   resolves to the one declared option.
//! - An exact spelling beats a prefix: `--exclude` is `--exclude` although `--exclude-caches`
//!   is declared too, and `--flag=x` for a declared flag `--flag` is unknown.
//! - The rule is strict: a stem that matches more than one spelling is ambiguous, even when the
//!   spellings are aliases of one option with the same arity (`--colo` for `--color` and
//!   `--colour`). `getopt_long` accepts such a prefix.
//! - An ambiguous stem, a stem that matches nothing, and a bare `--=...` are unknown options.
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
//!   the form `-<digits>` is never an error. With `option-abbreviations`, a unique prefix of a
//!   long option is that option, and an ambiguous or unmatched prefix is an unknown option.
//! - The first plain word at a level is the subcommand position. A known subcommand name or
//!   alias is highlighted as a subcommand and classification continues at that subcommand's
//!   level. An unknown word there is an error only where the level has subcommands and
//!   `complete = true`. A word containing an expansion is never an error.
//! - After the first plain word at a level, no further subcommand matching happens there.
//!
//! # Wrapped commands and path mode
//!
//! A level wraps a command when it sets `positional` or `wrapped-after`. The top level of a
//! `precommand = true` spec wraps with `positional = 0` unless it sets one of them. A top level
//! that wraps without `precommand` gets no precommand span; before `positional` became a level
//! key, a top-level `positional` without `precommand` was ignored, and now it wraps.
//! [`CommandSpec::tail`] finds the wrapped command:
//!
//! - It descends through subcommands as [`CommandSpec::classify_args`] does, and only the level
//!   it ends at counts. When that level does not wrap, there is no wrapped command.
//! - `positional = N`: the level's options and their values are skipped (unknown options,
//!   including ambiguous abbreviations, are assumed to be flags), then `NAME=value` words when
//!   `skip-assignments = true` and the level is the top level, then `N` plain words. The next
//!   word is the wrapped command. Option parsing stops at `--` or at the first word that is not
//!   an option, as POSIX `getopt` does.
//! - `wrapped-after = "--"`: options are parsed as in classification, and the word after the
//!   first `--` is the wrapped command, after `NAME=value` words when `skip-assignments = true`
//!   and the level is the top level. With no `--`, or nothing after it, there is no wrapped
//!   command. At an `options-first` level `--` is found only in option position; after a plain
//!   word it is plain.
//! - The wrapped command is classified as a command word in its own right, against the local
//!   command table: a command that exists only in a container or pod is an error. A
//!   precommand's own arguments are classified by its spec and not path-checked; another
//!   command's arguments before the wrapped command are highlighted as usual.
//!
//! ## `builtin` and `command`
//!
//! The specs of `builtin` and `command` decide only whether they are precommands. Their grammar
//! is built into the highlighter (`src/highlight.rs`), keyed by name, and ignores spec
//! overrides:
//!
//! - `builtin` takes no options and wraps its first argument, which must be a builtin. A
//!   precommand builtin (`builtin`, `command`, `exec`, `noglob`, `-`) continues the chain; any
//!   other word, `--` included, is an error.
//! - `command` takes `-p`, `-v`, and `-V`, combined in one word in any order (`-pv`), and `--`
//!   ends them. Several option words are accepted only when one has a `v` or `V` (`-p -v ls`);
//!   otherwise at most one may come before `--`, and a second makes the first the wrapped
//!   command. Any other word, an unknown option included, is the wrapped command. It must be
//!   an external command: a `$PATH` command or a path to an executable file. Functions,
//!   aliases, builtins, and reserved words are errors. An external precommand continues the
//!   chain, so a `$PATH` binary named `command` or `builtin` makes `command command` a
//!   precommand.
//! - With `-v` or `-V`, every remaining word is a lookup, styled by what it names and an error
//!   only when it names nothing. A lookup never continues a chain.
//! - The function and global-alias shadowing of precommands applies to every command word
//!   except the one directly after `builtin` or `command`. A word either rejects wraps
//!   nothing, and its arguments are only checked as paths. A word with an expansion or glob is
//!   never an error, a quoted word is looked up by its value, and no alias is expanded.
//!   `AUTO_CD` does not make a directory valid there.
//! - Global aliases after these words, the default `PATH` search of `command -p`, and
//!   `POSIX_BUILTINS` are not modelled.
//!
//! A word is `NAME=value` when, after quote removal, it starts with an ASCII shell identifier
//! and `=` before any expansion; the value may contain expansions. `FOO=1`, `FOO=$x`, and
//! `"FOO"=$(date)` are skipped; `FOO$x=1` and `$cmd` are not.
//!
//! ## Path mode
//!
//! `path-mode-options` lists options that make the remaining words operands, as `sudo -e FILE`
//! does. Each entry declares the option at its level with the syntax of `options`; an entry
//! also in `options` or `common-options` takes the arity given here. This lets a user file make
//! an existing option select path mode by listing it again; the built-in specs do not repeat a
//! spelling within a level. When such an option is given at the level the descent ends at,
//! while options are still parsed, there is no wrapped command:
//!
//! - The option counts when the word resolves to it: an exact spelling, the name of
//!   `--name=value`, an abbreviation (`--ed` for `--edit` with `option-abbreviations`), or a
//!   letter of a bundle up to and including the first letter that takes a value. `-Ee` and
//!   `-eu root` select the mode; `-ue` does not, because `e` is the value of `-u`. Unknown and
//!   ambiguous options never select it.
//! - The mode is sticky: later options are still parsed (`-u root` consumes `root`) and never
//!   cancel it. `NAME=value` words are not skipped and no plain words are counted.
//! - Every argument is classified and path-checked as for a command that wraps nothing: an
//!   existing file is a path, a missing one gets no span, and words after `--` are operands.
//!   Option values are path-checked too (`sudo -e -u root f` checks `root`).
//! - On a level that does not wrap, `path-mode-options` declares its options and has no other
//!   effect.
//!
//! # Loading and overriding
//!
//! A user file whose `name` matches a built-in replaces the built-in entirely. With
//! `merge = true` it is merged instead: option lists and aliases are appended (a later spelling
//! of the same option wins), keys present in the user file override, and subcommand tables are
//! merged recursively by name. Nothing can be removed by merging; replace the spec to do that.
//! A file with `merge = true` and no existing spec of that name is loaded as a new spec.
//! `path-mode-options` is appended like `options`. `positional` and `wrapped-after` are one
//! setting: a merged level that sets either replaces the level's start and clears the other.
//!
//! A file that fails to parse, has an unknown key, or declares an invalid option is skipped
//! with a warning; the other files still load. So is a file with a level that sets both
//! `positional` and `wrapped-after`, sets `wrapped-after` to anything but `"--"`, or lists a
//! pattern in `path-mode-options`; the warning names the command path of the level. A user file
//! is checked on its own before it is merged, so a `merge = true` file must pass these checks by
//! itself.

mod file;

#[cfg(test)]
mod tests;

use crate::token::TokenKind;
use file::SpecFile;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, OnceLock};

/// The spec for one command or subcommand.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandSpec {
    name: String,
    precommand: bool,
    skip_assignments: bool,
    /// Where this level's wrapped command starts, if it wraps one. See [`CommandSpec::tail`].
    wrap: Option<Wrap>,
    /// This level or a level below it wraps, so [`CommandSpec::tail`] has work to do.
    any_wrap: bool,
    complete: bool,
    options_complete: bool,
    options_first: bool,
    abbreviations: bool,
    options: OptionTable,
    subcommands: Vec<CommandSpec>,
    /// Subcommand names and aliases, mapped to indexes into `subcommands`.
    subcommand_names: HashMap<String, usize>,
}

/// How a level finds its wrapped command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wrap {
    /// `positional = N`: after the options and `N` plain words.
    Positional(usize),
    /// `wrapped-after = "--"`: after the first `--`.
    AfterDashDash,
}

/// What follows a command's own arguments. See [`CommandSpec::tail`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tail {
    /// No wrapped command (yet), and no option selected path mode.
    None,
    /// The index into the arguments of the wrapped command word.
    Command(usize),
    /// An option in `path-mode-options` was given: the remaining words are operands, not a
    /// command (`sudo -e FILE`).
    Paths,
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
    /// `option-abbreviations`: a unique prefix of a declared long option selects it. See
    /// [`OptionTable::match_abbreviation`].
    abbreviate: bool,
    /// Spellings from `path-mode-options`.
    mode: HashSet<String>,
    /// The single-letter options among `mode`, for bundles.
    mode_short: HashSet<char>,
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

/// An [`OptionMatch`], and whether the option it resolved to is in `path-mode-options`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WordMatch {
    kind: OptionMatch,
    mode: bool,
}

impl OptionMatch {
    fn mode(self, mode: bool) -> WordMatch {
        WordMatch { kind: self, mode }
    }
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

    fn is_mode(&self, spelling: &str) -> bool {
        !self.mode.is_empty() && self.mode.contains(spelling)
    }

    /// Matches an option-shaped word. `mode` is set when the option the word resolves to (an
    /// exact spelling, the name of `--name=value`, the spelling an abbreviation selects, or a
    /// letter of a bundle) is a path-mode option. Unknown words and patterns never set it.
    fn match_word(&self, word: &str) -> WordMatch {
        if let Some(&arity) = self.exact.get(word) {
            let kind = if arity == Arity::Required {
                OptionMatch::KnownTakesNext
            } else {
                OptionMatch::Known
            };
            return kind.mode(self.is_mode(word));
        }
        if self.prefixes.iter().any(|p| word.starts_with(p.as_str())) {
            return OptionMatch::Known.mode(false);
        }
        if let Some((name, _)) = word.split_once('=')
            && matches!(
                self.exact.get(name),
                Some(Arity::Required | Arity::Optional)
            )
        {
            return OptionMatch::Known.mode(self.is_mode(name));
        }
        if self.abbreviate && word.starts_with("--") {
            let (kind, spelling) = self.match_abbreviation(word);
            return kind.mode(spelling.is_some_and(|s| self.is_mode(s)));
        }
        if let Some(bundle) = word.strip_prefix('-')
            && !bundle.starts_with('-')
            && !bundle.is_empty()
        {
            // Every letter parsed as an option counts, up to the one that takes a value.
            let mut mode = false;
            for (i, c) in bundle.char_indices() {
                let Some(&arity) = self.short.get(&c) else {
                    return OptionMatch::Unknown.mode(false);
                };
                mode |= !self.mode_short.is_empty() && self.mode_short.contains(&c);
                match arity {
                    Arity::Flag => {}
                    Arity::Optional => return OptionMatch::Known.mode(mode),
                    Arity::Required => {
                        let kind = if i + c.len_utf8() < bundle.len() {
                            OptionMatch::Known
                        } else {
                            OptionMatch::KnownTakesNext
                        };
                        return kind.mode(mode);
                    }
                }
            }
            return OptionMatch::Known.mode(mode);
        }
        OptionMatch::Unknown.mode(false)
    }

    /// Matches `--stem` or `--stem=value`, which missed the exact lookup, as an abbreviation:
    /// the stem must be a proper prefix of exactly one declared long option. More than one
    /// candidate is ambiguous, even when the candidates are aliases of one option. Patterns are
    /// never candidates. A known result comes with the spelling the stem selected.
    fn match_abbreviation(&self, word: &str) -> (OptionMatch, Option<&str>) {
        let (stem, has_value) = match word.split_once('=') {
            Some((stem, _)) => (stem, true),
            None => (word, false),
        };
        // An exact stem was already decided by the exact lookup (`--flag=x` stays unknown).
        if stem.len() <= 2 || self.exact.contains_key(stem) {
            return (OptionMatch::Unknown, None);
        }
        let mut found = None;
        for (name, &arity) in &self.exact {
            if name.starts_with(stem) {
                if found.is_some() {
                    return (OptionMatch::Unknown, None);
                }
                found = Some((name.as_str(), arity));
            }
        }
        match (found, has_value) {
            (None, _) | (Some((_, Arity::Flag)), true) => (OptionMatch::Unknown, None),
            (Some((name, Arity::Required)), false) => (OptionMatch::KnownTakesNext, Some(name)),
            (Some((name, _)), _) => (OptionMatch::Known, Some(name)),
        }
    }
}

/// One argument word as the classifier sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgInput<'a> {
    /// The word's literal value after quote removal, or `None` when it contains expansions.
    pub literal: Option<&'a str>,
    /// The word starts with a literal `NAME=` ([`Word::name_eq`](crate::syntax::Word::name_eq)),
    /// whatever follows: `FOO=1` and `FOO=$x`, not `FOO$x=1`.
    pub name_eq: bool,
}

/// All loaded specs, keyed by command name. Cloning is cheap: the table is shared.
#[derive(Debug, Clone, Default)]
pub struct SpecRegistry {
    specs: Arc<HashMap<String, Arc<CommandSpec>>>,
}

/// The built-in spec files, embedded at compile time. build.rs generates this
/// table from specs/*.toml.
const BUILTIN_FILES: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/builtin_specs.rs"));

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

    /// The index into `args` of the wrapped command word, or `None` when there is none (yet).
    /// The same as [`CommandSpec::tail`] giving [`Tail::Command`].
    pub fn wrapped_command(&self, args: &[ArgInput<'_>]) -> Option<usize> {
        match self.tail(args) {
            Tail::Command(i) => Some(i),
            Tail::None | Tail::Paths => None,
        }
    }

    /// What follows the command's own arguments: a wrapped command, operands selected by a
    /// path-mode option, or neither.
    ///
    /// The subcommand descent is the one [`CommandSpec::classify_args`] makes. Only the level
    /// it ends at counts: its `positional` or `wrapped-after` (implicitly `positional = 0` at
    /// the top level of a precommand) finds the wrapped command, and its `path-mode-options`
    /// select path mode. A path-mode option given while options are still parsed wins over a
    /// wrapped command, whatever follows it.
    pub fn tail(&self, args: &[ArgInput<'_>]) -> Tail {
        if !self.any_wrap {
            return Tail::None;
        }
        let end = self.walk(args, None, None);
        let level = end.level;
        // `skip-assignments` is a top-level key.
        let assignments = self.skip_assignments && std::ptr::eq(level, self);
        match level.wrap {
            None => Tail::None,
            Some(Wrap::Positional(n)) => level.tail_positional(args, end.start, n, assignments),
            Some(Wrap::AfterDashDash) => level.tail_after_dashdash(args, end.start, assignments),
        }
    }

    /// [`CommandSpec::tail`] for `positional = n` at this level, from `args[start]`. Option
    /// parsing stops at `--` or at the first word that is not an option, as POSIX `getopt` does;
    /// unknown options are assumed to be flags. Then `NAME=value` words are skipped when
    /// `assignments` ([`ArgInput::name_eq`]), then `n` plain words.
    fn tail_positional(
        &self,
        args: &[ArgInput<'_>],
        start: usize,
        n: usize,
        assignments: bool,
    ) -> Tail {
        let mut remaining = n;
        let mut in_options = true;
        let mut mode = false;
        let mut i = start;
        while i < args.len() {
            if in_options && let Some(word) = args[i].literal {
                if word == "--" {
                    in_options = false;
                    i += 1;
                    continue;
                }
                if self.options.is_option_word(word) {
                    let m = self.options.match_word(word);
                    mode |= m.mode;
                    i += if m.kind == OptionMatch::KnownTakesNext {
                        2
                    } else {
                        1
                    };
                    continue;
                }
            }
            in_options = false;
            if mode {
                return Tail::Paths;
            }
            if assignments && remaining == n && args[i].name_eq {
                i += 1;
            } else if remaining > 0 {
                remaining -= 1;
                i += 1;
            } else {
                return Tail::Command(i);
            }
        }
        if mode { Tail::Paths } else { Tail::None }
    }

    /// [`CommandSpec::tail`] for `wrapped-after = "--"` at this level, from `args[start]`.
    /// Options are parsed as [`CommandSpec::classify_args`] parses them, so at an
    /// `options-first` level a `--` after a plain word is plain. The word after the first `--`,
    /// and after `NAME=value` words when `assignments`, is the wrapped command.
    fn tail_after_dashdash(&self, args: &[ArgInput<'_>], start: usize, assignments: bool) -> Tail {
        let mut positional_seen = false;
        let mut mode = false;
        let mut i = start;
        while i < args.len() {
            let Some(word) = args[i].literal else {
                positional_seen = true;
                i += 1;
                continue;
            };
            if self.options_first && positional_seen {
                break;
            }
            if word == "--" {
                if mode {
                    return Tail::Paths;
                }
                let mut j = i + 1;
                while assignments && j < args.len() && args[j].name_eq {
                    j += 1;
                }
                return if j < args.len() {
                    Tail::Command(j)
                } else {
                    Tail::None
                };
            }
            if self.options.is_option_word(word) {
                let m = self.options.match_word(word);
                mode |= m.mode;
                i += if m.kind == OptionMatch::KnownTakesNext {
                    2
                } else {
                    1
                };
                continue;
            }
            positional_seen = true;
            i += 1;
        }
        if mode { Tail::Paths } else { Tail::None }
    }

    /// Classifies each argument of a non-precommand: `Some(Subcommand)`, `Some(CmdOption)`,
    /// `Some(Error)` (unknown subcommand or option where the spec is complete), or `None` for a
    /// plain argument. The result has the same length as `args`.
    ///
    /// For a command with a wrapped command ([`CommandSpec::tail`]), pass only the arguments
    /// before it to classify the command's own arguments.
    pub fn classify_args(&self, args: &[ArgInput<'_>]) -> Vec<Option<TokenKind>> {
        let mut out = vec![None; args.len()];
        self.walk(args, Some(&mut out), None);
        out
    }

    /// True when `args[index]` is a literal that is a proper prefix of a subcommand name or
    /// alias (in subcommand position) or of an option spelling or pattern (in option position)
    /// at the level [`CommandSpec::classify_args`] reaches for it. The semantic pass uses this
    /// to avoid flagging a word as an error while the user is still typing it
    /// (`systemctl stat`, `git commit --am`).
    pub fn completes_at(&self, args: &[ArgInput<'_>], index: usize) -> bool {
        let Some(word) = args.get(index).and_then(|a| a.literal) else {
            return false;
        };
        let proper_prefix = |name: &str| name.len() > word.len() && name.starts_with(word);
        let end = self.walk(args, None, Some(index));
        let level = end.level;
        match end.stopped {
            Some(Slot::Option) => {
                level.options.exact.keys().any(|n| proper_prefix(n))
                    || level.options.prefixes.iter().any(|p| proper_prefix(p))
            }
            Some(Slot::Subcommand) => level.subcommand_names.keys().any(|n| proper_prefix(n)),
            None => false,
        }
    }

    /// Classifies `args` into `out` (same length) when it is given, and returns the level the
    /// walk ends at. When `stop` is `Some(i)` and the walk reaches `args[i]` as an option-shaped
    /// word or in subcommand position, returns the level and slot there without classifying
    /// further.
    fn walk<'s>(
        &'s self,
        args: &[ArgInput<'_>],
        mut out: Option<&mut [Option<TokenKind>]>,
        stop: Option<usize>,
    ) -> WalkEnd<'s> {
        let mut mark = |i: usize, kind: TokenKind| {
            if let Some(out) = out.as_deref_mut() {
                out[i] = Some(kind);
            }
        };
        let mut level = self;
        let mut start = 0;
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
                mark(i, TokenKind::CmdOption);
                break;
            }
            if level.options.is_option_word(word) {
                if stop == Some(i) {
                    return WalkEnd::stopped(level, start, Slot::Option);
                }
                match level.options.match_word(word).kind {
                    OptionMatch::Known => mark(i, TokenKind::CmdOption),
                    OptionMatch::KnownTakesNext => {
                        mark(i, TokenKind::CmdOption);
                        i += 1;
                    }
                    OptionMatch::Unknown => {
                        if level.options_complete && !is_negative_number(word) {
                            mark(i, TokenKind::Error);
                        }
                    }
                }
                i += 1;
                continue;
            }
            if !positional_seen && !level.subcommands.is_empty() && word != "-" {
                if stop == Some(i) {
                    return WalkEnd::stopped(level, start, Slot::Subcommand);
                }
                if let Some(sub) = level.subcommand(word) {
                    mark(i, TokenKind::Subcommand);
                    level = sub;
                    i += 1;
                    start = i;
                    continue;
                }
                if level.complete {
                    mark(i, TokenKind::Error);
                }
            }
            if stop == Some(i) {
                break;
            }
            positional_seen = true;
            i += 1;
        }
        WalkEnd {
            level,
            start,
            stopped: None,
        }
    }
}

/// Where [`CommandSpec::walk`] ended.
struct WalkEnd<'s> {
    /// The deepest subcommand level reached.
    level: &'s CommandSpec,
    /// The index of the first argument at `level`: just after its subcommand word, or 0 for the
    /// top level.
    start: usize,
    /// The slot of the `stop` word, when the walk stopped there as an option or subcommand.
    stopped: Option<Slot>,
}

impl<'s> WalkEnd<'s> {
    fn stopped(level: &'s CommandSpec, start: usize, slot: Slot) -> WalkEnd<'s> {
        WalkEnd {
            level,
            start,
            stopped: Some(slot),
        }
    }
}

/// Where [`CommandSpec::walk`] met the word it stopped at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Option,
    Subcommand,
}

/// `-` followed by one or more ASCII digits.
fn is_negative_number(word: &str) -> bool {
    word.strip_prefix('-')
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}
