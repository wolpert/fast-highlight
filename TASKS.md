# Remaining work

Numbered tasks for fast-highlight after v1, grouped by area. Task numbers run consecutively across
all groups and are stable identifiers; a completed task keeps its number.

## Command specs

### 1. Additional built-in specs (done)

Status: done.

The built-in set in `specs/` held 23 command specs. Common commands with subcommands or wrapped
commands had no spec: `podman`, `gh`, `apt`, `dnf`, `brew`, `pip`, `uv`, `go`, `rustup`, `make`,
`ssh`, `tar`, `xargs` (a precommand whose options precede the wrapped command), `journalctl`, `ip`,
and `rm` (added so that the command wrapped by `xargs` has options). The set now holds 39.

Acceptance criteria:

- Each listed command has a file under `specs/`. (The original criterion also asked for an entry in
  the built-in table; task 7 superseded that, since the table is generated from `specs/`.)
- `fast-highlight check-config` reports no errors with the new specs loaded.
- A snapshot test per new spec shows a valid subcommand, an unknown subcommand styled as an error,
  and a known option.
- For `xargs`, `xargs -0 -n1 rm -f` styles `rm` as a command and `-f` as an option of `rm`.

Notes on the snapshots:

- `ssh`, `tar`, `make`, `journalctl`, `xargs`, and `rm` have no subcommands, so their snapshots show
  only the parts that apply: a known option and an option that takes a value. For `xargs` the
  wrapped command stands in for the subcommand. An unknown option gets no span, because no built-in
  spec is options-complete.
- The top levels of `gh`, `dnf`, and `brew` are not complete, because of extensions, plugins,
  aliases, and external commands, so an unknown subcommand there is not an error. Their
  unknown-subcommand cases come from complete groups: `gh pr`, `dnf group`, and `brew services`.

### 2. Upstream subcommand and option refresh (done)

Status: done.

Several specs predated recent upstream releases. The subcommands named in the task were `git
backfill`, `last-modified`, `repo`, and `refs`, `git stash export` and `import`, `npm trust` and
`undeprecate`, and `systemctl sleep` and `enqueue-marked-jobs`. Option coverage was thin for git
plumbing commands and for `docker swarm`, `stack`, `service`, and `plugin`.

The lists were compared against git 2.53.0 and Docker 27.2.1. The Docker lists are old by design:
options and subcommands added after 27.2.1 are not declared and highlight as unknown. The refresh
covers the git plumbing commands, and `docker swarm`, `stack`, `service`, and `plugin`; the options
declared are those in the help of the checked version. `npm`, `systemctl`, `cargo`, and `kubectl`
were not swept: only the subcommands named above were checked, `npm` against 11.17.0 and
`systemctl` against systemd 259, and `cargo` and `kubectl` were not compared.

The bulk option tables (git plumbing, `docker swarm`, `stack`, `service`, and `plugin`) were
generated against the help and manual-page output of the checked versions. The snapshots sample
those tables; they do not cover them exhaustively, so an option missing from a table is not ruled
out by the tests.

Known drift left for follow-up, not fixed in this task:

- `docker stop` and `docker restart` declare `--timeout=`, while Docker 27.2.1 spells it
  `-t`/`--time`.
- `docker run` lacks options such as `--blkio-weight-device`.
- `docker node update` and several `ls` commands declare no options.
- The git porcelain commands were not compared and have gaps; only the plumbing commands and the
  commands named above were.

Findings on the named subcommands:

- `git backfill`, `last-modified`, `repo`, `refs`, and `git stash export` and `import` are present
  with their options.
- `npm trust` gained its subcommands `github`, `gitlab`, `circleci`, `list`, and `revoke` with their
  options, and `npm undeprecate` gained `--otp`. npm-trust(1) states that `trust` needs npm 11.15.0
  or later.
- `systemctl sleep` was already present and needs no verb-specific option; systemctl(1) says it was
  added in systemd 256. `enqueue-marked-jobs` is not a verb: systemd 259 rejects it as an unknown
  verb and its manual page does not list it (it is an internal function, and the user-facing form is
  `reload-or-restart --marked`). It was removed from the spec, so it now highlights as an unknown
  verb, as the snapshot shows.

