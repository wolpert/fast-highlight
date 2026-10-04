//! The daemon's copy of the shell's command namespace, plus its own `$PATH` scan.
//!
//! The zsh side sends aliases, functions, builtins, reserved words, and named directories with
//! state requests. The daemon scans `$PATH` itself and keeps the executable names in a cache
//! that it rebuilds when `PATH` changes or when the modification time of a listed directory
//! changes (checked at most once per [`PATH_RECHECK_INTERVAL`]).

use crate::protocol::StateUpdate;
use std::collections::{BTreeSet, HashMap};
use std::ops::Bound;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// What a command word resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandClass {
    Alias,
    SuffixAlias,
    GlobalAlias,
    Function,
    Builtin,
    ReservedWord,
    /// Found in `$PATH`, or an explicit path to an executable file.
    External,
    Unknown,
}

/// How often the `$PATH` directories' modification times are compared with the last scan.
pub const PATH_RECHECK_INTERVAL: Duration = Duration::from_secs(1);

/// zsh reserved words, used until the shell sends its own list.
const DEFAULT_RESERVED_WORDS: &[&str] = &[
    "!",
    "[[",
    "{",
    "}",
    "case",
    "coproc",
    "declare",
    "do",
    "done",
    "elif",
    "else",
    "end",
    "esac",
    "export",
    "fi",
    "float",
    "for",
    "foreach",
    "function",
    "if",
    "integer",
    "local",
    "nocorrect",
    "readonly",
    "repeat",
    "select",
    "then",
    "time",
    "typeset",
    "until",
    "while",
];

/// zsh builtins (core plus the modules an interactive shell almost always loads), used until
/// the shell sends its own list.
const DEFAULT_BUILTINS: &[&str] = &[
    "-",
    ".",
    ":",
    "[",
    "alias",
    "autoload",
    "bg",
    "bindkey",
    "break",
    "builtin",
    "bye",
    "cd",
    "chdir",
    "command",
    "compadd",
    "comparguments",
    "compcall",
    "compctl",
    "compdescribe",
    "compfiles",
    "compgroups",
    "compquote",
    "comptags",
    "comptry",
    "compvalues",
    "continue",
    "declare",
    "dirs",
    "disable",
    "disown",
    "echo",
    "echotc",
    "echoti",
    "emulate",
    "enable",
    "eval",
    "exec",
    "exit",
    "export",
    "false",
    "fc",
    "fg",
    "float",
    "functions",
    "getln",
    "getopts",
    "hash",
    "history",
    "integer",
    "jobs",
    "kill",
    "let",
    "limit",
    "local",
    "logout",
    "noglob",
    "popd",
    "print",
    "printf",
    "pushd",
    "pushln",
    "pwd",
    "r",
    "read",
    "readonly",
    "rehash",
    "return",
    "sched",
    "set",
    "setopt",
    "shift",
    "source",
    "suspend",
    "test",
    "times",
    "trap",
    "true",
    "ttyctl",
    "type",
    "typeset",
    "ulimit",
    "umask",
    "unalias",
    "unfunction",
    "unhash",
    "unlimit",
    "unset",
    "unsetopt",
    "vared",
    "wait",
    "whence",
    "where",
    "which",
    "zcompile",
    "zformat",
    "zle",
    "zmodload",
    "zparseopts",
    "zregexparse",
    "zstyle",
];

/// One directory of the `$PATH` scan and the modification time it had when it was read.
#[derive(Debug, Clone)]
struct ScannedDir {
    dir: PathBuf,
    /// `None` when the directory did not exist (or could not be read) at scan time.
    mtime: Option<SystemTime>,
}

/// The executable names found in `$PATH`.
#[derive(Debug, Default)]
struct PathCache {
    /// The `PATH` value the cache describes.
    path: String,
    /// The cache must be rebuilt before the next lookup (PATH changed or was never scanned).
    dirty: bool,
    dirs: Vec<ScannedDir>,
    commands: BTreeSet<String>,
    /// When the directory mtimes were last compared (or the last scan finished).
    last_check: Option<Instant>,
}

