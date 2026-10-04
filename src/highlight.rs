//! The semantic pass: combines a parse with shell state, specs, and the filesystem.
//!
//! The parser ([`crate::syntax::parse`]) produces syntactic spans and a list of simple commands.
//! This pass classifies each command word (alias, builtin, external command, unknown, ...),
//! unwraps precommands such as `sudo` and `env`, applies per-command specs to arguments, and
//! checks literal arguments as paths on disk. The resulting word-level spans are merged under
//! the syntactic spans, so a syntactic span with the same range wins.

use crate::config::{Config, Limits};
use crate::paths::{FileStat, PathChecker, PathKind};
use crate::specs::{ArgInput, CommandSpec, SpecRegistry};
use crate::state::{CommandClass, ShellState};
use crate::syntax::{ParseOptions, ParseOutput, Word};
use crate::token::{Span, TokenKind};
use std::path::Path;
use std::time::Instant;

/// Request-level options that are not purely syntactic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RequestOptions {
    pub parse: ParseOptions,
    /// `AUTO_CD`: a directory in command position is valid.
    pub auto_cd: bool,
}

/// One highlight request in byte offsets.
#[derive(Debug, Clone, Copy)]
pub struct HighlightRequest<'a> {
    /// `PREBUFFER` followed by `BUFFER`.
    pub text: &'a str,
    /// Byte offset in `text` where `BUFFER` begins.
    pub buffer_start: usize,
    /// Cursor as a byte offset in `text`.
    pub cursor: usize,
    /// The shell's current directory.
    pub cwd: &'a Path,
    pub opts: RequestOptions,
}

pub struct Highlighter {
    pub config: Config,
    pub state: ShellState,
    pub paths: PathChecker,
    pub specs: SpecRegistry,
}

