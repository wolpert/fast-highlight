# Implementation of the fast-highlight zsh plugin. Sourced by fast-highlight.plugin.zsh inside a
# function running `emulate -L zsh -o no_aliases`; do not source it directly. Every global is
# declared with `typeset -g` for that reason.
#
# Daemon lifecycle (see docs/protocol.md for the wire format):
#
#   dead      no daemon. A start is attempted on a hook once $_fasthl_retry_at has passed.
#   starting  the daemon was spawned on two FIFOs and sent a ping; the plugin has not read the
#             ack yet. The ack is read by the next precmd (bounded wait), by a redraw (poll
#             only), or by a `zle -F` handler as soon as it arrives while ZLE is idle.
#   ready     the ack was read, the FIFOs were unlinked, and requests flow.
#
# Every hook moves the lifecycle along, whether or not it has a buffer to highlight, so a
# daemon that died at an empty prompt is restarted without waiting for a keystroke.
#
# Any timeout, EOF, framing error, or write error kills the daemon and moves to `dead` with an
# exponential backoff (0.5 s doubling to 60 s, reset by a successful highlight). After EOF the
# daemon has exited and is not signalled, since its pid may already belong to another process. State (S)
# requests are not waited for: the daemon answers in order, so the next highlight read skips
# the ack, and a daemon still busy with the state shows up as a late answer to that highlight.

autoload -Uz is-at-least add-zle-hook-widget add-zsh-hook
is-at-least 5.8 || return 0
zmodload zsh/system zsh/datetime zsh/parameter 2>/dev/null || return 0
zmodload zsh/zselect 2>/dev/null
zmodload -F zsh/files b:zf_mkdir b:zf_rm b:zf_rmdir 2>/dev/null

typeset -gA FASTHL_STYLES
typeset -ga FASTHL_SERVE_ARGS

typeset -g _fasthl_bin=              # resolved binary, absolute path
typeset -gi _fasthl_enabled=0
typeset -g _fasthl_state=dead        # dead | starting | ready
typeset -gi _fasthl_pid=0            # daemon pid
typeset -gi _fasthl_wfd=-1           # requests to the daemon
typeset -gi _fasthl_rfd=-1           # responses from the daemon
typeset -gi _fasthl_watch_fd=-1      # fd with a zle -F handler installed
typeset -gi _fasthl_id=0             # last request id
typeset -gi _fasthl_ping_id=0        # id of the startup ping
typeset -g _fasthl_dir=              # FIFO directory while starting
typeset -gF _fasthl_retry_at=0       # earliest time of the next start attempt
typeset -gF _fasthl_backoff=0        # current backoff in seconds (0: none)
typeset -gF _fasthl_start_deadline=0 # a handshake not done by then is a failure
typeset -g _fasthl_rbuf=             # bytes read from the daemon but not consumed
typeset -g _fasthl_rtype= _fasthl_rbody=  # last frame read
typeset -gi _fasthl_synced=0         # full shell state sent to the current daemon
typeset -gA _fasthl_sent             # S field -> value the daemon has
typeset -g _fasthl_sent_cwd=         # cwd the daemon has
typeset -g _fasthl_key=              # editor state the cached entries belong to
typeset -ga _fasthl_entries          # region_highlight entries for $_fasthl_key
typeset -ga _fasthl_applied          # our entries as stored in region_highlight (zsh 5.8)
typeset -gi _fasthl_applied_at=0     # index of the first of them
typeset -gA _fasthl_theme            # FASTHL_STYLES as last loaded, before user overrides
typeset -ga _fasthl_tmp
typeset -gA _fasthl_smap             # kind -> region_highlight style with memo (non-empty only)
typeset -g _fasthl_smap_fp=          # FASTHL_STYLES and memo setting _fasthl_smap was built from
typeset -gi _fasthl_rehash=0         # the next S request asks the daemon to rescan $PATH
typeset -gi _fasthl_shell_pid=$sysparams[pid]
typeset -gi _fasthl_memo=0
is-at-least 5.9 && _fasthl_memo=1

