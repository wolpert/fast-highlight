//! The `serve` loop.
//!
//! [`serve`] reads request frames on standard input and writes response frames on standard
//! output until a `Q` request, end of file, a framing error, a write error, or the death of the
//! parent process. The request handling lives in [`Session`] and the loop in [`run`], both
//! generic over the [`Engine`] and the byte streams so they can be tested without a real
//! highlighter or real file descriptors.
//!
//! Nothing but frames is ever written to standard output, and nothing is written to standard
//! error. Diagnostics (panics, startup problems, the reason for an abnormal exit, and the
//! optional per-request timing lines) go to the log file.

use crate::config::{Config, Limits, config_dir};
use crate::highlight::{HighlightRequest, Highlighter, RequestOptions};
use crate::paths::PathChecker;
use crate::protocol::{
    Decoder, FramingError, HighlightFields, Request, StateUpdate, WireSpan, encode_ack,
    encode_error, encode_result,
};
use crate::specs::SpecRegistry;
use crate::state::ShellState;
use crate::syntax::ParseOptions;
use crate::text::{RequestText, Unit};
use crate::token::Span;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Options of `fast-highlight serve`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServeOptions {
    /// Log one timing line per request (also enabled by `log.timing` in the config).
    pub timing: bool,
    /// Log file, overriding `log.file` in the config and the default location.
    pub log: Option<PathBuf>,
    /// A process to watch in addition to the parent (`--parent PID`, the plugin passes the
    /// shell's `$$`). The daemon exits once it no longer exists. This closes the race where the
    /// shell dies before the daemon records its parent pid, which is then already that of the
    /// reaper and never changes.
    pub parent: Option<libc::pid_t>,
}

/// What the serve loop needs from a highlighter.
pub trait Engine {
    /// Returns spans for `req.text` in byte offsets, sorted and well nested.
    fn highlight(&mut self, req: &HighlightRequest<'_>) -> Vec<Span>;
    /// Applies an `S` request.
    fn update_state(&mut self, update: StateUpdate);
    /// Work deferred out of the requests: done after their responses are written, and about
    /// once per second while no input arrives. The daemon is then between requests, and the
    /// plugin is not waiting on it.
    fn idle(&mut self) {}
}

impl Engine for Highlighter {
    fn highlight(&mut self, req: &HighlightRequest<'_>) -> Vec<Span> {
        Highlighter::highlight(self, req)
    }

    fn update_state(&mut self, update: StateUpdate) {
        self.state.apply_update(update);
    }

    /// Brings the `$PATH` scan up to date. After the startup ping this scans the daemon's own
    /// `PATH` (normally the shell's) while the plugin finishes the handshake; after an `S`
    /// request that changed `PATH` or asked for `rehash`, it rescans once the ack is out, while
    /// the shell is between commands (the plugin sends state from `precmd`). It is also where
    /// the directories' modification times are compared, at most once per second, so a
    /// highlight request never pays for that. A highlight request still scans first when a
    /// rescan is pending.
    fn idle(&mut self) {
        self.state.refresh_path();
    }
}

/// Builds a highlighter from `config`, loading specs from the config directory. Returns the
/// spec loading warnings alongside it.
pub fn new_highlighter(config: &Config) -> (Highlighter, Vec<String>) {
    let (specs, warnings) = SpecRegistry::load(&config_dir());
    let highlighter = Highlighter {
        config: config.clone(),
        state: ShellState::new(),
        paths: PathChecker::default(),
        specs,
    };
    (highlighter, warnings)
}

/// Maps the letters of an `opt` field to request options. Unknown letters are ignored; `u`
/// selects the offset unit and does not appear in the result.
pub fn request_options(letters: &str) -> RequestOptions {
    let has = |c| letters.contains(c);
    RequestOptions {
        parse: ParseOptions {
            interactive_comments: has('c'),
            extended_glob: has('e'),
            ksh_glob: has('k'),
            ignore_braces: has('b'),
            ignore_close_braces: has('B'),
            rc_quotes: has('r'),
            ksh_arrays: has('K'),
            posix_identifiers: has('p'),
            sh_glob: has('s'),
            brace_ccl: has('C'),
            no_short_loops: has('L'),
        },
        auto_cd: has('a'),
        no_equals: has('E'),
    }
}

/// The daemon's log: an append-only file written one whole line at a time. Every failure to
/// write is ignored.
#[derive(Debug, Clone, Default)]
pub struct Log {
    path: Option<PathBuf>,
    timing: bool,
}

impl Log {
    /// A log that writes to `path` (nothing when `None`). Timing lines are written only when
    /// `timing` is true.
    pub fn new(path: Option<PathBuf>, timing: bool) -> Log {
        Log { path, timing }
    }

    /// A log that discards everything.
    pub fn disabled() -> Log {
        Log::default()
    }

    /// Appends one line, prefixed with a Unix timestamp.
    pub fn line(&self, message: &str) {
        if let Some(path) = &self.path {
            append_line(path, message);
        }
    }

