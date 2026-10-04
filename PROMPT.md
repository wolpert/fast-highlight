# Build prompt: fast-highlight

Build `fast-highlight`, a fast Rust syntax highlighter for zsh that replaces
zsh-syntax-highlighting and fast-syntax-highlighting. The repository at
`~/projects/fast-highlight` is empty; run `git init` and build the whole project.

## How to work

- Build everything in one pass. Plan first, then hand implementation work to
  Opus subagents (`model: opus`), choosing effort by difficulty:
  - high: the lexer/parser, the daemon and protocol, the zsh plugin glue
  - medium: per-command specs, theming/config, fuzz targets, test harness
  - low: README, man pages, TASKS.md (all written to `style.md`)
- Have independent pieces built in parallel once the token-type list and wire
  protocol are fixed, since everything else depends on them.
- After implementation, run a review pass (staff-engineer for code,
  qa-strategist for tests) and fix what they find.
- Commit at each working milestone with clear messages.
- Use the latest stable Rust (edition 2024). cargo-fuzz needs nightly; that is
  the only exception.
- Keep dependencies lean: no async runtime. serde + toml, and clap only if it
  stays small, are fine.

## Architecture

### Integration: persistent daemon

- The zsh plugin starts one long-lived `fast-highlight serve` process per
  interactive shell and talks to it over a pair of file descriptors.
  - Do not use zsh's single `coproc` slot, because users and other plugins use
    it. Use `zsh/zpty`, or FIFOs/fds opened with `exec {fd}<>`.
  - Use `zsh/system` `sysread`/`syswrite` with a timeout.
- Hook `zle-line-pre-redraw` (with `add-zle-hook-widget`). Send buffer,
  cursor, and `PREBUFFER`. Read the response. Fill `region_highlight`.
- Design a small framed protocol (length-prefixed or a clear terminator) with
  a request id, so a stale response can never be applied to a newer buffer.
  Document it in `docs/protocol.md`.
- If the daemon is missing, crashes, or misses the timeout, highlighting
  silently turns off for that redraw. The plugin then tries to restart the
  daemon with backoff. It must never block typing, print to the terminal, or
  break the line editor.
- Clean up the daemon when the shell exits, and never leave orphans.

### Shell state awareness (hybrid)

- The daemon scans `$PATH` itself, caches the executables, and rescans when
  PATH changes or on `rehash`.
- The zsh side sends aliases, functions, builtins, reserved words, and
  `hash -d` named directories:
  - at startup, and
  - from a `precmd` hook, only when they changed (compare a cheap
    fingerprint).
- Classify commands as alias, suffix alias, global alias, function, builtin,
  reserved word, external command, precommand, or unknown (error).

### Paths on disk

- Check paths on disk directly in Rust with `stat`/`metadata` calls. Don't
  depend on external tools.
- Expand `~`, `~user`, and named directories before checking. Resolve
  relative paths against the shell's current directory, which zsh sends with
  each request (or whenever it changes).
- Distinguish an existing file, an existing directory, and a prefix of an
  existing path (while the user is still typing it).
- Path checks must not break the latency budget. Cache results with a short
  TTL, and cap the number of checks per request.

### Per-command awareness

- Define subcommands and options for common commands (git, cargo, docker,
  kubectl, systemctl, npm, at minimum) in data files (TOML).
- Ship built-in specs, and let users add or override them in their config
  directory.
- Highlight valid subcommands and options distinctly. Mark unknown
  subcommands as errors only where the spec is marked complete.

### Lexer/parser (hand-written, zsh-specific)

- Error-tolerant: the input is usually partial, and the lexer must never
  panic.
- Cover broad zsh syntax:
  - quoting: single, double, `$'...'`, and backslash escapes
  - parameter expansion `${...}`, including flags and nested forms
  - command substitution `$(...)` and backticks
  - process substitution `<(...)`, `>(...)`, and `=(...)`
  - arithmetic `$((...))` and `((...))`
  - globs, extended globs, and glob qualifiers
  - redirections, including fd forms and here-docs/here-strings
  - `[[ ]]`, reserved words, assignments (including arrays)
  - precommand modifiers (`sudo`, `noglob`, `nocorrect`, `exec`, `command`,
    `builtin`, `env`, `time`)
  - pipelines, lists, and subshells/groups
  - history expansion (`!!`, `!$`, etc.)
  - comments when `INTERACTIVE_COMMENTS` is set
