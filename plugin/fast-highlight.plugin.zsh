# fast-highlight: syntax highlighting for the zsh line editor, served by a persistent daemon.
#
# Setup: source this file from ~/.zshrc (its position relative to other plugins does not
# matter):
#
#   source /path/to/fast-highlight/plugin/fast-highlight.plugin.zsh
#
# Requires zsh 5.8 or later. In non-interactive shells, shells that do not read commands from
# a terminal, and zsh older than 5.8, sourcing this file does nothing. Do not load
# zsh-syntax-highlighting or fast-syntax-highlighting in the same shell; both rewrite
# region_highlight on every redraw.
#
# User-facing variables (all optional):
#
#   FASTHL_BIN          Path (or command name) of the fast-highlight binary. When unset, the
#                       plugin uses `fast-highlight` from $PATH, else
#                       <plugin dir>/../target/release/fast-highlight. When no binary is found,
#                       the plugin stays inactive and prints nothing.
#   FASTHL_TIMEOUT      Seconds the plugin waits for the daemon per redraw before giving up on
#                       highlighting for that redraw and restarting the daemon. Default 0.05.
#   FASTHL_STYLES       Associative array mapping token kinds (see docs/protocol.md) to
#                       region_highlight styles such as `fg=green,bold`. Filled at load time
#                       from `fast-highlight styles` (the active theme), or from a built-in
#                       table when that fails. Entries assigned after sourcing override the
#                       theme and survive fasthl-reload. An empty style leaves a kind unstyled.
#   FASTHL_SERVE_ARGS   Array of extra arguments for `fast-highlight serve`, for example
#                       (--timing). Read when the daemon starts.
#
# User-facing functions:
#
#   fasthl-reload       Re-read the theme into FASTHL_STYLES (keeping user overrides) and
#                       restart the daemon, which then receives the full shell state again.
#   fasthl-disable      Stop the daemon, remove the highlighting, and unhook from ZLE.
#   fasthl-enable       Undo fasthl-disable (also retries finding the binary).
#
# Internal names start with `_fasthl_`.

# The ZLE option is on in every interactive shell, even one that never edits a line (such as
# `zsh -i -c cmd`, which editors run to read the environment), so also require a terminal on
# standard input and no -c string or script.
[[ -o interactive && -o zle && -t 0 ]] || return 0
(( ${+ZSH_EXECUTION_STRING} || ${+ZSH_SCRIPT} )) && return 0
(( ${+_fasthl_plugin_dir} )) && return 0
typeset -g _fasthl_plugin_dir=${${(%):-%x}:A:h}

# Parse and run the implementation under zsh defaults with aliases off, so the user's aliases
# and options cannot change how the function bodies are parsed.
() {
  builtin emulate -L zsh -o no_aliases
  builtin source "$_fasthl_plugin_dir/fasthl-core.zsh"
}