    fn timing(&self, id: u64, kind: char, bytes: usize, spans: usize, started: Instant) {
        if self.timing {
            let us = started.elapsed().as_micros();
            self.line(&format!(
                "id={id} type={kind} bytes={bytes} spans={spans} us={us}"
            ));
        }
    }
}

fn append_line(path: &Path, message: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let line = format!("{}.{:03} {message}\n", now.as_secs(), now.subsec_millis());
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}

/// `${XDG_STATE_HOME:-$HOME/.local/state}/fast-highlight/fast-highlight.log`, or `None` when
/// neither variable is usable.
pub fn default_log_path() -> Option<PathBuf> {
    let non_empty = |name| std::env::var_os(name).filter(|v| !v.is_empty());
    let state = non_empty("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| non_empty("HOME").map(|home| PathBuf::from(home).join(".local/state")))?;
    Some(state.join("fast-highlight").join("fast-highlight.log"))
}

/// The response to one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// A response frame to write.
    Frame(Vec<u8>),
    /// A `Q` request: stop without responding.
    Quit,
}

/// Per-connection state: the engine, the remembered current directory, and the limits.
pub struct Session<E> {
    engine: E,
    cwd: PathBuf,
    hard_cap_bytes: usize,
    max_spans: usize,
    log: Log,
}

impl<E: Engine> Session<E> {
    /// A session whose remembered current directory starts as `cwd`. Highlight requests whose
    /// text exceeds `limits.hard_cap_bytes` get an empty result without reaching the engine,
    /// and results are cut to `limits.max_spans` spans.
    pub fn new(engine: E, limits: &Limits, cwd: PathBuf, log: Log) -> Session<E> {
        Session {
            engine,
            cwd,
            hard_cap_bytes: limits.hard_cap_bytes,
            max_spans: limits.max_spans,
            log,
        }
    }

    pub fn engine(&self) -> &E {
        &self.engine
    }

    /// The remembered current directory.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn log(&self) -> &Log {
        &self.log
    }

    /// Runs the engine's deferred work. A panic there has been logged and is otherwise ignored;
    /// the next request retries the work.
    pub fn idle(&mut self) {
        let _ = catch(|| self.engine.idle());
    }

    /// Handles one request and returns its response.
    pub fn handle(&mut self, request: Request) -> Reply {
        let started = Instant::now();
        match request {
            Request::Highlight { id, fields } => {
                let bytes = fields.prebuffer.len() + fields.buffer.len();
                match self.highlight(&fields) {
                    Ok(spans) => {
                        self.log.timing(id, 'H', bytes, spans.len(), started);
                        Reply::Frame(encode_result(id, &spans))
                    }
                    Err(message) => {
                        self.log.timing(id, 'H', bytes, 0, started);
                        Reply::Frame(encode_error(id, &message))
                    }
                }
            }
            Request::State {
                id,
                update,
                warnings,
            } => {
                for warning in &warnings {
                    self.log.line(&format!("state request {id}: {warning}"));
                }
                let reply = match catch(|| self.engine.update_state(update)) {
                    Ok(()) => encode_ack(id),
                    Err(message) => encode_error(id, &message),
                };
                self.log.timing(id, 'S', 0, 0, started);
                Reply::Frame(reply)
            }
            Request::Ping { id } => {
                self.log.timing(id, 'P', 0, 0, started);
                Reply::Frame(encode_ack(id))
            }
            Request::Quit { .. } => Reply::Quit,
            Request::Invalid { id, reason } => Reply::Frame(encode_error(id, &reason)),
        }
    }

    /// Highlights one request and returns its wire spans (at most `max_spans` of them), or a
    /// diagnostic when the engine panicked. A present `cwd` field replaces the remembered
    /// directory, even when the text is over the hard cap.
    pub fn highlight(&mut self, fields: &HighlightFields) -> Result<Vec<WireSpan>, String> {
        if let Some(cwd) = &fields.cwd {
            self.cwd = PathBuf::from(OsStr::from_bytes(cwd));
        }
        if fields.prebuffer.len() + fields.buffer.len() > self.hard_cap_bytes {
            return Ok(Vec::new());
        }
        let text = RequestText::new(&fields.prebuffer, &fields.buffer);
        let unit = Unit::from_char_offsets(fields.char_offsets());
        let req = HighlightRequest {
            text: text.as_str(),
            buffer_start: text.buffer_start(),
            cursor: text.cursor_offset(fields.cursor, unit),
            cwd: &self.cwd,
            opts: request_options(&fields.opts),
        };
        let spans = catch(|| self.engine.highlight(&req))?;
        Ok(text.wire_spans_prefix(&spans, unit, self.max_spans))
    }
}

/// Runs `f`, turning a panic into an error message. The panic hook has already logged it.
fn catch<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    panic::catch_unwind(AssertUnwindSafe(f)).map_err(|payload| {
        let detail = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        format!("internal error: {detail}")
    })
}

/// Why the serve loop stopped.
#[derive(Debug)]
pub enum Exit {
    /// A `Q` request.
    Quit,
    /// End of input.
    Eof,
    /// The input was not a valid frame stream.
    Framing(FramingError),
    /// Reading failed, or the parent process exited.
    Read(io::Error),
    /// Writing a response failed (for example, the reader went away).
    Write(io::Error),
}

impl Exit {
    /// The process exit status: 0 for a requested or orderly stop, 1 otherwise.
    pub fn code(&self) -> i32 {
        match self {
            Exit::Quit | Exit::Eof => 0,
            Exit::Framing(_) | Exit::Read(_) | Exit::Write(_) => 1,
        }
    }
}

