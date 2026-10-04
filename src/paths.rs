//! Tilde expansion and cached filesystem checks.
//!
//! Path arguments are checked with `stat` calls on every keystroke, so every check goes through
//! [`PathChecker`]: results are cached by absolute path for a short TTL, and the number of
//! uncached filesystem calls per request is capped. A check that would exceed the cap is not
//! made; the word simply gets no path highlight.

use crate::config::Limits;
use crate::state::ShellState;
use std::collections::HashMap;
use std::ffi::{CStr, CString, OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// What a path argument refers to on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    File,
    Directory,
    /// Not an existing path, but a prefix of one.
    Prefix,
    Missing,
}

/// The result of one `stat` of a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStat {
    /// A regular file or anything else that is not a directory, including a dangling symlink.
    File {
        executable: bool,
    },
    Directory,
    Missing,
}

/// Maximum number of cached `stat` results.
const MAX_STAT_ENTRIES: usize = 4096;
/// Maximum number of cached directory listings (used for prefix detection).
const MAX_LISTINGS: usize = 64;
/// Maximum number of names read from one directory for prefix detection.
const MAX_LISTING_NAMES: usize = 16384;
/// Maximum number of cached `~user` lookups.
const MAX_USER_ENTRIES: usize = 256;

/// The longest path the platform accepts.
const PATH_MAX: usize = libc::PATH_MAX as usize;

#[derive(Debug)]
struct StatEntry {
    at: Instant,
    stat: FileStat,
}

#[derive(Debug)]
struct Listing {
    at: Instant,
    /// Entry names, sorted bytewise. Empty when the directory could not be read.
    names: Vec<Box<[u8]>>,
}

/// Cached filesystem checks with a per-request budget.
///
/// Call [`PathChecker::begin_request`] at the start of every request; it resets the budget and
/// sets the clock used for TTL decisions.
#[derive(Debug)]
pub struct PathChecker {
    ttl: Duration,
    max_checks: usize,
    checks_left: usize,
    now: Instant,
    last_sweep: Instant,
    // Keyed by the raw path bytes: `PathBuf` equality ignores a trailing `/`, which matters
    // here (`file/` is missing while `file` exists).
    stats: HashMap<OsString, StatEntry>,
    listings: HashMap<OsString, Listing>,
}

impl Default for PathChecker {
    fn default() -> PathChecker {
        PathChecker::new(&Limits::default())
    }
}

impl PathChecker {
    pub fn new(limits: &Limits) -> PathChecker {
        let now = Instant::now();
        PathChecker {
            ttl: Duration::from_millis(limits.path_cache_ttl_ms),
            max_checks: limits.max_path_checks,
            checks_left: limits.max_path_checks,
            now,
            last_sweep: now,
            stats: HashMap::new(),
            listings: HashMap::new(),
        }
    }

    /// Starts a request at time `now`: applies `limits` (they may have been reloaded), resets
    /// the per-request check budget, and evicts expired cache entries once per TTL.
    pub fn begin_request(&mut self, now: Instant, limits: &Limits) {
        self.ttl = Duration::from_millis(limits.path_cache_ttl_ms);
        self.max_checks = limits.max_path_checks;
        self.checks_left = self.max_checks;
        self.now = now;
        if now.saturating_duration_since(self.last_sweep) >= self.ttl {
            self.sweep();
        }
    }

    /// True when this request has used up its filesystem-check budget.
    pub fn budget_exhausted(&self) -> bool {
        self.checks_left == 0
    }

    fn fresh(&self, at: Instant) -> bool {
        self.now.saturating_duration_since(at) < self.ttl
    }

    fn sweep(&mut self) {
        let (now, ttl) = (self.now, self.ttl);
        self.stats
            .retain(|_, e| now.saturating_duration_since(e.at) < ttl);
        self.listings
            .retain(|_, e| now.saturating_duration_since(e.at) < ttl);
        self.last_sweep = now;
    }

    /// Spends one unit of the request budget; false when none is left.
    fn spend(&mut self) -> bool {
        if self.checks_left == 0 {
            return false;
        }
        self.checks_left -= 1;
        true
    }

