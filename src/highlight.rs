//! The semantic pass: combines a parse with shell state, specs, and the filesystem.
//!
//! The parser ([`crate::syntax::parse`]) produces syntactic spans and a list of simple commands.
//! This pass classifies each command word (alias, builtin, external command, unknown, ...),
//! unwraps precommands such as `sudo` and `env`, applies per-command specs to arguments, and
//! checks literal arguments as paths on disk. The resulting word-level spans are merged under
//! the syntactic spans, so a syntactic span with the same range wins.

use crate::config::{Config, Limits};
use crate::paths::{FileStat, PathChecker, PathKind, Resolution};
use crate::specs::{ArgInput, CommandSpec, SpecRegistry, Tail};
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
    /// `NO_EQUALS`: a word starting with an unquoted `=` is an ordinary word. With `EQUALS`, the
    /// zsh default, it names an external command (see [`Word::equals`]).
    pub no_equals: bool,
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
    /// See [`CommandSpec::tail`].
    fn tail(&self, args: &[ArgInput<'_>]) -> Tail;
    /// See [`CommandSpec::classify_args`].
    fn classify_args(&self, args: &[ArgInput<'_>]) -> Vec<Option<TokenKind>>;
    /// See [`CommandSpec::completes_at`].
    fn completes_at(&self, args: &[ArgInput<'_>], index: usize) -> bool;
}

/// Finds the spec for a command name. Implemented by [`SpecRegistry`].
pub trait SpecLookup {
    fn spec(&self, command: &str) -> Option<&dyn Spec>;
}

impl Spec for CommandSpec {
    fn is_precommand(&self) -> bool {
        CommandSpec::is_precommand(self)
    }

    fn tail(&self, args: &[ArgInput<'_>]) -> Tail {
        CommandSpec::tail(self, args)
    }

    fn classify_args(&self, args: &[ArgInput<'_>]) -> Vec<Option<TokenKind>> {
        CommandSpec::classify_args(self, args)
    }

    fn completes_at(&self, args: &[ArgInput<'_>], index: usize) -> bool {
        CommandSpec::completes_at(self, args, index)
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
        self.highlight_at(req, Instant::now())
    }

    /// [`Highlighter::highlight`] with an explicit clock for the filesystem cache, for tests.
    pub fn highlight_at(&mut self, req: &HighlightRequest<'_>, now: Instant) -> Vec<Span> {
        if req.text.len() > self.config.limits.hard_cap_bytes {
            return Vec::new();
        }
        if req.text.len() > self.config.limits.lex_only_bytes {
            // Lex-only: `run` would return the syntactic spans unchanged.
            return crate::syntax::parse_spans(req.text, &req.opts.parse);
        }
        let parse = crate::syntax::parse(req.text, &req.opts.parse);
        let Highlighter {
            config,
            state,
            paths,
            specs,
        } = self;
        run(&config.limits, state, paths, specs, req, parse, now)
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
    // Only a pending PATH change or `rehash` rescans here; the periodic mtime check runs
    // between requests (see `Engine::idle`).
    state.scan_path_if_dirty(now);
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
            pass.path_arg(w, false);
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
            name_eq: w.name_eq,
        })
        .collect()
}

/// For a precommand without a spec, the wrapped command in `args`: the first word that is not
/// an option (after `--`, the next word). `exec -a NAME` takes an argument.
fn fallback_wrapped(name: &str, args: &[ArgInput<'_>]) -> Tail {
    let mut i = 0;
    while i < args.len() {
        match args[i].literal {
            Some("--") if i + 1 < args.len() => return Tail::Command(i + 1),
            Some("--") => return Tail::None,
            Some("-a") if name == "exec" => i += 2,
            Some(s) if s.starts_with('-') && s.len() > 1 => i += 1,
            _ => return Tail::Command(i),
        }
    }
    Tail::None
}

/// The options of `command` in `args`: the index of the first word after them, and whether
/// `-v` or `-V` makes the remaining words lookups rather than a command. An option word is `-`
/// followed by one or more of `p`, `v`, and `V`; `--` ends the options and is skipped. Any
/// other word, a lone `-` included, is the command word.
///
/// zsh takes several option words only when a `v` or `V` appears among them (`-p -v ls`, `-v
/// -p -p ls`). Without one, at most one option word may come before `--` (`-p ls`, `-p -- ls`).
/// A second one (`-p -p ls`), or an invalid one after consumed ones (`-p -x ls`), makes the
/// first option word the command word, so nothing is consumed.
///
/// The grammar of `command` and `builtin` is built in and keyed by name: it overrides the
/// spec, which only decides whether they are precommands at all. [`fallback_wrapped`], by
/// contrast, runs only for a precommand with no spec.
fn command_options(args: &[ArgInput<'_>]) -> (usize, bool) {
    let (mut query, mut words) = (false, 0);
    let mut end = args.len();
    let mut bad_option = false;
    for (i, a) in args.iter().enumerate() {
        match a.literal {
            Some("--") => {
                end = i + 1;
                break;
            }
            Some(s) if s.len() > 1 && s.starts_with('-') => {
                if !s[1..].bytes().all(|b| matches!(b, b'p' | b'v' | b'V')) {
                    bad_option = true;
                    end = i;
                    break;
                }
                query |= s.contains(['v', 'V']);
                words += 1;
            }
            _ => {
                end = i;
                break;
            }
        }
    }
    if words > 0 && (bad_option || (words > 1 && !query)) {
        return (0, false);
    }
    (end, query)
}

/// For the precommand `name`: the wrapped command, whether the words from it on are `command
/// -v` lookups, and the restriction on the wrapped command word. `builtin` wraps its first
/// argument, whatever it is; `command` follows [`command_options`]; any other precommand
/// follows its spec, or [`fallback_wrapped`] without one, and restricts nothing.
fn precommand_tail(
    name: &str,
    spec: Option<&dyn Spec>,
    args: &[ArgInput<'_>],
) -> (Tail, bool, Wrap) {
    let at = |i: usize| match i < args.len() {
        true => Tail::Command(i),
        false => Tail::None,
    };
    match (name, spec) {
        ("builtin", _) => (at(0), false, Wrap::Builtin),
        ("command", _) => {
            let (i, query) = command_options(args);
            let next = if query { Wrap::None } else { Wrap::Command };
            (at(i), query, next)
        }
        (_, Some(s)) => (s.tail(args), false, Wrap::None),
        (_, None) => (fallback_wrapped(name, args), false, Wrap::None),
    }
}

/// The span kind of a command word of a known class; `None` for [`CommandClass::Unknown`].
fn class_kind(class: CommandClass) -> Option<TokenKind> {
    match class {
        CommandClass::Alias => Some(TokenKind::Alias),
        CommandClass::SuffixAlias => Some(TokenKind::SuffixAlias),
        CommandClass::GlobalAlias => Some(TokenKind::GlobalAlias),
        CommandClass::Function => Some(TokenKind::Function),
        CommandClass::Builtin => Some(TokenKind::Builtin),
        CommandClass::ReservedWord => Some(TokenKind::ReservedWord),
        CommandClass::External => Some(TokenKind::Command),
        CommandClass::Unknown => None,
    }
}

/// What the precommand before a command word restricts that word to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wrap {
    /// Full command lookup: the first word, or the word after any other precommand.
    None,
    /// After `builtin`: builtins only.
    Builtin,
    /// After `command` without `-v` or `-V`: external commands only.
    Command,
}

/// The classification of a command word.
enum Wrapped {
    /// A precommand: the chain continues with the command it wraps.
    Precommand,
    /// Any other command word, with its span kind (none while it is still being typed or
    /// cannot be resolved yet).
    Kind(Option<TokenKind>),
}

/// Which names [`Pass::unknown_command`] may still accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lookup {
    /// A command word with full lookup: `AUTO_CD` directories count, and a prefix of any
    /// command name is left unstyled while typed.
    Top,
    /// The word after `command`: only an explicit path to an executable file, and only a
    /// prefix of a `$PATH` command is left unstyled while typed.
    External,
    /// A word after `command -v` or `-V`: only an explicit path to an executable file, and a
    /// prefix of any command name is left unstyled while typed.
    Query,
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

    /// Classifies one simple command, following precommand chains and other wrapped commands
    /// (`kubectl exec POD -- cmd`) iteratively.
    fn command(&mut self, words: &[Word]) {
        let inputs = arg_inputs(words);
        let mut at = 0;
        let mut wrap = Wrap::None;
        while at < words.len() {
            let (cmd, args, arg_in) = (&words[at], &words[at + 1..], &inputs[at + 1..]);
            let Some(name) = cmd.literal.as_deref() else {
                // An expansion in command position: the syntactic spans show it.
                self.args(None, args, arg_in);
                return;
            };
            // `=cmd` runs the external command `cmd`, found as after `command`. After
            // `builtin` the path it expands to is never a builtin, so the word is kept whole.
            let (name, wrap_word) = match self.equals_target(cmd, name) {
                Some(target) if wrap != Wrap::Builtin => (target, Wrap::Command),
                _ => (name, wrap),
            };
            let spec = self.specs.spec(name);
            let precommand = match spec {
                Some(s) => s.is_precommand(),
                None => FALLBACK_PRECOMMANDS.contains(&name),
            };
            let kind = match self.wrapped_word(wrap_word, cmd, name, precommand) {
                Wrapped::Kind(kind) => kind,
                Wrapped::Precommand => {
                    self.push(cmd, TokenKind::Precommand);
                    let (tail, query, next) = precommand_tail(name, spec, arg_in);
                    match tail {
                        Tail::Command(i) if i < args.len() => {
                            self.own_args(name, spec, &args[..i], &arg_in[..i]);
                            if query {
                                for w in &args[i..] {
                                    self.query_word(w);
                                }
                                return;
                            }
                            at += 1 + i;
                            wrap = next;
                            continue;
                        }
                        // Operands, not a command (`sudo -e FILE`): checked as paths.
                        Tail::Paths => self.args(spec, args, arg_in),
                        Tail::Command(_) | Tail::None => self.own_args(name, spec, args, arg_in),
                    }
                    return;
                }
            };
            if let Some(k) = kind {
                self.push(cmd, k);
            }
            // A word `builtin` or `command` would not run wraps nothing, whatever its spec
            // says: its arguments are only checked as paths. So does an unknown `=cmd`.
            if wrap_word != Wrap::None && kind == Some(TokenKind::Error) {
                self.args(None, args, arg_in);
                return;
            }
            // A command that wraps another without being a precommand (`kubectl exec POD --
            // cmd`): its own arguments as usual, then the wrapped command word. A precommand
            // shadowed by a function or global alias wraps nothing.
            if !precommand
                && let Some(s) = spec
                && let Tail::Command(i) = s.tail(arg_in)
                && i < args.len()
            {
                self.args(spec, &args[..i], &arg_in[..i]);
                at += 1 + i;
                wrap = Wrap::None;
                continue;
            }
            self.args(spec, args, arg_in);
            return;
        }
    }

    /// Classifies the literal command word `name` under the restriction `wrap` left by the
    /// precommand before it. `precommand` says whether a spec or the fallback list makes the
    /// name a precommand.
    ///
    /// - [`Wrap::None`]: full lookup. A function or global alias of the same name shadows a
    ///   precommand; an alias does not (`alias sudo='sudo '` is the common case).
    /// - [`Wrap::Builtin`]: only a builtin, by name. There is no alias expansion and no shadow
    ///   check; a prefix of a builtin being typed gets no span; anything else is an error.
    /// - [`Wrap::Command`]: only a `$PATH` command or an explicit path to an executable file,
    ///   with no shadow check; see [`Lookup::External`] for the rest.
    fn wrapped_word(&mut self, wrap: Wrap, cmd: &Word, name: &str, precommand: bool) -> Wrapped {
        let accepted = |kind| match precommand {
            true => Wrapped::Precommand,
            false => Wrapped::Kind(Some(kind)),
        };
        match wrap {
            Wrap::None => {
                let class = self.state.classify_name(name, self.is_quoted(cmd, name));
                if precommand
                    && !matches!(class, CommandClass::Function | CommandClass::GlobalAlias)
                {
                    return Wrapped::Precommand;
                }
                Wrapped::Kind(
                    class_kind(class).or_else(|| self.unknown_command(cmd, name, Lookup::Top)),
                )
            }
            Wrap::Builtin if self.state.is_builtin(name) => accepted(TokenKind::Builtin),
            Wrap::Builtin if self.typing(cmd) && self.state.is_builtin_prefix(name) => {
                Wrapped::Kind(None)
            }
            Wrap::Builtin => Wrapped::Kind(Some(TokenKind::Error)),
            Wrap::Command if self.state.is_path_command(name) => accepted(TokenKind::Command),
            Wrap::Command => Wrapped::Kind(self.unknown_command(cmd, name, Lookup::External)),
        }
    }

    /// A word after `command -v` or `-V`: styled by what it names, as the lookup reports it,
    /// whether or not it is quoted. A word that names nothing is an error.
    fn query_word(&mut self, w: &Word) {
        let Some(name) = w.literal.as_deref() else {
            return;
        };
        if let Some(target) = self.equals_target(w, name) {
            if let Some(k) = self.external_command(w, target) {
                self.push(w, k);
            }
            return;
        }
        let kind = class_kind(self.state.classify_name(name, false))
            .or_else(|| self.unknown_command(w, name, Lookup::Query));
        if let Some(k) = kind {
            self.push(w, k);
        }
    }

    /// The command name in a word subject to `=` expansion: the literal after the `=`, when
    /// `EQUALS` is set and the word is not a lone `=`.
    fn equals_target<'w>(&self, w: &Word, literal: &'w str) -> Option<&'w str> {
        if self.req.opts.no_equals || !w.equals {
            return None;
        }
        literal.strip_prefix('=').filter(|n| !n.is_empty())
    }

    /// The span for the word `w` expanding to the path of the external command `name`
    /// (`=name`): a `$PATH` command, or with a `/` an executable file, is a command; zsh fails
    /// on anything else. While typed, see [`Lookup::External`].
    fn external_command(&mut self, w: &Word, name: &str) -> Option<TokenKind> {
        if self.state.is_path_command(name) {
            return Some(TokenKind::Command);
        }
        self.unknown_command(w, name, Lookup::External)
    }

    /// A precommand's own arguments: global aliases and spec classification. Their plain
    /// arguments (`-u root`, `-n 5`) are not path-checked. The options of `command` are styled
    /// by its built-in grammar (see [`command_options`]), whatever its spec says.
    fn own_args(
        &mut self,
        name: &str,
        spec: Option<&dyn Spec>,
        args: &[Word],
        inputs: &[ArgInput<'_>],
    ) {
        if name == "command" {
            for w in args {
                if w.literal.as_deref() != Some("--") {
                    self.push(w, TokenKind::CmdOption);
                }
            }
            return;
        }
        let Some(s) = spec else {
            for w in args {
                self.global_alias(w);
            }
            return;
        };
        let kinds = s.classify_args(inputs);
        for (i, w) in args.iter().enumerate() {
            if self.global_alias(w) {
                continue;
            }
            if let Some(k) = self.spec_kind(s, inputs, &kinds, i, w) {
                self.push(w, k);
            }
        }
    }

    /// The span for a command word that names no alias, function, builtin, or `$PATH` command.
    ///
    /// - A path containing `/` to an executable file is a command.
    /// - With `AUTO_CD`, an existing directory is a path-directory, for [`Lookup::Top`] only.
    /// - While the user is typing the word (cursor at its end):
    ///   - a path that is a prefix of an existing entry, or an existing directory, is a
    ///     path-prefix (the entry it leads to is not checked for executability);
    ///   - a name that is a prefix of a known alias, reserved word, function, builtin, or
    ///     `$PATH` command (for [`Lookup::External`], of a `$PATH` command only), or an
    ///     unresolvable `~user` prefix, gets no span. Marking it as an error on every keystroke
    ///     until the name is complete would only flash red.
    /// - When the per-request filesystem budget is exhausted (or a `~user` lookup is deferred,
    ///   see [`PathChecker::resolve`]), or a prefix check could not tell, the word gets no span
    ///   rather than a possibly wrong error.
    /// - Anything else is an error.
    fn unknown_command(&mut self, cmd: &Word, name: &str, lookup: Lookup) -> Option<TokenKind> {
        let typing = self.typing(cmd);
        let has_slash = name.contains('/');
        let auto_cd = self.req.opts.auto_cd && lookup == Lookup::Top;
        if (has_slash || auto_cd) && crate::paths::could_be_path(name) {
            let resolved = self
                .paths
                .resolve(name, cmd.tilde, self.req.cwd, self.state, typing);
            if let Resolution::Deferred = resolved {
                return None;
            }
            if let Resolution::Path(abs) = resolved {
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
        let prefix = |state: &ShellState| match lookup {
            Lookup::Top | Lookup::Query => state.is_command_prefix(name),
            Lookup::External => state.is_path_command_prefix(name),
        };
        if typing && !has_slash && (cmd.tilde || prefix(self.state)) {
            return None;
        }
        Some(TokenKind::Error)
    }

    /// Arguments of a non-precommand: global aliases, then spec classification, then path
    /// checks. After a `--` argument, words starting with `-` are operands and path-checked
    /// too.
    fn args(&mut self, spec: Option<&dyn Spec>, args: &[Word], inputs: &[ArgInput<'_>]) {
        if args.is_empty() {
            return;
        }
        let kinds = spec.map(|s| s.classify_args(inputs));
        let mut options_ended = false;
        for (i, w) in args.iter().enumerate() {
            let after_dashdash = options_ended;
            options_ended |= w.literal.as_deref() == Some("--");
            if self.global_alias(w) {
                continue;
            }
            let (Some(s), Some(kinds)) = (spec, kinds.as_ref()) else {
                self.path_arg(w, after_dashdash);
                continue;
            };
            match self.spec_kind(s, inputs, kinds, i, w) {
                Some(k) => self.push(w, k),
                // A suppressed error is a subcommand or option being typed, not a path.
                None if kinds.get(i) == Some(&Some(TokenKind::Error)) => {}
                None => self.path_arg(w, after_dashdash),
            }
        }
    }

    /// The spec classification of argument `i`, except that an error for the word being typed
    /// is dropped when the word is a prefix of a valid subcommand or option there: marking it
    /// red on every keystroke until it is complete would only flash, as for command words.
    fn spec_kind(
        &self,
        spec: &dyn Spec,
        inputs: &[ArgInput<'_>],
        kinds: &[Option<TokenKind>],
        i: usize,
        w: &Word,
    ) -> Option<TokenKind> {
        match kinds.get(i).copied().flatten() {
            Some(TokenKind::Error) if self.typing(w) && spec.completes_at(inputs, i) => None,
            k => k,
        }
    }

    /// Checks a literal word as a path. Only the word being typed can be a path-prefix; a
    /// missing path gets no span. A word starting with `-` is an option and not checked unless
    /// `options_ended` (it follows `--`). A word subject to `=` expansion is checked as an
    /// external command instead (see [`Pass::external_command`]).
    fn path_arg(&mut self, w: &Word, options_ended: bool) {
        // Spans wholly inside PREBUFFER are clipped away, so skip the filesystem work.
        if w.end <= self.req.buffer_start || w.has_glob {
            return;
        }
        let Some(lit) = w.literal.as_deref() else {
            return;
        };
        if let Some(target) = self.equals_target(w, lit) {
            if let Some(k) = self.external_command(w, target) {
                self.push(w, k);
            }
            return;
        }
        if lit.starts_with('-') && !options_ended {
            return;
        }
        let typing = self.typing(w);
        let kind = self
            .paths
            .check_word(lit, w.tilde, self.req.cwd, self.state, typing);
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
/// range comes later and wins, except that a semantic error goes last: a quoted unknown command
/// (`'nosuch'`) must still show as an error. A semantic span that partially overlaps a syntactic
/// one is dropped (the syntactic span is kept). Spans out of bounds or off character boundaries
/// are dropped too.
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
    // Among identical ranges: semantic, then syntactic, then a semantic error.
    let rank = |(s, semantic): &(Span, bool)| match (semantic, s.kind) {
        (true, TokenKind::Error) => 2,
        (true, _) => 0,
        (false, _) => 1,
    };
    all.sort_by(|a, b| {
        a.0.start
            .cmp(&b.0.start)
            .then(b.0.end.cmp(&a.0.end))
            .then(rank(a).cmp(&rank(b)))
    });
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
    /// A precommand wraps the first word after its options, unless a `path_mode` option came
    /// first; with `wrapped_after`, the word after the first `--` is wrapped.
    #[derive(Default)]
    struct FakeSpec {
        precommand: bool,
        takes_arg: &'static [&'static str],
        bad: &'static [&'static str],
        subcommands: &'static [&'static str],
        complete: bool,
        skip_assignments: bool,
        path_mode: &'static [&'static str],
        wrapped_after: bool,
    }

    impl Spec for FakeSpec {
        fn is_precommand(&self) -> bool {
            self.precommand
        }

        fn tail(&self, args: &[ArgInput<'_>]) -> Tail {
            if self.wrapped_after {
                return match args.iter().position(|a| a.literal == Some("--")) {
                    Some(j) if j + 1 < args.len() => Tail::Command(j + 1),
                    _ => Tail::None,
                };
            }
            if !self.precommand {
                return Tail::None;
            }
            let mut paths = false;
            let mut i = 0;
            while i < args.len() {
                match args[i].literal {
                    Some(s) if self.takes_arg.contains(&s) => i += 2,
                    Some(s) if s.starts_with('-') => {
                        paths |= self.path_mode.contains(&s);
                        i += 1;
                    }
                    _ if paths => return Tail::Paths,
                    _ if self.skip_assignments && args[i].name_eq => i += 1,
                    _ => return Tail::Command(i),
                }
            }
            if paths { Tail::Paths } else { Tail::None }
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

        /// A proper prefix of a subcommand name, wherever the word is.
        fn completes_at(&self, args: &[ArgInput<'_>], index: usize) -> bool {
            args[index].literal.is_some_and(|w| {
                self.subcommands
                    .iter()
                    .any(|s| s.len() > w.len() && s.starts_with(w))
            })
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
                path_mode: &["-e"],
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
        m.insert(
            "kubectl",
            FakeSpec {
                subcommands: &["exec", "get"],
                takes_arg: &["-n"],
                wrapped_after: true,
                ..Default::default()
            },
        );
        FakeSpecs(m)
    }

    /// Splits `text` on single spaces into words. A word containing `$` has no literal; one
    /// containing `*` is a glob; a leading `~` sets `tilde`; quotes are removed from the literal.
    /// `name_eq` is decided by the text before the first `$` or `*`, as the parser does.
    fn words_of(text: &str, base: usize) -> Vec<Word> {
        let mut out = Vec::new();
        let mut pos = base;
        for w in text.split(' ') {
            if !w.is_empty() {
                let has_glob = w.contains('*');
                let literal =
                    (!w.contains('$') && !has_glob).then(|| w.replace(['\'', '"', '\\'], ""));
                let head = w.split(['$', '*']).next().unwrap_or_default();
                out.push(Word {
                    start: pos,
                    end: pos + w.len(),
                    literal,
                    tilde: w.starts_with('~'),
                    has_glob,
                    name_eq: crate::syntax::is_name_eq(&head.replace(['\'', '"', '\\'], "")),
                    equals: w.starts_with('='),
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
        for exe in [
            "git", "make", "sudo", "env", "nice", "ls", "gitk", "kubectl",
        ] {
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
        // A spec precommand wrapping a fallback precommand (`noglob` has no spec here).
        assert_eq!(
            f.run("sudo noglob ls"),
            spans(&[
                ("sudo", K::Precommand),
                ("noglob", K::Precommand),
                ("ls", K::Command)
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

    fn literals(line: &str) -> Vec<ArgInput<'_>> {
        line.split_whitespace()
            .map(|w| ArgInput {
                literal: Some(w),
                name_eq: false,
            })
            .collect()
    }

    #[test]
    fn command_options_parsing() {
        let cases: &[(&str, (usize, bool))] = &[
            ("", (0, false)),
            ("ls", (0, false)),
            ("-p ls", (1, false)),
            ("-v ls", (1, true)),
            ("-V ls", (1, true)),
            ("-pv ls", (1, true)),
            ("-Vp ls", (1, true)),
            ("-p -V ls", (2, true)),
            ("-v", (1, true)),
            ("-v -- ls", (2, true)),
            ("-- -v ls", (1, false)),
            ("--", (1, false)),
            ("-x ls", (0, false)),
            ("-pvx ls", (0, false)),
            ("- ls", (0, false)),
            ("ls -v", (0, false)),
            ("-p ls -v", (1, false)),
            // Several option words need a v or V among them.
            ("-p -p ls", (0, false)),
            ("-p -pz ls", (0, false)),
            ("-pp -p ls", (0, false)),
            ("-p -p -- ls", (0, false)),
            ("-p -p", (0, false)),
            ("-p -v ls", (2, true)),
            ("-v -p -p ls", (3, true)),
            ("-p -p -v ls", (3, true)),
            ("-p -p -v -- ls", (4, true)),
            // An invalid option word after consumed ones: the first is the command word.
            ("-p -x ls", (0, false)),
            ("-v -x ls", (0, false)),
            ("-p -v -x ls", (0, false)),
            ("-p -- ls", (2, false)),
            ("-p --", (2, false)),
            ("-p - ls", (1, false)),
        ];
        for (line, want) in cases {
            assert_eq!(command_options(&literals(line)), *want, "{line:?}");
        }
        let expansion = [ArgInput {
            literal: None,
            name_eq: false,
        }];
        assert_eq!(command_options(&expansion), (0, false));
    }

    /// The fixture with a `-` builtin, as zsh has.
    fn wrapper_fixture() -> Fixture {
        let mut f = fixture();
        f.h.state.apply_update(StateUpdate {
            builtins: strings(&["echo", "cd", "noglob", "builtin", "command", "exec", "-"]),
            ..Default::default()
        });
        f
    }

    #[test]
    fn builtin_accepts_only_builtins() {
        let mut f = wrapper_fixture();
        assert_eq!(
            f.run("builtin echo"),
            spans(&[("builtin", K::Precommand), ("echo", K::Builtin)])
        );
        assert_eq!(
            f.run("builtin 'echo' x"),
            spans(&[("builtin", K::Precommand), ("'echo'", K::Builtin)])
        );
        // An external command, unknown name, function, reserved word, alias, or path.
        for word in [
            "ls",
            "nosuch",
            "myfn",
            "while",
            "typeset",
            "ll",
            "time",
            "nocorrect",
            "./tool",
            "--",
        ] {
            assert_eq!(
                f.run(&format!("builtin {word}")),
                spans(&[("builtin", K::Precommand), (word, K::Error)]),
                "{word}"
            );
        }
        // Precommand builtins continue the chain.
        for (text, last) in [
            ("builtin command ls", "ls"),
            ("builtin exec ls", "ls"),
            ("builtin noglob ls", "ls"),
            ("builtin - ls", "ls"),
        ] {
            let middle = text.split(' ').nth(1).unwrap();
            assert_eq!(
                f.run(text),
                spans(&[
                    ("builtin", K::Precommand),
                    (middle, K::Precommand),
                    (last, K::Command)
                ]),
                "{text}"
            );
        }
        assert_eq!(
            f.run("builtin builtin echo"),
            spans(&[
                ("builtin", K::Precommand),
                ("builtin", K::Precommand),
                ("echo", K::Builtin)
            ])
        );
        // Without a `-` builtin, `-` is not one.
        let mut f = fixture();
        assert_eq!(
            f.run("builtin - ls"),
            spans(&[("builtin", K::Precommand), ("-", K::Error)])
        );
    }

    #[test]
    fn command_accepts_only_externals() {
        let mut f = wrapper_fixture();
        assert_eq!(
            f.run("command ls"),
            spans(&[("command", K::Precommand), ("ls", K::Command)])
        );
        assert_eq!(
            f.run("command 'ls'"),
            spans(&[("command", K::Precommand), ("'ls'", K::Command)])
        );
        assert_eq!(
            f.run("command ./tool"),
            spans(&[("command", K::Precommand), ("./tool", K::Command)])
        );
        // Functions, aliases, suffix aliases, builtins, reserved words, precommand builtins,
        // unknown names, and paths that are not executable files.
        for word in [
            "myfn",
            "ll",
            "\\ll",
            "doc.pdf",
            "cd",
            "echo",
            "while",
            "typeset",
            "command",
            "builtin",
            "noglob",
            "exec",
            "-",
            "nosuch",
            "./notes.txt",
            "./src",
        ] {
            assert_eq!(
                f.run(&format!("command {word}")),
                spans(&[("command", K::Precommand), (word, K::Error)]),
                "{word}"
            );
        }
        // A builtin that also exists on PATH runs the external.
        f.dir.file("bin/echo", 0o755);
        f.h.state.invalidate_path();
        assert_eq!(
            f.run("command echo"),
            spans(&[("command", K::Precommand), ("echo", K::Command)])
        );
        // External precommands continue the chain.
        assert_eq!(
            f.run("command sudo -u root ls"),
            spans(&[
                ("command", K::Precommand),
                ("sudo", K::Precommand),
                ("-u", K::CmdOption),
                ("ls", K::Command)
            ])
        );
    }

    #[test]
    fn command_precommand_skips_the_shadow_check() {
        let mut f = wrapper_fixture();
        assert_eq!(
            f.run("command time ls"),
            spans(&[("command", K::Precommand), ("time", K::Error)])
        );
        f.dir.file("bin/time", 0o755);
        f.h.state.apply_update(StateUpdate {
            functions: strings(&["myfn", "nice", "time"]),
            reserved_words: strings(&["while", "typeset", "time"]),
            rehash: true,
            ..Default::default()
        });
        for (text, middle) in [("command nice ls", "nice"), ("command time ls", "time")] {
            assert_eq!(
                f.run(text),
                spans(&[
                    ("command", K::Precommand),
                    (middle, K::Precommand),
                    ("ls", K::Command)
                ]),
                "{text}"
            );
        }
        // Elsewhere the function still shadows the precommand, also after `sudo`.
        assert_eq!(f.run("nice ls"), spans(&[("nice", K::Function)]));
        assert_eq!(
            f.run("sudo nice ls"),
            spans(&[("sudo", K::Precommand), ("nice", K::Function)])
        );
    }

    #[test]
    fn command_v_looks_names_up() {
        let mut f = wrapper_fixture();
        assert_eq!(
            f.run("command -v myfn ll G doc.pdf echo while ls nosuch ./tool 'll' $x *.txt"),
            spans(&[
                ("command", K::Precommand),
                ("-v", K::CmdOption),
                ("myfn", K::Function),
                ("ll", K::Alias),
                ("G", K::GlobalAlias),
                ("doc.pdf", K::SuffixAlias),
                ("echo", K::Builtin),
                ("while", K::ReservedWord),
                ("ls", K::Command),
                ("nosuch", K::Error),
                ("./tool", K::Command),
                ("'ll'", K::Alias),
            ])
        );
        // Query words never continue a chain.
        assert_eq!(
            f.run("command -V sudo nosuch"),
            spans(&[
                ("command", K::Precommand),
                ("-V", K::CmdOption),
                ("sudo", K::Alias),
                ("nosuch", K::Error)
            ])
        );
        for (text, opts) in [
            ("command -pv ls", &["-pv"][..]),
            ("command -Vp ls", &["-Vp"]),
            ("command -p -v ls", &["-p", "-v"]),
            ("command -v -- ls", &["-v"]),
        ] {
            let mut want = vec![("command", K::Precommand)];
            want.extend(opts.iter().map(|o| (*o, K::CmdOption)));
            want.push(("ls", K::Command));
            assert_eq!(f.run(text), spans(&want), "{text}");
        }
        assert_eq!(
            f.run("command -v"),
            spans(&[("command", K::Precommand), ("-v", K::CmdOption)])
        );
        assert_eq!(
            f.run("command -p ls"),
            spans(&[
                ("command", K::Precommand),
                ("-p", K::CmdOption),
                ("ls", K::Command)
            ])
        );
    }

    #[test]
    fn command_unknown_option_is_the_command_word() {
        let mut f = wrapper_fixture();
        assert_eq!(
            f.run("command -x ls"),
            spans(&[("command", K::Precommand), ("-x", K::Error)])
        );
        // After `--`, `-v` is the command word, not an option.
        assert_eq!(
            f.run("command -- -v ls"),
            spans(&[("command", K::Precommand), ("-v", K::Error)])
        );
        assert_eq!(
            f.run("command -- git bad"),
            spans(&[
                ("command", K::Precommand),
                ("git", K::Command),
                ("bad", K::Error)
            ])
        );
    }

    #[test]
    fn command_option_words_need_v_for_several() {
        let mut f = wrapper_fixture();
        for (text, first) in [
            ("command -p -p ls", "-p"),
            ("command -p -pz ls", "-p"),
            ("command -pp -p ls", "-pp"),
            ("command -p -p -- ls", "-p"),
            ("command -p -x ls", "-p"),
            ("command -v -x ls", "-v"),
        ] {
            assert_eq!(
                f.run(text),
                spans(&[("command", K::Precommand), (first, K::Error)]),
                "{text}"
            );
        }
        assert_eq!(
            f.run("command -p -p -v ls"),
            spans(&[
                ("command", K::Precommand),
                ("-p", K::CmdOption),
                ("-p", K::CmdOption),
                ("-v", K::CmdOption),
                ("ls", K::Command)
            ])
        );
        assert_eq!(
            f.run("command -p -- ls"),
            spans(&[
                ("command", K::Precommand),
                ("-p", K::CmdOption),
                ("ls", K::Command)
            ])
        );
    }

    #[test]
    fn command_query_never_continues_a_chain() {
        let mut f = wrapper_fixture();
        // `nice` is an unshadowed external precommand: run, its `-n` would be an option.
        assert_eq!(
            f.run("command nice -n 5 ls"),
            spans(&[
                ("command", K::Precommand),
                ("nice", K::Precommand),
                ("-n", K::CmdOption),
                ("ls", K::Command)
            ])
        );
        // Looked up, it is just a name, and the words after it are names too.
        assert_eq!(
            f.run("command -v nice -n 5"),
            spans(&[
                ("command", K::Precommand),
                ("-v", K::CmdOption),
                ("nice", K::Command),
                ("-n", K::Error),
                ("5", K::Error)
            ])
        );
        let inputs = literals("-v nice -n 5");
        let (tail, query, wrap) = precommand_tail("command", None, &inputs);
        assert_eq!((tail, query, wrap), (Tail::Command(1), true, Wrap::None));
        let inputs = literals("-p nice -n 5");
        let (tail, query, wrap) = precommand_tail("command", None, &inputs);
        assert_eq!(
            (tail, query, wrap),
            (Tail::Command(1), false, Wrap::Command)
        );
    }

    #[test]
    fn bare_and_quoted_wrapper_words() {
        let mut f = wrapper_fixture();
        assert_eq!(f.run("command"), spans(&[("command", K::Precommand)]));
        assert_eq!(f.run("builtin"), spans(&[("builtin", K::Precommand)]));
        assert_eq!(
            f.run("command -p"),
            spans(&[("command", K::Precommand), ("-p", K::CmdOption)])
        );
        assert_eq!(
            f.run("command 'myfn'"),
            spans(&[("command", K::Precommand), ("'myfn'", K::Error)])
        );
        assert_eq!(
            f.run("builtin 'ls'"),
            spans(&[("builtin", K::Precommand), ("'ls'", K::Error)])
        );
        // Typing: a word being typed after the wrapper is not yet an error.
        assert_eq!(
            f.run_typing("builtin -"),
            spans(&[("builtin", K::Precommand), ("-", K::Precommand)])
        );
        // Without a `-` builtin, the word being typed is already an error.
        assert_eq!(
            fixture().run_typing("builtin -"),
            spans(&[("builtin", K::Precommand), ("-", K::Error)])
        );
        assert_eq!(
            f.run_typing("command -p gi"),
            spans(&[("command", K::Precommand), ("-p", K::CmdOption)])
        );
    }

    #[test]
    fn wrapper_chains() {
        let mut f = wrapper_fixture();
        assert_eq!(
            f.run("command builtin echo"),
            spans(&[("command", K::Precommand), ("builtin", K::Error)])
        );
        assert_eq!(
            f.run("noglob command ls"),
            spans(&[
                ("noglob", K::Precommand),
                ("command", K::Precommand),
                ("ls", K::Command)
            ])
        );
        assert_eq!(
            f.run("noglob command myfn"),
            spans(&[
                ("noglob", K::Precommand),
                ("command", K::Precommand),
                ("myfn", K::Error)
            ])
        );
        // Query mode reached through a chain.
        assert_eq!(
            f.run("builtin command -v myfn nosuch"),
            spans(&[
                ("builtin", K::Precommand),
                ("command", K::Precommand),
                ("-v", K::CmdOption),
                ("myfn", K::Function),
                ("nosuch", K::Error)
            ])
        );
        assert_eq!(
            f.run("builtin command -v"),
            spans(&[
                ("builtin", K::Precommand),
                ("command", K::Precommand),
                ("-v", K::CmdOption)
            ])
        );
        // A PATH binary named `command` is an external precommand.
        f.dir.file("bin/command", 0o755);
        f.h.state.invalidate_path();
        assert_eq!(
            f.run("command command ls"),
            spans(&[
                ("command", K::Precommand),
                ("command", K::Precommand),
                ("ls", K::Command)
            ])
        );
    }

    #[test]
    fn rejected_wrapped_word_stops_the_chain() {
        let mut f = wrapper_fixture();
        // The arguments are only checked as paths: no wrapped command, no spec.
        assert_eq!(
            f.run("builtin sudo notes.txt"),
            spans(&[
                ("builtin", K::Precommand),
                ("sudo", K::Error),
                ("notes.txt", K::Path)
            ])
        );
        assert_eq!(
            f.run("builtin git status src"),
            spans(&[
                ("builtin", K::Precommand),
                ("git", K::Error),
                ("src", K::PathDirectory)
            ])
        );
        assert_eq!(
            f.run("command exec ls"),
            spans(&[("command", K::Precommand), ("exec", K::Error)])
        );
        assert_eq!(
            f.run("builtin kubectl exec pod -- myfn"),
            spans(&[("builtin", K::Precommand), ("kubectl", K::Error)])
        );
    }

    #[test]
    fn wrapped_non_precommand_tail_lifts_the_restriction() {
        let mut f = wrapper_fixture();
        assert_eq!(
            f.run("command kubectl exec pod -- myfn"),
            spans(&[
                ("command", K::Precommand),
                ("kubectl", K::Command),
                ("exec", K::Subcommand),
                ("--", K::CmdOption),
                ("myfn", K::Function)
            ])
        );
    }

    #[test]
    fn non_literal_wrapped_word_is_never_an_error() {
        let mut f = wrapper_fixture();
        assert_eq!(
            f.run("command $x notes.txt"),
            spans(&[("command", K::Precommand), ("notes.txt", K::Path)])
        );
        assert_eq!(f.run("builtin $x"), spans(&[("builtin", K::Precommand)]));
        assert_eq!(f.run("command *.sh"), spans(&[("command", K::Precommand)]));
    }

    #[test]
    fn wrapped_word_under_auto_cd() {
        let mut f = wrapper_fixture();
        let auto = RequestOptions {
            auto_cd: true,
            ..Default::default()
        };
        assert_eq!(
            f.run_opts("command src", usize::MAX, auto),
            spans(&[("command", K::Precommand), ("src", K::Error)])
        );
        assert_eq!(
            f.run_opts("command sr", 10, auto),
            spans(&[("command", K::Precommand), ("sr", K::Error)])
        );
        assert_eq!(
            f.run_opts("command -v src", usize::MAX, auto),
            spans(&[
                ("command", K::Precommand),
                ("-v", K::CmdOption),
                ("src", K::Error)
            ])
        );
        assert_eq!(
            f.run_opts("builtin src", usize::MAX, auto),
            spans(&[("builtin", K::Precommand), ("src", K::Error)])
        );
        // The first word still may be a directory.
        assert_eq!(
            f.run_opts("src", usize::MAX, auto),
            spans(&[("src", K::PathDirectory)])
        );
    }

    #[test]
    fn wrapped_word_while_typing() {
        let mut f = wrapper_fixture();
        // A prefix of a name the wrapper accepts: no span.
        for text in ["command gi", "builtin ec", "builtin e"] {
            assert_eq!(
                f.run_typing(text),
                spans(&[(text.split(' ').next().unwrap(), K::Precommand)]),
                "{text}"
            );
        }
        // A prefix only of names the wrapper rejects: error.
        for (text, word) in [
            ("command my", "my"),
            ("command ec", "ec"),
            ("builtin l", "l"),
            ("builtin my", "my"),
        ] {
            assert_eq!(
                f.run_typing(text),
                spans(&[
                    (text.split(' ').next().unwrap(), K::Precommand),
                    (word, K::Error)
                ]),
                "{text}"
            );
        }
        // A query accepts a prefix of any name.
        assert_eq!(
            f.run_typing("command -v my"),
            spans(&[("command", K::Precommand), ("-v", K::CmdOption)])
        );
        assert_eq!(
            f.run_typing("command ./to"),
            spans(&[("command", K::Precommand), ("./to", K::PathPrefix)])
        );
        // Not at the cursor: error.
        assert_eq!(
            f.run("builtin ec"),
            spans(&[("builtin", K::Precommand), ("ec", K::Error)])
        );
        assert_eq!(
            f.run("command gi"),
            spans(&[("command", K::Precommand), ("gi", K::Error)])
        );
    }

    #[test]
    fn path_mode_checks_operands_as_paths() {
        let mut f = fixture();
        assert_eq!(
            f.run("sudo -e notes.txt src"),
            spans(&[
                ("sudo", K::Precommand),
                ("-e", K::CmdOption),
                ("notes.txt", K::Path),
                ("src", K::PathDirectory),
            ])
        );
        // A missing file is not an error, and no operand yet is not an error either.
        assert_eq!(
            f.run("sudo -u root -e nosuch"),
            spans(&[
                ("sudo", K::Precommand),
                ("-u", K::CmdOption),
                ("-e", K::CmdOption)
            ])
        );
        assert_eq!(
            f.run("sudo -e"),
            spans(&[("sudo", K::Precommand), ("-e", K::CmdOption)])
        );
        assert_eq!(
            f.run_typing("sudo -e not"),
            spans(&[
                ("sudo", K::Precommand),
                ("-e", K::CmdOption),
                ("not", K::PathPrefix)
            ])
        );
        // Without the mode the same word is the wrapped command.
        assert_eq!(
            f.run("sudo notes.txt"),
            spans(&[("sudo", K::Precommand), ("notes.txt", K::Error)])
        );
    }

    #[test]
    fn non_precommand_wraps_after_its_own_arguments() {
        let mut f = fixture();
        assert_eq!(
            f.run("kubectl exec pod -- ls notes.txt"),
            spans(&[
                ("kubectl", K::Command),
                ("exec", K::Subcommand),
                ("--", K::CmdOption),
                ("ls", K::Command),
                ("notes.txt", K::Path),
            ])
        );
        // No `--`: the words are kubectl's, and `ls` is not a command.
        assert_eq!(
            f.run("kubectl exec pod ls"),
            spans(&[("kubectl", K::Command), ("exec", K::Subcommand)])
        );
        assert_eq!(
            f.run("kubectl exec pod --"),
            spans(&[
                ("kubectl", K::Command),
                ("exec", K::Subcommand),
                ("--", K::CmdOption)
            ])
        );
        // The wrapped command is checked against the local command table.
        assert_eq!(
            f.run("kubectl exec pod -- nosuch"),
            spans(&[
                ("kubectl", K::Command),
                ("exec", K::Subcommand),
                ("--", K::CmdOption),
                ("nosuch", K::Error)
            ])
        );
        // Nested: a precommand in path mode inside the wrapped command.
        assert_eq!(
            f.run("kubectl exec pod -- sudo -e notes.txt"),
            spans(&[
                ("kubectl", K::Command),
                ("exec", K::Subcommand),
                ("--", K::CmdOption),
                ("sudo", K::Precommand),
                ("-e", K::CmdOption),
                ("notes.txt", K::Path),
            ])
        );
        // The wrapping does not depend on the command's class.
        f.h.state.apply_update(StateUpdate {
            functions: strings(&["kubectl"]),
            ..Default::default()
        });
        assert_eq!(
            f.run("kubectl exec pod -- ls"),
            spans(&[
                ("kubectl", K::Function),
                ("exec", K::Subcommand),
                ("--", K::CmdOption),
                ("ls", K::Command)
            ])
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
        // With a spec: the precommand's wrapping does not apply to the function either. The
        // fixture's `sudo` alias would win over the function, so it goes.
        f.h.state.apply_update(StateUpdate {
            aliases: strings(&["ll"]),
            functions: strings(&["noglob", "sudo", "nice"]),
            ..Default::default()
        });
        assert_eq!(f.run("sudo ls"), spans(&[("sudo", K::Function)]));
        assert_eq!(
            f.run("nice -n 5 nosuch arg"),
            spans(&[("nice", K::Function), ("-n", K::CmdOption)])
        );
    }

    #[test]
    fn global_alias_shadows_precommand() {
        let mut f = fixture();
        f.h.state.apply_update(StateUpdate {
            aliases: strings(&["ll"]),
            global_aliases: strings(&["sudo"]),
            ..Default::default()
        });
        assert_eq!(f.run("sudo ls"), spans(&[("sudo", K::GlobalAlias)]));
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
    fn spec_error_suppressed_while_typing_a_subcommand_prefix() {
        let mut f = fixture();
        // `stat` is a prefix of `status`: no span while the cursor is at its end. The word is
        // not path-checked either; it sits in the subcommand slot.
        assert_eq!(f.run_typing("git stat"), spans(&[("git", K::Command)]));
        assert_eq!(f.run_typing("git com"), spans(&[("git", K::Command)]));
        // Not at the cursor, not a prefix, or already complete: unchanged.
        assert_eq!(
            f.run("git stat"),
            spans(&[("git", K::Command), ("stat", K::Error)])
        );
        assert_eq!(
            f.run_typing("git statx"),
            spans(&[("git", K::Command), ("statx", K::Error)])
        );
        assert_eq!(
            f.run_opts("git stat notes.txt", 8, RequestOptions::default()),
            spans(&[("git", K::Command), ("notes.txt", K::Path)])
        );
        assert_eq!(
            f.run_typing("git stat notes.txt"),
            spans(&[
                ("git", K::Command),
                ("stat", K::Error),
                ("notes.txt", K::Path)
            ])
        );
        // Through a precommand chain too.
        assert_eq!(
            f.run_typing("sudo git sta"),
            spans(&[("sudo", K::Precommand), ("git", K::Command)])
        );
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
    fn size_thresholds_on_a_parse() {
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
    fn merge_semantic_error_wins_over_identical_syntactic_span() {
        let text = "'nosuch' x";
        let out = merge(
            vec![Span::new(0, 8, K::Error)],
            &[Span::new(0, 8, K::SingleQuoted)],
            text,
        );
        assert_eq!(
            out,
            vec![Span::new(0, 8, K::SingleQuoted), Span::new(0, 8, K::Error)]
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
        let req = HighlightRequest {
            text: &text,
            buffer_start: 0,
            cursor: text.len(),
            cwd: &cwd,
            opts: RequestOptions::default(),
        };
        let ttl = Duration::from_millis(f.h.config.limits.path_cache_ttl_ms + 1);
        // The full request, parse included. Each iteration moves the clock past the cache TTL,
        // so every path check goes to the filesystem again.
        let mut times = Vec::new();
        for _ in 0..200 {
            f.now += ttl;
            let start = Instant::now();
            let spans = f.h.highlight_at(&req, f.now);
            times.push(start.elapsed());
            assert!(spans.iter().any(|s| s.kind == K::Path));
        }
        times.sort_unstable();
        let median = times[times.len() / 2];
        let limit = if cfg!(debug_assertions) {
            Duration::from_millis(10)
        } else {
            Duration::from_millis(1)
        };
        assert!(median < limit, "median {median:?} per request");
    }

    #[test]
    fn dash_words_after_double_dash_are_paths() {
        let mut f = fixture();
        f.dir.file("work/-dash.txt", 0o644);
        assert_eq!(f.run("ls -dash.txt"), spans(&[("ls", K::Command)]));
        assert_eq!(
            f.run("ls -l -- -dash.txt -nope notes.txt"),
            spans(&[
                ("ls", K::Command),
                ("-dash.txt", K::Path),
                ("notes.txt", K::Path)
            ])
        );
        assert_eq!(
            f.run_typing("ls -- -da"),
            spans(&[("ls", K::Command), ("-da", K::PathPrefix)])
        );
    }

    #[test]
    fn user_lookups_are_bounded_per_request() {
        let mut f = fixture();
        // Names no system has; the lookups fail and are cached as unknown.
        let text = "ls ~fh_nouser_hl1/x ; ~fh_nouser_hl2/bin/tool";
        // The argument's lookup uses this request's one uncached lookup, so the command word
        // cannot be resolved yet: no span rather than an error.
        assert_eq!(f.run(text), spans(&[("ls", K::Command)]));
        // The next request may look the second name up; it does not exist, so error.
        f.now += Duration::from_millis(10);
        assert_eq!(
            f.run(text),
            spans(&[("ls", K::Command), ("~fh_nouser_hl2/bin/tool", K::Error)])
        );
    }

    #[test]
    fn user_lookup_skipped_for_the_word_being_typed() {
        let mut f = fixture();
        // While typing, an uncached name is not looked up: no span, and nothing cached, so the
        // same name still gets no span (not an error) when typed as a command word.
        assert_eq!(f.run_typing("~fh_nouser_hl3/x"), vec![]);
        assert_eq!(
            f.run_typing("ls ~fh_nouser_hl3/x"),
            spans(&[("ls", K::Command)])
        );
        assert_eq!(f.run_typing("~fh_nouser_hl3/x"), vec![]);
        // Once the cursor moves away the lookup happens.
        f.now += Duration::from_millis(10);
        assert_eq!(
            f.run("~fh_nouser_hl3/x"),
            spans(&[("~fh_nouser_hl3/x", K::Error)])
        );
    }

    #[test]
    fn truncated_listing_is_not_an_unknown_command() {
        let mut f = fixture();
        for i in 0..crate::paths::MAX_LISTING_NAMES + 5 {
            f.dir.file(&format!("work/big/f{i:04}"), 0o644);
        }
        // Not in the part of the directory that was read, so the prefix check cannot tell.
        assert_eq!(f.run_typing("./big/zz"), vec![]);
        assert_eq!(
            f.run_typing("./big/f"),
            spans(&[("./big/f", K::PathPrefix)])
        );
        // A small directory still gives a definite answer.
        assert_eq!(f.run_typing("./src/zz"), spans(&[("./src/zz", K::Error)]));
    }

    #[test]
    fn arg_inputs_carry_name_eq_and_literal() {
        let words = words_of("FOO=$x BAR=1 $y ls", 0);
        let got: Vec<_> = arg_inputs(&words)
            .iter()
            .map(|a| (a.literal, a.name_eq))
            .collect();
        assert_eq!(
            got,
            vec![
                (None, true),
                (Some("BAR=1"), true),
                (None, false),
                (Some("ls"), false),
            ]
        );
    }
}