/// Reads frames from `input` and writes one response per request to `output`, flushing after
/// each, until the loop stops for one of the reasons in [`Exit`]. Once every complete request
/// read so far is answered, the session's deferred work runs before the next read. It runs too
/// whenever a read fails with [`io::ErrorKind::TimedOut`], which [`serve`]'s input reports
/// after a second without input; the loop then reads again.
pub fn run<E: Engine, R: Read, W: Write>(
    session: &mut Session<E>,
    input: &mut R,
    output: &mut W,
) -> Exit {
    let mut decoder = Decoder::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let mut answered = false;
        loop {
            match decoder.next_request() {
                Ok(Some(request)) => match session.handle(request) {
                    Reply::Quit => return Exit::Quit,
                    Reply::Frame(frame) => {
                        if let Err(e) = output.write_all(&frame).and_then(|()| output.flush()) {
                            return Exit::Write(e);
                        }
                        answered = true;
                    }
                },
                Ok(None) => break,
                Err(e) => return Exit::Framing(e),
            }
        }
        if answered {
            session.idle();
        }
        match input.read(&mut buf) {
            Ok(0) => return Exit::Eof,
            Ok(n) => decoder.feed(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::TimedOut => session.idle(),
            Err(e) => return Exit::Read(e),
        }
    }
}

/// True when no process with id `pid` exists. A process we may not signal (`EPERM`) exists.
fn process_gone(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 performs only the existence and permission checks.
    let rc = unsafe { libc::kill(pid, 0) };
    rc != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// Standard input read through `poll` with a one-second timeout, so the parent process can be
/// checked while the input is idle. A read that times out fails with
/// [`io::ErrorKind::TimedOut`], so the serve loop can run its idle work. Reads fail for good
/// once the parent pid differs from the one recorded at startup (the parent exited and this
/// process was reparented), or once the watched process, if any, no longer exists.
struct PollStdin {
    parent: libc::pid_t,
    watched: Option<libc::pid_t>,
}

impl Read for PollStdin {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            // SAFETY: getppid has no preconditions.
            if unsafe { libc::getppid() } != self.parent || self.watched.is_some_and(process_gone) {
                return Err(io::Error::other("parent process exited"));
            }
            let mut pfd = libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `pfd` is a valid pollfd and the count is 1.
            let ready = unsafe { libc::poll(&mut pfd, 1, 1000) };
            if ready < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if ready == 0 {
                return Err(io::ErrorKind::TimedOut.into());
            }
            // Readable, hung up, or in error: read reports which.
            // SAFETY: `buf` is valid for writes of `buf.len()` bytes.
            let n = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) {
                    continue;
                }
                return Err(e);
            }
            return Ok(n as usize);
        }
    }
}

/// Unbuffered standard output: each `write` is one `write(2)` call.
struct RawStdout;

impl Write for RawStdout {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            // SAFETY: `buf` is valid for reads of `buf.len()` bytes.
            let n = unsafe { libc::write(libc::STDOUT_FILENO, buf.as_ptr().cast(), buf.len()) };
            if n >= 0 {
                return Ok(n as usize);
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Replaces the panic hook with one that appends the panic to `path` and prints nothing.
fn install_panic_hook(path: Option<PathBuf>) {
    panic::set_hook(Box::new(move |info| {
        if let Some(path) = &path {
            append_line(path, &format!("panic: {info}"));
        }
    }));
}

/// Builds the highlighter on first use, so a failure to build it (a panic while loading the
/// config directory, say) turns into `E` responses instead of a dead daemon, and is retried on
/// the next request.
struct LazyHighlighter {
    config: Config,
    log: Log,
    inner: Option<Highlighter>,
}

impl LazyHighlighter {
    fn get(&mut self) -> &mut Highlighter {
        let config = &self.config;
        let log = &self.log;
        self.inner.get_or_insert_with(|| {
            let (highlighter, warnings) = new_highlighter(config);
            for warning in warnings {
                log.line(&format!("spec warning: {warning}"));
            }
            highlighter
        })
    }
}

impl Engine for LazyHighlighter {
    fn highlight(&mut self, req: &HighlightRequest<'_>) -> Vec<Span> {
        self.get().highlight(req)
    }

    fn update_state(&mut self, update: StateUpdate) {
        Engine::update_state(self.get(), update);
    }

    fn idle(&mut self) {
        Engine::idle(self.get());
    }
}

/// Keeps the memory of large requests in the heap between requests, on glibc.
///
/// The parse of a buffer near the hard cap holds tens of thousands of spans and words. With
/// glibc's defaults the blocks behind them are mapped and unmapped (or trimmed from the heap)
/// on every such request, and the page faults alone roughly double the request time (about
/// 5.8 ms instead of 2.7 ms for 64 KiB of `a|a|...`). Blocks up to 4 MiB therefore come from
/// the heap, and up to 32 MiB of free heap is kept instead of being returned to the system.
/// Other platforms' allocators are left alone.
pub fn tune_allocator() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: mallopt only changes allocator parameters; it is called before any other thread
    // exists.
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, 4 << 20);
        libc::mallopt(libc::M_TRIM_THRESHOLD, 32 << 20);
    }
}