- Multi-line buffers and `PREBUFFER` continuation lines are supported in v1.
- Highlight syntax errors and unknown commands in red (as an `error` token
  type). Examples: an unclosed construct at a point where it cannot be
  completed, a stray `fi`, a bad redirection.
- Offsets: `region_highlight` uses character offsets. Handle multibyte UTF-8
  correctly, and test with non-ASCII input including wide characters.

### Output and theming

- The daemon returns token types, not styles: `start end token-type`.
- The zsh side maps token types to styles through an associative array.
- Themes and config are TOML files under `${XDG_CONFIG_HOME:-~/.config}/fast-highlight/`.
  A built-in default theme is used when no file exists.
- `fast-highlight styles` (or similar) compiles the active TOML theme into the
  zsh style map, which the plugin loads at startup and on an explicit reload
  command.
- Support basic colors, 256-color, and truecolor (`fg=#rrggbb`), plus
  bold/underline/standout. Do not add NO_COLOR handling.

## Performance

- Under 1 ms of daemon processing per highlight for a 200-character buffer.
- Under 5 ms worst case for the full round trip as seen by zsh.
- Large buffers degrade gracefully:
  - above a configurable threshold (default about 10 KB), skip filesystem and
    command lookups and lex only
  - above a hard cap, return no highlighting
- No criterion benchmark suite is needed. Do add a `--timing` debug option, or
  a log, that reports per-request latency so the targets can be checked.

## Platforms and installation

- Linux, macOS, and FreeBSD.
- Install via `cargo install --git <repo>` or `cargo install --path .` from a
  local clone. No other packaging.
- The plugin is sourced from the clone (or an installed share path) in
  `.zshrc`. Document both setups.

## Testing

- Unit tests for the lexer and command classification.
- Snapshot tests (`insta`) mapping input lines to token output. Include
  partial input, errors, multi-line, and multibyte cases.
- Integration tests that drive a real zsh (through `zsh/zpty` or `expect`).
  They check that `region_highlight` is populated, that daemon crash and
  restart fall back to no highlighting, and that typing never blocks.
- `cargo-fuzz` targets for the lexer and the protocol decoder. Asserted
  invariants: no panics, spans in bounds, spans on character boundaries, and
  no overlapping spans when they should not overlap.
- CI is not required, but `cargo test`, `cargo clippy -- -D warnings`, and
  `cargo fmt --check` must pass.

## Docs and license

- `style.md` in the repository root is the style guide for every document
  the project produces. That includes `README.md`, `TASKS.md`, everything
  under `docs/`, the man pages, and Cargo.toml `description` text. Read it
  before writing any docs.
- Give every docs subagent `style.md`. After the docs are written, have a
  copy-editor pass check each one against it.
- Conflicts between the guide and the man-page format:
  - roff section structure (NAME, SYNOPSIS, DESCRIPTION, etc.) follows man
    conventions
  - the prose inside each section follows `style.md`
- `README.md`: what it is, requirements (zsh version), `cargo install`
  steps, `.zshrc` setup, how to remove or disable the old highlighters, config
  and theme reference, per-command spec format, and troubleshooting.
- Man pages in roff:
  - `fast-highlight(1)` for the binary
  - `fast-highlight(5)` for the config/theme format
  Explain how to install them (for example, `MANPATH` or copying into
  `~/.local/share/man`).
- License: BSD-3-Clause (`LICENSE` file, `license` field in Cargo.toml).

## Definition of done (v1)

- In a live zsh session, the plugin highlights:
  - commands by kind
  - strings, variables, expansions, redirections, globs, and paths
  - per-command subcommands and options
  - errors in red
- It meets the latency targets above.
- It falls back silently to no highlighting on any daemon failure.
- Tests, clippy, fmt, and the fuzz targets build. Each fuzz target has run for
  a short smoke period without crashes.
- README and man pages are complete.
- `TASKS.md` lists the remaining work as numbered tasks, each with a short
  description and acceptance criteria. Examples: more command specs, more zsh
  edge cases, packaging, performance work, plus anything deferred during the
  build. The project must be usable as shipped; TASKS.md is for what comes
  next.
