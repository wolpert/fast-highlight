# Remaining work

Numbered tasks for fast-highlight after v1, grouped by area. Task numbers run consecutively across
all groups and are stable identifiers; a completed task keeps its number.

## Command specs

### 1. Additional built-in specs

The built-in set in `specs/` holds 23 command specs. Common commands with subcommands or wrapped
commands have no spec: `podman`, `gh`, `apt`, `dnf`, `brew`, `pip`, `uv`, `go`, `rustup`, `make`,
`ssh`, `tar`, `xargs` (a precommand whose options precede the wrapped command), `journalctl`, and
`ip`.

Acceptance criteria:

- Each listed command has a file under `specs/` and an entry in the built-in table.
- `fast-highlight check-config` reports no errors with the new specs loaded.
- A snapshot test per new spec shows a valid subcommand, an unknown subcommand styled as an error,
  and a known option.
- For `xargs`, `xargs -0 -n1 rm -f` styles `rm` as a command and `-f` as an option of `rm`.

### 2. Upstream subcommand and option refresh

Several existing specs predate recent upstream releases. Missing subcommands include
`git backfill`, `last-modified`, `repo`, and `refs`, `git stash export` and `import`, `npm trust`
and `undeprecate`, and `systemctl sleep` and `enqueue-marked-jobs`. Option coverage is thin for git
plumbing commands and for `docker swarm`, `stack`, `service`, and `plugin`.

Acceptance criteria:

- Each named subcommand highlights as a valid subcommand, checked by a snapshot test.
- Each spec file records the upstream version its lists were checked against.
- `docker service create --replicas 3 nginx` highlights `--replicas` as a known option.

### 3. Assignments with expansions after `env`

In `env FOO=$x cmd`, a word containing an expansion has no literal text, so the spec engine takes
the assignment for the wrapped command. The argument input needs a flag for a word that starts with
a literal `NAME=`.

Acceptance criteria:

- `env FOO=$x ls` styles `FOO=$x` as an assignment and `ls` as a command.
- `env FOO="$(date)" BAR=1 ls` does the same for both assignments.
- A word without a literal `NAME=` prefix, such as `env $cmd`, keeps its current styling.

### 4. Long-option abbreviations

GNU-style programs accept any unambiguous prefix of a long option (`--verb` for `--verbose`). The
spec engine recognises only exact names.

Acceptance criteria:

- A spec field marks a command as accepting abbreviations; it is off by default.
- With the field set, an unambiguous prefix highlights as the full option, including its argument
  arity.
- An ambiguous prefix and a prefix of no option highlight as unknown options.

### 5. Positional arguments and option-selected modes

Specs model options before a wrapped command and nothing more. There is no model of positional
arguments, no wrapped command inside a subcommand (`kubectl exec pod -- cmd`), and no option that
changes how the remaining words are read (`sudo -e` takes files, not a command).

Acceptance criteria:

- The spec format documents a positional-argument model in `man/man5/fast-highlight.5`.
- `kubectl exec mypod -- ls -l` styles `ls` as a command.
- `sudo -e /etc/hosts` styles `/etc/hosts` as a path, not as an unknown command.

### 6. Word restrictions after `builtin` and `command`

A word after `builtin` is accepted when it names any command, and so is a word after `command`. zsh
accepts only builtins after `builtin`, and `command` skips functions and aliases.

Acceptance criteria:

- `builtin ls` styles `ls` as an error when `ls` is not a builtin.
- `command myfunc` styles `myfunc` as an error when it exists only as a function.
- `builtin echo` and `command ls` keep their current styling.

### 7. Generated built-in spec table

`BUILTIN_FILES` in `src/specs/mod.rs` is a hand-maintained table of `include_str!` entries, so a new
file in `specs/` is silently ignored until someone adds a line.

Acceptance criteria:

- A build script or a test derives the table from the contents of `specs/`.
- Adding a file to `specs/` without other changes makes the spec active, or fails `cargo test` with
  a message naming the file.

## zsh syntax coverage

### 8. Shell option modelling

The parser assumes default option settings and `SHORT_LOOPS` set. It does not model
`IGNORE_BRACES`, `IGNORE_CLOSE_BRACES`, `RC_QUOTES`, `KSH_ARRAYS`, `POSIX_IDENTIFIERS`, `SH_GLOB`,
`BRACE_CCL`, or `EQUALS`, so `=cmd` gets no span. The plugin would have to send the relevant option
state to the daemon.

Acceptance criteria:

- The protocol carries the state of each modelled option, documented in `docs/protocol.md`.
- With `RC_QUOTES` set, `echo 'it''s'` is one string span; without it, two.
- With `EQUALS` set, `=ls` gets a command-path span; with `NO_EQUALS`, it does not.
- A snapshot test per modelled option covers both settings.

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

The README installation command uses a placeholder URL
(`https://github.com/OWNER/fast-highlight`). Packages need a public repository, tagged releases, and
source archives with checksums.

Acceptance criteria:

- The README and man pages contain the real repository URL.
- Tag `v0.1.0` exists, with a release archive and its SHA-256 checksum published.
- The Rust version in the README requirements table matches `rust-version` in `Cargo.toml`
  (currently 1.85 and 1.88).

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
