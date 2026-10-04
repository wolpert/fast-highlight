//! The on-disk spec format: serde structs, merging, and compilation into [`CommandSpec`].

use super::{Arity, CommandSpec, OptionTable};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};

/// One spec file as written. Scalar keys are `Option` so that `merge = true` can tell an absent
/// key from an explicit `false`. (`serde(flatten)` cannot be combined with
/// `deny_unknown_fields`, so the level keys are repeated here and in [`SubcommandFile`].)
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct TopFile {
    name: String,
    #[serde(default)]
    merge: bool,
    #[serde(default)]
    aliases: Vec<String>,
    precommand: Option<bool>,
    skip_assignments: Option<bool>,
    positional: Option<usize>,
    #[serde(default)]
    common_options: Vec<String>,
    complete: Option<bool>,
    options_complete: Option<bool>,
    options_first: Option<bool>,
    abbreviations: Option<bool>,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    subcommands: BTreeMap<String, SubcommandFile>,
}

/// A `[subcommands.NAME]` table as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct SubcommandFile {
    #[serde(default)]
    aliases: Vec<String>,
    complete: Option<bool>,
    options_complete: Option<bool>,
    options_first: Option<bool>,
    abbreviations: Option<bool>,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    subcommands: BTreeMap<String, SubcommandFile>,
}

/// A parsed but not yet compiled spec file. Kept around so user files can merge into built-ins.
#[derive(Debug, Clone)]
pub(super) struct SpecFile {
    pub(super) name: String,
    pub(super) merge: bool,
    aliases: Vec<String>,
    precommand: Option<bool>,
    skip_assignments: Option<bool>,
    positional: Option<usize>,
    common_options: Vec<String>,
    level: Level,
}

/// The keys shared by the top level and every subcommand table.
#[derive(Debug, Clone, Default)]
struct Level {
    aliases: Vec<String>,
    complete: Option<bool>,
    options_complete: Option<bool>,
    options_first: Option<bool>,
    abbreviations: Option<bool>,
    options: Vec<String>,
    subcommands: BTreeMap<String, Level>,
}

impl From<SubcommandFile> for Level {
    fn from(f: SubcommandFile) -> Level {
        Level {
            aliases: f.aliases,
            complete: f.complete,
            options_complete: f.options_complete,
            options_first: f.options_first,
            abbreviations: f.abbreviations,
            options: f.options,
            subcommands: f
                .subcommands
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
        }
    }
}

impl SpecFile {
    /// Parses TOML text. The error is the TOML or schema error message.
    pub(super) fn parse(text: &str) -> Result<SpecFile, String> {
        let f: TopFile = toml::from_str(text).map_err(|e| e.to_string().trim_end().to_string())?;
        Ok(SpecFile {
            name: f.name,
            merge: f.merge,
            aliases: f.aliases,
            precommand: f.precommand,
            skip_assignments: f.skip_assignments,
            positional: f.positional,
            common_options: f.common_options,
            level: Level {
                aliases: Vec::new(),
                complete: f.complete,
                options_complete: f.options_complete,
                options_first: f.options_first,
                abbreviations: f.abbreviations,
                options: f.options,
                subcommands: f
                    .subcommands
                    .into_iter()
                    .map(|(k, v)| (k, v.into()))
                    .collect(),
            },
        })
    }

    /// The top-level command aliases.
    pub(super) fn aliases(&self) -> &[String] {
        &self.aliases
    }

    /// Merges `other` into `self`: lists are extended (later entries win on conflict), scalar
    /// keys present in `other` override, and subcommands are merged recursively by name.
    pub(super) fn merge_from(&mut self, other: SpecFile) {
        self.aliases.extend(other.aliases);
        self.precommand = other.precommand.or(self.precommand);
        self.skip_assignments = other.skip_assignments.or(self.skip_assignments);
        self.positional = other.positional.or(self.positional);
        self.common_options.extend(other.common_options);
        self.level.merge_from(other.level);
    }

    /// Checks and compiles the file into a spec.
    pub(super) fn compile(&self) -> Result<CommandSpec, String> {
        if self.name.is_empty() {
            return Err("`name` must not be empty".to_string());
        }
        if self.aliases.iter().any(String::is_empty) {
            return Err("an entry in `aliases` is empty".to_string());
        }
        let common = parse_options(&self.common_options, "common-options")?;
        let mut spec = compile_level(&self.name, &self.name, &self.level, &common)?;
        spec.precommand = self.precommand.unwrap_or(false);
        spec.skip_assignments = self.skip_assignments.unwrap_or(false);
        spec.positional = self.positional.unwrap_or(0);
        Ok(spec)
    }
}

