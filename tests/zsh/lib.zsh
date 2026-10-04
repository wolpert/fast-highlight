# Helpers for the zsh-driven plugin tests. Sourced by run.zsh.
#
# Each test starts an interactive `zsh -d -i` under zsh/zpty with ZDOTDIR pointing at a fresh
# test directory whose .zshrc sources tests/zsh/zshrc. That file points FASTHL_BIN at a wrapper
# script, sources the plugin, and installs a zle-line-pre-redraw widget after the plugin's own
# that writes the buffer, region_highlight, and the time the plugin's hook took to $T/dump.
# Tests poll that file. Every wait is bounded.

zmodload zsh/zpty zsh/datetime zsh/system zsh/zselect
zmodload -F zsh/files b:zf_rm b:zf_mkdir

typeset -g ROOT=${${(%):-%x}:A:h:h:h}
typeset -g PLUGIN=$ROOT/plugin/fast-highlight.plugin.zsh
typeset -g MOCK=$ROOT/tests/fixtures/mock-daemon.py
typeset -g TEST_BIN=${FASTHL_TEST_BIN:-}
typeset -g ZSH_UNDER_TEST=${FASTHL_TEST_ZSH:-${commands[zsh]}}

typeset -g T=                # per-test temp dir
typeset -g OUT=              # everything the pty printed
typeset -ga T_ENV            # extra environment for the shell under test
typeset -g T_PRE= T_POST=    # .zshrc code before and after sourcing the plugin
typeset -g T_LANG=           # LANG for the shell under test (default en_US.UTF-8)
typeset -gi T_USE_MOCK=0     # force the mock even when FASTHL_TEST_BIN is set
typeset -gi T_NO_WAIT_READY=0 # t_spawn does not wait for a ready daemon
typeset -g D_STATE=
typeset -ga T_FAILS
typeset -gi D_N=0            # fields of the last dump read
typeset -gF D_DT=0
typeset -g D_BUF=
typeset -ga D_RH

t_log() { print -r -- "    $*" }

t_fail() {
  T_FAILS+=("$*")
  print -r -- "    FAIL: $*"
}

# t_check DESCRIPTION COMMAND...: run the command; record a failure if it fails.
t_check() {
  local desc=$1
  shift
  "$@" || t_fail "$desc"
}

# True when the shell under test talks to the mock daemon.
t_is_mock() { (( T_USE_MOCK )) || [[ -z $TEST_BIN ]] }

t_setup() {
  T=$(mktemp -d "${TMPDIR:-/tmp}/fasthl-test.XXXXXX") || return 1
  zf_mkdir $T/bin
  OUT=
  T_ENV=()
  T_PRE= T_POST= T_LANG=
  T_USE_MOCK=0 T_NO_WAIT_READY=0 D_STATE=
  T_FAILS=()
  D_N=0 D_BUF= D_RH=()
}

t_write_files() {
  local target
  if t_is_mock; then
    target="exec ${(q)${commands[python3]:-python3}} ${(q)MOCK}"
  else
    target="exec ${(q)TEST_BIN}"
  fi
  print -r -- '#!/bin/sh' >$T/bin/fast-highlight
  print -r -- '[ "$1" = serve ] && echo $$ >>'"${(q)T}/pids" >>$T/bin/fast-highlight
  print -r -- "$target"' "$@"' >>$T/bin/fast-highlight
  chmod +x $T/bin/fast-highlight
  print -r -- "source ${(q)ROOT}/tests/zsh/zshrc" >$T/.zshrc
  [[ -n $T_PRE ]] && print -r -- "$T_PRE" >$T/pre.zsh
  [[ -n $T_POST ]] && print -r -- "$T_POST" >$T/post.zsh
  return 0
}