/// The parts of a command spec the semantic pass uses. Implemented by [`CommandSpec`]; tests
/// and tools can supply their own.
pub trait Spec {
    /// See [`CommandSpec::is_precommand`].
    fn is_precommand(&self) -> bool;
    /// See [`CommandSpec::wrapped_command`].
    fn wrapped_command(&self, args: &[ArgInput<'_>]) -> Option<usize>;
    /// See [`CommandSpec::classify_args`].
    fn classify_args(&self, args: &[ArgInput<'_>]) -> Vec<Option<TokenKind>>;
}

/// Finds the spec for a command name. Implemented by [`SpecRegistry`].
pub trait SpecLookup {
    fn spec(&self, command: &str) -> Option<&dyn Spec>;
}

impl Spec for CommandSpec {
    fn is_precommand(&self) -> bool {
        CommandSpec::is_precommand(self)
    }

    fn wrapped_command(&self, args: &[ArgInput<'_>]) -> Option<usize> {
        CommandSpec::wrapped_command(self, args)
    }

    fn classify_args(&self, args: &[ArgInput<'_>]) -> Vec<Option<TokenKind>> {
        CommandSpec::classify_args(self, args)
    }
}

impl SpecLookup for SpecRegistry {
    fn spec(&self, command: &str) -> Option<&dyn Spec> {
        self.get(command).map(|s| s as &dyn Spec)
    }
}

/// Precommand modifiers recognised when no spec describes the command: the zsh builtins and
/// reserved words that run the following words as a command.
const FALLBACK_PRECOMMANDS: &[&str] = &[
    "-",
    "builtin",
    "command",
    "exec",
    "noglob",
    "nocorrect",
    "time",
];

impl Highlighter {
    /// Returns the final spans for `req.text` in byte offsets, sorted and well nested. Spans may
    /// cover the `PREBUFFER` part; the caller clips them to the buffer.
    pub fn highlight(&mut self, req: &HighlightRequest<'_>) -> Vec<Span> {
        if req.text.len() > self.config.limits.hard_cap_bytes {
            return Vec::new();
        }
        let parse = crate::syntax::parse(req.text, &req.opts.parse);
        let Highlighter {
            config,
            state,
            paths,
            specs,
        } = self;
        run(
            &config.limits,
            state,
            paths,
            specs,
            req,
            parse,
            Instant::now(),
        )
    }

    /// [`Highlighter::highlight`] for an existing parse of `req.text`, with an explicit spec
    /// lookup and clock. Applies the same size limits.
    pub fn highlight_parsed(
        &mut self,
        req: &HighlightRequest<'_>,
        parse: ParseOutput,
        specs: &dyn SpecLookup,
        now: Instant,
    ) -> Vec<Span> {
        run(
            &self.config.limits,
            &mut self.state,
            &mut self.paths,
            specs,
            req,
            parse,
            now,
        )
    }
}

fn run(
    limits: &Limits,
    state: &mut ShellState,
    paths: &mut PathChecker,
    specs: &dyn SpecLookup,
    req: &HighlightRequest<'_>,
    parse: ParseOutput,
    now: Instant,
) -> Vec<Span> {
    let len = req.text.len();
    if len > limits.hard_cap_bytes {
        return Vec::new();
    }
    if len > limits.lex_only_bytes {
        return parse.spans;
    }
    state.refresh_path_at(now);
    paths.begin_request(now, limits);
    let mut pass = Pass {
        req,
        state,
        paths,
        specs,
        out: Vec::new(),
    };
    for cmd in &parse.commands {
        pass.command(&cmd.words);
    }
    for w in &parse.path_words {
        if !pass.global_alias(w) {
            pass.path_arg(w);
        }
    }
    let spans = merge(pass.out, &parse.spans, req.text);
    debug_assert!(
        crate::token::check_spans(req.text, &spans).is_ok(),
        "{:?}",
        crate::token::check_spans(req.text, &spans)
    );
    spans
}

/// State for one semantic pass over a parse.
struct Pass<'a, 'r> {
    req: &'a HighlightRequest<'r>,
    state: &'a ShellState,
    paths: &'a mut PathChecker,
    specs: &'a dyn SpecLookup,
    out: Vec<Span>,
}

fn arg_inputs(words: &[Word]) -> Vec<ArgInput<'_>> {
    words
        .iter()
        .map(|w| ArgInput {
            literal: w.literal.as_deref(),
        })
        .collect()
}

/// For a precommand without a spec, the index of the wrapped command in `args`: the first word
/// that is not an option (after `--`, the next word). `exec -a NAME` takes an argument.
fn fallback_wrapped(name: &str, args: &[ArgInput<'_>]) -> Option<usize> {
    let mut i = 0;
    while i < args.len() {
        match args[i].literal {
            Some("--") => return (i + 1 < args.len()).then_some(i + 1),
            Some("-a") if name == "exec" => i += 2,
            Some(s) if s.starts_with('-') && s.len() > 1 => i += 1,
            _ => return Some(i),
        }
    }
    None
}

impl Pass<'_, '_> {
    fn push(&mut self, w: &Word, kind: TokenKind) {
        self.out.push(Span::new(w.start, w.end, kind));
    }

    /// True when the word's source text differs from its literal value, i.e. it contains
    /// quoting or a backslash. zsh does not alias-expand such words.
    fn is_quoted(&self, w: &Word, literal: &str) -> bool {
        self.req.text.get(w.start..w.end) != Some(literal)
    }

    /// True when the cursor sits at the end of `w`: the user is presumably still typing it.
    fn typing(&self, w: &Word) -> bool {
        self.req.cursor == w.end
    }

    /// Emits a global-alias span for an unquoted word naming a global alias.
    fn global_alias(&mut self, w: &Word) -> bool {
        match w.literal.as_deref() {
            Some(lit) if self.state.is_global_alias(lit) && !self.is_quoted(w, lit) => {
                self.push(w, TokenKind::GlobalAlias);
                true
            }
            _ => false,
        }
    }

    /// Classifies one simple command, following precommand chains iteratively.
    fn command(&mut self, words: &[Word]) {
        let inputs = arg_inputs(words);
        let mut at = 0;
        while at < words.len() {
            let (cmd, args, arg_in) = (&words[at], &words[at + 1..], &inputs[at + 1..]);
            let Some(name) = cmd.literal.as_deref() else {
                // An expansion in command position: the syntactic spans show it.
                self.args(None, args, arg_in);
                return;
            };
            let quoted = self.is_quoted(cmd, name);
            let class = self.state.classify_name(name, quoted);
            let spec = self.specs.spec(name);
            let precommand = match spec {
                Some(s) => s.is_precommand(),
                None => FALLBACK_PRECOMMANDS.contains(&name),
            };
            // A function or global alias of the same name shadows the precommand. An alias
            // does not: `alias sudo='sudo '` is the common case.
            if precommand && !matches!(class, CommandClass::Function | CommandClass::GlobalAlias) {
                self.push(cmd, TokenKind::Precommand);
                let wrapped = match spec {
                    Some(s) => s.wrapped_command(arg_in),
                    None => fallback_wrapped(name, arg_in),
                }
                .filter(|&i| i < args.len());
                let own = wrapped.unwrap_or(args.len());
                // The precommand's own options; their plain arguments (`-u root`, `-n 5`) are
                // not path-checked.
                let kinds = spec.map(|s| s.classify_args(&arg_in[..own]));
                for (i, w) in args[..own].iter().enumerate() {
                    if self.global_alias(w) {
                        continue;
                    }
                    if let Some(Some(k)) = kinds.as_ref().and_then(|k| k.get(i)) {
                        self.push(w, *k);
                    }
                }
                match wrapped {
                    Some(i) => at += 1 + i,
                    None => return,
                }
                continue;
            }
            let kind = match class {
                CommandClass::Alias => Some(TokenKind::Alias),
                CommandClass::SuffixAlias => Some(TokenKind::SuffixAlias),
                CommandClass::GlobalAlias => Some(TokenKind::GlobalAlias),
                CommandClass::Function => Some(TokenKind::Function),
                CommandClass::Builtin => Some(TokenKind::Builtin),
                CommandClass::ReservedWord => Some(TokenKind::ReservedWord),
                CommandClass::External => Some(TokenKind::Command),
                CommandClass::Unknown => self.unknown_command(cmd, name),
            };
            if let Some(k) = kind {
                self.push(cmd, k);
            }
            self.args(spec, args, arg_in);
            return;
        }
    }

    /// The span for a command word that names no alias, function, builtin, or `$PATH` command.
    ///
    /// - A path containing `/` to an executable file is a command.
    /// - With `AUTO_CD`, an existing directory is a path-directory.
    /// - While the user is typing the word (cursor at its end):
    ///   - a path that is a prefix of an existing entry, or an existing directory, is a
    ///     path-prefix (the entry it leads to is not checked for executability);
    ///   - a name that is a prefix of a known alias, reserved word, function, builtin, or
    ///     `$PATH` command, or an unresolvable `~user` prefix, gets no span. Marking it as an
    ///     error on every keystroke until the name is complete would only flash red.
    /// - When the per-request filesystem budget is exhausted, the word gets no span rather
    ///   than a possibly wrong error.
    /// - Anything else is an error.
    fn unknown_command(&mut self, cmd: &Word, name: &str) -> Option<TokenKind> {
        let typing = self.typing(cmd);
        let has_slash = name.contains('/');
        let auto_cd = self.req.opts.auto_cd;
        if (has_slash || auto_cd) && crate::paths::could_be_path(name) {
            let resolved = crate::paths::resolve(name, cmd.tilde, self.req.cwd, self.state);
            if let Some(abs) = resolved {
                match self.paths.stat(&abs) {
                    None => return None,
                    Some(FileStat::File { executable: true }) if has_slash => {
                        return Some(TokenKind::Command);
                    }
                    Some(FileStat::Directory) if auto_cd => return Some(TokenKind::PathDirectory),
                    Some(FileStat::Directory) if typing => return Some(TokenKind::PathPrefix),
                    Some(FileStat::Missing) if typing => match self.paths.is_prefix(&abs) {
                        Some(true) => return Some(TokenKind::PathPrefix),
                        None => return None,
                        Some(false) => {}
                    },
                    Some(_) => {}
                }
            }
        }
        if typing && !has_slash && (cmd.tilde || self.state.is_command_prefix(name)) {
            return None;
        }
        Some(TokenKind::Error)
    }

    /// Arguments of a non-precommand: global aliases, then spec classification, then path
    /// checks.
    fn args(&mut self, spec: Option<&dyn Spec>, args: &[Word], inputs: &[ArgInput<'_>]) {
        if args.is_empty() {
            return;
        }
        let kinds = spec.map(|s| s.classify_args(inputs));
        for (i, w) in args.iter().enumerate() {
            if self.global_alias(w) {
                continue;
            }
            match kinds.as_ref().and_then(|k| k.get(i)) {
                Some(Some(k)) => self.push(w, *k),
                _ => self.path_arg(w),
            }
        }
    }

    /// Checks a literal word as a path. Only the word being typed can be a path-prefix; a
    /// missing path gets no span.
    fn path_arg(&mut self, w: &Word) {
        // Spans wholly inside PREBUFFER are clipped away, so skip the filesystem work.
        if w.end <= self.req.buffer_start || w.has_glob {
            return;
        }
        let Some(lit) = w.literal.as_deref() else {
            return;
        };
        let allow_prefix = self.typing(w);
        let kind = self
            .paths
            .check_word(lit, w.tilde, self.req.cwd, self.state, allow_prefix);
        let kind = match kind {
            PathKind::File => TokenKind::Path,
            PathKind::Directory => TokenKind::PathDirectory,
            PathKind::Prefix => TokenKind::PathPrefix,
            PathKind::Missing => return,
        };
        self.push(w, kind);
    }
}

/// Merges word-level semantic spans with the syntactic spans into one sorted, well-nested list.
///
/// Semantic spans go first so that, after the stable sort, a syntactic span with the identical
/// range comes later and wins. A semantic span that partially overlaps a syntactic one is
/// dropped (the syntactic span is kept). Spans out of bounds or off character boundaries are
/// dropped too.
fn merge(semantic: Vec<Span>, syntactic: &[Span], text: &str) -> Vec<Span> {
    let valid = |s: &Span| {
        s.start < s.end
            && s.end <= text.len()
            && text.is_char_boundary(s.start)
            && text.is_char_boundary(s.end)
    };
    let mut all: Vec<(Span, bool)> = semantic
        .into_iter()
        .map(|s| (s, true))
        .chain(syntactic.iter().map(|s| (*s, false)))
        .filter(|(s, _)| valid(s))
        .collect();
    all.sort_by(|a, b| a.0.start.cmp(&b.0.start).then(b.0.end.cmp(&a.0.end)));
    let mut keep = vec![true; all.len()];
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..all.len() {
        let s = all[i].0;
        while let Some(&top) = stack.last() {
            let (t, t_semantic) = all[top];
            if t.end <= s.start {
                stack.pop();
            } else if t.contains(&s) {
                break;
            } else if t_semantic {
                // Removing a span never creates a new conflict among the rest.
                keep[top] = false;
                stack.pop();
            } else {
                keep[i] = false;
                break;
            }
        }
        if keep[i] {
            stack.push(i);
        }
    }
    all.into_iter()
        .zip(keep)
        .filter_map(|((s, _), k)| k.then_some(s))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::StateUpdate;
    use crate::state::tests::{TempDir, empty_state};
    use crate::syntax::SimpleCommand;
    use TokenKind as K;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;

    /// A spec for tests: options start with `-` (those in `bad` are errors), `takes_arg`
    /// options consume the next word, the first positional is checked against `subcommands`.
    #[derive(Default)]
    struct FakeSpec {
        precommand: bool,
        takes_arg: &'static [&'static str],
        bad: &'static [&'static str],
        subcommands: &'static [&'static str],
        complete: bool,
        skip_assignments: bool,
    }

    impl Spec for FakeSpec {
        fn is_precommand(&self) -> bool {
            self.precommand
        }

        fn wrapped_command(&self, args: &[ArgInput<'_>]) -> Option<usize> {
            let mut i = 0;
            while i < args.len() {
                match args[i].literal {
                    Some(s) if self.takes_arg.contains(&s) => i += 2,
                    Some(s) if s.starts_with('-') => i += 1,
                    Some(s) if self.skip_assignments && s.contains('=') => i += 1,
                    _ => return Some(i),
                }
            }
            None
        }

        fn classify_args(&self, args: &[ArgInput<'_>]) -> Vec<Option<TokenKind>> {
            let mut out = Vec::new();
            let mut positional = 0;
            let mut skip = false;
            for a in args {
                let k = match a.literal {
                    _ if skip => {
                        skip = false;
                        None
                    }
                    Some(s) if self.bad.contains(&s) => Some(K::Error),
                    Some(s) if s.starts_with('-') => {
                        skip = self.takes_arg.contains(&s);
                        Some(K::CmdOption)
                    }
                    Some(s) => {
                        positional += 1;
                        if positional == 1 && !self.subcommands.is_empty() {
                            if self.subcommands.contains(&s) {
                                Some(K::Subcommand)
                            } else if self.complete {
                                Some(K::Error)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }
                    None => None,
                };
                out.push(k);
            }
            out
        }
    }

    struct FakeSpecs(HashMap<&'static str, FakeSpec>);

    impl SpecLookup for FakeSpecs {
        fn spec(&self, command: &str) -> Option<&dyn Spec> {
            self.0.get(command).map(|s| s as &dyn Spec)
        }
    }

    fn specs() -> FakeSpecs {
        let mut m = HashMap::new();
        m.insert(
            "sudo",
            FakeSpec {
                precommand: true,
                takes_arg: &["-u", "-g"],
                ..Default::default()
            },
        );
        m.insert(
            "env",
            FakeSpec {
                precommand: true,
                takes_arg: &["-u"],
                skip_assignments: true,
                ..Default::default()
            },
        );
        m.insert(
            "nice",
            FakeSpec {
                precommand: true,
                takes_arg: &["-n"],
                ..Default::default()
            },
        );
        m.insert(
            "git",
            FakeSpec {
                subcommands: &["status", "commit"],
                complete: true,
                bad: &["--bogus"],
                ..Default::default()
            },
        );
        m.insert("make", FakeSpec::default());
        FakeSpecs(m)
    }

    /// Splits `text` on single spaces into words. A word containing `$` has no literal; one
    /// containing `*` is a glob; a leading `~` sets `tilde`; quotes are removed from the literal.
    fn words_of(text: &str, base: usize) -> Vec<Word> {
        let mut out = Vec::new();
        let mut pos = base;
        for w in text.split(' ') {
            if !w.is_empty() {
                let has_glob = w.contains('*');
                let literal =
                    (!w.contains('$') && !has_glob).then(|| w.replace(['\'', '"', '\\'], ""));
                out.push(Word {
                    start: pos,
                    end: pos + w.len(),
                    literal,
                    tilde: w.starts_with('~'),
                    has_glob,
                });
            }
            pos += w.len() + 1;
        }
        out
    }

    /// A parse with one simple command per `;`-separated part of `text` (separators are
    /// `" ; "`), and no syntactic spans.
    fn parse_of(text: &str) -> ParseOutput {
        let mut commands = Vec::new();
        let mut pos = 0;
        for part in text.split(" ; ") {
            let words = words_of(part, pos);
            if !words.is_empty() {
                commands.push(SimpleCommand { words });
            }
            pos += part.len() + 3;
        }
        ParseOutput {
            spans: Vec::new(),
            commands,
            path_words: Vec::new(),
        }
    }

    struct Fixture {
        dir: TempDir,
        h: Highlighter,
        specs: FakeSpecs,
        now: Instant,
    }

    fn strings(v: &[&str]) -> Option<Vec<String>> {
        Some(v.iter().map(|s| (*s).to_owned()).collect())
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new("hl");
        for exe in ["git", "make", "sudo", "env", "nice", "ls", "gitk"] {
            dir.file(&format!("bin/{exe}"), 0o755);
        }
        dir.file("work/tool", 0o755);
        dir.file("work/notes.txt", 0o644);
        dir.dir("work/src/deep");
        dir.file("home/dotfile", 0o644);
        dir.dir("proj");
        let mut state = empty_state(&dir.path().join("bin").to_string_lossy());
        state.set_home(Some(dir.path().join("home")));
        state.apply_update(StateUpdate {
            aliases: strings(&["ll", "sudo"]),
            global_aliases: strings(&["G"]),
            suffix_aliases: strings(&["pdf"]),
            functions: strings(&["myfn"]),
            builtins: strings(&["echo", "cd", "noglob", "builtin", "command", "exec"]),
            reserved_words: strings(&["while", "typeset"]),
            named_dirs: Some(vec![(
                "pj".into(),
                dir.path().join("proj").to_string_lossy().into(),
            )]),
            ..Default::default()
        });
        let h = Highlighter {
            config: Config::default(),
            state,
            paths: PathChecker::default(),
            specs: SpecRegistry::default(),
        };
        Fixture {
            dir,
            h,
            specs: specs(),
            now: Instant::now(),
        }
    }

    impl Fixture {
        fn cwd(&self) -> PathBuf {
            self.dir.path().join("work")
        }

        fn run_opts(
            &mut self,
            text: &str,
            cursor: usize,
            opts: RequestOptions,
        ) -> Vec<(String, K)> {
            let cwd = self.cwd();
            let req = HighlightRequest {
                text,
                buffer_start: 0,
                cursor,
                cwd: &cwd,
                opts,
            };
            let spans = self
                .h
                .highlight_parsed(&req, parse_of(text), &self.specs, self.now);
            crate::token::check_spans(text, &spans).unwrap();
            spans
                .iter()
                .map(|s| (text[s.start..s.end].to_owned(), s.kind))
                .collect()
        }

        /// Highlights with the cursor away from every word (at offset 0 is inside the first
        /// word, so use a position that is no word end).
        fn run(&mut self, text: &str) -> Vec<(String, K)> {
            self.run_opts(text, usize::MAX, RequestOptions::default())
        }

        fn run_typing(&mut self, text: &str) -> Vec<(String, K)> {
            self.run_opts(text, text.len(), RequestOptions::default())
        }
    }

    fn spans(v: &[(&str, K)]) -> Vec<(String, K)> {
        v.iter().map(|(s, k)| ((*s).to_owned(), *k)).collect()
    }

    #[test]
    fn command_classes() {
        let mut f = fixture();
        let cases: &[(&str, K)] = &[
            ("ll", K::Alias),
            ("G", K::GlobalAlias),
            ("doc.pdf", K::SuffixAlias),
            ("myfn", K::Function),
            ("echo", K::Builtin),
            ("typeset", K::ReservedWord),
            ("ls", K::Command),
            ("./tool", K::Command),
            ("nosuch", K::Error),
            ("./notes.txt", K::Error),
            ("./src", K::Error),
        ];
        for (text, kind) in cases {
            assert_eq!(f.run(text), spans(&[(text, *kind)]), "{text}");
        }
        let tool = f.cwd().join("tool");
        let tool = tool.to_str().unwrap();
        assert_eq!(f.run(tool), spans(&[(tool, K::Command)]));
    }

    #[test]
    fn quoted_command_word_skips_aliases() {
        let mut f = fixture();
        // `\ll` has literal `ll` but is quoted: not an alias, and nothing else named `ll`.
        assert_eq!(f.run("\\ll"), spans(&[("\\ll", K::Error)]));
        assert_eq!(f.run("'ls'"), spans(&[("'ls'", K::Command)]));
    }

    #[test]
    fn precommand_chain_with_options() {
        let mut f = fixture();
        // `sudo` is also an alias in the fixture; the precommand still wins.
        assert_eq!(
            f.run("sudo -u root git status"),
            spans(&[
                ("sudo", K::Precommand),
                ("-u", K::CmdOption),
                ("git", K::Command),
                ("status", K::Subcommand),
            ])
        );
        assert_eq!(
            f.run("env FOO=1 nice -n 5 make"),
            spans(&[
                ("env", K::Precommand),
                ("nice", K::Precommand),
                ("-n", K::CmdOption),
                ("make", K::Command)
            ])
        );
        assert_eq!(
            f.run("sudo env nosuch"),
            spans(&[
                ("sudo", K::Precommand),
                ("env", K::Precommand),
                ("nosuch", K::Error)
            ])
        );
    }

    #[test]
    fn precommand_without_wrapped_command() {
        let mut f = fixture();
        assert_eq!(
            f.run("sudo -u"),
            spans(&[("sudo", K::Precommand), ("-u", K::CmdOption)])
        );
        assert_eq!(f.run("sudo"), spans(&[("sudo", K::Precommand)]));
    }

    #[test]
    fn fallback_precommands_without_specs() {
        let mut f = fixture();
        assert_eq!(
            f.run("noglob ls"),
            spans(&[("noglob", K::Precommand), ("ls", K::Command)])
        );
        assert_eq!(
            f.run("exec -a name ls"),
            spans(&[("exec", K::Precommand), ("ls", K::Command)])
        );
        assert_eq!(
            f.run("command -- git bad"),
            spans(&[
                ("command", K::Precommand),
                ("git", K::Command),
                ("bad", K::Error)
            ])
        );
        assert_eq!(
            f.run("builtin echo"),
            spans(&[("builtin", K::Precommand), ("echo", K::Builtin)])
        );
        assert_eq!(
            f.run("- ls"),
            spans(&[("-", K::Precommand), ("ls", K::Command)])
        );
    }

    #[test]
    fn function_shadows_precommand() {
        let mut f = fixture();
        f.h.state.apply_update(StateUpdate {
            functions: strings(&["noglob"]),
            ..Default::default()
        });
        assert_eq!(f.run("noglob ls"), spans(&[("noglob", K::Function)]));
    }

    #[test]
    fn long_precommand_chain_does_not_recurse() {
        let mut f = fixture();
        let text = "- ".repeat(5000) + "ls";
        let out = f.run(&text);
        assert_eq!(out.len(), 5001);
        assert_eq!(out.last().unwrap().1, K::Command);
    }

    #[test]
    fn spec_arguments() {
        let mut f = fixture();
        assert_eq!(
            f.run("git --bogus nope notes.txt"),
            spans(&[
                ("git", K::Command),
                ("--bogus", K::Error),
                ("nope", K::Error),
                ("notes.txt", K::Path)
            ])
        );
        // A global alias in an argument wins over the spec.
        assert_eq!(
            f.run("git G"),
            spans(&[("git", K::Command), ("G", K::GlobalAlias)])
        );
    }

    #[test]
    fn unknown_command_while_typing() {
        let mut f = fixture();
        // A prefix of a known name at the cursor: unstyled.
        for text in ["gi", "my", "ec", "whi", "l"] {
            assert_eq!(f.run_typing(text), vec![], "{text}");
        }
        // Not at the cursor, or not a prefix: error.
        assert_eq!(f.run("gi"), spans(&[("gi", K::Error)]));
        assert_eq!(f.run_typing("gix"), spans(&[("gix", K::Error)]));
        // A path prefix at the cursor.
        assert_eq!(f.run_typing("./to"), spans(&[("./to", K::PathPrefix)]));
        assert_eq!(f.run_typing("./src"), spans(&[("./src", K::PathPrefix)]));
        assert_eq!(f.run_typing("./zz"), spans(&[("./zz", K::Error)]));
        assert_eq!(f.run("./to"), spans(&[("./to", K::Error)]));
        // A `~user` that is not resolvable yet, while typing.
        assert_eq!(f.run_typing("~no_such_us"), vec![]);
        // The cursor at the end of a later word does not affect the command word.
        assert_eq!(f.run_typing("gi x"), spans(&[("gi", K::Error)]));
    }

    #[test]
    fn auto_cd() {
        let mut f = fixture();
        let auto = RequestOptions {
            auto_cd: true,
            ..Default::default()
        };
        assert_eq!(
            f.run_opts("src", usize::MAX, auto),
            spans(&[("src", K::PathDirectory)])
        );
        assert_eq!(
            f.run_opts("~pj", usize::MAX, auto),
            spans(&[("~pj", K::PathDirectory)])
        );
        assert_eq!(f.run_opts("sr", 2, auto), spans(&[("sr", K::PathPrefix)]));
        assert_eq!(f.run("src"), spans(&[("src", K::Error)]));
    }

    #[test]
    fn path_arguments() {
        let mut f = fixture();
        assert_eq!(
            f.run("ls notes.txt src src/ src/deep missing -la ~/dotfile ~pj ~pj/x ~- *.txt $HOME"),
            spans(&[
                ("ls", K::Command),
                ("notes.txt", K::Path),
                ("src", K::PathDirectory),
                ("src/", K::PathDirectory),
                ("src/deep", K::PathDirectory),
                ("~/dotfile", K::Path),
                ("~pj", K::PathDirectory),
            ])
        );
        // A quoted tilde is literal, relative to the cwd.
        assert_eq!(f.run("ls '~'/dotfile"), spans(&[("ls", K::Command)]));
        // Absolute paths.
        let abs = format!("ls {}", f.cwd().join("notes.txt").display());
        assert_eq!(f.run(&abs).len(), 2);
    }

    #[test]
    fn path_prefix_only_at_cursor() {
        let mut f = fixture();
        assert_eq!(f.run("ls not"), spans(&[("ls", K::Command)]));
        assert_eq!(
            f.run_typing("ls not"),
            spans(&[("ls", K::Command), ("not", K::PathPrefix)])
        );
        assert_eq!(
            f.run_typing("ls src/de"),
            spans(&[("ls", K::Command), ("src/de", K::PathPrefix)])
        );
        assert_eq!(
            f.run_typing("ls ~/dot"),
            spans(&[("ls", K::Command), ("~/dot", K::PathPrefix)])
        );
        assert_eq!(f.run_typing("ls zzz"), spans(&[("ls", K::Command)]));
        // Cursor at the end of the first argument, not the last.
        assert_eq!(
            f.run_opts("ls not src", 6, RequestOptions::default()),
            spans(&[
                ("ls", K::Command),
                ("not", K::PathPrefix),
                ("src", K::PathDirectory)
            ])
        );
    }

    #[test]
    fn non_literal_command_word_still_checks_arguments() {
        let mut f = fixture();
        assert_eq!(f.run("$EDITOR notes.txt"), spans(&[("notes.txt", K::Path)]));
    }

    #[test]
    fn path_words_and_global_aliases() {
        let mut f = fixture();
        let text = "echo G > notes.txt";
        let mut parse = parse_of("echo G");
        parse.path_words = words_of("notes.txt", 9);
        let cwd = f.cwd();
        let req = HighlightRequest {
            text,
            buffer_start: 0,
            cursor: usize::MAX,
            cwd: &cwd,
            opts: RequestOptions::default(),
        };
        let out = f.h.highlight_parsed(&req, parse, &f.specs, f.now);
        assert_eq!(
            out,
            vec![
                Span::new(0, 4, K::Builtin),
                Span::new(5, 6, K::GlobalAlias),
                Span::new(9, 18, K::Path)
            ]
        );
    }

    #[test]
    fn prebuffer_words_skip_path_checks() {
        let mut f = fixture();
        let text = "ls notes.txt \\\nsrc";
        let mut parse = parse_of("ls notes.txt");
        parse.commands[0].words.extend(words_of("src", 15));
        let cwd = f.cwd();
        let req = HighlightRequest {
            text,
            buffer_start: 15,
            cursor: usize::MAX,
            cwd: &cwd,
            opts: RequestOptions::default(),
        };
        let out = f.h.highlight_parsed(&req, parse, &f.specs, f.now);
        assert_eq!(
            out,
            vec![
                Span::new(0, 2, K::Command),
                Span::new(15, 18, K::PathDirectory)
            ]
        );
    }

    #[test]
    fn check_budget_exhaustion_is_not_an_error() {
        let mut f = fixture();
        f.h.config.limits.max_path_checks = 1;
        assert_eq!(
            f.run("ls notes.txt src ; ./tool"),
            spans(&[("ls", K::Command), ("notes.txt", K::Path)])
        );
        // The next request has a new budget; `notes.txt` is cached.
        f.now += Duration::from_millis(10);
        assert_eq!(
            f.run("ls notes.txt ./tool"),
            spans(&[
                ("ls", K::Command),
                ("notes.txt", K::Path),
                ("./tool", K::Path)
            ])
        );
    }

    #[test]
    fn size_thresholds() {
        let mut f = fixture();
        let text = "ls src";
        let syntactic = vec![Span::new(3, 6, K::Glob)];
        let cwd = f.cwd();
        let req = HighlightRequest {
            text,
            buffer_start: 0,
            cursor: usize::MAX,
            cwd: &cwd,
            opts: RequestOptions::default(),
        };
        let mut parse = parse_of(text);
        parse.spans = syntactic.clone();

        f.h.config.limits.lex_only_bytes = 5;
        assert_eq!(
            f.h.highlight_parsed(&req, parse.clone(), &f.specs, f.now),
            syntactic
        );
        f.h.config.limits.lex_only_bytes = 6;
        assert_eq!(
            f.h.highlight_parsed(&req, parse.clone(), &f.specs, f.now)
                .len(),
            3
        );

        f.h.config.limits.hard_cap_bytes = 5;
        assert_eq!(
            f.h.highlight_parsed(&req, parse.clone(), &f.specs, f.now),
            vec![]
        );
        assert_eq!(f.h.highlight(&req), vec![]);
        f.h.config.limits.hard_cap_bytes = 6;
        assert_eq!(f.h.highlight_parsed(&req, parse, &f.specs, f.now).len(), 3);
    }

    #[test]
    fn merge_orders_and_nests() {
        let text = "echo \"a$b\" 'x' zzz";
        let semantic = vec![
            Span::new(0, 4, K::Builtin),
            Span::new(5, 10, K::Path),
            Span::new(11, 14, K::Path),
            Span::new(15, 17, K::Path),
        ];
        let syntactic = vec![
            Span::new(5, 10, K::DoubleQuoted),
            Span::new(7, 9, K::Parameter),
            Span::new(11, 14, K::SingleQuoted),
            Span::new(16, 18, K::Glob),
        ];
        let out = merge(semantic, &syntactic, text);
        assert_eq!(
            out,
            vec![
                Span::new(0, 4, K::Builtin),
                // Identical range: semantic first, syntactic last (wins).
                Span::new(5, 10, K::Path),
                Span::new(5, 10, K::DoubleQuoted),
                Span::new(7, 9, K::Parameter),
                Span::new(11, 14, K::Path),
                Span::new(11, 14, K::SingleQuoted),
                // `15..17` partially overlaps `16..18` and is dropped.
                Span::new(16, 18, K::Glob),
            ]
        );
        crate::token::check_spans(text, &out).unwrap();
    }

    #[test]
    fn merge_semantic_containing_syntactic() {
        let text = "ls a$x/b c";
        let out = merge(
            vec![Span::new(3, 8, K::Path), Span::new(9, 10, K::Path)],
            &[
                Span::new(0, 10, K::Substitution),
                Span::new(4, 6, K::Parameter),
            ],
            text,
        );
        assert_eq!(
            out,
            vec![
                Span::new(0, 10, K::Substitution),
                Span::new(3, 8, K::Path),
                Span::new(4, 6, K::Parameter),
                Span::new(9, 10, K::Path),
            ]
        );
    }

    #[test]
    fn merge_drops_semantic_span_starting_inside_a_syntactic_span() {
        let text = "abcdefgh";
        let out = merge(
            vec![Span::new(2, 6, K::Path)],
            &[Span::new(0, 4, K::DoubleQuoted)],
            text,
        );
        assert_eq!(out, vec![Span::new(0, 4, K::DoubleQuoted)]);
        let out = merge(
            vec![Span::new(0, 99, K::Path), Span::new(1, 2, K::Path)],
            &[],
            "é",
        );
        assert_eq!(out, vec![]);
    }

    #[test]
    fn budget_for_a_200_char_buffer() {
        let mut f = fixture();
        let mut text = String::new();
        while text.len() < 200 {
            text.push_str("sudo -u root git status notes.txt src ; ls ~/dotfile ; ");
        }
        text.push_str("ech");
        let cwd = f.cwd();
        let parse = parse_of(&text);
        // Warm the caches, then time repeated requests within one TTL.
        let req = HighlightRequest {
            text: &text,
            buffer_start: 0,
            cursor: text.len(),
            cwd: &cwd,
            opts: RequestOptions::default(),
        };
        f.h.highlight_parsed(&req, parse.clone(), &f.specs, f.now);
        let start = Instant::now();
        let n = 200;
        for _ in 0..n {
            f.h.highlight_parsed(&req, parse.clone(), &f.specs, f.now);
        }
        let per = start.elapsed() / n;
        // Generous for unoptimised builds; release builds are far below 1 ms.
        assert!(per < Duration::from_millis(5), "{per:?} per request");
    }
}