Acceptance criteria:

- Each named subcommand that exists highlights as a valid subcommand, checked by a snapshot test
  (`spec_git_refresh`, `spec_npm_systemctl_refresh`).
- Each spec file records the upstream version its lists were checked against, in a first line of the
  form `# Verified against: <tool> <version>.`, or `# Verified against: nothing (<reason>).` for a
  spec that was not compared (`cargo`, `kubectl`, `doas`). `sudo` records 1.9.17p2. A version is
  recorded only where the comparison was made; `npm` and `systemctl` say which parts were compared.
  The test `every_spec_records_a_checked_version` in `src/specs/tests.rs` requires one of the two
  forms in every built-in spec.
- `docker service create --replicas 3 nginx` highlights `--replicas` as a known option
  (`spec_docker_swarm_service`).

### 3. Assignments with expansions after `env` (done)

Status: done.

In `env FOO=$x cmd`, a word containing an expansion has no literal text, so the spec engine takes
the assignment for the wrapped command. The argument input needs a flag for a word that starts with
a literal `NAME=`.

Acceptance criteria:

- `env FOO=$x ls` styles `FOO=$x` as an assignment and `ls` as a command.
- `env FOO="$(date)" BAR=1 ls` does the same for both assignments.
- A word without a literal `NAME=` prefix, such as `env $cmd`, keeps its current styling.

"Styles as an assignment" means "skipped as an assignment": `env FOO=$x ls` highlights exactly as
`env FOO=1 ls` does. The `NAME=value` word gets no span of its own (only its expansions keep their
syntactic spans), and the next word is the wrapped command. The fix applies to every spec with
`skip-assignments` (`env`, `sudo`). One visible change follows: `env FOO=$x nosuchcmd` now shows an
error on `nosuchcmd`, as `env FOO=1 nosuchcmd` already did.

### 4. Long-option abbreviations (done)

Status: done.

GNU-style programs accept any unambiguous prefix of a long option (`--verb` for `--verbose`). The
spec engine recognises only exact names.

Acceptance criteria:

- A spec field marks a command as accepting abbreviations; it is off by default.
- With the field set, an unambiguous prefix highlights as the full option, including its argument
  arity.
- An ambiguous prefix and a prefix of no option highlight as unknown options.

The field is `option-abbreviations` (the existing `abbreviations` stays for subcommands). A level
that leaves it unset inherits its parent's value. Only long options abbreviate, only declared
spellings count, an exact spelling beats a prefix, and the rule is strict: two candidate spellings
are ambiguous even when they are aliases of one option. Unknown options are errors only with
`options-complete`, as before (`user_spec_option_abbreviations`).

It is enabled in `tar`, `make`, `xargs`, `rm`, `journalctl`, `env`, `timeout`, `nice`, `nohup`,
`stdbuf`, `time`, `ionice`, `taskset`, and `chrt`. For each, every proper prefix of a declared long
option, and every `--x` and `--xy` stem, was run against the installed program and gave the same
verdict (unique, ambiguous, or unknown) as the spec. That check found options missing from `--help`,
now declared: `make` `--jobserver-auth`, `--jobserver-fds`, `--sync-mutex`, and `--temp-stdin`, and
`journalctl` `--new-id128` and `--this-boot`. Two differences remain: chrt 2.41.3 lacks the
declared `--ext`, and uutils `env` has an undeclared `--file`.

### 5. Positional arguments and option-selected modes (done)

Status: done.

Specs modelled options before a wrapped command and nothing more. There was no model of positional
arguments, no wrapped command inside a subcommand (`kubectl exec pod -- cmd`), and no option that
changes how the remaining words are read (`sudo -e` takes files, not a command).

Acceptance criteria:

- The spec format documents a positional-argument model in `man/man5/fast-highlight.5`.
- `kubectl exec mypod -- ls -l` styles `ls` as a command.
- `sudo -e /etc/hosts` styles `/etc/hosts` as a path, not as an unknown command.