impl PathCache {
    fn set_path(&mut self, path: String) {
        if path != self.path || self.last_check.is_none() {
            self.path = path;
            self.dirty = true;
        }
    }

    /// Rescans when dirty, or when a directory's mtime changed and the last check is at least
    /// [`PATH_RECHECK_INTERVAL`] old. Returns true when a scan happened.
    fn refresh(&mut self, now: Instant) -> bool {
        if !self.dirty {
            let due = self
                .last_check
                .is_none_or(|t| now.saturating_duration_since(t) >= PATH_RECHECK_INTERVAL);
            if !due {
                return false;
            }
            self.last_check = Some(now);
            if self.dirs.iter().all(|d| dir_mtime(&d.dir) == d.mtime) {
                return false;
            }
        }
        self.scan();
        self.dirty = false;
        self.last_check = Some(now);
        true
    }

    fn scan(&mut self) {
        self.dirs.clear();
        self.commands.clear();
        for entry in self.path.split(':') {
            // zsh treats an empty entry and `.` as the current directory; the daemon cannot
            // follow the shell's cwd for a cached scan, so relative entries are skipped.
            if !entry.starts_with('/') {
                continue;
            }
            let dir = PathBuf::from(entry);
            if self.dirs.iter().any(|d| d.dir == dir) {
                continue;
            }
            // Record the mtime before reading, so a change during the read triggers a rescan.
            let mtime = dir_mtime(&dir);
            if mtime.is_some() {
                scan_dir(&dir, &mut self.commands);
            }
            self.dirs.push(ScannedDir { dir, mtime });
        }
    }
}

fn dir_mtime(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(dir).and_then(|m| m.modified()).ok()
}

/// Adds the names of executable regular files (or symlinks to them) in `dir` to `out`.
fn scan_dir(dir: &Path, out: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        // The entry's file type usually comes from the directory listing itself; only files and
        // symlinks need a stat to learn executability (and, for symlinks, the target type).
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_dir() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if out.contains(&name) {
            continue;
        }
        if is_executable_file(&entry.path()) {
            out.insert(name);
        }
    }
}

/// True when `path` (following symlinks) is a regular file with any execute bit set.
pub fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The shell's command namespace as last reported by the shell, plus the `$PATH` scan.
#[derive(Debug)]
pub struct ShellState {
    aliases: BTreeSet<String>,
    global_aliases: BTreeSet<String>,
    suffix_aliases: BTreeSet<String>,
    functions: BTreeSet<String>,
    builtins: BTreeSet<String>,
    reserved_words: BTreeSet<String>,
    named_dirs: HashMap<String, PathBuf>,
    home: Option<PathBuf>,
    path: PathCache,
}

impl Default for ShellState {
    fn default() -> ShellState {
        ShellState::new()
    }
}

fn to_set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| (*s).to_owned()).collect()
}

/// True when `set` holds a name that starts with `prefix`.
fn has_prefix(set: &BTreeSet<String>, prefix: &str) -> bool {
    set.range::<str, _>((Bound::Included(prefix), Bound::Unbounded))
        .next()
        .is_some_and(|s| s.starts_with(prefix))
}

impl ShellState {
    /// A state with the built-in builtin and reserved-word lists, `PATH` taken from the daemon's
    /// own environment, and `HOME` from the environment. The `$PATH` scan runs lazily on the
    /// first [`ShellState::refresh_path`].
    pub fn new() -> ShellState {
        let mut path = PathCache::default();
        path.set_path(std::env::var("PATH").unwrap_or_default());
        ShellState {
            aliases: BTreeSet::new(),
            global_aliases: BTreeSet::new(),
            suffix_aliases: BTreeSet::new(),
            functions: BTreeSet::new(),
            builtins: to_set(DEFAULT_BUILTINS),
            reserved_words: to_set(DEFAULT_RESERVED_WORDS),
            named_dirs: HashMap::new(),
            home: std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(PathBuf::from),
            path,
        }
    }

