# fast-highlight

fast-highlight is a syntax highlighter for the zsh line editor that replaces zsh-syntax-highlighting
and fast-syntax-highlighting. A zsh plugin starts one `fast-highlight serve` daemon per interactive
shell and sends it the command line on every redraw. The daemon, written in Rust, parses the line
and answers with a list of ranges, each tagged with a **token type** such as `command`, `path`, or
`error`. The plugin maps each token type to a zsh style through the associative array
`FASTHL_STYLES` and writes the result to `region_highlight`.

The daemon classifies command words against the shell's aliases, functions, builtins, reserved
words, and `$PATH`, which the plugin sends it. It checks arguments against the filesystem and
recognises the subcommands and options of commands described by per-command spec files. When the
daemon is missing, crashes, or misses its time budget, the line stays unhighlighted for that redraw
and the plugin restarts the daemon with backoff. The plugin prints nothing to the terminal in any
of these cases.

The wire protocol between the plugin and the daemon is specified in
[`docs/protocol.md`](docs/protocol.md).

## Contents

- [Requirements](#requirements)
- [Installation](#installation)
- [Shell setup](#shell-setup)
- [Removal of other highlighters](#removal-of-other-highlighters)
- [Runtime controls](#runtime-controls)
- [Token types](#token-types)
- [Shell options](#shell-options)
- [Configuration](#configuration)
- [Themes](#themes)
- [Command specs](#command-specs)
- [Command-line interface](#command-line-interface)
- [Performance and limits](#performance-and-limits)
- [Man pages](#man-pages)
- [Troubleshooting](#troubleshooting)
- [Development](#development)
- [Licence](#licence)

## Requirements

| Component         | Requirement                                                                  |
|-------------------|------------------------------------------------------------------------------|
| zsh               | 5.8 or later; 5.9 recommended                                                |
| zsh modules       | `zsh/system`, `zsh/datetime`, and `zsh/parameter`; `zsh/zselect` optional    |
| External commands | `mkfifo`, run each time the daemon starts                                    |
| Temporary storage | A writable `${TMPDIR:-/tmp}`, where the plugin creates a private FIFO directory |
| Rust              | A stable toolchain, 1.88 or later (edition 2024), to build                   |
| Operating system  | Linux, macOS, or FreeBSD                                                     |

zsh 5.9 adds the `memo=` field to `region_highlight` entries, which the plugin uses to remove
exactly the entries it added. On zsh 5.8 the plugin finds its own entries by their position and
their sequence of styles, which keeps entries added by other plugins but is a heuristic.

`zsh/zselect` lets the plugin write requests larger than 4096 bytes in pieces, each after the pipe
reports itself writable, so a daemon that stops reading cannot block the shell. Without the module,
a large request goes out in a single write.

Sourcing the plugin does nothing in zsh older than 5.8, when a required module fails to load, in
non-interactive shells, in shells whose standard input is not a terminal, and in shells running a
`-c` string or a script.

## Installation

The binary installs with `cargo install`, either directly from the repository:

```sh
cargo install --git https://github.com/wolpert/fast-highlight
```

or from a local clone:

```sh
git clone https://github.com/wolpert/fast-highlight
cd fast-highlight
cargo install --path .
```

Both place `fast-highlight` in cargo's binary directory, `~/.cargo/bin` by default. `cargo install`
installs the binary only. The plugin files and the man pages stay in the repository and are
installed by hand, as described in [Plugin files](#plugin-files) and [Man pages](#man-pages). The
built-in themes and command specs are compiled into the binary.

A clone also works without `cargo install`: `cargo build --release` writes the binary to
`target/release/fast-highlight`, which the plugin uses when no `fast-highlight` is found in
`$PATH`.

### Plugin files

The plugin consists of two files, `plugin/fast-highlight.plugin.zsh` and `plugin/fasthl-core.zsh`.
They stay in the same directory: the first sources the second from its own location.

A clone needs no copying; `.zshrc` sources the plugin file in place (see
[Shell setup](#shell-setup)). For a setup that does not depend on a clone, the files go in a share
directory.

1. Copy both files from a clone at the same revision as the installed binary:

   ```sh
   mkdir -p ~/.local/share/fast-highlight
   cp plugin/fast-highlight.plugin.zsh plugin/fasthl-core.zsh ~/.local/share/fast-highlight/
   ```

2. Point `fast-highlight plugin-path` at that directory in `~/.zshrc`, before the source line:

   ```zsh
   export FAST_HIGHLIGHT_SHARE=~/.local/share/fast-highlight
   ```

3. Repeat step 1 after every upgrade of the binary, so that the plugin and the daemon come from the
   same revision.

`fast-highlight plugin-path` prints the first of these files that exists, and exits with status 1
when none does:

| Order | Candidate                                                                                |
|-------|------------------------------------------------------------------------------------------|
| 1     | `$FAST_HIGHLIGHT_SHARE/fast-highlight.plugin.zsh`, when the variable is set and non-empty |
| 2     | `<directory of the binary>/../share/fast-highlight/fast-highlight.plugin.zsh`            |
| 3     | `plugin/fast-highlight.plugin.zsh` in the source tree the binary was built from          |

For a binary in `~/.cargo/bin`, candidate 2 is `~/.cargo/share/fast-highlight/`. Copying the
plugin files there instead of `~/.local/share/fast-highlight/` makes `FAST_HIGHLIGHT_SHARE`
unnecessary.

Candidate 3 is the path recorded at build time. After `cargo install --path .` it is the clone, so
`plugin-path` works without copying for as long as the clone stays in place. After
`cargo install --git` it is cargo's checkout in its git cache, which cargo may remove when it cleans
the cache; copying the files is the dependable setup in that case.

## Shell setup

`~/.zshrc` sources the plugin with one line. From a clone:

```zsh
source /path/to/clone/plugin/fast-highlight.plugin.zsh
```

From a share directory:

```zsh
source "$(fast-highlight plugin-path)"
```

or, equivalently, `source ~/.local/share/fast-highlight/fast-highlight.plugin.zsh`.

The plugin attaches through `add-zle-hook-widget` (`zle-line-pre-redraw` and `zle-line-finish`) and
`add-zsh-hook` (`precmd` and `zshexit`). It does not wrap widgets, so its position relative to
`compinit` and to other plugins does not affect it. Three ordering constraints apply:

- The binary is located while the plugin is sourced. `$PATH` must contain its directory, or
  `FASTHL_BIN` must be set, before the source line. When no binary is found, the plugin stays
  inactive until `fasthl-enable` is run.
- The daemon starts while the plugin is sourced, so `FASTHL_SERVE_ARGS` must be set before the
  source line.
- Style overrides in `FASTHL_STYLES` go after the source line.

A source line near the end of `.zshrc`, after `$PATH` setup, `compinit`, and any plugin manager,
satisfies all three:

```zsh
path=(~/.cargo/bin $path)
autoload -Uz compinit && compinit
# Other plugins load here.
source ~/.local/share/fast-highlight/fast-highlight.plugin.zsh
FASTHL_STYLES[comment]='fg=244'
```

Sourcing the plugin a second time has no effect.

## Removal of other highlighters

zsh-syntax-highlighting and fast-syntax-highlighting both rewrite `region_highlight` on every
redraw. Loaded in the same shell as fast-highlight, they apply conflicting styles to the same text.
fast-highlight does not detect them, and neither may be loaded alongside it.

1. Remove whatever loads zsh-syntax-highlighting and fast-syntax-highlighting:

   | Loaded by            | Line or entry to remove                                                   |
   |----------------------|---------------------------------------------------------------------------|
   | Direct `source`      | `source …/zsh-syntax-highlighting.zsh` or `source …/fast-syntax-highlighting.plugin.zsh` |
   | System package       | A `source` line under `/usr/share/`, `/usr/local/share/`, or `$(brew --prefix)/share/` |
   | Oh My Zsh            | `zsh-syntax-highlighting` or `fast-syntax-highlighting` in `plugins=(…)`  |
   | Prezto               | `'syntax-highlighting'` in the `pmodules` list in `.zpreztorc`            |
   | zinit                | `zinit light` or `zinit load` of `zsh-users/zsh-syntax-highlighting` or `zdharma-continuum/fast-syntax-highlighting`, and any `zinit ice` line for it |
   | antidote             | The repository's line in `${ZDOTDIR:-~}/.zsh_plugins.txt`                 |
   | Antigen              | `antigen bundle` of either repository                                     |
   | zplug                | `zplug` of either repository                                              |

2. Start a new shell with `exec zsh`.

3. Confirm that neither is still loaded. This command prints nothing when both are gone:

   ```zsh
   typeset -p ZSH_HIGHLIGHT_VERSION FAST_HIGHLIGHT_STYLES 2>/dev/null
   ```

Disabling the load is sufficient. Uninstalling the old plugins' files is optional.

## Runtime controls

The plugin defines three user-facing functions. Each returns 1 without doing anything when run in a
subshell, so a forked copy of the shell cannot stop the parent's daemon.

| Function         | Effect                                                                                  |
|------------------|-----------------------------------------------------------------------------------------|
| `fasthl-disable` | Stops the daemon, removes the plugin's highlighting, and unhooks the plugin from ZLE.   |
| `fasthl-enable`  | Undoes `fasthl-disable`. Searches for the binary again; when there is none, prints `fasthl-enable: fast-highlight binary not found (set FASTHL_BIN)` to standard error and returns 1. |
| `fasthl-reload`  | Searches for the binary again, reloads the theme into `FASTHL_STYLES` (keeping entries set or changed since the last load), and restarts the daemon, which rereads `config.toml` and the spec files and receives the full shell state. |

The plugin reads five variables, all optional:

| Variable            | Meaning                                                                              |
|---------------------|--------------------------------------------------------------------------------------|
| `FASTHL_BIN`        | Path or command name of the binary. A value containing `/` is a path; any other value is looked up in `$PATH`. When unset, the plugin uses `fast-highlight` from `$PATH`, else `<plugin dir>/../target/release/fast-highlight`. Read when the plugin is sourced and by `fasthl-enable` and `fasthl-reload`. |
| `FASTHL_TIMEOUT`    | Seconds the plugin waits for the daemon per redraw. Default `0.05`. A value that is not a decimal number counts as unset. Read on every redraw. |
| `FASTHL_STYLES`     | Associative array mapping token types to `region_highlight` styles. Filled from `fast-highlight styles` when the plugin loads. Entries assigned after sourcing take precedence over the theme and survive `fasthl-reload`. An empty value leaves a token type unstyled. The plugin passes values to zsh without validating them. |
| `FASTHL_SERVE_ARGS` | Array of extra arguments for `fast-highlight serve`, such as `(--timing)` or `(--log /tmp/fh.log)`. Read each time the daemon starts. |
| `FASTHL_MAX_LENGTH` | Most bytes of `PREBUFFER` followed by `BUFFER` the plugin sends to the daemon. Default `65536`. A longer command line is not sent and gets no highlighting. A value that is not a decimal integer of at most 15 digits counts as unset. Raising it has an effect only together with `limits.hard-cap-bytes`. Read on every redraw. |

When the daemon misses `FASTHL_TIMEOUT`, crashes, or sends a malformed frame, the plugin kills it
and leaves that redraw unhighlighted. It starts a new daemon after a backoff that begins at 0.5
seconds, doubles with each consecutive failure up to 60 seconds, and resets after a successful
highlight.

Nothing is highlighted when the buffer is empty, or in the `vared` and `select` contexts, where the
edited text is not a command line.

## Token types

Token type names are the keys of the `[styles]` table in a theme file and of `FASTHL_STYLES`. The
default style column gives the style in the built-in `default` theme; an empty cell means
unstyled.

Ranges nest: a parameter inside a double-quoted string is reported inside the string's range, and
zsh applies the inner style over the outer one.

| Token type             | Meaning                                                                    | Default style      |
|------------------------|----------------------------------------------------------------------------|--------------------|
| `default`              | Plain text with no particular meaning. Rarely emitted.                     |                    |
| `error`                | A syntax error, unknown command, unknown subcommand, or invalid option.    | `fg=red,bold`      |
| `reserved-word`        | A reserved word in command position: `if`, `then`, `for`, `{`, `[[`, `!`.  | `fg=yellow`        |
| `alias`                | A command word that names a regular alias.                                 | `fg=green`         |
| `suffix-alias`         | A command word resolved through a suffix alias (`alias -s`).               | `fg=magenta`       |
| `global-alias`         | A word anywhere on the line that names a global alias (`alias -g`).        | `fg=magenta,bold`  |
| `function`             | A command word that names a shell function, or the name in a function definition. | `fg=green`  |
| `builtin`              | A command word that names a shell builtin.                                 | `fg=green`         |
| `command`              | A command word that names an external command in `$PATH` or by explicit path, and a word `=cmd` that expands to the path of one. | `fg=green` |
| `precommand`           | A precommand modifier such as `sudo`, `noglob`, `exec`, `command`, `env`, or `time`. | `fg=green,underline` |
| `separator`            | A command separator or pipeline operator: `;`, `&`, `&&`, `\|\|`, `\|`, `\|&`, `&!`, `&\|`. | `bold` |
| `redirection`          | A redirection operator with any fd prefix: `>`, `2>`, `>&2`, `<<`, `<<<`, `&>`. | `bold`        |
| `heredoc`              | A here-document delimiter word and the here-document body.                 | `fg=yellow`        |
| `single-quoted`        | A single-quoted string `'...'`.                                            | `fg=yellow`        |
| `double-quoted`        | A double-quoted string `"..."`.                                            | `fg=yellow`        |
| `dollar-quoted`        | A dollar-quoted string `$'...'`.                                           | `fg=yellow`        |
| `backquoted`           | A backquoted command substitution, delimiters included.                    | `fg=magenta`       |
| `escape`               | A backslash escape, inside or outside quotes.                              | `fg=cyan`          |
| `parameter`            | A parameter expansion: `$name`, `${...}`, `$1`, `$?`.                      | `fg=cyan`          |
| `substitution`         | The delimiters `$(` and `)` of a command substitution.                     | `fg=magenta`       |
| `process-substitution` | The delimiters `<(`, `>(`, `=(`, and `)` of a process substitution.        | `fg=magenta`       |
| `arithmetic`           | An arithmetic expansion or command: `$((...))` and `((...))`.              | `fg=magenta`       |
| `glob`                 | Glob characters and extended glob operators: `*`, `?`, `[...]`, `**`, `#`, `^`, `~`. | `fg=blue` |
| `glob-qualifier`       | A glob qualifier list such as `(.)` or `(om[1,3])` at the end of a word.   | `fg=blue,bold`     |
| `brace-expansion`      | A brace expansion `{a,b}` or `{1..10}`.                                    | `fg=blue`          |
| `history-expansion`    | A history expansion: `!!`, `!$`, `!-2`, `^old^new`.                        | `fg=blue`          |
| `comment`              | A comment, recognised only when `INTERACTIVE_COMMENTS` is set.             | `fg=8`             |
| `assignment`           | The `name=` or `name+=` part of an assignment, and the parentheses of an array value. |         |
| `grouping`             | Subshell and group delimiters `(` and `)`, and the parentheses of a function definition. | `fg=yellow` |
| `operator`             | An operator inside `[[ ... ]]`: `-f`, `==`, `=~`, `&&`, `!`, `<`.          | `fg=yellow`        |
| `path`                 | An argument that names an existing file.                                   | `underline`        |
| `path-directory`       | An argument that names an existing directory.                              | `bold,underline`   |
| `path-prefix`          | The word at the cursor, when it is a prefix of an existing path.           | `underline`        |
| `subcommand`           | A subcommand known to the command's spec (`git commit`, `cargo build`).    | `fg=blue`          |
| `option`               | An option known to the command's spec (`--verbose`, `-C`).                 | `fg=cyan`          |

`fg=8` is bright black, which terminals with 16 or more colours show as grey.

## Shell options

The plugin sends the state of the zsh options that change how a command line is read with every
request, and the daemon highlights the line as zsh with those options reads it. The option letters
are listed in [`docs/protocol.md`](docs/protocol.md).

| Option                 | Effect on highlighting                                                     |
|------------------------|----------------------------------------------------------------------------|
| `INTERACTIVE_COMMENTS` | A word starting with `#` begins a comment.                                 |
| `AUTO_CD`              | A directory in command position is valid.                                  |
| `EXTENDED_GLOB`        | `#`, `##`, `~`, `^`, and globbing flags such as `(#i)` are glob operators. |
| `KSH_GLOB`             | `@(...)`, `*(...)`, `+(...)`, `?(...)`, and `!(...)` are glob patterns.    |
| `IGNORE_BRACES`        | No brace expansion. A `{` is a reserved word only as a whole word, `{fd}>` is no redirection, and a `}` is never split off the end of a word. Includes the effect of `IGNORE_CLOSE_BRACES`. |
| `IGNORE_CLOSE_BRACES`  | A `}` closes a group only in command position, so `{ echo a }` leaves the group open. After `fi`, `done`, and `end`, a `}` is an error. A `case ... {` is closed only by `esac`. |
| `RC_QUOTES`            | `''` inside a single-quoted string is a quote character: `'it''s'` is one string. |
| `KSH_ARRAYS`           | An unbraced parameter takes no subscript and no modifier: `$a[1]` is the parameter `$a` followed by the glob `[1]`. |
| `POSIX_IDENTIFIERS`    | Parameter and assignment names are ASCII only, and `$#name` is `$#` followed by `name`. |
| `SH_GLOB`              | A parenthesised glob group or qualifier list (`*(.)`, `a(b)`, `(a|b)`) is an error. |
| `BRACE_CCL`            | Braces with no comma and no `..`, such as `{abc}`, are a brace expansion.  |
| `EQUALS`               | Set by default. A word `=cmd` is a `command` when `cmd` is an external command, and an error otherwise. |
| `SHORT_LOOPS`          | Set by default. When it is unset, a `for`, `select`, or `repeat` body that starts with neither `do` nor `{` is an error. |

The following effects of these options are not modelled:

- `=` expansion in an assignment value (`x==ls`, `PATH=$PATH:=ls`), in a brace expansion
  (`{=ls,=cat}`), and in a word with another expansion (`=$cmd`).
- The short form `function f() cmd`, which zsh rejects with `SHORT_LOOPS` unset.
- A group nested in a `KSH_GLOB` group (`@(a|(b))`), which is a bad pattern with `SH_GLOB` set.

## Configuration

### Configuration directory

The binary resolves its configuration directory as the first of:

1. `$FAST_HIGHLIGHT_CONFIG_DIR`, when set and non-empty.
2. `$XDG_CONFIG_HOME/fast-highlight`, when `XDG_CONFIG_HOME` is set to an absolute path.
3. `$HOME/.config/fast-highlight`, taking the home directory from the password database when `HOME`
   is unset or empty.

`fast-highlight check-config` prints the directory it resolved. The daemon inherits its environment
from the shell when it starts, so a change to these variables takes effect after `fasthl-reload`.

Every file in the directory is optional, and none is created automatically:

| File               | Content                                                    |
|--------------------|------------------------------------------------------------|
| `config.toml`      | The settings described below.                              |
| `theme.toml`       | The theme used when `config.toml` sets no `theme`.         |
| `themes/NAME.toml` | A theme selected with `theme = "NAME"`.                    |
| `specs/*.toml`     | Per-command specs, described in [Command specs](#command-specs). |

The daemon reads `config.toml` and the spec files when it starts. The plugin reads the theme,
through `fast-highlight styles`, when it loads. After any change in the directory,
`fasthl-reload` applies it to the current shell.

### Settings

| Key                        | Type    | Default    | Meaning                                                        |
|----------------------------|---------|------------|----------------------------------------------------------------|
| `theme`                    | string  | unset      | Theme name; see [Theme selection](#theme-selection). Must be non-empty, must not start with `.`, and must not contain `/`. |
| `limits.lex-only-bytes`    | integer | `10240`    | Above this many bytes of text, the daemon reports syntax only. |
| `limits.hard-cap-bytes`    | integer | `65536`    | Above this many bytes of text, the daemon reports nothing. Must be greater than 0 and at least `limits.lex-only-bytes`. |
| `limits.max-spans`         | integer | `500`      | Most ranges the daemon returns per request; a longer list is cut to its first `limits.max-spans` ranges. Must be greater than 0. |
| `limits.max-path-checks`   | integer | `64`       | Uncached filesystem checks allowed per request. `0` disables path checks. |
| `limits.path-cache-ttl-ms` | integer | `1000`     | Milliseconds a cached filesystem check stays valid.            |
| `log.timing`               | boolean | `false`    | Write one timing line per request to the log.                  |
| `log.file`                 | string  | `${XDG_STATE_HOME:-$HOME/.local/state}/fast-highlight/fast-highlight.log` | Log file. An absolute path, or one starting with `~/`; `~user` is rejected. |

The text measured by the two size limits is `PREBUFFER` followed by `BUFFER`. Unknown keys and
values of the wrong type are errors. When `config.toml` has an error, the daemon logs it and runs
with the defaults, `fast-highlight styles` exits with status 1 (see
[Style table output](#style-table-output)), and `fast-highlight check-config` reports it.

A `config.toml` that sets every key to its default, except for the theme:

```toml
theme = "mine"

[limits]
lex-only-bytes = 10240
hard-cap-bytes = 65536
max-spans = 500
max-path-checks = 64
path-cache-ttl-ms = 1000

[log]
timing = false
file = "~/.local/state/fast-highlight/fast-highlight.log"
```

## Themes

A theme maps token types to zsh `region_highlight` styles. The plugin loads it into
`FASTHL_STYLES`; the daemon never sees styles.

### Theme selection

- With no `theme` key in `config.toml`, the theme is `<config_dir>/theme.toml` when that file
  exists, else the built-in `truecolor` theme.
- With `theme = "NAME"`, the theme is `<config_dir>/themes/NAME.toml` when that file exists, else
  the built-in theme `NAME`. When neither exists, the theme is an error.

### Built-in themes

| Name        | Description                                                                          |
|-------------|--------------------------------------------------------------------------------------|
| `default`   | Basic colours only, readable on dark and light terminals and on terminals without truecolor support. Every token type has an entry. |
| `truecolor` | The default theme. 24-bit colours for dark terminals, inheriting from `default`. Requires a terminal with truecolor support, or the `zsh/nearcolor` module on 88- and 256-colour terminals. |

The sources are `themes/default.toml` and `themes/truecolor.toml`. On a terminal without truecolor
support, set `theme = "default"` in `config.toml`.

### Theme file format

```toml
inherits = "default"

[meta]
name = "mine"
description = "Basic colours with bolder errors and grey comments"

[styles]
error = { fg = "red", bg = "#202020", bold = true }
comment = { fg = 244 }
command = "fg=green,bold"
path-prefix = ""
```

| Key                | Type   | Default     | Meaning                                                           |
|--------------------|--------|-------------|-------------------------------------------------------------------|
| `inherits`         | string | `"truecolor"` | The base theme: `"none"` or the name of a built-in theme. A theme file in the configuration directory cannot be inherited. |
| `meta.name`        | string | unset       | Informational.                                                    |
| `meta.description` | string | unset       | Informational.                                                    |
| `styles.KIND`      | string or table | inherited | The style for token type `KIND`. An empty string leaves the type unstyled, removing the inherited style. |

Any other key, and any token type name not in [Token types](#token-types), is an error.

A style in string form is a comma-separated list of items, with whitespace around each item
ignored:

- `fg=COLOUR` and `bg=COLOUR`: the foreground and background colour.
- `bold`, `underline`, and `standout`: attributes.
- `none`: no styling. It cannot be combined with other items.

Each of `fg`, `bg`, and each attribute appears at most once.

A style in table form has the keys `fg` and `bg`, each a colour string or an integer from 0 to 255,
and `bold`, `underline`, and `standout`, each `true` or `false`. It is converted to string form in
the order `fg`, `bg`, attributes.

A colour is one of `black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `white`, and
`default`; a number from 0 to 255; or a hexadecimal colour `#rgb` or `#rrggbb`. `italic` and `faint`
are rejected, because zsh 5.9 does not support them in `region_highlight`.

### Style table output

`fast-highlight styles` compiles the active theme into zsh code, which the plugin evaluates when it
loads and on `fasthl-reload`:

```zsh
typeset -gA FASTHL_STYLES
FASTHL_STYLES=(
  alias 'fg=#8fd18f'
  arithmetic 'fg=#d19a66'
  ...
)
```

Token types with an empty style are omitted. When `config.toml` has an error, the command prints
the error to standard error and selects the theme as if `config.toml` set no `theme`. When the
theme has an error, it prints the error and the built-in `truecolor` theme. Either error makes the
exit status 1. The plugin uses the output whatever the exit status. Only when `styles` prints no
table at all does the plugin use a fallback table of its own, which matches the `truecolor` theme.
`fast-highlight check-config` shows the error.

### Style overrides in .zshrc

Individual entries of `FASTHL_STYLES` assigned after the source line take precedence over the
theme:

```zsh
FASTHL_STYLES[command]='fg=green,bold'
FASTHL_STYLES[path-prefix]=''
```

These overrides survive `fasthl-reload`. Unlike theme files, they are not validated.

## Command specs

A spec describes the subcommands and options of one command. The daemon uses specs to give
known subcommands the `subcommand` type, known options the `option` type, and, where a spec says
its list is complete, unknown ones the `error` type.

### Spec locations

Built-in specs are compiled into the binary from `specs/*.toml` in the repository. User specs are
the files matching `<config_dir>/specs/*.toml`, read in file-name order after the built-ins. The
file name has no meaning; the `name` key identifies the command.

### Built-in specs

Specs with subcommands:

| Command     | Top level complete | Complete subcommand groups                                          |
|-------------|--------------------|---------------------------------------------------------------------|
| `git`       | No                 | `bundle`, `notes`, `remote`, `rerere`, `sparse-checkout`, `stash`, `submodule`, `worktree` |
| `cargo`     | No                 | None                                                                |
| `docker`    | No                 | `container`, `image`, `network`, `volume`                           |
| `kubectl`   | No                 | `auth`, `certificate`, `config`, `create secret`, `create service`, `plugin`, `rollout`, `set`, `top` |
| `npm`       | Yes                | None                                                                |
| `systemctl` | Yes                | None                                                                |
| `podman`    | Yes                | `artifact`, `container`, `farm`, `generate`, `healthcheck`, `image`, `image trust`, `kube`, `machine`, `machine os`, `manifest`, `network`, `pod`, `quadlet`, `secret`, `system`, `system connection`, `volume` |
| `gh`        | No                 | Every command group, such as `pr`, `issue`, `repo`, `release`, `run`, and `workflow` |
| `apt`       | Yes                | None                                                                |
| `dnf`       | No                 | `advisory`, `config-manager`, `copr`, `environment`, `group`, `history`, `manifest`, `mark`, `module`, `offline`, `repo`, `system-upgrade`, `versionlock` |
| `brew`      | No                 | `services`                                                          |
| `pip`       | Yes                | None                                                                |
| `uv`        | Yes                | `auth`, `cache`, `pip`, `python`, `self`, `tool`, `workspace`       |
| `go`        | Yes                | `mod`, `work`                                                       |
| `rustup`    | Yes                | `component`, `override`, `self`, `set`, `show`, `target`, `toolchain` |
| `ip`        | Yes                | None; the object levels are not complete, since their verbs are not listed |

The top levels of `git`, `cargo`, `docker`, `kubectl`, `gh`, `dnf`, and `brew` are not complete,
because each runs external subcommands (`git-NAME`, `cargo-NAME`, Docker CLI plugins, `kubectl-NAME`,
`gh` extensions and aliases, `dnf` plugins and aliases, and `brew-NAME`). An unknown subcommand of
these commands is therefore not an error. `ip` resolves any prefix of an object name, so its spec
declares each prefix as an alias of the object it selects. No built-in spec sets `options-complete`,
so an unknown option is never an error under a built-in spec.

Option-only specs, for commands whose arguments are not subcommands: `make`, `ssh`, `tar`,
`journalctl`, and `rm`. They declare options and no subcommands, and `ssh` stops option parsing at
the destination. `tar` recognises its long options and the options that start with `-`, but not the
old style with no leading dash (`tar xzf archive`).

Long-option abbreviations (`option-abbreviations`) are enabled in the specs of commands that accept
any unique prefix of a long option and whose specs declare every long option of the checked
version: `tar`, `make`, `xargs`, `rm`, `journalctl`, `env`, `timeout`, `nice`, `nohup`, `stdbuf`,
`time`, `ionice`, `taskset`, and `chrt`. In these, `tar --dir src` is `--directory src`. The other
built-in specs do not enable it: `git`, `npm`, `docker`, `cargo`, `kubectl`, `gh`, `uv`, `ip`, and
`ssh` either do not accept abbreviations or have option lists that are not complete enough, and
the `sudo` checked here (sudo-rs) rejects them.

Precommand specs: `sudo`, `doas`, `env`, `nice`, `nohup`, `time`, `timeout`, `stdbuf`, `ionice`,
`chrt`, `taskset`, `xargs`, `noglob`, `nocorrect`, `exec`, `command`, `builtin`, and `-`. A
precommand word gets the `precommand` type, its own options are classified by its spec, and the
command it wraps is classified as a command word. A function or global alias with the same name as
a precommand takes precedence over it, unless a regular alias or a reserved word of the same name
also exists. A regular alias outranks both and keeps the word a precommand, so `alias sudo='sudo '`
keeps `sudo` a precommand even when a function `sudo` is defined; the reserved words `time` and
`nocorrect` outrank a function in the same way. A quoted word skips alias lookup, so `\sudo` is the
function `sudo` when one is defined. This shadowing applies to every command word except the one
directly after `builtin` or `command`, which skip functions and aliases.

The word after `builtin` or `command` is looked up as zsh does:

- After `builtin`, only a builtin is accepted. A builtin that is a precommand (`builtin`, `command`,
  `exec`, `noglob`, `-`) gets the `precommand` type and its chain continues. Any other word is an
  error: an external command, a function, an alias, a reserved word such as `time`, a path, an
  unknown name, and `--` (`builtin` takes no options).
- After `command`, only an external command is accepted: a command found in `PATH`, or a path to an
  executable file. Functions, aliases, builtins (`cd`, `typeset`), reserved words, and precommand
  builtins (`command builtin echo`, `command noglob ls`) are errors. An external precommand keeps
  the `precommand` type and its chain continues, even when a function of the same name exists:
  `command time ls` and `command nice ls` run the programs. A `PATH` binary named `command` or
  `builtin` makes `command command` a precommand in the same way.
- `command` takes the options `-p`, `-v`, and `-V`, combined in one word in any order (`-pv`, `-Vp`),
  and `--` ends them. Several option words (`-p -v ls`) are accepted only when one of them has a
  `v` or `V`; otherwise one option word is the most, so in `command -p -p ls` the first `-p` is the
  command word, an error. Any other word, an unknown option (`command -x ls`, `command -p -x ls`)
  or a lone `-` included, is the command word; after `--`, `command -- -v ls` takes `-v` as the command word.
- With `-v` or `-V`, `command` runs nothing: every remaining word is a lookup. Each gets the type of
  what it names (alias, global or suffix alias, function, builtin, reserved word, or command),
  whether quoted or not, and is an error only when it names nothing. `command -v myfunc` is not an
  error. A lookup never continues a precommand chain.
- A word that `builtin` or `command` rejects wraps nothing: its arguments are only checked as
  paths, so `builtin git status` does not classify `status` as a `git` subcommand. A word with an
  expansion or a glob is never an error. A quoted word is looked up by its value, and no alias is
  expanded. With `AUTO_CD`, a directory after either word is not a command. While the word is being
  typed, a prefix of a name the lookup accepts (a builtin after `builtin`, a `PATH` command after
  `command`, any name after `command -v`) has no highlighting.

The specs of `builtin` and `command` decide only whether they are precommands; the lookups and the
options above are built in and ignore spec overrides. Global aliases after these words, the
default `PATH` search of `command -p`, and the `POSIX_BUILTINS` option are not modelled.

Two built-in specs use `wrapped-after` and `path-mode-options`, described in
[Wrapped commands and path mode](#wrapped-commands-and-path-mode): `kubectl exec` wraps the
command after `--` (`kubectl exec mypod -- ls -l`), and `sudo -e` (`--edit`) takes files rather
than a command (`sudo -e /etc/hosts`).

### Spec file format

```toml
name = "git"
common-options = ["-h", "--help"]
options = ["-C=", "-c=", "-p", "--paginate", "--git-dir=", "--version"]

[subcommands.commit]
options = ["-m=", "--message=", "-a", "--all", "--amend", "-S=?", "--gpg-sign=?"]

[subcommands.remote]
complete = true

[subcommands.remote.subcommands.remove]
aliases = ["rm"]
```

Top-level keys:

| Key                | Type             | Default    | Meaning                                                   |
|--------------------|------------------|------------|-----------------------------------------------------------|
| `name`             | string           | (required) | The command name.                                         |
| `aliases`          | array of strings | `[]`       | Other command names that share this spec.                 |
| `merge`            | boolean          | `false`    | In a user file: merge into the existing spec of the same name instead of replacing it. |
| `common-options`   | array of strings | `[]`       | Options valid at the top level and at every subcommand level, at any depth. |
| `precommand`       | boolean          | `false`    | The command runs another command (`sudo`, `env`, `nice`). The top level wraps with `positional = 0` unless it sets `positional` or `wrapped-after`. |
| `skip-assignments` | boolean          | `false`    | When the top level wraps: `NAME=value` words before the wrapped command are skipped (`env`), also when the value contains an expansion (`FOO=$x`). |

The top level also accepts every level key.

Level keys, valid at the top level and in every `[subcommands.NAME]` table:

| Key                    | Type             | Default | Meaning                                                      |
|------------------------|------------------|---------|--------------------------------------------------------------|
| `options`              | array of strings | `[]`    | Options valid at this level only.                            |
| `subcommands`          | table            | `{}`    | Subcommands of this level, keyed by name.                    |
| `aliases`              | array of strings | `[]`    | Subcommand tables only: other names for this subcommand (`docker container ls` and `ps`). |
| `complete`             | boolean          | `false` | The subcommand list is complete: an unknown word in subcommand position is an error. |
| `options-complete`     | boolean          | `false` | The option list is complete: an unknown option is an error.  |
| `options-first`        | boolean          | `false` | Options are recognised only before the first plain argument; later words are plain (`docker run IMAGE CMD -x`). |
| `abbreviations`        | boolean          | `false` | A unique prefix of a subcommand name or alias selects that subcommand (`npm inst`). For options, see `option-abbreviations`. |
| `option-abbreviations` | boolean          | `false` | A unique prefix of a declared long option selects that option (`--verb` for `--verbose`). For subcommands, see `abbreviations`. |
| `positional`           | integer          | unset   | The level wraps a command: the number of plain arguments between the options and the wrapped command (`timeout DURATION cmd`). |
| `wrapped-after`        | string           | unset   | The level wraps the command after the first `--` (`kubectl exec POD -- cmd`). `"--"` is the only accepted value. |
| `path-mode-options`    | array of strings | `[]`    | Options of this level that make the remaining words files rather than a command (`sudo -e FILE`). |

Subcommand tables nest to any depth, as in `[subcommands.remote.subcommands.add]`. A level inherits
nothing from its parent except `common-options` and `option-abbreviations`: a level that leaves
`option-abbreviations` unset uses its parent's setting, and an explicit value applies to that level
and to its descendants that leave it unset. Options in a level's `options` apply only at that
level, so `git -C dir` is an option before the subcommand and unknown after it. A level sets at
most one of `positional` and `wrapped-after`.

### Option syntax

Each entry in `options`, `common-options`, and `path-mode-options` is one spelling of one option.
`path-mode-options` takes no patterns.

| Form                     | Meaning                                                                       |
|--------------------------|-------------------------------------------------------------------------------|
| `-v`, `--verbose`, `-name` | A flag that takes no value.                                                 |
| `-m=`, `--message=`      | An option with a required value, accepted attached (`--message=text`, and `-mtext` for single-letter options) or as the next word (`--message text`, `-m text`). The value word is a plain argument. |
| `-S=?`, `--color=?`      | An option with an optional value, accepted only attached (`--color=always`, `-Skey`). The next word is never consumed. |
| `+*`, `--no-*`           | A pattern: a trailing `*` matches every word that starts with the text before it. A pattern takes no separate value. Patterns are the only way a word that does not start with `-` can be an option (`cargo +nightly build`). |

Single-letter options bundle: `-av` is `-a -v` when every letter is a declared single-letter
option. A letter that takes a value ends the bundle, and the rest of the word, or else the next
word, is its value (`-xzf archive.tar`).

With `option-abbreviations = true`, a word that starts with `--` and is not a declared spelling is
matched by its stem, the text before the first `=`. The stem selects an option when it is a proper
prefix of exactly one declared spelling that starts with `--`, and the word then takes that
option's arity: for `--output=`, `--out file` consumes `file` and `--out=file` is complete; for the
flag `--verbose`, `--verb=1` is unknown.

- Only long options abbreviate. A word with a single `-` never does.
- Only declared spellings count, and patterns are never candidates. A spec that enables the key
  should declare every long option of its command; otherwise a prefix that the command finds
  ambiguous selects the one option that is declared.
- An exact spelling takes precedence over a prefix: `--exclude` is `--exclude` even when
  `--exclude-caches` is declared, and `--flag=x` for a declared flag `--flag` is unknown.
- The rule is strict: a stem that matches more than one spelling is ambiguous, even when the
  spellings are aliases of one option with the same arity (`--colo` for `--color` and `--colour`),
  which `getopt_long` would accept.
- An ambiguous stem and a stem that matches nothing are unknown options, which are errors only
  where the level has `options-complete = true`.

### Classification rules

The daemon walks a command's arguments in order, starting at the top level of its spec:

- `--` is an option and ends option parsing; every later word is plain. At an `options-first`
  level, every word after the first plain argument is plain, `--` included.
- A word that is just `-` is plain, unless `-` is declared as an option.
- A word starting with `-`, or matching a pattern, is a known or an unknown option. An unknown
  option is an error only at a level with `options-complete = true`. A word of the form
  `-<digits>` is never an error. With `option-abbreviations`, a unique prefix of a long option is
  that option, and an ambiguous or unmatched prefix is an unknown option.
- The first plain word at a level is in subcommand position. A known subcommand name or alias gets
  the `subcommand` type, and classification continues at that subcommand's level. An unknown word
  there is an error only at a level that has subcommands and `complete = true`. A word containing
  an expansion is never an error.
- After the first plain word at a level, no further subcommand matching happens at that level.
- An error is withheld for the word at the cursor while that word is a proper prefix of a valid
  subcommand or option in its position. With the cursor at the end of `systemctl stat`, `stat` has
  no highlighting, because it begins `status`; with the cursor elsewhere, it is an error.

### Wrapped commands and path mode

A level wraps a command when it sets `positional` or `wrapped-after`. The top level of a
`precommand = true` spec wraps with `positional = 0` unless it sets one of them. A top level that
wraps without `precommand` gets no `precommand` type. Before `positional` became a level key, a
top-level `positional` without `precommand` was ignored; it now wraps.

The daemon descends through subcommands as in classification, and only the level it ends at
counts; when that level does not wrap, there is no wrapped command.

- `positional = N`: the daemon skips the level's options and their values (treating unknown
  options, ambiguous abbreviations included, as flags), then `NAME=value` words when
  `skip-assignments = true` and the level is the top level, then `N` plain words. The next word is
  the wrapped command. Option parsing stops at `--` or at the first word that is not an option, as
  POSIX `getopt` does.
- `wrapped-after = "--"`: options are parsed as in classification, and the word after the first
  `--` is the wrapped command, after `NAME=value` words when `skip-assignments = true` and the level
  is the top level. With no `--`, or nothing after it, there is no wrapped command. At an
  `options-first` level `--` is found only in option position; after a plain word it is plain.
- The wrapped command is classified as a command word in its own right, against the local command
  table, so a command that exists only in a container or pod shows as an error. The arguments of a
  precommand before it are classified by its spec and not checked as paths; the arguments of
  another command before it are highlighted as usual.

A word is `NAME=value` when, after quote removal, it starts with an ASCII shell identifier and `=`
before any expansion: `FOO=1`, `FOO=$x`, and `"FOO"=$(date)` are skipped, `FOO$x=1` and `$cmd` are
not.

```toml
[subcommands.exec]
wrapped-after = "--"
options = ["-c=", "--container=", "-i", "-t"]
```

With this level in the `kubectl` spec, `kubectl exec -it mypod -- ls -l` highlights `ls` as a
command and `-l` by the spec of `ls`, if it has one. `kubectl exec mypod ls` has no wrapped command.

`path-mode-options` lists options that make the remaining words operands, as `sudo -e FILE` does.
Each entry declares the option at its level with the syntax of `options`; an entry also in
`options` or `common-options` takes the arity given here. This lets a user file make an existing
option select path mode by listing it again; the built-in specs do not repeat a spelling within a
level. When such an option is given at the level the descent ends at, while options are still
parsed, there is no wrapped command:

- The option counts when the word resolves to it: an exact spelling, the name of `--name=value`, an
  abbreviation (`--ed` for `--edit` with `option-abbreviations`), or a letter of a bundle up to and
  including the first letter that takes a value. `-Ee` and `-eu root` select the mode; `-ue` does
  not, because `e` is the value of `-u`. Unknown and ambiguous options never select it.
- The mode is sticky: later options are still parsed (`-u root` consumes `root`) and never cancel
  it. `NAME=value` words are not skipped and no plain words are counted.
- Every argument is classified and checked as a path as for a command that wraps nothing: an
  existing file gets a path type, a missing one gets none, and words after `--` are operands.
  Option values are checked too (`sudo -e -u root f` checks `root`).
- On a level that does not wrap, `path-mode-options` declares its options and has no other effect.

```toml
name = "sudo"
precommand = true
options-first = true
path-mode-options = ["-e", "--edit"]
options = ["-u=", "--user=", "-E"]
```

With this spec, `sudo -e /etc/hosts` highlights `/etc/hosts` as a path, while `sudo ls` still
highlights `ls` as a command.

### Merge and override

A user file whose `name` matches an existing spec replaces that spec entirely. With `merge = true`
it is merged instead: option lists and aliases are appended (a later spelling of the same option
takes precedence), keys present in the user file override, and subcommand tables are merged
recursively by name. Merging cannot remove anything; replacing the spec is the only way to do that.
A file with `merge = true` and no existing spec of that name loads as a new spec.
`path-mode-options` is appended like `options`. `positional` and `wrapped-after` are one setting: a
merged level that sets either replaces the level's start and clears the other.

A file that fails to parse, has an unknown key, or declares an invalid option is skipped with a
warning, and the other files still load. So is a file with a level that sets both `positional` and
`wrapped-after`, sets `wrapped-after` to anything but `"--"`, or lists a pattern in
`path-mode-options`; the warning names the command path of the level. A user file is checked on
its own before it is merged, so a `merge = true` file must pass these checks by itself. The daemon
writes the warnings to its log; `fast-highlight check-config` prints them.

A user file that makes the `git` top level complete and the options of `git commit` complete:

```toml
name = "git"
merge = true
complete = true

[subcommands.commit]
options-complete = true
```

A new spec for a command with no built-in spec:

```toml
name = "rsync"
options-complete = true
options = [
  "-a", "--archive", "-v", "--verbose", "-n", "--dry-run", "-z", "--compress",
  "-e=", "--rsh=", "--exclude=", "--delete", "--progress", "--no-*",
]
```

## Command-line interface

| Command                       | Effect                                                                  |
|-------------------------------|-------------------------------------------------------------------------|
| `fast-highlight serve [--timing] [--log PATH] [--parent PID]` | Runs the daemon on standard input and output. Started by the plugin. |
| `fast-highlight styles`       | Prints the active theme as zsh code for `FASTHL_STYLES`.                |
| `fast-highlight highlight [OPTIONS] [--] [TEXT...]` | Highlights a command line and prints the ranges.  |
| `fast-highlight check-config` | Loads the configuration, the theme, and the specs, and reports problems. |
| `fast-highlight plugin-path`  | Prints the path of the plugin file; see [Plugin files](#plugin-files).  |
| `fast-highlight help`, `--help`, `-h` | Prints the usage summary.                                       |
| `fast-highlight --version`, `-V` | Prints the version.                                                  |

The exit status is 0 on success, 1 on failure, and 2 on a usage error. Every command other than
`serve` ends quietly, without an error message, when its standard output is a closed pipe, as in
`fast-highlight check-config | head -1`.

### serve

| Option         | Effect                                                                           |
|----------------|----------------------------------------------------------------------------------|
| `--timing`     | Writes one timing line per request to the log, as `log.timing = true` does.     |
| `--log PATH`   | Uses `PATH` as the log file, overriding `log.file`. A relative `PATH` is resolved against the directory the daemon started in. |
| `--parent PID` | Exits once process `PID` no longer exists. `PID` must be a positive integer.    |

The daemon exits on a quit request, at end of input, on a framing error, when reading standard
input or writing standard output fails, and when its parent process exits. The plugin passes
`--parent` with the shell's process ID, so the daemon also exits when the shell is gone but
something else still holds the FIFOs open. The daemon writes nothing but response frames to
standard output and nothing to standard error; diagnostics go to the log file.

The daemon's working directory is `/`, so it does not keep the file system it started on busy. It
resolves relative paths against the directory it started in until a request names another.

### highlight

| Option                    | Effect                                                                 |
|---------------------------|------------------------------------------------------------------------|
| `--cwd DIR`               | Resolves relative paths against `DIR`. Default: the current directory. |
| `--opts LETTERS`          | Request option letters from [`docs/protocol.md`](docs/protocol.md): `u` character offsets, and one letter per [shell option](#shell-options) not in its default state, such as `c` for `INTERACTIVE_COMMENTS` or `E` for `NO_EQUALS`. Default: `u`. |
| `--format spans`          | Prints one `start end kind "text"` line per range. The default.        |
| `--format ansi`           | Prints the text coloured with the active theme.                        |
| `--timing`                | Prints the minimum, median, and maximum processing time to standard error. |
| `--repeat N`              | Highlights the text `N` times, for `--timing`. Default: 1.             |

The text is the `TEXT` arguments joined by spaces, or else standard input without its final
newline. The cursor is at the end of the text. The command knows the current `$PATH` and zsh's
default builtins and reserved words, but no aliases, functions, or named directories, so its output
can differ from highlighting in a shell that defines them.

```console
$ fast-highlight highlight -- 'git commit -m "x" | grep foo > /tmp/out'
0 3 command "git"
4 10 subcommand "commit"
11 13 option "-m"
14 17 double-quoted "\"x\""
18 19 separator "|"
20 24 command "grep"
29 30 redirection ">"
```

### check-config

`check-config` prints the configuration directory, then `ok` or a count of errors and warnings.
Errors in `config.toml` or the theme set exit status 1; spec warnings do not change the exit
status.

## Performance and limits

The design targets are under 1 ms of daemon processing per request for a 200-character buffer, and
under 5 ms for the round trip as seen by zsh.

The plugin caches the last result, keyed by the buffer, `PREBUFFER`, cursor position, current
directory, and option letters. A redraw with an unchanged key reuses the cached ranges without a
round trip.

Size limits, measured in bytes of `PREBUFFER` followed by `BUFFER`:

| Limit                   | Default        | Behaviour above the limit                                          |
|-------------------------|----------------|--------------------------------------------------------------------|
| `limits.lex-only-bytes` | 10240 (10 KiB) | Syntax only: no command classification, spec matching, or path checks, so no unknown-command errors. |
| `limits.hard-cap-bytes` | 65536 (64 KiB) | No highlighting at all.                                            |

The plugin applies its own limit, `FASTHL_MAX_LENGTH`, with the same default as
`limits.hard-cap-bytes`. A command line above it is not sent to the daemon and gets no
highlighting. Raising one of the two limits has no effect without raising the other.

The daemon returns at most `limits.max-spans` ranges per request, the first ones in order.

Filesystem checks are cached for `limits.path-cache-ttl-ms` and limited to
`limits.max-path-checks` uncached checks per request. An argument whose check would exceed the
limit gets no path highlighting, and a command word containing `/` gets no highlighting rather than
a possibly wrong error.

Every entry of a `$PATH` directory that is not a directory counts as a command, as in zsh with
`HASH_EXECUTABLES_ONLY` unset; permissions are not checked. The daemon caches the names in each
directory together with the directory's modification time. When `$PATH` changes, it reads the
directories it has not read before and those whose modification time changed. Between requests,
at most once per second, it rereads each listed directory whose modification time has changed. A
change the modification time does not show is picked up after `rehash` or `hash -r` in the shell:
the plugin then asks the daemon to reread every directory.

### Timing measurement

The daemon log records per-request processing time when timing is on. It is enabled by
`FASTHL_SERVE_ARGS=(--timing)` before the source line, or by `log.timing = true` in `config.toml`
followed by `fasthl-reload`. Each request adds one line:

```text
1759590000.123 id=42 type=H bytes=37 spans=9 us=85
```

The fields are a Unix timestamp, the request ID, the request type (`H` highlight, `S` state, `P`
ping), the bytes of text, the number of ranges returned, and the processing time in microseconds.
The time covers handling of the decoded request and excludes reading and writing the pipes; the
round trip is not logged.

`fast-highlight highlight` measures processing time outside the shell:

```sh
fast-highlight highlight --timing --repeat 1000 -- 'git commit -m "message" src/main.rs'
```

It prints the ranges to standard output and a line such as
`runs=1000 min=1.2us median=1.3us max=67.2us` to standard error. All runs share one process, so
after the first run the filesystem checks come from the cache while it is valid.

## Man pages

The repository provides `man/man1/fast-highlight.1` for the binary and `man/man5/fast-highlight.5`
for the configuration, theme, and spec formats. The `man` directory has the layout `man` expects,
so `MANPATH` can point at the clone directly:

```zsh
export MANPATH="/path/to/fast-highlight/man:$(manpath)"
```

A copy into a per-user manual directory works without a clone on `MANPATH`:

1. Copy the pages:

   ```sh
   mkdir -p ~/.local/share/man/man1 ~/.local/share/man/man5
   cp man/man1/fast-highlight.1 ~/.local/share/man/man1/
   cp man/man5/fast-highlight.5 ~/.local/share/man/man5/
   ```

2. Run `man fast-highlight`. If the page is not found, add the directory to `MANPATH` in
   `~/.zshrc`:

   ```zsh
   export MANPATH="$HOME/.local/share/man:$(manpath)"
   ```

`man 5 fast-highlight` shows the format page.

## Troubleshooting

### No highlighting

1. Check that the plugin loaded. `(( ${+functions[fasthl-reload]} )) && print loaded` prints nothing
   when the plugin returned early: zsh older than 5.8, a missing zsh module, a non-interactive
   shell, or standard input that is not a terminal.
2. Check that the binary is found, with `whence -p fast-highlight` or the value of `FASTHL_BIN`.
   Then run `fasthl-enable`, which reports a missing binary.
3. Run `fast-highlight check-config` and fix any reported error.
4. Read the log file, by default
   `${XDG_STATE_HOME:-~/.local/state}/fast-highlight/fast-highlight.log`. It records configuration
   errors, spec warnings, panics, and the reason for any abnormal daemon exit. The file is created
   on the first write.
5. Run `fast-highlight highlight --format ansi -- 'ls ~'` to see the daemon's output outside zsh.
6. If highlighting appears and disappears, the daemon is missing its time budget. Turn on timing
   (see [Timing measurement](#timing-measurement)), or raise the budget, for example with
   `FASTHL_TIMEOUT=0.2`.

### Unexpected colours

- A theme error makes the plugin use the built-in `default` theme. A `config.toml` error makes it
  ignore the `theme` key and use `theme.toml` or the `default` theme. `fast-highlight check-config`
  shows the error.
- An entry assigned to `FASTHL_STYLES` in `.zshrc` overrides the theme.
- zsh-syntax-highlighting or fast-syntax-highlighting is still loaded; see
  [Removal of other highlighters](#removal-of-other-highlighters).

### Conflicts with other highlighters

Any plugin that rewrites `region_highlight` on every redraw competes with fast-highlight for the
same text. zsh-syntax-highlighting and fast-syntax-highlighting are the common cases; the plugin
does not detect them. Symptoms are flickering styles, styles from the old highlighter's theme, and
ranges that do not match the text. The remedy is to stop loading the other highlighter.

### zsh-autosuggestions compatibility

fast-highlight imposes no ordering on zsh-autosuggestions, because it attaches through ZLE hooks
rather than by wrapping widgets. It sends only `BUFFER` to the daemon, so the suggested text, which
lives in `POSTDISPLAY`, is never highlighted by fast-highlight. It removes only its own
`region_highlight` entries, so the entry zsh-autosuggestions adds to style the suggestion is kept.
The plugin's test suite covers coexistence with other plugins' `region_highlight` entries but does
not run zsh-autosuggestions itself.

### Slow filesystems

Path checks are `stat` calls and directory reads made by the daemon while it handles a request. On
a network mount that stalls, the daemon stalls with it, the plugin abandons the redraw after
`FASTHL_TIMEOUT`, and highlighting stops until the backoff expires. Three settings reduce the
exposure:

- A lower `limits.max-path-checks`; `0` disables path checks entirely.
- A higher `limits.path-cache-ttl-ms`, so repeated checks of the same path come from the cache.
- A `$PATH` without directories on slow mounts, since the daemon scans and watches every `$PATH`
  directory.

## Development

The checks that must pass:

```sh
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

`cargo test` includes snapshot tests written with `insta`, stored in `tests/snapshots/`. After an
intended change in output, `cargo insta review` (from the `cargo-insta` tool) updates them.

`cargo test` also runs the CLI tests (`tests/cli.rs`), a replay of the committed fuzz seeds
(`tests/fuzz_replay.rs`), and the zsh plugin suite described below (`tests/zsh_plugin.rs`), which
passes without running the suite when `zsh` or `python3` is not installed. The latency tests run
only in an optimised build, with `cargo test --release`.

### zsh plugin tests

`tests/zsh/run.zsh` drives real interactive zsh sessions through `zsh/zpty`. It checks that
`region_highlight` is populated, that daemon crashes and hangs fall back to no highlighting and
recover, that typing never blocks, and that the daemon is cleaned up when the shell exits. It
requires zsh with `zsh/zpty`, `python3` for the mock daemon `tests/fixtures/mock-daemon.py`, and a
UTF-8 locale.

```sh
zsh tests/zsh/run.zsh                  # every test, against the mock daemon
zsh tests/zsh/run.zsh basic hang       # selected tests by name
FASTHL_TEST_BIN=target/release/fast-highlight zsh tests/zsh/run.zsh
```

| Variable           | Meaning                                                                          |
|--------------------|----------------------------------------------------------------------------------|
| `FASTHL_TEST_BIN`  | Binary under test for tests that do not depend on the mock. Default: the mock. Failure-handling tests always use the mock. |
| `FASTHL_TEST_ZSH`  | zsh to run under the pseudo-terminal. Default: `zsh` from `$PATH`.               |
| `FASTHL_TEST_LANG` | UTF-8 locale for the shells. Default: `en_US.UTF-8`.                             |
| `FASTHL_TEST_KEEP` | When set, keeps every test directory. Failed tests always keep theirs.           |

### Fuzzing

The `fuzz/` directory holds `cargo-fuzz` targets for the lexer (`lexer`), the full highlight pass
(`highlight`), and the protocol decoder (`protocol`). Fuzzing requires a nightly toolchain and
`cargo install cargo-fuzz`, and runs from the repository root:

```sh
mkdir -p fuzz/corpus && cp -r fuzz/seeds/* fuzz/corpus/
cargo +nightly fuzz list
cargo +nightly fuzz run lexer fuzz/corpus/lexer fuzz/seeds/lexer -- -max_len=4096
cargo +nightly fuzz run highlight fuzz/corpus/highlight fuzz/seeds/highlight -- -max_len=4096
cargo +nightly fuzz run protocol fuzz/corpus/protocol fuzz/seeds/protocol -- -max_len=4096
```

libFuzzer writes new inputs to the first directory, which must exist. `-max_total_time=60` limits a
run to 60 seconds. `cargo +nightly fuzz run TARGET FILE` replays a crashing input. The committed
seeds in `fuzz/seeds/` are regenerated from the snapshot tests and the protocol examples by
`python3 fuzz/make_seeds.py`; `fuzz/corpus/` and `fuzz/artifacts/` are not committed.

## Licence

BSD-3-Clause. The full text is in [`LICENSE`](LICENSE).