The model has three level keys, valid at the top level and in every subcommand table, and
documented in the section "Wrapped commands and path mode" of the manual page, the README, and the
module documentation of `src/specs/mod.rs`:

- `positional = N` (moved from the top-level keys) wraps the command after the level's options and
  `N` plain words. The top level of a `precommand = true` spec wraps with `positional = 0`. A
  top-level `positional` without `precommand`, which was ignored, now wraps.
- `wrapped-after = "--"` wraps the command after the first `--`. `kubectl exec` sets it, so
  `kubectl exec mypod ls`, without `--`, has no wrapped command.
- `path-mode-options` declares options that make the remaining words operands, checked as paths.
  `sudo` lists `-e` and `--edit`, and now sets `options-first`, as the `+` at the start of the
  getopt string of sudo 1.9.17p2 requires.

The descent through subcommands is the one classification makes, and only the level it ends at
counts. A level that sets both start keys, a `wrapped-after` other than `"--"`, and a pattern in
`path-mode-options` are load errors. The wrapped command is checked against the local command
table, so a command that exists only in the pod shows as an error (`spec_wrapped_and_modes`). The
literal `sudo -e /etc/hosts` case is the test `sudo_edit_etc_hosts`, skipped on a host without
`/etc/hosts`; the snapshot uses fixture paths.

Follow-ups, not done in this task:

- `rustup run TOOLCHAIN cmd` and `uv run cmd` fit `positional`; their specs do not set it.
- `docker exec CONTAINER cmd` and `podman exec CONTAINER cmd` fit `positional = 1` at the `exec`
  level; their specs do not set it.
- `ssh host cmd` runs `cmd` on the remote host; the spec does not wrap it.
- `sudoedit` takes files only, as `sudo -e` does; it has no spec.
- `kubectl run NAME -- cmd` and `kubectl debug POD -- cmd` take a command after `--` too; only
  `exec` is marked. kubectl is not installed here, so neither was checked.
- `go run PACKAGE ARGS...` passes the words after the package to the program; the spec does not
  model them.
- A "no wrapped command" mode: `sudo -l`, `chrt -p PID`, and `taskset -p MASK PID` still take the
  next word as the wrapped command.

### 6. Word restrictions after `builtin` and `command` (done)

Status: done.

A word after `builtin` was accepted when it named any command, and so was a word after `command`.
zsh accepts only builtins after `builtin`. `command` skips builtins and reserved words as well as
functions and aliases, and runs external commands only; with `-v` or `-V` it looks names up
instead of running one.

Acceptance criteria:

- `builtin ls` styles `ls` as an error when `ls` is not a builtin.
- `command myfunc` styles `myfunc` as an error when it exists only as a function.
- `builtin echo` and `command ls` keep their current styling.
- `command cd` and `command while` style the builtin and the reserved word as errors; `command
  builtin echo` styles `builtin` as an error, and `builtin command ls` is a precommand chain.
- An external precommand after `command` (`command nice ls`, `command time ls`) stays a
  precommand, even when a function of the same name exists.
- `command -v` and `-V`, alone or combined with `-p`, make every remaining word a lookup styled by
  what it names; `command -v myfunc` is not an error.
- A word rejected after either wrapper wraps nothing, and its arguments are only checked as paths.

The rules are built in and keyed by name; the specs of `builtin` and `command` decide only whether
they are precommands. They are documented in the README, in the section "Wrapped commands and path
mode" of the manual page, and in the module documentation of `src/specs/mod.rs`, and covered by
unit tests in `src/highlight.rs` and the snapshot `wrapper_word_restrictions`. Global aliases after
the wrappers, the default `PATH` search of `command -p`, and `POSIX_BUILTINS` are out of scope.

### 7. Generated built-in spec table (done)

Status: done.

`BUILTIN_FILES` in `src/specs/mod.rs` is a hand-maintained table of `include_str!` entries, so a new
file in `specs/` is silently ignored until someone adds a line.

Acceptance criteria:

- A build script or a test derives the table from the contents of `specs/`.
- Adding a file to `specs/` without other changes makes the spec active, or fails `cargo test` with
  a message naming the file.

## zsh syntax coverage

### 8. Shell option modelling (done)

Status: done.

The parser assumed default option settings and `SHORT_LOOPS` set. It does not model
`IGNORE_BRACES`, `IGNORE_CLOSE_BRACES`, `RC_QUOTES`, `KSH_ARRAYS`, `POSIX_IDENTIFIERS`, `SH_GLOB`,
`BRACE_CCL`, or `EQUALS`, so `=cmd` gets no span. The plugin would have to send the relevant option
state to the daemon.

Acceptance criteria:

- The protocol carries the state of each modelled option, documented in `docs/protocol.md`.
- With `RC_QUOTES` set, `echo 'it''s'` is one string span; without it, two.
- With `EQUALS` set, `=ls` gets a command-path span; with `NO_EQUALS`, it does not.
- A snapshot test per modelled option covers both settings.

The `opt` field gained one letter per option: `b` `IGNORE_BRACES`, `B` `IGNORE_CLOSE_BRACES`, `r`
`RC_QUOTES`, `K` `KSH_ARRAYS`, `p` `POSIX_IDENTIFIERS`, `s` `SH_GLOB`, `C` `BRACE_CCL`, `E`
`NO_EQUALS`, and `L` `NO_SHORT_LOOPS`. A letter marks an option that is not in its zsh default
state, as the existing letters already did, so an empty field is a shell with default options and
the plugin sends no extra letters for one. An older daemon ignores the new letters; an older plugin
sends none of them, and its users get the defaults, `EQUALS` included. The plugin reads the options
at every redraw. The effects are documented in the section "Shell options" of the README, the
letters in `docs/protocol.md` and under `--opts` in `fast-highlight(1)`, and each one is covered by
a snapshot `option_*` in `tests/highlight_snapshots.rs`, by unit tests in `src/syntax/tests.rs`,
and by fuzz seeds for the new extra option byte of the `lexer` and `highlight` targets.

The options behave as in zsh 5.9:

- `=cmd` is a `command` span when `cmd` is a `$PATH` command, or with a `/` an executable file, and
  an error otherwise, since zsh fails on it; aliases, functions, and builtins do not count (`=echo`
  is `/usr/bin/echo`). The command name is not a token kind of its own: `command` already names
  external commands, and the acceptance criterion's command-path span is that. In command position
  the word is looked up as after `command`, its spec applies (`sudo =ls`), and after `builtin` it
  is an error. Redirection targets, `for` lists, and `[[ ... ]]` operands expand too.
- `IGNORE_CLOSE_BRACES` still splits a `}` off the end of a word, as an ordinary argument
  (`echo a}` prints `a }`). zsh takes a `}` as being in command position after `esac`, `}`, `)`,
  `))`, and `]]`, but not after `fi`, `done`, and `end`, so `{ if a; then b; fi }` is an error with
  either brace option. A `case ... {` cannot be closed by `}` with either option; zsh lets `esac`
  close it in every mode, and the parser now accepts that too, without options as well.
- `IGNORE_BRACES` makes `{echo` a command word and `{fd}` a command word before a redirection.
- `POSIX_IDENTIFIERS` changes `$#name` only; `$+name` keeps its meaning.
- `SH_GLOB` makes zsh fail to parse a glob group or qualifier list, which the parser marks as an
  error from `(` to its `)`. `<1-5>` stays a glob.
- With `NO_SHORT_LOOPS`, the word that starts a short body of `for`, `select`, or `repeat` is an
  error, and the words after it are its arguments. The body is not recorded as a command, so the
  error is the word's only span. A prefix of `do` at the end of the input (`for i in a; d`) is
  partial input and not an error.

Not modelled:

- `=` expansion in an assignment value (`x==ls`, `PATH=$PATH:=ls`), in a brace expansion
  (`{=ls,=cat}`), and in a word with another expansion (`=$cmd`).