    /// Stats `abs` (following symlinks), from the cache when fresh. `None` when the request
    /// budget is exhausted and the result is not cached.
    pub fn stat(&mut self, abs: &Path) -> Option<FileStat> {
        if abs.as_os_str().len() > PATH_MAX {
            return Some(FileStat::Missing);
        }
        if let Some(e) = self.stats.get(abs.as_os_str())
            && self.fresh(e.at)
        {
            return Some(e.stat);
        }
        if !self.spend() {
            return None;
        }
        let stat = stat_uncached(abs);
        if self.stats.len() >= MAX_STAT_ENTRIES {
            self.sweep();
            if self.stats.len() >= MAX_STAT_ENTRIES {
                self.stats.clear();
            }
        }
        self.stats
            .insert(abs.as_os_str().to_owned(), StatEntry { at: self.now, stat });
        Some(stat)
    }

    /// True when the final component of `abs` is a proper or complete prefix of an entry in its
    /// parent directory. `None` when the budget is exhausted. A path ending in `/` has no
    /// final component and is never a prefix.
    pub fn is_prefix(&mut self, abs: &Path) -> Option<bool> {
        let bytes = abs.as_os_str().as_bytes();
        let Some(slash) = bytes.iter().rposition(|&b| b == b'/') else {
            return Some(false);
        };
        let name = &bytes[slash + 1..];
        if name.is_empty() {
            return Some(false);
        }
        let parent = OsStr::from_bytes(if slash == 0 { b"/" } else { &bytes[..slash] });
        let fresh = self.listings.get(parent).is_some_and(|l| self.fresh(l.at));
        if !fresh {
            if !self.spend() {
                return None;
            }
            let names = list_dir(Path::new(parent));
            if self.listings.len() >= MAX_LISTINGS {
                self.sweep();
                if self.listings.len() >= MAX_LISTINGS {
                    self.listings.clear();
                }
            }
            self.listings.insert(
                parent.to_owned(),
                Listing {
                    at: self.now,
                    names,
                },
            );
        }
        let names = &self.listings[parent].names;
        let i = names.partition_point(|n| &n[..] < name);
        Some(names.get(i).is_some_and(|n| n.starts_with(name)))
    }

    /// Checks an absolute path. With `allow_prefix`, a missing path whose final component
    /// starts an entry of its existing parent is [`PathKind::Prefix`]. Over the request budget
    /// the result is [`PathKind::Missing`] without touching the filesystem.
    pub fn check(&mut self, abs: &Path, allow_prefix: bool) -> PathKind {
        match self.stat(abs) {
            Some(FileStat::File { .. }) => PathKind::File,
            Some(FileStat::Directory) => PathKind::Directory,
            Some(FileStat::Missing) if allow_prefix && self.is_prefix(abs) == Some(true) => {
                PathKind::Prefix
            }
            _ => PathKind::Missing,
        }
    }

    /// Resolves a shell word (see [`resolve`]) and checks it. Words that cannot be paths
    /// cheaply (empty, starting with `-`, longer than `PATH_MAX`) are [`PathKind::Missing`].
    pub fn check_word(
        &mut self,
        word: &str,
        tilde: bool,
        cwd: &Path,
        state: &ShellState,
        allow_prefix: bool,
    ) -> PathKind {
        if !could_be_path(word) {
            return PathKind::Missing;
        }
        match resolve(word, tilde, cwd, state) {
            Some(abs) => self.check(&abs, allow_prefix),
            None => PathKind::Missing,
        }
    }
}

/// True unless `word` obviously is not a path worth a filesystem check.
pub fn could_be_path(word: &str) -> bool {
    !word.is_empty() && !word.starts_with('-') && word.len() <= PATH_MAX && !word.contains('\0')
}

fn stat_uncached(abs: &Path) -> FileStat {
    match std::fs::metadata(abs) {
        Ok(m) if m.is_dir() => FileStat::Directory,
        Ok(m) => FileStat::File {
            executable: m.is_file() && m.permissions().mode() & 0o111 != 0,
        },
        // A dangling symlink is still a file the user can name.
        Err(_) if std::fs::symlink_metadata(abs).is_ok() => FileStat::File { executable: false },
        Err(_) => FileStat::Missing,
    }
}

fn list_dir(dir: &Path) -> Vec<Box<[u8]>> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<Box<[u8]>> = entries
        .flatten()
        .take(MAX_LISTING_NAMES)
        .map(|e| e.file_name().as_bytes().into())
        .collect();
    names.sort_unstable();
    names
}