impl Level {
    fn merge_from(&mut self, other: Level) {
        self.aliases.extend(other.aliases);
        self.complete = other.complete.or(self.complete);
        self.options_complete = other.options_complete.or(self.options_complete);
        self.options_first = other.options_first.or(self.options_first);
        self.abbreviations = other.abbreviations.or(self.abbreviations);
        self.options.extend(other.options);
        for (name, sub) in other.subcommands {
            match self.subcommands.get_mut(&name) {
                Some(existing) => existing.merge_from(sub),
                None => {
                    self.subcommands.insert(name, sub);
                }
            }
        }
    }
}

/// A parsed option declaration.
enum OptionDecl {
    /// `NAME`, `NAME=`, or `NAME=?`.
    Named(String, Arity),
    /// `PREFIX*`: any word starting with `PREFIX`.
    Prefix(String),
}

fn parse_option(decl: &str) -> Result<OptionDecl, String> {
    if let Some(prefix) = decl.strip_suffix('*') {
        if prefix.is_empty() || prefix.contains(['=', '*']) {
            return Err(format!("invalid option pattern {decl:?}"));
        }
        return Ok(OptionDecl::Prefix(prefix.to_string()));
    }
    let (name, arity) = if let Some(name) = decl.strip_suffix("=?") {
        (name, Arity::Optional)
    } else if let Some(name) = decl.strip_suffix('=') {
        (name, Arity::Required)
    } else {
        (decl, Arity::Flag)
    };
    if !name.starts_with('-') || name.contains('=') {
        return Err(format!(
            "invalid option {decl:?} (an option starts with `-` and may end in `=` or `=?`)"
        ));
    }
    if name == "--" {
        return Err("`--` always ends options and cannot be declared".to_string());
    }
    Ok(OptionDecl::Named(name.to_string(), arity))
}

fn parse_options(decls: &[String], context: &str) -> Result<Vec<OptionDecl>, String> {
    decls
        .iter()
        .map(|d| parse_option(d).map_err(|e| format!("{e} in {context}")))
        .collect()
}

fn add_option(table: &mut OptionTable, decl: &OptionDecl) {
    match decl {
        OptionDecl::Named(name, arity) => {
            let mut chars = name.chars();
            if let (Some('-'), Some(c), None) = (chars.next(), chars.next(), chars.next())
                && c != '-'
            {
                table.short.insert(c, *arity);
            }
            table.exact.insert(name.clone(), *arity);
        }
        OptionDecl::Prefix(prefix) => {
            if !table.prefixes.contains(prefix) {
                table.prefixes.push(prefix.clone());
            }
        }
    }
}

/// Compiles one level. `path` is the command path (`git remote add`) used in error messages.
fn compile_level(
    path: &str,
    name: &str,
    level: &Level,
    common: &[OptionDecl],
) -> Result<CommandSpec, String> {
    let mut options = OptionTable::default();
    for decl in common {
        add_option(&mut options, decl);
    }
    for decl in parse_options(&level.options, &format!("the options of `{path}`"))? {
        add_option(&mut options, &decl);
    }

    let mut subcommands = Vec::with_capacity(level.subcommands.len());
    let mut subcommand_names = HashMap::with_capacity(level.subcommands.len());
    for (sub_name, sub) in &level.subcommands {
        if sub_name.is_empty() {
            return Err(format!("a subcommand of `{path}` has an empty name"));
        }
        let sub_path = format!("{path} {sub_name}");
        if sub.aliases.iter().any(String::is_empty) {
            return Err(format!("an entry in the aliases of `{sub_path}` is empty"));
        }
        subcommand_names.insert(sub_name.clone(), subcommands.len());
        subcommands.push(compile_level(&sub_path, sub_name, sub, common)?);
    }
    // Aliases never shadow a real subcommand name or an earlier alias.
    for (i, sub) in level.subcommands.values().enumerate() {
        for alias in &sub.aliases {
            subcommand_names.entry(alias.clone()).or_insert(i);
        }
    }

    Ok(CommandSpec {
        name: name.to_string(),
        precommand: false,
        skip_assignments: false,
        positional: 0,
        complete: level.complete.unwrap_or(false),
        options_complete: level.options_complete.unwrap_or(false),
        options_first: level.options_first.unwrap_or(false),
        abbreviations: level.abbreviations.unwrap_or(false),
        options,
        subcommands,
        subcommand_names,
    })
}