- The short form `function f() cmd`, which zsh rejects with `SHORT_LOOPS` unset.
- A group nested in a `KSH_GLOB` group (`@(a|(b))`), a bad pattern with `SH_GLOB` set.

`KSH_ARRAYS` and `SH_GLOB` break the dispatcher of zsh 5.9's own `add-zle-hook-widget`, which runs
under the user's options, so `test_parse_options` in `tests/zsh/run.zsh` checks their letters by a
direct call rather than in a redraw.

The committed fuzz seeds lag behind `fuzz/make_seeds.py`: running it also adds seeds for every
snapshot added since the seeds were last generated. Only the new option seeds were added.

### 9. Glob qualifier validation

Any final parenthesised group without `|` counts as a glob qualifier, and qualifier letters are not
checked.

Acceptance criteria:

- `ls *(.)` and `ls *(om[1,3])` highlight as valid qualifiers.
- `ls *(Z)` highlights the qualifier as an error.
- `ls (a|b)` remains an alternation, not a qualifier.

### 10. History expansion coverage

History expansion is not recognised inside `${...}` or arithmetic, and `^old^new` is recognised only
at offset 0 of the buffer.

Acceptance criteria:

- `echo ${!!}` and `echo $(( !! + 1 ))` highlight `!!` as history expansion where zsh expands it.
- `^old^new` after leading whitespace highlights as quick substitution where zsh treats it as one.
- Snapshot tests cover each case.

### 11. Arithmetic highlighting

Inside `$(( ))` and `(( ))` only expansions get spans. Numbers, operators, and bare variable names
are unstyled.

Acceptance criteria:

- `(( i += 0x1f ))` gives `i` a variable span and `0x1f` a number span, using token types listed in
  the README.
- The theme files in `themes/` define styles for any new token types.
- Snapshot tests cover decimal, hexadecimal, `base#n`, and floating-point literals.

### 12. Error detection for invalid compound forms

Several forms that zsh rejects highlight as valid: `x=1 {echo}`, `x=1 ( ... )`, `nocorrect { ... }`,
`[[ a b ]]` with no operator, and a lone `}(` taken as a closer.

Acceptance criteria:

- Each listed form highlights its offending word as an error, checked by a snapshot test.
- The same forms run through `zsh -n -c` produce a parse error, recorded in the test as the
  reference.

### 13. Accepted forms the parser mishandles

zsh accepts `{ ls }always{ echo }` without spaces, but the parser does not. `time` is recognised
only as an exact unquoted word. `echo $"x"` styles the `$` inconsistently with zsh's treatment.
Here-document bodies that start inside quoted multi-line constructs begin at a slightly different
line than in zsh.

Acceptance criteria:

- `{ ls }always{ echo }` highlights with `always` as a reserved word and no error spans.
- The `time` and `$"x"` cases match zsh's parse, checked against `zsh -n`.
- A snapshot test covers a here-document whose introducing line contains an unterminated quoted
  string, with the body start matching zsh.

### 14. Nesting beyond the depth limit

Constructs nested deeper than `MAX_DEPTH` (256, in `src/syntax/parser.rs`) are left unhighlighted.

Acceptance criteria:

- A buffer with 300 nested subshells highlights the first 256 levels and styles the remainder as
  plain text, with no panic and within the latency target.
- The behaviour is documented in the README section on performance and limits.

## Shell state and paths

### 15. Shell `HOME` and directory stack

The daemon takes `HOME` from its own environment, so a later change to `$HOME` in the shell is not
seen. The protocol carries no `OLDPWD` or directory stack, so `~-`, `~2`, `~+2`, and `~-2` are never
resolved.

Acceptance criteria:

- The state update carries `HOME`, `OLDPWD`, and the directory stack, documented in
  `docs/protocol.md`.
- After `HOME=/tmp` in the shell, `ls ~/x` resolves against `/tmp`.
- After `cd /etc; cd /`, `ls ~-/hosts` highlights as an existing path.
- A zsh plugin test covers the `HOME` change.

### 16. Relative `PATH` entries

Empty, `.`, and relative entries in `PATH` are skipped, so commands found through them in zsh
highlight as unknown.