    /// Replaces each part of the state whose field in `update` is `Some`. A changed `path`
    /// schedules a rescan on the next [`ShellState::refresh_path`].
    pub fn apply_update(&mut self, update: StateUpdate) {
        let StateUpdate {
            aliases,
            global_aliases,
            suffix_aliases,
            functions,
            builtins,
            reserved_words,
            named_dirs,
            path,
        } = update;
        let replace = |dst: &mut BTreeSet<String>, src: Option<Vec<String>>| {
            if let Some(v) = src {
                *dst = v.into_iter().filter(|s| !s.is_empty()).collect();
            }
        };
        replace(&mut self.aliases, aliases);
        replace(&mut self.global_aliases, global_aliases);
        replace(&mut self.suffix_aliases, suffix_aliases);
        replace(&mut self.functions, functions);
        replace(&mut self.builtins, builtins);
        replace(&mut self.reserved_words, reserved_words);
        if let Some(dirs) = named_dirs {
            self.named_dirs = dirs
                .into_iter()
                .filter(|(name, dir)| !name.is_empty() && !dir.is_empty())
                .map(|(name, dir)| (name, PathBuf::from(dir)))
                .collect();
        }
        if let Some(p) = path {
            self.path.set_path(p);
        }
    }

    /// Brings the `$PATH` cache up to date; call once per request before classifying.
    pub fn refresh_path(&mut self) {
        self.refresh_path_at(Instant::now());
    }

    /// [`ShellState::refresh_path`] with an explicit clock, for tests. Returns true when the
    /// directories were rescanned.
    pub fn refresh_path_at(&mut self, now: Instant) -> bool {
        self.path.refresh(now)
    }

    /// Forces a full rescan on the next refresh (for an explicit `rehash`).
    pub fn invalidate_path(&mut self) {
        self.path.dirty = true;
    }

    /// The directory `hash -d` maps `name` to.
    pub fn named_dir(&self, name: &str) -> Option<&Path> {
        self.named_dirs.get(name).map(PathBuf::as_path)
    }

    /// The home directory used for `~`.
    pub fn home(&self) -> Option<&Path> {
        self.home.as_deref()
    }

    /// Overrides the home directory used for `~` (taken from `$HOME` by default).
    pub fn set_home(&mut self, home: Option<PathBuf>) {
        self.home = home;
    }

    /// True when `word` names a global alias.
    pub fn is_global_alias(&self, word: &str) -> bool {
        self.global_aliases.contains(word)
    }

    /// True when `word` names an executable found by the last `$PATH` scan.
    pub fn is_path_command(&self, word: &str) -> bool {
        !word.contains('/') && self.path.commands.contains(word)
    }

    /// Classifies a command-position word by name only, without touching the filesystem:
    /// alias, suffix alias, reserved word, function, builtin, then `$PATH` command. Returns
    /// [`CommandClass::Unknown`] when no name matches; an explicit path (`./run`, `~/bin/x`)
    /// is then for the caller to check, see [`ShellState::classify`].
    ///
    /// The order follows zsh: alias expansion (regular, then suffix) happens in the lexer
    /// before reserved words and command lookup. `quoted` means the word contained quoting or a
    /// backslash; zsh expands neither aliases nor reserved words in a quoted word.
    pub fn classify_name(&self, word: &str, quoted: bool) -> CommandClass {
        if !quoted {
            if self.aliases.contains(word) {
                return CommandClass::Alias;
            }
            if self.global_aliases.contains(word) {
                return CommandClass::GlobalAlias;
            }
            if self.is_suffix_aliased(word) {
                return CommandClass::SuffixAlias;
            }
            if self.reserved_words.contains(word) {
                return CommandClass::ReservedWord;
            }
        }
        if self.functions.contains(word) {
            return CommandClass::Function;
        }
        if self.builtins.contains(word) {
            return CommandClass::Builtin;
        }
        if self.is_path_command(word) {
            return CommandClass::External;
        }
        CommandClass::Unknown
    }