/// Expands a leading tilde (when `tilde` is true and the word starts with `~`) and resolves the
/// result against `cwd`. Returns `None` when the tilde cannot be expanded:
///
/// - `~` and `~/...` use the home directory from [`ShellState::home`].
/// - `~+` is `cwd`.
/// - `~-` (`$OLDPWD`) and the directory stack forms `~N`, `~+N`, `~-N` are unknown to the
///   daemon.
/// - `~name` is a `hash -d` named directory, else the home directory of user `name`.
pub fn resolve(word: &str, tilde: bool, cwd: &Path, state: &ShellState) -> Option<PathBuf> {
    let expanded;
    let path: &Path = if tilde && let Some(rest) = word.strip_prefix('~') {
        let (head, tail) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, ""),
        };
        let base: PathBuf = match head {
            "" => state.home()?.to_path_buf(),
            "+" => cwd.to_path_buf(),
            h if h.starts_with(['+', '-']) || h.bytes().all(|b| b.is_ascii_digit()) => {
                return None;
            }
            h => match state.named_dir(h) {
                Some(d) => d.to_path_buf(),
                None => user_home(h)?,
            },
        };
        expanded = if tail.is_empty() && !rest.ends_with('/') {
            base
        } else {
            // Keep a trailing slash: `~/dir/` names a directory.
            let mut s = base.into_os_string();
            s.push("/");
            s.push(tail);
            PathBuf::from(s)
        };
        &expanded
    } else {
        Path::new(word)
    };
    Some(if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    })
}

static USER_HOMES: Mutex<Option<HashMap<String, Option<PathBuf>>>> = Mutex::new(None);

/// The home directory of user `name`, cached for the life of the process (negative results
/// too). Names that cannot be user names are rejected without a lookup.
pub fn user_home(name: &str) -> Option<PathBuf> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return None;
    }
    let mut guard = USER_HOMES.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some(home) = cache.get(name) {
        return home.clone();
    }
    let home = lookup_user_home(name);
    if cache.len() >= MAX_USER_ENTRIES {
        cache.clear();
    }
    cache.insert(name.to_owned(), home.clone());
    home
}