Acceptance criteria:

- With `PATH=.:$PATH` and an executable `./tool`, `tool` highlights as a command.
- An empty entry is treated as the current directory, as zsh does.
- A change of working directory updates the result without a restart.

### 17. Command path accuracy

Command-name prefix detection reads at most `MAX_LISTING_NAMES` (512) entries per directory, does
not confirm that a completed path is executable, and counts a symlink to a directory in a `PATH`
directory as a command because no entry is stat'ed.

Acceptance criteria:

- A symlink to a directory in a `PATH` directory does not highlight as a command.
- A non-executable file in the current directory, typed as `./fi`, does not highlight as a command
  prefix.
- The 512-entry limit is either removed or documented in the README with its effect.
- None of the changes moves `cargo test --release` latency results outside the targets.

### 18. Background `PATH` scan

A highlight request that arrives right after a state update that changed `PATH` waits for the
rescan, and a scan longer than 50 ms causes one restart. A background scan thread removes the wait.

Acceptance criteria:

- A highlight request issued immediately after a `PATH` change returns within the latency target,
  using the previous command table until the scan completes.
- A daemon test with a `PATH` directory of 20,000 entries shows no restart.

## Daemon robustness

### 19. Daemon fault containment

A panic during highlighter construction is logged on every repetition with no rate limit. The daemon
sends spans from the highlighter without re-checking the span contract (ordered, in range, on
character boundaries).

Acceptance criteria:

- Repeated construction panics produce at most one log line per interval, with a repeat count.
- The daemon validates spans before sending and replaces an invalid response with an empty one,
  logged once.
- A test injects an invalid span and observes an empty response and one log line.

## zsh plugin

### 20. Side effects of daemon start

Each daemon start forks `mkfifo`, and a restart changes the user's `$!`.

Acceptance criteria:

- After a forced daemon restart, `$!` holds the value it held before.
- The FIFO directory is reused across restarts within one shell, so `mkfifo` runs once per shell.
- A zsh plugin test checks both.

### 21. Style table load timeout

`$(fast-highlight styles)` runs once when the plugin loads, with no timeout, so a hanging binary
blocks shell start-up.

Acceptance criteria:

- With a binary that sleeps forever on `styles`, the shell reaches its prompt within
  `FASTHL_TIMEOUT` plus a fixed margin, with highlighting disabled.
- A zsh plugin test with the mock daemon covers the case.

### 22. Offsets in non-UTF-8 multibyte locales

In a multibyte locale other than UTF-8, the plugin sends byte offsets while ZLE counts characters,
so spans land in the wrong place.

Acceptance criteria:

- In such a locale the plugin either converts offsets correctly or disables itself with one
  message.
- A zsh plugin test runs under `FASTHL_TEST_LANG` set to an EUC-JP or GB18030 locale, skipped where
  the locale is not installed.

### 23. Hook order after re-enabling

After `fasthl-enable`, the plugin's hook runs after any hooks added since it first loaded, rather
than in its original position.

Acceptance criteria:

- After `fasthl-disable`, adding another `zle-line-pre-redraw` hook, and `fasthl-enable`, the hook
  order matches the order before the disable.
- A zsh plugin test checks the order.

### 24. Rehash detection

The plugin detects `rehash` by scanning the words of the typed command. A `rehash` inside a function
or a sourced script is not seen, so the command table can lag.

Acceptance criteria:

- Calling a function that runs `rehash` refreshes the daemon's command table.
- `source` of a script that runs `hash -r` does the same.
- A zsh plugin test covers both.

### 25. zsh-autosuggestions coexistence test

The README states that fast-highlight coexists with zsh-autosuggestions, but the plugin test suite
does not run zsh-autosuggestions.

Acceptance criteria:

- `tests/zsh/run.zsh` has a test that loads zsh-autosuggestions from a pinned revision, skipped when
  it is not available.
- The test shows the suggestion's `region_highlight` entry survives a highlight update and that the
  suggested text receives no fast-highlight spans.

## Theming and configuration