/// Runs the daemon on standard input and output. Returns the process exit status.
pub fn serve(opts: ServeOptions) -> i32 {
    tune_allocator();
    // SAFETY: setting signal dispositions to SIG_IGN has no preconditions. Ignoring SIGPIPE
    // makes a write to a closed pipe fail with EPIPE instead of killing the process; ignoring
    // SIGINT keeps a stray Ctrl-C sent to the process group from killing it.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        libc::signal(libc::SIGINT, libc::SIG_IGN);
    }
    // SAFETY: getppid has no preconditions.
    let parent = unsafe { libc::getppid() };
    // The daemon changes to `/` below; a relative `--log` names a file in the starting
    // directory.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let opts = ServeOptions {
        log: opts.log.map(|log| cwd.join(log)),
        ..opts
    };

    let early_log_path = opts.log.clone().or_else(default_log_path);
    install_panic_hook(early_log_path.clone());

    let mut startup = Vec::new();
    let config = match panic::catch_unwind(Config::load) {
        Ok(Ok(config)) => config,
        Ok(Err(e)) => {
            startup.push(format!("config error, using defaults: {e}"));
            Config::default()
        }
        Err(_) => {
            startup.push("config loading panicked, using defaults".to_string());
            Config::default()
        }
    };
    let log_path = opts
        .log
        .or_else(|| config.log.file.clone())
        .or_else(default_log_path);
    if log_path != early_log_path {
        install_panic_hook(log_path.clone());
    }
    let log = Log::new(log_path, opts.timing || config.log.timing);
    for message in &startup {
        log.line(message);
    }

    let mut engine = LazyHighlighter {
        config: config.clone(),
        log: log.clone(),
        inner: None,
    };
    // Build eagerly so the first highlight does not pay for it; on failure the next request
    // retries.
    let _ = catch(|| {
        engine.get();
    });

    // The starting directory is the remembered cwd until a request names one. Holding it as
    // the process's own working directory would keep its file system busy (`umount` fails)
    // for the life of the shell, so move to the root; every path the daemon checks is
    // resolved against the remembered cwd anyway.
    let _ = std::env::set_current_dir("/");
    let mut session = Session::new(engine, &config.limits, cwd, log);
    let mut input = PollStdin {
        parent,
        watched: opts.parent,
    };
    let exit = run(&mut session, &mut input, &mut RawStdout);
    match &exit {
        Exit::Quit | Exit::Eof => {}
        Exit::Framing(e) => session.log().line(&format!("exit: {e}")),
        Exit::Read(e) => session.log().line(&format!("exit: read: {e}")),
        Exit::Write(e) => session.log().line(&format!("exit: write: {e}")),
    }
    exit.code()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Frame, encode_frame, encode_highlight, encode_ping, encode_quit};
    use crate::protocol::{encode_request, encode_state};
    use crate::token::TokenKind;

    /// What the fake engine saw for one highlight call.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Seen {
        text: String,
        buffer_start: usize,
        cursor: usize,
        cwd: PathBuf,
        opts: RequestOptions,
    }

    #[derive(Default)]
    struct FakeEngine {
        seen: Vec<Seen>,
        updates: Vec<StateUpdate>,
        /// Returned for every highlight call.
        spans: Vec<Span>,
        panic_on: Option<&'static str>,
    }

    impl Engine for FakeEngine {
        fn highlight(&mut self, req: &HighlightRequest<'_>) -> Vec<Span> {
            if let Some(trigger) = self.panic_on
                && req.text.contains(trigger)
            {
                panic!("fake engine exploded");
            }
            self.seen.push(Seen {
                text: req.text.to_string(),
                buffer_start: req.buffer_start,
                cursor: req.cursor,
                cwd: req.cwd.to_path_buf(),
                opts: req.opts,
            });
            self.spans.clone()
        }

        fn update_state(&mut self, update: StateUpdate) {
            if update.path.as_deref() == Some("panic") {
                panic!("bad state");
            }
            self.updates.push(update);
        }
    }

    fn limits(hard_cap_bytes: usize) -> Limits {
        Limits {
            hard_cap_bytes,
            ..Limits::default()
        }
    }

    fn session(engine: FakeEngine) -> Session<FakeEngine> {
        Session::new(
            engine,
            &limits(1000),
            PathBuf::from("/start"),
            Log::disabled(),
        )
    }

    fn frames(bytes: &[u8]) -> Vec<Frame> {
        let mut d = Decoder::new();
        d.feed(bytes);
        let mut out = Vec::new();
        while let Some(f) = d.next_frame().expect("daemon output is well framed") {
            out.push(f);
        }
        assert_eq!(d.pending(), 0, "trailing partial frame");
        out
    }

    fn run_bytes<E: Engine>(session: &mut Session<E>, input: &[u8]) -> (Exit, Vec<Frame>) {
        let mut out = Vec::new();
        let exit = run(session, &mut &input[..], &mut out);
        (exit, frames(&out))
    }

    fn hl(id: u64, buf: &str, opts: &str) -> Vec<u8> {
        encode_highlight(
            id,
            &HighlightFields {
                buffer: buf.as_bytes().to_vec(),
                opts: opts.to_string(),
                ..HighlightFields::default()
            },
        )
    }

    #[test]
    fn ping_ack_then_eof() {
        let mut s = session(FakeEngine::default());
        let mut input = encode_ping(1);
        input.extend(encode_ping(2));
        let (exit, out) = run_bytes(&mut s, &input);
        assert!(matches!(exit, Exit::Eof));
        assert_eq!(exit.code(), 0);
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].kind, out[0].id), (b'A', 1));
        assert_eq!((out[1].kind, out[1].id), (b'A', 2));
    }

    #[test]
    fn quit_stops_without_response_and_ignores_the_rest() {
        let mut s = session(FakeEngine::default());
        let mut input = encode_ping(1);
        input.extend(encode_quit(2));
        input.extend(encode_ping(3));
        let (exit, out) = run_bytes(&mut s, &input);
        assert!(matches!(exit, Exit::Quit));
        assert_eq!(out.iter().map(|f| f.id).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn framing_error_stops_after_answering_earlier_requests() {
        let mut s = session(FakeEngine::default());
        let mut input = encode_ping(1);
        input.extend(b"garbage\n");
        input.extend(encode_ping(2));
        let (exit, out) = run_bytes(&mut s, &input);
        assert!(matches!(exit, Exit::Framing(_)));
        assert_eq!(exit.code(), 1);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn highlight_result_in_char_units() {
        let engine = FakeEngine {
            spans: vec![
                Span::new(0, 2, TokenKind::Command),
                Span::new(3, 9, TokenKind::Path),
            ],
            ..FakeEngine::default()
        };
        let mut s = session(engine);
        let (_, out) = run_bytes(&mut s, &hl(7, "ls 日本", "u"));
        assert_eq!(out[0].kind, b'R');
        assert_eq!(out[0].id, 7);
        assert_eq!(out[0].body, b"0 2 command\n3 5 path\n");
        let (_, out) = run_bytes(&mut s, &hl(8, "ls 日本", ""));
        assert_eq!(out[0].body, b"0 2 command\n3 9 path\n");
    }

    #[test]
    fn engine_sees_prebuffer_cursor_and_opts() {
        let mut s = session(FakeEngine::default());
        let fields = HighlightFields {
            buffer: "é x".as_bytes().to_vec(),
            prebuffer: b"if true\n".to_vec(),
            cursor: Some(1),
            cwd: None,
            opts: "uacekz".into(),
        };
        run_bytes(&mut s, &encode_highlight(1, &fields));
        let seen = &s.engine().seen[0];
        assert_eq!(seen.text, "if true\né x");
        assert_eq!(seen.buffer_start, 8);
        assert_eq!(seen.cursor, 10, "cursor 1 char into the buffer, after é");
        assert_eq!(
            seen.opts,
            RequestOptions {
                parse: ParseOptions {
                    interactive_comments: true,
                    extended_glob: true,
                    ksh_glob: true,
                    ..ParseOptions::default()
                },
                auto_cd: true,
                ..RequestOptions::default()
            }
        );
        // Byte units: cursor 1 lands inside é and rounds down; no cursor means end of text.
        let fields = HighlightFields {
            opts: String::new(),
            ..fields
        };
        run_bytes(&mut s, &encode_highlight(2, &fields));
        assert_eq!(s.engine().seen[1].cursor, 8);
        assert_eq!(s.engine().seen[1].opts, RequestOptions::default());
        let fields = HighlightFields {
            cursor: None,
            ..fields
        };
        run_bytes(&mut s, &encode_highlight(3, &fields));
        assert_eq!(s.engine().seen[2].cursor, "if true\né x".len());
    }

    #[test]
    fn cwd_is_remembered_until_changed() {
        let mut s = session(FakeEngine::default());
        let with_cwd = |id, cwd: &[u8]| {
            encode_highlight(
                id,
                &HighlightFields {
                    cwd: Some(cwd.to_vec()),
                    ..HighlightFields::default()
                },
            )
        };
        let mut input = hl(1, "a", "");
        input.extend(with_cwd(2, b"/tmp/x"));
        input.extend(hl(3, "b", ""));
        input.extend(with_cwd(4, b"/odd/\xff"));
        run_bytes(&mut s, &input);
        let cwds: Vec<_> = s.engine().seen.iter().map(|x| x.cwd.clone()).collect();
        assert_eq!(cwds[0], PathBuf::from("/start"));
        assert_eq!(cwds[1], PathBuf::from("/tmp/x"));
        assert_eq!(cwds[2], PathBuf::from("/tmp/x"));
        assert_eq!(cwds[3].as_os_str().as_bytes(), b"/odd/\xff");
        assert_eq!(s.cwd().as_os_str().as_bytes(), b"/odd/\xff");
    }

    #[test]
    fn hard_cap_returns_empty_result_without_engine() {
        let engine = FakeEngine {
            // Byte 5 is the first byte of the buffer, after the 5-byte prebuffer.
            spans: vec![Span::new(5, 6, TokenKind::Error)],
            ..FakeEngine::default()
        };
        let mut s = Session::new(engine, &limits(10), PathBuf::from("/"), Log::disabled());
        let at_cap = HighlightFields {
            buffer: b"12345".to_vec(),
            prebuffer: b"67890".to_vec(),
            ..HighlightFields::default()
        };
        let over_cap = HighlightFields {
            buffer: b"123456".to_vec(),
            cwd: Some(b"/new".to_vec()),
            ..at_cap.clone()
        };
        let mut input = encode_highlight(1, &at_cap);
        input.extend(encode_highlight(2, &over_cap));
        let (_, out) = run_bytes(&mut s, &input);
        assert_eq!(out[0].body, b"0 1 error\n");
        assert_eq!((out[1].kind, out[1].id, out[1].body.len()), (b'R', 2, 0));
        assert_eq!(s.engine().seen.len(), 1);
        assert_eq!(s.cwd(), Path::new("/new"), "cwd still updated over the cap");
    }

    #[test]
    fn results_are_cut_to_max_spans() {
        let engine = FakeEngine {
            spans: vec![
                Span::new(0, 6, TokenKind::DoubleQuoted),
                Span::new(1, 3, TokenKind::Parameter),
                Span::new(3, 5, TokenKind::Parameter),
                Span::new(7, 8, TokenKind::Glob),
            ],
            ..FakeEngine::default()
        };
        let limits = Limits {
            max_spans: 3,
            ..limits(1000)
        };
        let mut s = Session::new(engine, &limits, PathBuf::from("/"), Log::disabled());
        let (_, out) = run_bytes(&mut s, &hl(1, "\"$a$b\" *", ""));
        assert_eq!(
            out[0].body,
            b"0 6 double-quoted\n1 3 parameter\n3 5 parameter\n"
        );
    }

    #[test]
    fn prebuffer_spans_do_not_count_against_max_spans() {
        let engine = FakeEngine {
            // Two spans wholly inside the 3-byte prebuffer, then two in the buffer.
            spans: vec![
                Span::new(0, 1, TokenKind::Glob),
                Span::new(1, 2, TokenKind::Glob),
                Span::new(3, 4, TokenKind::Glob),
                Span::new(4, 5, TokenKind::Glob),
            ],
            ..FakeEngine::default()
        };
        let limits = Limits {
            max_spans: 1,
            ..limits(1000)
        };
        let mut s = Session::new(engine, &limits, PathBuf::from("/"), Log::disabled());
        let fields = HighlightFields {
            prebuffer: b"**\n".to_vec(),
            buffer: b"**".to_vec(),
            ..HighlightFields::default()
        };
        let (_, out) = run_bytes(&mut s, &encode_highlight(1, &fields));
        assert_eq!(out[0].body, b"0 1 glob\n");
    }

    #[test]
    fn state_warnings_are_logged_and_the_rest_applies() {
        let dir = std::env::temp_dir().join(format!("fh-daemon-warn-{}", std::process::id()));
        let path = dir.join("w.log");
        let _ = fs::remove_dir_all(&dir);
        let mut s = Session::new(
            FakeEngine::default(),
            &limits(1000),
            PathBuf::from("/"),
            Log::new(Some(path.clone()), false),
        );
        let body = b"alias 3\nll\0\nnameddirs 2\nx\0\n";
        let (_, out) = run_bytes(&mut s, &encode_frame(b'S', 9, body));
        assert_eq!((out[0].kind, out[0].id), (b'A', 9));
        assert_eq!(
            s.engine().updates,
            vec![StateUpdate {
                aliases: Some(vec!["ll".into()]),
                ..StateUpdate::default()
            }]
        );
        let log = fs::read_to_string(&path).unwrap();
        assert!(log.contains("state request 9: nameddirs"), "{log}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rehash_finds_a_command_added_without_an_mtime_change() {
        let dir = std::env::temp_dir().join(format!("fh-daemon-rehash-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut h = Highlighter {
            config: Config::default(),
            state: ShellState::new(),
            paths: PathChecker::default(),
            specs: SpecRegistry::default(),
        };
        h.state.apply_update(StateUpdate {
            path: Some(dir.to_string_lossy().into_owned()),
            ..StateUpdate::default()
        });
        let mut s = Session::new(h, &limits(1000), PathBuf::from("/"), Log::disabled());
        let (_, out) = run_bytes(&mut s, &hl(1, "fh-new-tool", ""));
        assert_eq!(out[0].body, b"0 11 error\n");

        // Install the tool but put the directory's mtime back, as a copy that preserves
        // timestamps or a coarse-grained file system might.
        let before = fs::metadata(&dir).unwrap().modified().unwrap();
        let tool = dir.join("fh-new-tool");
        fs::write(&tool, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        fs::File::open(&dir).unwrap().set_modified(before).unwrap();
        let (_, out) = run_bytes(&mut s, &hl(2, "fh-new-tool", ""));
        assert_eq!(
            out[0].body, b"0 11 error\n",
            "the mtime check sees no change"
        );

        let rehash = StateUpdate {
            rehash: true,
            ..StateUpdate::default()
        };
        let mut input = encode_state(3, &rehash);
        input.extend(hl(4, "fh-new-tool", ""));
        let (_, out) = run_bytes(&mut s, &input);
        assert_eq!((out[0].kind, out[0].id), (b'A', 3));
        assert_eq!(out[1].body, b"0 11 command\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn engine_panic_becomes_error_response() {
        let engine = FakeEngine {
            panic_on: Some("boom"),
            ..FakeEngine::default()
        };
        let mut s = session(engine);
        let mut input = hl(1, "boom", "u");
        input.extend(hl(2, "fine", "u"));
        let state = StateUpdate {
            path: Some("panic".into()),
            ..StateUpdate::default()
        };
        input.extend(encode_state(3, &state));
        input.extend(encode_ping(4));
        let (exit, out) = run_bytes(&mut s, &input);
        assert!(matches!(exit, Exit::Eof));
        let summary: Vec<_> = out.iter().map(|f| (f.kind, f.id)).collect();
        assert_eq!(summary, vec![(b'E', 1), (b'R', 2), (b'E', 3), (b'A', 4)]);
        let message = String::from_utf8(out[0].body.clone()).unwrap();
        assert!(message.contains("fake engine exploded"), "{message}");
    }

    #[test]
    fn state_update_reaches_engine() {
        let mut s = session(FakeEngine::default());
        let update = StateUpdate {
            aliases: Some(vec!["ll".into()]),
            path: Some("/bin".into()),
            ..StateUpdate::default()
        };
        let req = Request::State {
            id: 5,
            update: update.clone(),
            warnings: vec![],
        };
        let (_, out) = run_bytes(&mut s, &encode_request(&req).unwrap());
        assert_eq!((out[0].kind, out[0].id), (b'A', 5));
        assert_eq!(s.engine().updates, vec![update]);
    }

    #[test]
    fn invalid_requests_get_error_responses() {
        let mut s = session(FakeEngine::default());
        let mut input = encode_frame(b'Z', 1, b"");
        input.extend(encode_frame(b'H', 2, b"buf 9\nab\n"));
        input.extend(encode_ping(3));
        let (_, out) = run_bytes(&mut s, &input);
        let summary: Vec<_> = out.iter().map(|f| (f.kind, f.id)).collect();
        assert_eq!(summary, vec![(b'E', 1), (b'E', 2), (b'A', 3)]);
        assert!(s.engine().seen.is_empty());
    }

    /// Delivers its input one byte per read call.
    struct Trickle<'a>(&'a [u8]);

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.0.split_first() {
                Some((&b, rest)) if !buf.is_empty() => {
                    buf[0] = b;
                    self.0 = rest;
                    Ok(1)
                }
                _ => Ok(0),
            }
        }
    }

    #[test]
    fn split_reads_are_reassembled() {
        let mut s = session(FakeEngine::default());
        let mut input = hl(1, "日本", "u");
        input.extend(encode_ping(2));
        let mut out = Vec::new();
        let exit = run(&mut s, &mut Trickle(&input), &mut out);
        assert!(matches!(exit, Exit::Eof));
        let ids: Vec<_> = frames(&out).iter().map(|f| f.id).collect();
        assert_eq!(ids, vec![1, 2]);
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn write_error_stops_the_loop() {
        let mut s = session(FakeEngine::default());
        let input = encode_ping(1);
        let exit = run(&mut s, &mut &input[..], &mut FailingWriter);
        assert!(matches!(exit, Exit::Write(_)));
        assert_eq!(exit.code(), 1);
    }

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("parent process exited"))
        }
    }

    #[test]
    fn read_error_stops_the_loop() {
        let mut s = session(FakeEngine::default());
        let exit = run(&mut s, &mut FailingReader, &mut Vec::new());
        assert!(matches!(exit, Exit::Read(_)));
    }

    #[test]
    fn timing_lines_are_logged() {
        let dir = std::env::temp_dir().join(format!("fh-daemon-log-{}", std::process::id()));
        let path = dir.join("sub").join("t.log");
        let _ = fs::remove_dir_all(&dir);
        let mut s = Session::new(
            FakeEngine {
                spans: vec![Span::new(0, 1, TokenKind::Glob)],
                ..FakeEngine::default()
            },
            &limits(1000),
            PathBuf::from("/"),
            Log::new(Some(path.clone()), true),
        );
        let mut input = hl(11, "*x", "");
        input.extend(encode_ping(12));
        run_bytes(&mut s, &input);
        let log = fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = log.lines().collect();
        assert_eq!(lines.len(), 2, "{log}");
        assert!(
            lines[0].contains("id=11 type=H bytes=2 spans=1 us="),
            "{log}"
        );
        assert!(lines[1].contains("id=12 type=P"), "{log}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn timing_disabled_writes_nothing() {
        let dir = std::env::temp_dir().join(format!("fh-daemon-nolog-{}", std::process::id()));
        let path = dir.join("t.log");
        let _ = fs::remove_dir_all(&dir);
        let mut s = Session::new(
            FakeEngine::default(),
            &limits(1000),
            PathBuf::from("/"),
            Log::new(Some(path.clone()), false),
        );
        run_bytes(&mut s, &encode_ping(1));
        assert!(!path.exists());
    }

    #[test]
    fn idle_scans_path_after_a_state_update() {
        let dir = std::env::temp_dir().join(format!("fh-daemon-path-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let tool = dir.join("fh-tool");
        fs::write(&tool, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let mut h = Highlighter {
            config: Config::default(),
            state: ShellState::new(),
            paths: PathChecker::default(),
            specs: SpecRegistry::default(),
        };
        Engine::update_state(
            &mut h,
            StateUpdate {
                path: Some(dir.to_string_lossy().into_owned()),
                ..StateUpdate::default()
            },
        );
        assert!(
            !h.state.is_path_command("fh-tool"),
            "the scan waits until the ack is out"
        );
        Engine::idle(&mut h);
        assert!(h.state.is_path_command("fh-tool"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Records the order of engine calls and response writes in one list.
    #[derive(Clone, Default)]
    struct Events(std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>);

    impl Events {
        fn push(&self, event: &'static str) {
            self.0.borrow_mut().push(event);
        }

        fn take(&self) -> Vec<&'static str> {
            std::mem::take(&mut self.0.borrow_mut())
        }
    }

    struct RecordingEngine {
        events: Events,
        panic_in_idle: bool,
    }

    impl Engine for RecordingEngine {
        fn highlight(&mut self, _: &HighlightRequest<'_>) -> Vec<Span> {
            self.events.push("H");
            Vec::new()
        }

        fn update_state(&mut self, _: StateUpdate) {
            self.events.push("S");
        }

        fn idle(&mut self) {
            self.events.push("idle");
            if self.panic_in_idle {
                panic!("idle exploded");
            }
        }
    }

    struct RecordingWriter(Events);

    impl Write for RecordingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.push("write");
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn recording_session(events: &Events, panic_in_idle: bool) -> Session<RecordingEngine> {
        let engine = RecordingEngine {
            events: events.clone(),
            panic_in_idle,
        };
        Session::new(engine, &limits(1000), PathBuf::from("/"), Log::disabled())
    }

    #[test]
    fn idle_work_runs_after_the_responses_are_written() {
        let events = Events::default();
        let mut s = recording_session(&events, false);
        let mut input = encode_ping(1);
        input.extend(encode_state(2, &StateUpdate::default()));
        input.extend(hl(3, "ls", ""));
        let exit = run(
            &mut s,
            &mut &input[..],
            &mut RecordingWriter(events.clone()),
        );
        assert!(matches!(exit, Exit::Eof));
        // One read delivers all three requests: idle work runs once, after the last answer,
        // and never before the first request.
        assert_eq!(
            events.take(),
            vec!["write", "S", "write", "H", "write", "idle"]
        );

        // Requests that arrive one at a time each get their answer before the idle work.
        let mut input = encode_state(4, &StateUpdate::default());
        input.extend(encode_ping(5));
        let exit = run(
            &mut s,
            &mut Trickle(&input),
            &mut RecordingWriter(events.clone()),
        );
        assert!(matches!(exit, Exit::Eof));
        assert_eq!(events.take(), vec!["S", "write", "idle", "write", "idle"]);
    }

    /// Times out once before each chunk of its input, as the daemon's polled stdin does while
    /// the plugin is quiet.
    struct Sleepy<'a> {
        chunks: Vec<&'a [u8]>,
        timed_out: bool,
    }

    impl Read for Sleepy<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.timed_out {
                self.timed_out = true;
                return Err(io::ErrorKind::TimedOut.into());
            }
            self.timed_out = false;
            if self.chunks.is_empty() {
                return Ok(0);
            }
            let chunk = self.chunks.remove(0);
            buf[..chunk.len()].copy_from_slice(chunk);
            Ok(chunk.len())
        }
    }

    #[test]
    fn idle_work_runs_when_the_input_times_out() {
        let events = Events::default();
        let mut s = recording_session(&events, false);
        let ping = encode_ping(1);
        let mut input = Sleepy {
            chunks: vec![&ping],
            timed_out: false,
        };
        let exit = run(&mut s, &mut input, &mut RecordingWriter(events.clone()));
        assert!(matches!(exit, Exit::Eof));
        assert_eq!(events.take(), vec!["idle", "write", "idle", "idle"]);
    }

    #[test]
    fn idle_panic_does_not_stop_the_loop() {
        let events = Events::default();
        let mut s = recording_session(&events, true);
        let mut input = encode_ping(1);
        input.extend(encode_ping(2));
        let mut out = Vec::new();
        let exit = run(&mut s, &mut Trickle(&input), &mut out);
        assert!(matches!(exit, Exit::Eof));
        let ids: Vec<_> = frames(&out).iter().map(|f| f.id).collect();
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(events.take(), vec!["idle", "idle"]);
    }

    #[test]
    fn process_gone_detects_reaped_children_only() {
        // SAFETY: getpid has no preconditions.
        assert!(!process_gone(unsafe { libc::getpid() }));
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id() as libc::pid_t;
        assert!(!process_gone(pid));
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(process_gone(pid));
    }

    #[test]
    fn option_letters() {
        assert_eq!(request_options(""), RequestOptions::default());
        assert_eq!(request_options("uxyz"), RequestOptions::default());
        assert!(request_options("c").parse.interactive_comments);
        assert!(request_options("a").auto_cd);
        assert!(request_options("e").parse.extended_glob);
        assert!(request_options("k").parse.ksh_glob);
        assert!(request_options("b").parse.ignore_braces);
        assert!(request_options("B").parse.ignore_close_braces);
        assert!(request_options("r").parse.rc_quotes);
        assert!(request_options("K").parse.ksh_arrays);
        assert!(request_options("p").parse.posix_identifiers);
        assert!(request_options("s").parse.sh_glob);
        assert!(request_options("C").parse.brace_ccl);
        assert!(request_options("E").no_equals);
        assert!(request_options("L").parse.no_short_loops);
        // Letters are case-sensitive.
        assert!(!request_options("b").parse.ignore_close_braces);
        assert!(!request_options("K").parse.ksh_glob);
    }
}