# Start the shell under test and wait for its first prompt.
t_spawn() {
  t_write_files
  local -a env=(
    HOME=$T ZDOTDIR=$T TERM=xterm PATH=$PATH LANG=${T_LANG:-en_US.UTF-8}
    TMPDIR=$T MOCK_LOG=$T/mock.log MOCK_MARKER=$T/marker
    FHT_DIR=$T FHT_PLUGIN=$PLUGIN
    "${T_ENV[@]}"
  )
  cd $T || return 1
  zpty -b fhtest env -i ${(q)env} ${(q)ZSH_UNDER_TEST} -d -i || { t_fail "zpty spawn"; return 1 }
  # zle-line-init of the first prompt writes the first dump.
  t_wait_dump '' 10 || { t_fail "no first prompt"; return 1 }
  (( T_NO_WAIT_READY )) && return 0
  t_wait_ready
}

# Daemon start-up time varies with load (the mock is a Python script). With an empty command
# line, redraw with ^L until the plugin reports a ready daemon, so tests start from the same
# state.
t_wait_ready() {
  local -F end=$(( EPOCHREALTIME + 10 ))
  until [[ $D_STATE == ready ]] || (( EPOCHREALTIME > end )); do
    t_send $'\x0c'
    t_wait_next 1
  done
  [[ $D_STATE == ready ]] || { t_fail "daemon not ready after start-up"; return 1 }
}

# Append everything the pty printed so far to $OUT.
t_drain() {
  local chunk
  while zpty -rt fhtest chunk 2>/dev/null; do
    OUT+=$chunk
  done
  return 0
}

t_send() { zpty -w -n fhtest "$1" }

t_sleep() {
  local -i cs=$(( $1 * 100 ))
  zselect -t $cs 2>/dev/null
  return 0
}