### 26. Italic and faint on newer zsh

Themes reject `italic` and `faint` because zsh 5.9 does not support them. A later zsh that does
would need a version-gated check.

Acceptance criteria:

- The theme loader accepts `italic` and `faint` when the running zsh supports them and rejects them,
  with the current message, otherwise.
- Tests cover both outcomes.

### 27. Theme listing and inheritance

The `[meta]` table is parsed and discarded, there is no command that lists themes, and a user theme
cannot inherit from another user theme.

Acceptance criteria:

- `fast-highlight themes` lists built-in and user themes with the description from `[meta]`.
- A user theme can inherit from another user theme; an inheritance cycle is reported by
  `fast-highlight check-config`.
- `man/man1/fast-highlight.1` and `man/man5/fast-highlight.5` document both.

### 28. Colour name leniency

Colour abbreviations and mixed-case names are rejected, and an unknown colour gets no suggestion.

Acceptance criteria:

- `Red` and `RED` are accepted as `red`.
- `fast-highlight check-config` reports `rd` with a suggestion of `red`.

### 29. Log path resolution

`log.file` rejects `~user`. A relative `XDG_STATE_HOME` is accepted, although the XDG specification
says to ignore it. With `HOME` unset, there is no fallback to the password database for the log
path.

Acceptance criteria:

- `log.file = "~alice/fh.log"` resolves through the password database.
- A relative `XDG_STATE_HOME` is ignored and the default under `HOME` is used.
- With `HOME` unset, the log path is derived from the password database entry.

### 30. ANSI renderer fidelity

`fast-highlight highlight --format ansi` approximates theme styles.

Acceptance criteria:

- Each style attribute the theme format supports renders as the matching SGR sequence.
- A snapshot test compares the ANSI output for the default theme against expected escape sequences.

## Performance

### 31. Large buffers in the plugin

Writes above 64 KiB are not strictly linear in time, which matters if `FASTHL_MAX_LENGTH` is raised.
zsh redraw cost with about 500 `region_highlight` entries is unmeasured. Validating that each span's
start does not exceed its end costs about 2.2 ms per 1000 spans.

Acceptance criteria:

- A benchmark script measures request write time at 16, 64, 256, and 1024 KiB, and redraw time at
  100, 500, and 2000 entries, with results recorded in the README.
- Span validation cost falls below 1 ms per 1000 spans on the benchmark machine.

### 32. Parser allocation and worst-case paths

A full parse allocates one `String` per word (about 100 ns per word). The `[...]` failure cache
holds one range, so unusual inputs with `(` inside character classes can rescan. The spans-only
worst case for a 64 KiB buffer is about 1.4 ms; the lex-only full-parse path for `a|a|...` takes
3.1 ms if the lex-only threshold is raised.

Acceptance criteria:

- A full parse of a 200-character buffer performs no per-word heap allocation, measured with a
  counting allocator in a test.
- An input built to defeat the one-range cache parses in linear time, checked by a test with a
  time bound.
- The 64 KiB worst cases stay at or below their current figures.

## Platforms and packaging

### 33. Release publication

The repository is public at `https://github.com/wolpert/fast-highlight`. Packages need tagged
releases and source archives with checksums.

Acceptance criteria:

- Tag `v0.1.0` exists, with a release archive and its SHA-256 checksum published.
- A check fails the build when the Rust version in the README requirements table differs from
  `rust-version` in `Cargo.toml`.

### 34. Installation of plugin files and man pages

`cargo install` installs only the binary; the plugin files and man pages are copied by hand.

Acceptance criteria:

- A `make install` target (or equivalent script) honours `PREFIX` and `DESTDIR` and installs the
  binary to `bin/`, the plugin to `share/fast-highlight/`, and the man pages to `share/man/man1/`
  and `share/man/man5/`.
- After `make install PREFIX=$HOME/.local`, `fast-highlight plugin-path` prints the installed plugin
  file and `man 5 fast-highlight` opens the format page.
- The packages in tasks 35, 36, and 37 use this target.

### 35. Homebrew formula