    /// Classifies an unquoted command-position word, including an explicit path containing `/`
    /// that names an executable file (tilde-expanded, relative to `cwd`). Uncached; the
    /// highlighter uses [`ShellState::classify_name`] plus the cached [`crate::paths::PathChecker`].
    pub fn classify(&self, word: &str, cwd: &Path) -> CommandClass {
        let class = self.classify_name(word, false);
        if class != CommandClass::Unknown || !word.contains('/') {
            return class;
        }
        match crate::paths::resolve(word, word.starts_with('~'), cwd, self) {
            Some(p) if is_executable_file(&p) => CommandClass::External,
            _ => CommandClass::Unknown,
        }
    }

    /// True when `word` has the form `text.ext` with non-empty `text` and `ext` a suffix alias.
    /// Like zsh, only the suffix matters; the file need not exist.
    fn is_suffix_aliased(&self, word: &str) -> bool {
        match word.rfind('.') {
            Some(i) if i > 0 && i + 1 < word.len() => self.suffix_aliases.contains(&word[i + 1..]),
            _ => false,
        }
    }

    /// True when some alias, reserved word, function, builtin, or `$PATH` command starts with
    /// `prefix` (an incomplete command word the user may still be typing).
    pub fn is_command_prefix(&self, prefix: &str) -> bool {
        has_prefix(&self.aliases, prefix)
            || has_prefix(&self.reserved_words, prefix)
            || has_prefix(&self.functions, prefix)
            || has_prefix(&self.builtins, prefix)
            || (!prefix.contains('/') && has_prefix(&self.path.commands, prefix))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;

    /// A unique temporary directory removed on drop.
    pub(crate) struct TempDir(pub PathBuf);

    impl TempDir {
        pub(crate) fn new(tag: &str) -> TempDir {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "fast-highlight-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }

        pub(crate) fn file(&self, rel: &str, mode: u32) -> PathBuf {
            let p = self.0.join(rel);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&p, b"#!/bin/sh\n").unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
            p
        }

        pub(crate) fn dir(&self, rel: &str) -> PathBuf {
            let p = self.0.join(rel);
            fs::create_dir_all(&p).unwrap();
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn strings(v: &[&str]) -> Option<Vec<String>> {
        Some(v.iter().map(|s| (*s).to_owned()).collect())
    }

    /// A state with no names at all and the given PATH, already scanned.
    pub(crate) fn empty_state(path: &str) -> ShellState {
        let mut s = ShellState::new();
        s.apply_update(StateUpdate {
            aliases: strings(&[]),
            global_aliases: strings(&[]),
            suffix_aliases: strings(&[]),
            functions: strings(&[]),
            builtins: strings(&[]),
            reserved_words: strings(&[]),
            named_dirs: Some(vec![]),
            path: Some(path.to_owned()),
        });
        s.refresh_path_at(Instant::now());
        s
    }

    #[test]
    fn defaults_classify_builtins_and_reserved_words() {
        let s = ShellState::new();
        assert_eq!(s.classify_name("echo", false), CommandClass::Builtin);
        assert_eq!(s.classify_name("cd", false), CommandClass::Builtin);
        assert_eq!(s.classify_name("while", false), CommandClass::ReservedWord);
        assert_eq!(s.classify_name("[[", false), CommandClass::ReservedWord);
    }

    #[test]
    fn precedence_follows_zsh() {
        let t = TempDir::new("prec");
        t.file("bin/foo", 0o755);
        t.file("bin/ext", 0o755);
        let mut s = empty_state(&t.path().join("bin").to_string_lossy());
        s.apply_update(StateUpdate {
            aliases: strings(&["foo", "a.py"]),
            global_aliases: strings(&["G"]),
            suffix_aliases: strings(&["py"]),
            functions: strings(&["foo", "fn", "ext", "if"]),
            builtins: strings(&["foo", "fn", "bi", "ext"]),
            reserved_words: strings(&["if", "foo"]),
            ..Default::default()
        });
        // Alias beats everything.
        assert_eq!(s.classify_name("foo", false), CommandClass::Alias);
        assert_eq!(s.classify_name("a.py", false), CommandClass::Alias);
        assert_eq!(s.classify_name("G", false), CommandClass::GlobalAlias);
        assert_eq!(s.classify_name("b.py", false), CommandClass::SuffixAlias);
        assert_eq!(s.classify_name("if", false), CommandClass::ReservedWord);
        assert_eq!(s.classify_name("fn", false), CommandClass::Function);
        assert_eq!(s.classify_name("ext", false), CommandClass::Function);
        assert_eq!(s.classify_name("bi", false), CommandClass::Builtin);
        assert_eq!(s.classify_name("nope", false), CommandClass::Unknown);
        // Quoting disables alias and reserved-word recognition.
        assert_eq!(s.classify_name("foo", true), CommandClass::Function);
        assert_eq!(s.classify_name("if", true), CommandClass::Function);
        assert_eq!(s.classify_name("b.py", true), CommandClass::Unknown);
        assert!(s.is_global_alias("G"));
        assert!(!s.is_global_alias("foo"));
    }

    #[test]
    fn suffix_alias_needs_text_and_suffix() {
        let mut s = empty_state("");
        s.apply_update(StateUpdate {
            suffix_aliases: strings(&["txt"]),
            ..Default::default()
        });
        assert_eq!(
            s.classify_name("notes.txt", false),
            CommandClass::SuffixAlias
        );
        assert_eq!(
            s.classify_name("dir/notes.txt", false),
            CommandClass::SuffixAlias
        );
        assert_eq!(s.classify_name(".txt", false), CommandClass::Unknown);
        assert_eq!(s.classify_name("notes.", false), CommandClass::Unknown);
        assert_eq!(
            s.classify_name("notes.txt.bak", false),
            CommandClass::Unknown
        );
    }

    #[test]
    fn path_scan_finds_executables_only() {
        let t = TempDir::new("scan");
        t.file("a/run", 0o755);
        t.file("a/data", 0o644);
        t.dir("a/subdir");
        t.file("b/run", 0o755);
        t.file("b/other", 0o700);
        std::os::unix::fs::symlink(t.path().join("a/run"), t.path().join("a/link")).unwrap();
        std::os::unix::fs::symlink(t.path().join("a/data"), t.path().join("a/datalink")).unwrap();
        std::os::unix::fs::symlink(t.path().join("nowhere"), t.path().join("a/dangling")).unwrap();
        let a = t.path().join("a");
        let b = t.path().join("b");
        let path = format!(
            "{}::.:relative:{}:{}/missing:{}",
            a.display(),
            b.display(),
            t.path().display(),
            a.display()
        );
        let s = empty_state(&path);
        for name in ["run", "other", "link"] {
            assert_eq!(
                s.classify_name(name, false),
                CommandClass::External,
                "{name}"
            );
        }
        for name in ["data", "subdir", "datalink", "dangling", "missing"] {
            assert_eq!(
                s.classify_name(name, false),
                CommandClass::Unknown,
                "{name}"
            );
        }
        assert_eq!(
            s.path.dirs.len(),
            3,
            "relative and duplicate entries are skipped"
        );
    }

    #[test]
    fn path_change_rescans() {
        let t = TempDir::new("pathchange");
        t.file("a/one", 0o755);
        t.file("b/two", 0o755);
        let mut s = empty_state(&t.path().join("a").to_string_lossy());
        assert!(s.is_path_command("one"));
        assert!(!s.is_path_command("two"));
        s.apply_update(StateUpdate {
            path: Some(t.path().join("b").to_string_lossy().into_owned()),
            ..Default::default()
        });
        assert!(s.refresh_path_at(Instant::now()));
        assert!(!s.is_path_command("one"));
        assert!(s.is_path_command("two"));
        // The same PATH again does not trigger a scan.
        s.apply_update(StateUpdate {
            path: Some(t.path().join("b").to_string_lossy().into_owned()),
            ..Default::default()
        });
        assert!(!s.path.dirty);
    }

    fn set_mtime(dir: &Path, secs: u64) {
        let f = fs::File::open(dir).unwrap();
        f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    #[test]
    fn mtime_change_rescans_at_most_once_per_interval() {
        let t = TempDir::new("mtime");
        let a = t.dir("a");
        set_mtime(&a, 1_000_000);
        let mut s = empty_state(&a.to_string_lossy());
        let t0 = s.path.last_check.unwrap();
        assert!(!s.is_path_command("new"));

        t.file("a/new", 0o755);
        set_mtime(&a, 2_000_000);
        // Within the interval: no check, no rescan.
        assert!(!s.refresh_path_at(t0 + Duration::from_millis(500)));
        assert!(!s.is_path_command("new"));
        // After the interval: the mtime changed, so rescan.
        assert!(s.refresh_path_at(t0 + Duration::from_millis(1100)));
        assert!(s.is_path_command("new"));
        // Unchanged mtime after another interval: no rescan.
        assert!(!s.refresh_path_at(t0 + Duration::from_millis(2200)));
    }

    #[test]
    fn missing_path_dir_appearing_triggers_rescan() {
        let t = TempDir::new("appear");
        let a = t.path().join("later");
        let mut s = empty_state(&a.to_string_lossy());
        let t0 = s.path.last_check.unwrap();
        t.file("later/tool", 0o755);
        assert!(s.refresh_path_at(t0 + Duration::from_secs(2)));
        assert!(s.is_path_command("tool"));
    }

    #[test]
    fn invalidate_forces_rescan() {
        let t = TempDir::new("rehash");
        let a = t.dir("a");
        let mut s = empty_state(&a.to_string_lossy());
        t.file("a/x", 0o755);
        s.invalidate_path();
        assert!(s.refresh_path_at(Instant::now()));
        assert!(s.is_path_command("x"));
    }

    #[test]
    fn classify_explicit_paths() {
        let t = TempDir::new("explicit");
        t.file("tool", 0o755);
        t.file("plain", 0o644);
        t.dir("sub");
        let mut s = empty_state("");
        s.set_home(Some(t.path().to_path_buf()));
        assert_eq!(s.classify("./tool", t.path()), CommandClass::External);
        assert_eq!(
            s.classify(&t.path().join("tool").to_string_lossy(), Path::new("/")),
            CommandClass::External
        );
        assert_eq!(s.classify("~/tool", Path::new("/")), CommandClass::External);
        assert_eq!(s.classify("./plain", t.path()), CommandClass::Unknown);
        assert_eq!(s.classify("./sub", t.path()), CommandClass::Unknown);
        assert_eq!(s.classify("./nope", t.path()), CommandClass::Unknown);
        // Without a slash, a file in the cwd is not a command.
        assert_eq!(s.classify("tool", t.path()), CommandClass::Unknown);
    }

    #[test]
    fn command_prefix_lookup() {
        let t = TempDir::new("prefix");
        t.file("bin/gitk", 0o755);
        let mut s = empty_state(&t.path().join("bin").to_string_lossy());
        s.apply_update(StateUpdate {
            aliases: strings(&["ll"]),
            functions: strings(&["myfunc"]),
            builtins: strings(&["echo"]),
            reserved_words: strings(&["while"]),
            ..Default::default()
        });
        for p in ["gi", "gitk", "l", "myf", "ec", "whi"] {
            assert!(s.is_command_prefix(p), "{p}");
        }
        for p in ["gix", "zz", "echoo", "bin/g"] {
            assert!(!s.is_command_prefix(p), "{p}");
        }
    }

    #[test]
    fn named_dirs_and_empty_entries() {
        let mut s = empty_state("");
        s.apply_update(StateUpdate {
            named_dirs: Some(vec![
                ("proj".into(), "/src/proj".into()),
                ("".into(), "/x".into()),
                ("y".into(), "".into()),
            ]),
            aliases: strings(&["", "ok"]),
            ..Default::default()
        });
        assert_eq!(s.named_dir("proj"), Some(Path::new("/src/proj")));
        assert_eq!(s.named_dir(""), None);
        assert_eq!(s.named_dir("y"), None);
        assert_eq!(s.classify_name("", false), CommandClass::Unknown);
        // Absent fields leave the rest unchanged.
        s.apply_update(StateUpdate::default());
        assert_eq!(s.named_dir("proj"), Some(Path::new("/src/proj")));
        assert_eq!(s.classify_name("ok", false), CommandClass::Alias);
    }
}