fn lookup_user_home(name: &str) -> Option<PathBuf> {
    let cname = CString::new(name).ok()?;
    let mut buf: Vec<libc::c_char> = vec![0; 1024];
    loop {
        // SAFETY: `passwd` is plain old data; all-zero is a valid (empty) value.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the duration of the call and `buf.len()` is the
        // true size of `buf`.
        let rc = unsafe {
            libc::getpwnam_r(
                cname.as_ptr(),
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
        // SAFETY: on success `pw_dir` points to a NUL-terminated string inside `buf`.
        let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
        return Some(PathBuf::from(OsStr::from_bytes(dir.to_bytes())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::StateUpdate;
    use crate::state::tests::{TempDir, empty_state};

    fn limits(max_checks: usize, ttl_ms: u64) -> Limits {
        Limits {
            max_path_checks: max_checks,
            path_cache_ttl_ms: ttl_ms,
            ..Limits::default()
        }
    }

    fn checker(max_checks: usize, ttl_ms: u64, now: Instant) -> PathChecker {
        let l = limits(max_checks, ttl_ms);
        let mut c = PathChecker::new(&l);
        c.begin_request(now, &l);
        c
    }

    #[test]
    fn kinds() {
        let t = TempDir::new("kinds");
        t.file("file.txt", 0o644);
        t.dir("dir");
        std::os::unix::fs::symlink(t.path().join("gone"), t.path().join("dangling")).unwrap();
        let s = empty_state("");
        let mut c = checker(64, 1000, Instant::now());
        let cwd = t.path();
        let check = |c: &mut PathChecker, w: &str, p: bool| c.check_word(w, false, cwd, &s, p);
        assert_eq!(check(&mut c, "file.txt", false), PathKind::File);
        assert_eq!(check(&mut c, "dir", false), PathKind::Directory);
        assert_eq!(check(&mut c, "dir/", false), PathKind::Directory);
        assert_eq!(check(&mut c, "file.txt/", false), PathKind::Missing);
        assert_eq!(check(&mut c, "dangling", false), PathKind::File);
        assert_eq!(check(&mut c, "fil", false), PathKind::Missing);
        assert_eq!(check(&mut c, "fil", true), PathKind::Prefix);
        assert_eq!(check(&mut c, "di", true), PathKind::Prefix);
        assert_eq!(check(&mut c, "xyz", true), PathKind::Missing);
        assert_eq!(check(&mut c, "nodir/fil", true), PathKind::Missing);
        assert_eq!(check(&mut c, "dir/x/", true), PathKind::Missing);
        assert_eq!(
            check(&mut c, &format!("{}/fi", cwd.display()), true),
            PathKind::Prefix
        );
        assert_eq!(check(&mut c, "-rf", false), PathKind::Missing);
        assert_eq!(check(&mut c, "", true), PathKind::Missing);
        assert_eq!(
            check(&mut c, &"a".repeat(PATH_MAX + 1), true),
            PathKind::Missing
        );
    }

    #[test]
    fn prefix_at_root() {
        let mut c = checker(64, 1000, Instant::now());
        assert_eq!(c.is_prefix(Path::new("/")), Some(false));
        // Every Unix system has a top-level entry starting with one of these.
        let found = ["/u", "/e", "/b", "/t"]
            .iter()
            .any(|p| c.is_prefix(Path::new(p)) == Some(true));
        assert!(found);
    }

    #[test]
    fn tilde_forms() {
        let t = TempDir::new("tilde");
        t.file("home/notes", 0o644);
        t.dir("proj/src");
        let mut s = empty_state("");
        s.set_home(Some(t.path().join("home")));
        s.apply_update(StateUpdate {
            named_dirs: Some(vec![(
                "p".into(),
                t.path().join("proj").to_string_lossy().into(),
            )]),
            ..Default::default()
        });
        let cwd = Path::new("/cwd");
        assert_eq!(resolve("~", true, cwd, &s), Some(t.path().join("home")));
        assert_eq!(
            resolve("~/notes", true, cwd, &s),
            Some(t.path().join("home/notes"))
        );
        assert_eq!(
            resolve("~p/src", true, cwd, &s),
            Some(t.path().join("proj/src"))
        );
        assert_eq!(
            resolve("~p/", true, cwd, &s)
                .unwrap()
                .as_os_str()
                .as_bytes()
                .last(),
            Some(&b'/')
        );
        assert_eq!(resolve("~+", true, cwd, &s), Some(PathBuf::from("/cwd")));
        assert_eq!(
            resolve("~+/x", true, cwd, &s),
            Some(PathBuf::from("/cwd/x"))
        );
        assert_eq!(resolve("~-", true, cwd, &s), None);
        assert_eq!(resolve("~2", true, cwd, &s), None);
        assert_eq!(resolve("~+1", true, cwd, &s), None);
        assert_eq!(resolve("~no_such_user_fh_xyz", true, cwd, &s), None);
        assert_eq!(resolve("~bad name", true, cwd, &s), None);
        // Quoted tilde: a literal relative path.
        assert_eq!(
            resolve("~/x", false, cwd, &s),
            Some(PathBuf::from("/cwd/~/x"))
        );
        assert_eq!(
            resolve("rel/x", false, cwd, &s),
            Some(PathBuf::from("/cwd/rel/x"))
        );
        assert_eq!(resolve("/abs", false, cwd, &s), Some(PathBuf::from("/abs")));

        let mut c = checker(64, 1000, Instant::now());
        assert_eq!(
            c.check_word("~/notes", true, cwd, &s, false),
            PathKind::File
        );
        assert_eq!(
            c.check_word("~p", true, cwd, &s, false),
            PathKind::Directory
        );
        assert_eq!(c.check_word("~p/sr", true, cwd, &s, true), PathKind::Prefix);
        assert_eq!(c.check_word("~-", true, cwd, &s, true), PathKind::Missing);
        s.set_home(None);
        assert_eq!(resolve("~", true, cwd, &s), None);
    }

    /// The current user's name and home directory from the password database.
    fn current_user() -> Option<(String, PathBuf)> {
        let mut buf: Vec<libc::c_char> = vec![0; 1 << 16];
        // SAFETY: as in `lookup_user_home`.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: valid pointers and buffer length.
        let rc = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                &mut pwd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            )
        };
        if rc != 0 || result.is_null() {
            return None;
        }
        // SAFETY: NUL-terminated strings inside `buf` on success.
        let (name, dir) = unsafe { (CStr::from_ptr(pwd.pw_name), CStr::from_ptr(pwd.pw_dir)) };
        Some((
            name.to_str().ok()?.to_owned(),
            PathBuf::from(OsStr::from_bytes(dir.to_bytes())),
        ))
    }

    #[test]
    fn tilde_user() {
        let Some((name, home)) = current_user() else {
            return;
        };
        let s = empty_state("");
        let cwd = Path::new("/");
        assert_eq!(
            resolve(&format!("~{name}"), true, cwd, &s),
            Some(home.clone())
        );
        assert_eq!(
            resolve(&format!("~{name}/a"), true, cwd, &s),
            Some(home.join("a"))
        );
        // Cached on the second call.
        assert_eq!(user_home(&name), Some(home));
    }

    #[test]
    fn cache_ttl() {
        let t = TempDir::new("ttl");
        let s = empty_state("");
        let l = limits(64, 1000);
        let t0 = Instant::now();
        let mut c = checker(64, 1000, t0);
        assert_eq!(
            c.check_word("f", false, t.path(), &s, false),
            PathKind::Missing
        );
        t.file("f", 0o644);
        // Within the TTL the cached Missing stands.
        c.begin_request(t0 + Duration::from_millis(500), &l);
        assert_eq!(
            c.check_word("f", false, t.path(), &s, false),
            PathKind::Missing
        );
        // After the TTL the path is checked again.
        c.begin_request(t0 + Duration::from_millis(1500), &l);
        assert_eq!(
            c.check_word("f", false, t.path(), &s, false),
            PathKind::File
        );

        // Directory listings for prefix detection follow the same TTL.
        assert_eq!(
            c.check_word("gx", false, t.path(), &s, true),
            PathKind::Missing
        );
        t.file("gxy", 0o644);
        c.begin_request(t0 + Duration::from_millis(1600), &l);
        assert_eq!(
            c.check_word("gx", false, t.path(), &s, true),
            PathKind::Missing
        );
        c.begin_request(t0 + Duration::from_millis(3000), &l);
        assert_eq!(
            c.check_word("gx", false, t.path(), &s, true),
            PathKind::Prefix
        );
    }

    #[test]
    fn expired_entries_are_swept() {
        let t = TempDir::new("sweep");
        let s = empty_state("");
        let l = limits(64, 100);
        let t0 = Instant::now();
        let mut c = checker(64, 100, t0);
        for i in 0..10 {
            c.check_word(&format!("f{i}"), false, t.path(), &s, true);
        }
        assert_eq!(c.stats.len(), 10);
        assert_eq!(c.listings.len(), 1);
        c.begin_request(t0 + Duration::from_millis(200), &l);
        assert!(c.stats.is_empty());
        assert!(c.listings.is_empty());
    }

    #[test]
    fn stat_cache_is_bounded() {
        let s = empty_state("");
        let t0 = Instant::now();
        let mut c = checker(usize::MAX, 60_000, t0);
        for i in 0..MAX_STAT_ENTRIES + 10 {
            c.check_word(
                &format!("/nonexistent-fh/{i}"),
                false,
                Path::new("/"),
                &s,
                false,
            );
        }
        assert!(c.stats.len() <= MAX_STAT_ENTRIES);
    }

    #[test]
    fn per_request_cap() {
        let t = TempDir::new("cap");
        for i in 0..5 {
            t.file(&format!("f{i}"), 0o644);
        }
        let s = empty_state("");
        let l = limits(3, 1000);
        let t0 = Instant::now();
        let mut c = checker(3, 1000, t0);
        let kinds: Vec<_> = (0..5)
            .map(|i| c.check_word(&format!("f{i}"), false, t.path(), &s, false))
            .collect();
        assert_eq!(&kinds[..3], &[PathKind::File; 3]);
        assert_eq!(&kinds[3..], &[PathKind::Missing; 2]);
        assert!(c.budget_exhausted());
        assert_eq!(c.stat(&t.path().join("f4")), None);
        // Cached results stay available over budget.
        assert_eq!(
            c.check_word("f0", false, t.path(), &s, false),
            PathKind::File
        );
        // The next request has a fresh budget.
        c.begin_request(t0 + Duration::from_millis(10), &l);
        assert_eq!(
            c.check_word("f4", false, t.path(), &s, false),
            PathKind::File
        );
    }

    #[test]
    fn executable_bit() {
        let t = TempDir::new("exec");
        t.file("x", 0o755);
        t.file("n", 0o644);
        let mut c = checker(64, 1000, Instant::now());
        assert_eq!(
            c.stat(&t.path().join("x")),
            Some(FileStat::File { executable: true })
        );
        assert_eq!(
            c.stat(&t.path().join("n")),
            Some(FileStat::File { executable: false })
        );
        assert_eq!(c.stat(t.path()), Some(FileStat::Directory));
        assert_eq!(c.stat(&t.path().join("none")), Some(FileStat::Missing));
    }
}