# Seconds the plugin waits per redraw.
_fasthl_timeout() {
  emulate -L zsh
  setopt extendedglob
  if [[ ${FASTHL_TIMEOUT-} == ([0-9]##(.[0-9]#|)|.[0-9]##) ]]; then
    REPLY=$FASTHL_TIMEOUT
  else
    REPLY=0.05
  fi
}

# Most bytes of PREBUFFER plus BUFFER the plugin sends to the daemon.
_fasthl_max_length() {
  emulate -L zsh
  if [[ ${FASTHL_MAX_LENGTH-} == <-> ]] && (( ${#FASTHL_MAX_LENGTH} <= 15 )); then
    REPLY=$FASTHL_MAX_LENGTH
  else
    REPLY=65536
  fi
}

# Locate the binary into $_fasthl_bin. Returns 1 when there is none.
_fasthl_find_bin() {
  emulate -L zsh
  local b=
  if [[ -n ${FASTHL_BIN-} ]]; then
    if [[ $FASTHL_BIN == */* ]]; then
      b=$FASTHL_BIN
    else
      b=${commands[$FASTHL_BIN]-}
    fi
  elif (( ${+commands[fast-highlight]} )); then
    b=$commands[fast-highlight]
  else
    b=$_fasthl_plugin_dir/../target/release/fast-highlight
  fi
  if [[ -n $b && -f $b && -x $b ]]; then
    _fasthl_bin=${b:a}
    return 0
  fi
  _fasthl_bin=
  return 1
}

# Evaluate the output of `fast-highlight styles` with the style tables local, so the output may
# use `typeset -A`, `typeset -gA`, or plain assignment. Leaves the table in $_fasthl_tmp.
_fasthl_eval_styles() {
  emulate -L zsh
  setopt noaliases
  local -A FASTHL_STYLES FAST_HIGHLIGHT_STYLES
  eval "$1" 2>/dev/null
  if (( ${#FASTHL_STYLES} )); then
    _fasthl_tmp=("${(@kv)FASTHL_STYLES}")
  else
    # Older theme compilers used this name; accept it without touching the global of
    # fast-syntax-highlighting, which uses the same name.
    _fasthl_tmp=("${(@kv)FAST_HIGHLIGHT_STYLES}")
  fi
}

# Load the theme into FASTHL_STYLES. Entries the user set or changed since the last load win.
_fasthl_load_styles() {
  emulate -L zsh
  local out= k
  local -A theme
  # `styles` exits 1 on a broken config or theme but still prints the theme it fell back to
  # (the built-in default), so the output is used whatever the exit status.
  [[ -n $_fasthl_bin ]] && out=$("$_fasthl_bin" styles </dev/null 2>/dev/null)
  _fasthl_tmp=()
  [[ -n $out ]] && _fasthl_eval_styles "$out"
  theme=("${(@)_fasthl_tmp}")
  if (( ! ${#theme} )); then
    # Used only when `styles` printed no table at all. Keep in sync with the default theme
    # (themes/truecolor.toml over themes/default.toml).
    theme=(
      default              ''
      error                'fg=#f05f5f,bold'
      reserved-word        'fg=#e5a55f'
      alias                'fg=#8fd18f'
      suffix-alias         'fg=#d787d7'
      global-alias         'fg=#d787d7,bold'
      function             'fg=#5fd7af'
      builtin              'fg=#b5d75f'
      command              'fg=#8fd75f'
      precommand           'fg=#8fd75f,underline'
      separator            'fg=#a8a8a8,bold'
      redirection          'fg=#d7af87,bold'
      heredoc              'fg=#d7d787'
      single-quoted        'fg=#d7d787'
      double-quoted        'fg=#e5c07b'
      dollar-quoted        'fg=#d7af5f'
      backquoted           'fg=#c678dd'
      escape               'fg=#56b6c2'
      parameter            'fg=#61afef'
      substitution         'fg=#c678dd'
      process-substitution 'fg=#c678dd'
      arithmetic           'fg=#d19a66'
      glob                 'fg=#7f9fff'
      glob-qualifier       'fg=#7f9fff,bold'
      brace-expansion      'fg=#9fbfff'
      history-expansion    'fg=#7f9fff,bold'
      comment              'fg=#7a7a7a'
      assignment           'fg=#d7875f'
      grouping             'fg=#e5a55f'
      operator             'fg=#e5a55f'
      path                 'underline'
      path-directory       'fg=#87afd7,underline'
      path-prefix          'fg=#a8a8a8,underline'
      subcommand           'fg=#5fafd7'
      option               'fg=#87d7d7'
    )
  fi
  local -A merged
  merged=("${(@kv)theme}")
  for k in ${(k)FASTHL_STYLES}; do
    if (( ! ${+_fasthl_theme[$k]} )) || [[ ${FASTHL_STYLES[$k]} != "${_fasthl_theme[$k]}" ]]; then
      merged[$k]=${FASTHL_STYLES[$k]}
    fi
  done
  _fasthl_theme=("${(@kv)theme}")
  FASTHL_STYLES=("${(@kv)merged}")
}

# Install a zle -F handler that finishes the handshake as soon as the ack arrives.
_fasthl_watch() {
  emulate -L zsh
  (( _fasthl_rfd >= 0 && _fasthl_watch_fd < 0 )) || return 0
  zle -F -w $_fasthl_rfd _fasthl_ready 2>/dev/null && _fasthl_watch_fd=$_fasthl_rfd
  return 0
}

_fasthl_unwatch() {
  emulate -L zsh
  (( _fasthl_watch_fd >= 0 )) || return 0
  zle -F $_fasthl_watch_fd 2>/dev/null
  _fasthl_watch_fd=-1
  return 0
}

_fasthl_close_fd() {
  emulate -L zsh
  local -i fd=$1
  (( fd >= 0 )) || return 0
  { exec {fd}>&- } 2>/dev/null
  return 0
}

_fasthl_rm_dir() {
  emulate -L zsh
  [[ -n $_fasthl_dir ]] || return 0
  if (( ${+builtins[zf_rm]} && ${+builtins[zf_rmdir]} )); then
    zf_rm -f -- $_fasthl_dir/in $_fasthl_dir/out 2>/dev/null
    zf_rmdir -- $_fasthl_dir 2>/dev/null
  else
    command rm -rf -- $_fasthl_dir 2>/dev/null
  fi
  _fasthl_dir=
  return 0
}

# Send signal $1 to the daemon if it still runs. zsh reaps every child, disowned ones too, so
# the pid of a daemon that exited may already name another process. Where /proc shows it,
# require that the process is still a child of this shell.
_fasthl_signal() {
  emulate -L zsh
  (( _fasthl_pid > 0 )) && kill -0 $_fasthl_pid 2>/dev/null || return 0
  local stat
  if [[ -r /proc/$_fasthl_pid/stat ]]; then
    stat=$(</proc/$_fasthl_pid/stat)
    # pid (comm) state ppid ...; comm may contain spaces and parentheses.
    stat=${stat##*\) }
    [[ ${${(s: :)stat}[2]} == $sysparams[pid] ]] || return 0
  fi
  kill -$1 $_fasthl_pid 2>/dev/null
  return 0
}

# Tear down the daemon connection. $1 is the signal for the daemon process; none when empty
# (the daemon is known to have exited).
_fasthl_kill() {
  emulate -L zsh
  _fasthl_unwatch
  [[ -n $1 ]] && _fasthl_signal $1
  _fasthl_close_fd $_fasthl_wfd
  _fasthl_close_fd $_fasthl_rfd
  _fasthl_rm_dir
  _fasthl_wfd=-1 _fasthl_rfd=-1 _fasthl_pid=0
  _fasthl_state=dead
  _fasthl_rbuf=
  _fasthl_synced=0
  _fasthl_rehash=0                   # a new daemon scans $PATH when it starts
  _fasthl_sent=()
  _fasthl_sent_cwd=
  _fasthl_key=
  _fasthl_entries=()
  return 0
}

# Graceful stop: ask the daemon to quit, then close and signal it.
_fasthl_stop() {
  emulate -L zsh
  if [[ $_fasthl_state == ready ]] && (( _fasthl_wfd >= 0 )); then
    syswrite -o $_fasthl_wfd "FH1 Q $(( ++_fasthl_id )) 0"$'\n' 2>/dev/null
  fi
  _fasthl_kill TERM
}

# The daemon failed: kill it and schedule a restart with backoff. With $1 `eof` the daemon
# closed its output, which it does only when exiting, so it is not signalled.
_fasthl_fail() {
  emulate -L zsh
  if [[ ${1-} == eof ]]; then
    _fasthl_kill ''
  else
    _fasthl_kill KILL
  fi
  if (( _fasthl_backoff <= 0 )); then
    _fasthl_backoff=0.5
  elif (( _fasthl_backoff < 60 )); then
    (( _fasthl_backoff = _fasthl_backoff * 2 > 60 ? 60 : _fasthl_backoff * 2 ))
  fi
  (( _fasthl_retry_at = EPOCHREALTIME + _fasthl_backoff ))
  return 0
}

# Spawn the daemon on two FIFOs and send the handshake ping without waiting for the answer.
_fasthl_start() {
  emulate -L zsh
  [[ $_fasthl_state == dead ]] || return 0
  [[ -n $_fasthl_bin && -x $_fasthl_bin ]] || return 1
  local base=${TMPDIR:-/tmp} dir= try
  base=${base%/}
  for try in 1 2 3; do
    dir=$base/fasthl.$sysparams[pid].$RANDOM$RANDOM
    if (( ${+builtins[zf_mkdir]} )); then
      zf_mkdir -m 700 -- $dir 2>/dev/null && break
    else
      command mkdir -m 700 -- $dir 2>/dev/null && break
    fi
    dir=
  done
  [[ -n $dir ]] || return 1
  _fasthl_dir=$dir
  command mkfifo -m 600 -- $dir/in $dir/out 2>/dev/null || return 1
  # `&!` disowns at once: no job table entry, so no job-control messages. Redirections are
  # opened by the child; the opens of the FIFOs complete when the parent opens its ends.
  # --parent makes the daemon exit when this shell is gone, even if something else still
  # holds the FIFOs open. The arguments of a background command are expanded after the fork,
  # so the pid is taken here, in the shell itself.
  local -i shell=$sysparams[pid]
  "$_fasthl_bin" serve --parent $shell "${(@)FASTHL_SERVE_ARGS}" \
    2>/dev/null <$dir/in >$dir/out &!
  _fasthl_pid=$!
  # Read-write opens of a FIFO never block. Close-on-exec keeps the fds out of commands the
  # user runs, so the daemon sees EOF when the shell goes away. Writes do not block either
  # (see _fasthl_write).
  sysopen -rw -o cloexec,nonblock -u _fasthl_wfd $dir/in 2>/dev/null || return 1
  sysopen -rw -o cloexec -u _fasthl_rfd $dir/out 2>/dev/null || return 1
  _fasthl_ping_id=$(( ++_fasthl_id ))
  syswrite -o $_fasthl_wfd "FH1 P $_fasthl_ping_id 0"$'\n' 2>/dev/null || return 1
  _fasthl_state=starting
  _fasthl_rbuf=
  (( _fasthl_start_deadline = EPOCHREALTIME + 2 ))
  _fasthl_watch
  return 0
}

# Read one frame answering request $1, discarding older frames, until time $2. Past the
# deadline the fd is polled once, so a caller with no time to wait still sees data that is
# already there. Sets _fasthl_rtype and _fasthl_rbody. Returns 0, 1 timeout, 2 EOF or read
# error, 3 framing.
_fasthl_read_frame() {
  emulate -L zsh
  setopt nomultibyte
  local -i expect=$1 hl len need have polled st
  local -F deadline=$2
  local -F 6 rem
  local hdr chunk
  local -a parts chunks
  while true; do
    need=0
    if [[ $_fasthl_rbuf == *$'\n'* ]]; then
      hdr=${_fasthl_rbuf%%$'\n'*}
      hl=${#hdr}
      (( hl < 64 )) || return 3
      parts=(${(s: :)hdr})
      (( ${#parts} == 4 )) || return 3
      [[ $parts[1] == FH1 && $parts[2] == [A-Za-z] && $parts[3] == <-> && $parts[4] == <-> ]] ||
        return 3
      (( ${#parts[3]} <= 18 && ${#parts[4]} <= 10 )) || return 3
      len=$parts[4]
      (( len <= 16777216 )) || return 3
      need=$(( hl + 1 + len ))
      have=${#_fasthl_rbuf}
      if (( have >= need )); then
        _fasthl_rtype=$parts[2]
        _fasthl_rbody=${_fasthl_rbuf[hl+2,need]}
        _fasthl_rbuf=${_fasthl_rbuf[need+1,-1]}
        (( parts[3] < expect )) && continue
        (( parts[3] == expect )) || return 3
        return 0
      fi
    elif (( ${#_fasthl_rbuf} >= 64 )); then
      return 3
    fi
    # Read until the frame is complete (or, without a header yet, once). The pieces of a large
    # body are joined once at the end: appending each to the buffer would copy it every time.
    chunks=()
    while true; do
      (( rem = deadline - EPOCHREALTIME ))
      if (( rem <= 0 )); then
        (( polled++ )) && return 1
        rem=0
      fi
      chunk=
      sysread -t $rem -s 65536 -i $_fasthl_rfd chunk 2>/dev/null
      st=$?
      case $st in
        (0) chunks+=("$chunk") ;;
        (4) return 1 ;;
        (*) return 2 ;;
      esac
      (( need )) || break
      (( have += ${#chunk} ))
      (( have >= need )) && break
    done
    _fasthl_rbuf+=${(j::)chunks}
  done
}

# Write $1 to the daemon by time $2. The fd is non-blocking: syswrite stops at a full pipe and
# reports how much went out, and the rest waits for zselect to report the pipe writable, so a
# daemon that stopped reading cannot block the shell. Returns 1 when the deadline passed first.
#
# Each syswrite gets at most 64 KiB, taken by byte offset. Handing it the whole remainder
# instead would copy the remainder for every call, and slicing by index rescans the string
# more slowly; the offset form still scans up to the offset, but a 1 MiB frame goes out in
# tens of milliseconds rather than half a second.
_fasthl_write() {
  emulate -L zsh
  setopt nomultibyte
  local -F deadline=$2
  local -i total=${#1} off=0 n cs
  while true; do
    n=0
    syswrite -c n -o $_fasthl_wfd -- "${1:$off:65536}" 2>/dev/null
    (( off += n ))
    (( off < total )) || return 0
    (( deadline > EPOCHREALTIME )) || return 1
    (( n == 65536 )) && continue
    (( cs = (deadline - EPOCHREALTIME) * 100 ))
    if (( ${+builtins[zselect]} )); then
      zselect -t $cs -w $_fasthl_wfd 2>/dev/null || return 1
    fi
  done
}

# Send a frame: type $1, id $2, body $3, by time $4.
_fasthl_send() {
  emulate -L zsh
  setopt nomultibyte
  _fasthl_write "FH1 $1 $2 ${#3}"$'\n'"$3" $4
}

# Read the handshake ack, waiting until time $1. On success the daemon is ready.
_fasthl_handshake() {
  emulate -L zsh
  [[ $_fasthl_state == starting ]] || return 0
  local -i st fd
  _fasthl_read_frame $_fasthl_ping_id $1
  st=$?
  if (( st == 1 )); then
    if ! kill -0 $_fasthl_pid 2>/dev/null; then
      _fasthl_fail eof
    elif (( EPOCHREALTIME > _fasthl_start_deadline )); then
      _fasthl_fail
    fi
    return 1
  fi
  if (( st == 2 )); then
    _fasthl_fail eof
    return 1
  fi
  if (( st != 0 )) || [[ $_fasthl_rtype != A ]]; then
    _fasthl_fail
    return 1
  fi
  _fasthl_unwatch
  # The daemon has its end of `out` open now, so a read-only open does not block. Reading
  # through it turns a dead daemon into EOF instead of a timeout.
  if sysopen -r -o cloexec,nonblock -u fd $_fasthl_dir/out 2>/dev/null; then
    _fasthl_close_fd $_fasthl_rfd
    _fasthl_rfd=$fd
  fi
  _fasthl_rm_dir
  _fasthl_state=ready
  return 0
}

# Make sure a daemon is ready, starting one if the backoff allows. Waits for the handshake
# until time $1. Returns 1 when no daemon is ready.
_fasthl_ensure() {
  emulate -L zsh
  (( _fasthl_enabled )) || return 1
  case $_fasthl_state in
    (ready) return 0 ;;
    (dead)
      (( EPOCHREALTIME >= _fasthl_retry_at )) || return 1
      _fasthl_start || { _fasthl_fail; return 1 }
      ;;
  esac
  _fasthl_handshake $1
}

# Encode the arguments as a list value (docs/protocol.md): each entry followed by a NUL, so an
# empty entry (the directory of `hash -d name=`) survives at any position. Sets $REPLY.
_fasthl_list() {
  if (( $# )); then
    REPLY="${(pj:\0:)@}"$'\0'
  else
    REPLY=
  fi
}

# Send the parts of the shell's command namespace that changed, by time $1, and the `rehash`
# field after the user ran `rehash` or `hash -r` (see _fasthl_preexec). The ack is not read
# here: waiting for it would put the daemon's work on the state (a $PATH rescan, say) on a
# deadline meant for a round trip, and there is nothing to do with the answer anyway (an E
# means the daemon rejected the frame, and resending it would not help). _fasthl_read_frame
# discards the ack when it reads the answer to a later request.
_fasthl_sync_state() {
  emulate -L zsh
  setopt nomultibyte
  local -A cur
  local body= k REPLY
  local -i id
  _fasthl_list "${(@k)aliases}";    cur[alias]=$REPLY
  _fasthl_list "${(@k)galiases}";   cur[galias]=$REPLY
  _fasthl_list "${(@k)saliases}";   cur[salias]=$REPLY
  _fasthl_list "${(@k)functions}";  cur[func]=$REPLY
  _fasthl_list "${(@k)builtins}";   cur[builtin]=$REPLY
  _fasthl_list "${(@)reswords}";    cur[reswords]=$REPLY
  _fasthl_list "${(@kv)nameddirs}"; cur[nameddirs]=$REPLY
  cur[path]=$PATH
  for k in alias galias salias func builtin reswords nameddirs path; do
    if (( ! ${+_fasthl_sent[$k]} )) || [[ $cur[$k] != "$_fasthl_sent[$k]" ]]; then
      body+="$k ${#cur[$k]}"$'\n'"$cur[$k]"$'\n'
    fi
  done
  (( _fasthl_rehash )) && body+="rehash 0"$'\n\n'
  if [[ -n $body ]]; then
    id=$(( ++_fasthl_id ))
    _fasthl_send S $id "$body" $1 || { _fasthl_fail; return 1 }
    _fasthl_sent=("${(@kv)cur}")
    _fasthl_rehash=0
  fi
  _fasthl_synced=1
  return 0
}

# Replace our region_highlight entries with the arguments.
_fasthl_apply() {
  emulate -L zsh
  if (( _fasthl_memo )); then
    region_highlight=("${(@)region_highlight:#*memo=fast-highlight}" "$@")
  else
    # ZLE shifts the offsets of region_highlight entries when text is inserted or deleted, and
    # other plugins remove and re-add their own entries, so neither the exact strings nor the
    # position of ours survive. Our entries were appended as one block: find a block with the
    # same styles in the same order, trying the position it was appended at first, then from
    # the end. When there is none, remove exact matches.
    local -i n=${#_fasthl_applied} len=${#region_highlight} at=0 i k ok
    if (( n )); then
      local -a styles=("${(@)_fasthl_applied#* * }") starts=($_fasthl_applied_at)
      for (( i = len - n + 1; i >= 1; i-- )); do
        starts+=($i)
      done
      for i in $starts; do
        (( i >= 1 && i + n - 1 <= len )) || continue
        ok=1
        for (( k = 0; k < n; k++ )); do
          if [[ ${region_highlight[i+k]#* * } != "$styles[k+1]" ]]; then
            ok=0
            break
          fi
        done
        if (( ok )); then
          at=$i
          break
        fi
      done
      if (( at )); then
        region_highlight[at,at+n-1]=()
      else
        region_highlight=("${(@)region_highlight:|_fasthl_applied}")
      fi
    fi
    if (( $# )); then
      _fasthl_applied_at=$(( ${#region_highlight} + 1 ))
      region_highlight+=("$@")
      # Remember the entries as zsh stores them, which may differ in spelling from ours.
      _fasthl_applied=("${(@)region_highlight[-$#,-1]}")
    else
      _fasthl_applied=()
    fi
  fi
}

# Turn the R body in $_fasthl_rbody into region_highlight entries in $_fasthl_entries, by time
# $1. $2 is the length of BUFFER in the request's offset unit. Lines that are not
# `start end kind` with start <= end <= $2, and kinds without a style, are dropped. Returns 1,
# with no entries, when the deadline passed first.
#
# A response can hold a thousand spans, so the work is done with whole-array expansions (a
# loop with a few statements per span costs several times as much) and in slices, with the
# deadline checked between them.
_fasthl_parse_spans() {
  emulate -L zsh
  setopt extendedglob
  local -F deadline=$1
  local -i max=$2 i
  local k memo= fp pat
  local -a lines part
  (( _fasthl_memo )) && memo=' memo=fast-highlight'
  # kind -> "style memo" for the kinds with a style, rebuilt when FASTHL_STYLES changes.
  fp="$memo ${(@kv)FASTHL_STYLES}"
  if [[ $fp != "$_fasthl_smap_fp" ]]; then
    _fasthl_smap=()
    for k in ${(k)FASTHL_STYLES}; do
      [[ -n $FASTHL_STYLES[$k] ]] && _fasthl_smap[$k]=$FASTHL_STYLES[$k]$memo
    done
    _fasthl_smap_fp=$fp
  fi
  _fasthl_entries=()
  lines=(${(f)_fasthl_rbody})
  pat="<0-$max> <0-$max> [[:alpha:]-]##"
  for (( i = 1; i <= ${#lines}; i += 256 )); do
    if (( i > 1 && EPOCHREALTIME >= deadline )); then
      _fasthl_entries=()
      return 1
    fi
    part=(${(M)lines[i,i+255]:#${~pat}})
    # A start past the end becomes -1 and an unstyled kind leaves a trailing space; both are
    # then removed.
    part=("${(@)part/(#b)(<->) (<->) (*)/$(( match[1] > match[2] ? -1 : match[1] )) $match[2] ${_fasthl_smap[$match[3]]-}}")
    _fasthl_entries+=(${part:#(-*|* )})
  done
  return 0
}

# Highlight the current editor state. $1 holds the user's option letters (see
# _fasthl_capture_opts); $2 is the most a pending handshake may wait, in seconds; $3, when
# given, is the time by which everything must be done (default: now + FASTHL_TIMEOUT).
_fasthl_highlight() {
  emulate -L zsh
  setopt extendedglob
  local opt=$1 key REPLY
  local -F deadline=${3:-0}
  local -i bytes len
  (( _fasthl_enabled )) || return 0
  () { setopt localoptions nomultibyte; (( bytes = ${#PREBUFFER} + ${#BUFFER} )) }
  _fasthl_max_length
  if [[ -z $BUFFER || $CONTEXT == (select|vared) ]] || (( bytes > REPLY )); then
    _fasthl_apply
    # Nothing to highlight (or too much: the daemon would answer with nothing, after the cost
    # of sending it all), but a dead daemon is restarted (or a pending handshake polled) so it
    # is ready by the time something is typed.
    if [[ $_fasthl_state != ready ]] && _fasthl_ensure $(( EPOCHREALTIME + $2 )); then
      _fasthl_timeout
      (( ! _fasthl_synced )) && _fasthl_sync_state $(( EPOCHREALTIME + REPLY ))
    fi
    return 0
  fi
  # ZLE counts characters of the locale whatever the MULTIBYTE option says (zshoptions(1)), so
  # offsets are characters exactly when zsh decodes UTF-8: a two-byte character has length 1
  # with the option on. The daemon decodes UTF-8 only, so other locales are sent as bytes.
  setopt multibyte
  local u8=$'\xc3\xa9'
  if (( ${#u8} == 1 )) && [[ ${LC_ALL:-${LC_CTYPE:-${LANG-}}} == (#i)*utf(-|)8* ]]; then
    opt=u$opt
    len=${#BUFFER}
  else
    () { setopt localoptions nomultibyte; len=${#BUFFER} }
  fi
  key=$opt$'\0'$CURSOR$'\0'$PWD$'\0'$PREBUFFER$'\0'$BUFFER
  if [[ $_fasthl_state == ready && $key == "$_fasthl_key" ]]; then
    _fasthl_apply "${(@)_fasthl_entries}"
    return 0
  fi
  if (( deadline <= 0 )); then
    _fasthl_timeout
    (( deadline = EPOCHREALTIME + REPLY ))
  fi
  if [[ $_fasthl_state != ready ]]; then
    if ! _fasthl_ensure $(( EPOCHREALTIME + $2 < deadline ? EPOCHREALTIME + $2 : deadline )); then
      _fasthl_apply
      return 0
    fi
  fi
  if (( ! _fasthl_synced )) && ! _fasthl_sync_state $deadline; then
    _fasthl_apply
    return 0
  fi
  _fasthl_request "$opt" $deadline $len || { _fasthl_apply; return 0 }
  _fasthl_key=$key
  _fasthl_apply "${(@)_fasthl_entries}"
}

# Send an H request for the current editor state with option letters $1, by time $2, and read
# the answer into $_fasthl_entries. $3 is the length of BUFFER in the request's offset unit.
_fasthl_request() {
  emulate -L zsh
  setopt nomultibyte
  local opt=$1 body
  local -F deadline=$2
  local -i id st
  body="buf ${#BUFFER}"$'\n'"$BUFFER"$'\n'
  [[ -n $PREBUFFER ]] && body+="pre ${#PREBUFFER}"$'\n'"$PREBUFFER"$'\n'
  body+="cur ${#CURSOR}"$'\n'"$CURSOR"$'\n'
  [[ $PWD != "$_fasthl_sent_cwd" ]] && body+="cwd ${#PWD}"$'\n'"$PWD"$'\n'
  body+="opt ${#opt}"$'\n'"$opt"$'\n'
  id=$(( ++_fasthl_id ))
  _fasthl_send H $id "$body" $deadline || { _fasthl_fail; return 1 }
  _fasthl_read_frame $id $deadline
  st=$?
  if (( st == 2 )); then
    _fasthl_fail eof
    return 1
  elif (( st )); then
    _fasthl_fail
    return 1
  fi
  _fasthl_sent_cwd=$PWD
  case $_fasthl_rtype in
    (R)
      # Running out of time here is no fault of the daemon's: this state goes unhighlighted.
      _fasthl_parse_spans $deadline $3
      _fasthl_backoff=0
      ;;
    (E) _fasthl_entries=() ;;
    (*) _fasthl_fail; return 1 ;;
  esac
  return 0
}

# Collect the option letters of the H request. Runs under the caller's options, so it must be
# called before `emulate`. Sets $_fasthl_uo in the caller's scope.
_fasthl_capture_opts() {
  _fasthl_uo=
  [[ -o interactivecomments ]] && _fasthl_uo+=c
  [[ -o autocd ]] && _fasthl_uo+=a
  [[ -o extendedglob ]] && _fasthl_uo+=e
  [[ -o kshglob ]] && _fasthl_uo+=k
  [[ -o ignorebraces ]] && _fasthl_uo+=b
  [[ -o ignoreclosebraces ]] && _fasthl_uo+=B
  [[ -o rcquotes ]] && _fasthl_uo+=r
  [[ -o ksharrays ]] && _fasthl_uo+=K
  [[ -o posixidentifiers ]] && _fasthl_uo+=p
  [[ -o shglob ]] && _fasthl_uo+=s
  [[ -o braceccl ]] && _fasthl_uo+=C
  [[ -o equals ]] || _fasthl_uo+=E
  [[ -o shortloops ]] || _fasthl_uo+=L
  return 0
}

# zle-line-pre-redraw and zle-line-finish hook widget.
_fasthl_redraw() {
  local _fasthl_uo
  _fasthl_capture_opts
  emulate -L zsh
  local REPLY MATCH MBEGIN MEND
  local -a reply match mbegin mend
  { _fasthl_highlight "$_fasthl_uo" 0 } 2>/dev/null
  return 0
}

# zle -F handler widget: the daemon's first answer arrived while ZLE was idle.
_fasthl_ready() {
  local _fasthl_uo
  _fasthl_capture_opts
  emulate -L zsh
  local REPLY MATCH MBEGIN MEND
  local -a reply match mbegin mend
  {
    _fasthl_unwatch
    if [[ $_fasthl_state == starting ]]; then
      local -F deadline
      _fasthl_timeout
      (( deadline = EPOCHREALTIME + REPLY ))
      if _fasthl_handshake $deadline; then
        _fasthl_sync_state $deadline && _fasthl_highlight "$_fasthl_uo" 0 $deadline
        zle -R
      elif [[ $_fasthl_state == starting ]]; then
        _fasthl_watch
      fi
    fi
  } 2>/dev/null
  return 0
}

# precmd hook: finish a pending handshake and send shell state changes.
_fasthl_precmd() {
  local -i _fasthl_ret=$?
  emulate -L zsh
  local REPLY MATCH MBEGIN MEND
  local -a reply match mbegin mend
  {
    if (( _fasthl_enabled )); then
      _fasthl_timeout
      # A handshake may use the whole wait; sending the state gets a budget of its own.
      if _fasthl_ensure $(( EPOCHREALTIME + REPLY )); then
        _fasthl_sync_state $(( EPOCHREALTIME + REPLY ))
      fi
    fi
  } 2>/dev/null
  return _fasthl_ret
}

# preexec hook: after `rehash` or `hash -r` the next state update asks the daemon to rescan
# $PATH. The daemon notices most changes by itself, from the modification times of the
# directories, but not changes on filesystems that leave a directory's mtime alone. $3 is the
# command line with aliases expanded. Any word `rehash` counts, as does `hash` followed by an
# option word containing r; a false match costs one rescan.
_fasthl_preexec() {
  emulate -L zsh
  [[ $3 == *hash* ]] || return 0
  local -a w
  local -i i
  w=(${(z)3})
  for (( i = 1; i <= ${#w}; i++ )); do
    if [[ $w[i] == rehash ]] || [[ $w[i] == hash && $w[i+1] == -*r* ]]; then
      _fasthl_rehash=1
      return 0
    fi
  done
  return 0
} 2>/dev/null

_fasthl_zshexit() {
  emulate -L zsh
  # A forked copy of the shell must not stop the parent's daemon.
  (( sysparams[pid] == _fasthl_shell_pid )) || return 0
  { _fasthl_stop } 2>/dev/null
  return 0
}

# Hook in and start the daemon. Silent; returns 1 when there is no binary.
_fasthl_enable() {
  emulate -L zsh
  _fasthl_find_bin || return 1
  (( ${#_fasthl_theme} )) || _fasthl_load_styles
  add-zle-hook-widget zle-line-pre-redraw _fasthl_redraw
  add-zle-hook-widget zle-line-finish _fasthl_redraw
  add-zsh-hook precmd _fasthl_precmd
  add-zsh-hook preexec _fasthl_preexec
  add-zsh-hook zshexit _fasthl_zshexit
  _fasthl_enabled=1
  _fasthl_backoff=0
  _fasthl_retry_at=0
  [[ $_fasthl_state == dead ]] && { _fasthl_start || _fasthl_fail }
  return 0
} 2>/dev/null

zle -N _fasthl_redraw
zle -N _fasthl_ready

fasthl-enable() {
  emulate -L zsh
  (( sysparams[pid] == _fasthl_shell_pid )) || return 1
  if ! _fasthl_enable; then
    print -ru2 -- "fasthl-enable: fast-highlight binary not found (set FASTHL_BIN)"
    return 1
  fi
}

fasthl-disable() {
  emulate -L zsh
  (( sysparams[pid] == _fasthl_shell_pid )) || return 1
  {
    _fasthl_enabled=0
    _fasthl_stop
    add-zle-hook-widget -d zle-line-pre-redraw _fasthl_redraw
    add-zle-hook-widget -d zle-line-finish _fasthl_redraw
    add-zsh-hook -d precmd _fasthl_precmd
    add-zsh-hook -d preexec _fasthl_preexec
    add-zsh-hook -d zshexit _fasthl_zshexit
    zle && _fasthl_apply
  } 2>/dev/null
  return 0
}

fasthl-reload() {
  emulate -L zsh
  (( sysparams[pid] == _fasthl_shell_pid )) || return 1
  {
    _fasthl_find_bin
    _fasthl_load_styles
    if (( _fasthl_enabled )); then
      _fasthl_stop
      _fasthl_backoff=0
      _fasthl_retry_at=0
      _fasthl_start || _fasthl_fail
    fi
  } 2>/dev/null
  return 0
}

_fasthl_enable