# Read $T/dump into the D_* globals. Returns 1 when there is no dump.
t_read_dump() {
  [[ -r $T/dump ]] || return 1
  local line
  local -a lines
  lines=("${(@f)$(<$T/dump)}")
  D_RH=()
  for line in $lines; do
    case $line in
      (n=*) D_N=${line#n=} ;;
      (dt=*) D_DT=${line#dt=} ;;
      (buf=*) D_BUF=${line#buf=} ;;
      (state=*) D_STATE=${line#state=} ;;
      (rh=?*) D_RH+=("${line#rh=}") ;;
    esac
  done
  return 0
}

# t_wait_dump BUF [TIMEOUT [PATTERN]]: wait until a dump shows BUF and, when PATTERN is given,
# a region_highlight entry matching PATTERN.
t_wait_dump() {
  local want=$1 pat=${3-}
  local -F end=$(( EPOCHREALTIME + ${2:-5} ))
  while (( EPOCHREALTIME < end )); do
    t_drain
    if t_read_dump && [[ $D_BUF == "$want" ]]; then
      [[ -z $pat ]] && return 0
      (( ${D_RH[(I)$~pat]} )) && return 0
    fi
    t_sleep 0.01
  done
  return 1
}

# t_has PATTERN: true when an entry of the last dump matches PATTERN.
t_has() { (( ${D_RH[(I)$~1]} )) }

# t_ours: true when the last dump has an entry of the plugin.
t_ours() { t_has '*memo=fast-highlight*' }

# t_count PATTERN: number of entries of the last dump matching PATTERN, in $REPLY.
t_count() { REPLY=${#${(M)D_RH:#$~1}} }

# t_log_has PATTERN: true when a line of the mock log matches PATTERN.
t_log_has() {
  local -a lines
  [[ -r $T/mock.log ]] && lines=("${(@f)$(<$T/mock.log)}")
  (( ${lines[(I)$~1]} ))
}

# t_log_count PATTERN: number of mock log lines matching PATTERN, in $REPLY.
t_log_count() {
  local -a lines
  [[ -r $T/mock.log ]] && lines=("${(@f)$(<$T/mock.log)}")
  REPLY=${#${(M)lines:#$~1}}
}

# Clear the edit buffer with ^U and wait for the redraw.
t_clear() {
  t_send $'\x15'
  t_wait_dump '' 5 || t_fail "line not cleared"
}

# t_output_has PATTERN: true when the cleaned pty output matches PATTERN.
t_output_has() {
  t_drain
  t_clean_output
  [[ $REPLY == $~1 ]]
}

# Wait for the next dump after the current one (any buffer).
t_wait_next() {
  local -i n=$D_N
  local -F end=$(( EPOCHREALTIME + ${1:-5} ))
  while (( EPOCHREALTIME < end )); do
    t_drain
    t_read_dump && (( D_N > n )) && return 0
    t_sleep 0.01
  done
  return 1
}

# t_type TEXT: type TEXT one character at a time, waiting for each redraw.
t_type() {
  local c buf=$D_BUF
  for c in ${(s::)1}; do
    buf+=$c
    t_send $c
    t_wait_dump "$buf" 5 || { t_fail "no redraw for ${(qq)buf}"; return 1 }
  done
}

# t_run COMMAND: type COMMAND, press Enter, and wait for the next empty prompt.
t_run() {
  local -i n=$D_N
  t_send "$1"$'\r'
  local -F end=$(( EPOCHREALTIME + 10 ))
  while (( EPOCHREALTIME < end )); do
    t_drain
    t_read_dump && (( D_N > n )) && [[ -z $D_BUF ]] && return 0
    t_sleep 0.01
  done
  t_fail "command did not finish: $1"
  return 1
}

# The pty output with terminal control sequences removed, in $REPLY.
t_clean_output() {
  setopt localoptions extendedglob
  local s=$OUT
  s=${s//$'\e'\][^$'\a']#$'\a'/}
  s=${s//$'\e'\[[0-9;?<=>]#[@-~]/}
  s=${s//$'\e'[\(\)][A-Z0-9]/}
  s=${s//$'\e'[=>78]/}
  s=${s//[$'\r'$'\a'$'\b']/}
  REPLY=$s
}

# Fail when the terminal shows job-control messages or errors.
t_check_output() {
  setopt localoptions extendedglob
  t_drain
  t_clean_output
  local s=$REPLY pat
  local -a bad=(
    '*\[[0-9]##\] [0-9]##*'
    '*\[[0-9]##\]  #[+-]#  #(done|exit|killed|terminated|running|suspended)*'
    '*zsh:*' '*\(eval\)*' '*_fasthl*' '*fast-highlight:*' '*sysread*' '*syswrite*'
    '*sysopen*' '*FH1*' '*Traceback*' '*(#i)killed*' '*(#i)not found*'
    '*(#i)no such*' '*(#i)permission denied*' '*(#i)bad*descriptor*' '*(#i)error*'
  )
  for pat in $bad; do
    if [[ $s == $~pat ]]; then
      t_fail "unexpected terminal output (matched $pat)"
      print -r -- "$s" | sed 's/^/      | /' | tail -n 30
      return 1
    fi
  done
  return 0
}

# Daemon pids recorded by the wrapper, in start order, in $reply.
t_pids() {
  reply=()
  [[ -r $T/pids ]] && reply=("${(@f)$(<$T/pids)}")
  return 0
}

t_alive() { kill -0 $1 2>/dev/null }

# Wait until pid $1 is gone.
t_wait_gone() {
  local -F end=$(( EPOCHREALTIME + ${2:-3} ))
  while (( EPOCHREALTIME < end )); do
    t_alive $1 || return 0
    t_sleep 0.02
  done
  return 1
}

# Wait until the shell under test has exited.
t_wait_exit() {
  local -F end=$(( EPOCHREALTIME + ${1:-5} ))
  while (( EPOCHREALTIME < end )); do
    t_drain
    zpty -t fhtest 2>/dev/null || return 0
    t_sleep 0.02
  done
  return 1
}

t_teardown() {
  local p
  t_drain
  print -rn -- "$OUT" >$T/pty.out
  zpty -d fhtest 2>/dev/null
  t_pids
  for p in $reply; do
    if [[ $p == <-> ]] && ! t_wait_gone $p 3; then
      kill -9 $p 2>/dev/null
      t_fail "daemon $p left running at teardown"
    fi
  done
  if (( ${#T_FAILS} )) || (( ${+FASTHL_TEST_KEEP} )); then
    print -r -- "    (kept $T)"
  else
    zf_rm -rf -- $T
  fi
}