Acceptance criteria:

- A formula in a tap builds from the release archive and installs the binary, plugin files, and
  man pages.
- `brew test fast-highlight` runs `fast-highlight check-config` and
  `fast-highlight highlight` on a sample line.
- `brew audit --strict` passes.
- The formula's caveats give the `.zshrc` source line.

### 36. AUR package

Acceptance criteria:

- A `PKGBUILD` builds from the release archive with `makepkg` in a clean chroot.
- `namcap` reports no errors for the `PKGBUILD` or the package.
- The package installs the plugin under `/usr/share/fast-highlight/` and the man pages under
  `/usr/share/man/`.

### 37. FreeBSD port

Acceptance criteria:

- A port under `textproc/` or `shells/` builds with `poudriere testport` on a supported FreeBSD
  release.
- `portlint -AC` reports no errors.
- The installed `pkg-plist` includes the plugin files and both man pages.

### 38. macOS verification

The daemon and plugin are untested on macOS. Points of difference include the pipe write size,
non-blocking `sysopen`, the absence of `/proc` for the daemon process identity check, the system
allocator (the `mallopt` tuning applies only to glibc), and the 64 KiB worst-case latency.

Acceptance criteria:

- `cargo test --release` and `zsh tests/zsh/run.zsh` with `FASTHL_TEST_BIN` set to the release
  binary pass on macOS with the system zsh and with Homebrew zsh.
- The README records daemon memory after a 64 KiB request and the 64 KiB worst-case latency on
  macOS, with the machine and OS version.

### 39. FreeBSD verification

The same points as task 38 apply on FreeBSD, whose allocator is jemalloc and whose `/proc` is
usually not mounted.

Acceptance criteria:

- `cargo test --release` and `zsh tests/zsh/run.zsh` with `FASTHL_TEST_BIN` set pass on a supported
  FreeBSD release without `/proc` mounted.
- The README records memory and latency figures for FreeBSD as in task 38.

### 40. zsh 5.8 verification

zsh 5.8 lacks the `memo=` field, so the plugin removes its own `region_highlight` entries by
position and style sequence. That path has been exercised only by simulation on zsh 5.9.

Acceptance criteria:

- `FASTHL_TEST_ZSH` pointing at a real zsh 5.8 build runs `tests/zsh/run.zsh` with all tests
  passing.
- A test with another plugin's `region_highlight` entries present shows those entries kept on
  zsh 5.8.

## Testing and tooling

### 41. Continuous integration

There is no CI configuration.

Acceptance criteria:

- A GitHub Actions workflow runs on push and pull request on `ubuntu-latest` and `macos-latest`.
- Each job runs `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, and
  `cargo test --release` (which includes the latency tests).
- Each job runs `zsh tests/zsh/run.zsh` with `FASTHL_TEST_BIN=target/release/fast-highlight`.
- A job on the nightly toolchain runs each fuzz target (`lexer`, `highlight`, `protocol`) for 60
  seconds with `-max_total_time=60` and fails on any crash, uploading `fuzz/artifacts/`.
- A job builds with the toolchain named by `rust-version` in `Cargo.toml`.

### 42. Protocol seed replay modes

The `protocol` fuzz target has a stream mode and a round-trip mode, selected by the low bit of the
first byte. `tests/fuzz_replay.rs` replays round-trip seeds in stream mode, so `cargo test` does not
exercise the round-trip invariants.

Acceptance criteria:

- `cargo test --test fuzz_replay` replays each odd-mode seed through the same round-trip decoding
  and checks as `fuzz/fuzz_targets/protocol.rs`.
- A deliberately broken round-trip seed fails the test.

### 43. Fuzz scratch directory cleanup

The `highlight` fuzz target creates `fast-highlight-fuzz-<pid>` under the system temp directory and
removes it only at the start of a run with the same process ID, so each run leaves one behind.

Acceptance criteria:

- After a `cargo +nightly fuzz run highlight -- -max_total_time=10` run ends, no
  `fast-highlight-fuzz-*` directory from that run remains in `$TMPDIR`.
