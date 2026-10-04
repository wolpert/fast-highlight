#!/usr/bin/env zsh
# Runs the zsh-driven tests of the fast-highlight plugin. Exits nonzero when a test fails.
#
#   tests/zsh/run.zsh [TEST...]     run all tests, or only the named ones (e.g. basic hang)
#
# Environment:
#   FASTHL_TEST_BIN   binary under test for the tests that do not depend on mock-specific
#                     behaviour (default: tests/fixtures/mock-daemon.py). Failure-handling
#                     tests always use the mock.
#   FASTHL_TEST_ZSH   zsh to run under the pty (default: zsh from $PATH)
#   FASTHL_TEST_LANG  UTF-8 locale for the shells (default: en_US.UTF-8)
#   FASTHL_TEST_KEEP  keep every test directory (failed tests always keep theirs)

emulate -R zsh
setopt extendedglob no_nomatch
export LANG=${FASTHL_TEST_LANG:-en_US.UTF-8}
unset LC_ALL LC_CTYPE
source ${0:A:h}/lib.zsh

# Slack allowed on top of FASTHL_TIMEOUT when measuring how long a redraw blocked.
typeset -gF SLACK=${FASTHL_TEST_SLACK:-0.04}

# --- tests -----------------------------------------------------------------------------------

# Typing fills region_highlight with the plugin's entries; requests carry the expected fields.
test_basic() {
  t_spawn || return
  t_type 'ls foo'
  t_has '0 2 *memo=fast-highlight' || t_fail "no command span for ls"
  if t_is_mock; then
    t_has '0 2 fg=green,bold memo=fast-highlight' || t_fail "style not taken from the theme"
    t_has '3 6 *' && t_fail "a kind with an empty style got an entry"
    t_log_has "ARGS serve --parent $(<$T/shell.pid)" || t_fail "daemon not started with --parent"
    t_log_has "S <-> alias,galias,salias,func,builtin,reswords,nameddirs,path" ||
      t_fail "full state not sent at startup"
    t_log_has "H <-> buf=l cur=1 cwd=$T opt=u pre=-" || t_fail "first request lacks cwd"
    t_log_has "H <-> buf=ls foo cur=6 cwd=- opt=u pre=-" || t_fail "cwd resent unchanged"
    # Moving the cursor alone is a new editor state.
    t_send $'\x02'
    t_wait_next 5
    t_log_has "H <-> buf=ls foo cur=5 cwd=- opt=u pre=-" || t_fail "cursor move not sent"
  fi
  t_check_output
}

# Unchanged editor state is served from the cache without a round trip.
test_cache() {
  T_USE_MOCK=1
  t_spawn || return
  t_type 'ls'
  t_log_count 'H *'
  local -i before=$REPLY
  # Redraws without an edit: clear-screen redraws the line.
  t_send $'\x0c'
  t_wait_next 5 || t_fail "no redraw after clear-screen"
  t_has '0 2 fg=green,bold memo=fast-highlight' || t_fail "cached entries not reapplied"
  t_log_count 'H *'
  (( REPLY == before )) || t_fail "unchanged state sent again ($before -> $REPLY requests)"
  t_check_output
}

# A crash turns highlighting off; after the backoff the daemon restarts and highlighting
# returns without further typing.
test_crash_restart() {
  T_USE_MOCK=1
  T_ENV=(MOCK_MODE=crash-once)
  t_spawn || return
  t_type 'e'
  t_ours && t_fail "entries applied although the daemon crashed"
  (( D_DT < 0.04 )) || t_fail "crash took ${D_DT}s to detect (EOF expected, not a timeout)"
  t_pids
  (( ${#reply} == 1 )) || t_fail "expected 1 daemon, saw ${#reply}"
  t_wait_gone $reply[1] 2 || t_fail "crashed daemon still running"
  t_type 'c'
  t_ours && t_fail "restarted before the backoff"
  t_sleep 0.6
  t_type 'h'
  # The keystroke restarts the daemon without waiting for it; the zle -F handler finishes the
  # handshake and highlights once the daemon answers, with no further keystroke.
  local -F end=$(( EPOCHREALTIME + 5 ))
  until t_log_has 'H <-> buf=ech *' || (( EPOCHREALTIME > end )); do t_sleep 0.02; done
  t_log_has 'H <-> buf=ech *' || t_fail "restarted daemon got no request without a keystroke"
  # A redraw without an edit shows the handler's result from the cache.
  t_send $'\x0c'
  t_wait_dump 'ech' 5 '0 3 fg=green,bold memo=fast-highlight' ||
    t_fail "no highlighting after restart"
  t_log_count 'H <-> buf=ech *'
  (( REPLY == 1 )) || t_fail "handler result not cached ($REPLY requests for the same state)"
  t_pids
  (( ${#reply} == 2 )) || t_fail "expected 2 daemons, saw ${#reply}"
  t_alive $reply[-1] || t_fail "restarted daemon not running"
  t_type 'o hi'
  t_has '0 4 fg=green,bold memo=fast-highlight' || t_fail "highlighting stopped after restart"
  t_run ''
  t_output_has '*hi*' || t_fail "command output missing"
  t_check_output
}

# A daemon that never answers costs one bounded wait; later keystrokes do not wait.
test_hang() {
  T_USE_MOCK=1
  T_ENV=(MOCK_MODE=hang)
  t_spawn || return
  local -F t0=$EPOCHREALTIME
  t_type 'x'
  local -F wall=$(( EPOCHREALTIME - t0 ))
  t_ours && t_fail "entries applied without an answer"
  (( D_DT >= 0.045 && D_DT < 0.05 + SLACK )) || t_fail "first redraw blocked ${D_DT}s"
  t_log "first keystroke: hook ${D_DT}s, wall ${wall}s"
  t_pids
  t_wait_gone $reply[1] 2 || t_fail "hung daemon not killed"
  local c
  for c in y z; do
    t0=$EPOCHREALTIME
    t_type $c
    wall=$(( EPOCHREALTIME - t0 ))
    (( D_DT < 0.01 )) || t_fail "keystroke $c waited ${D_DT}s with the daemon marked dead"
    t_log "keystroke $c: hook ${D_DT}s, wall ${wall}s"
  done
  # After the backoff a new daemon starts, hangs too, and is killed again.
  t_sleep 0.6
  t_type 'w'
  local -F end=$(( EPOCHREALTIME + 3 ))
  while (( EPOCHREALTIME < end )); do
    t_pids
    (( ${#reply} >= 2 )) && ! t_alive $reply[2] && break
    t_sleep 0.05
  done
  t_pids
  (( ${#reply} >= 2 )) || t_fail "no restart after the backoff"
  (( ${#reply} >= 2 )) && t_alive $reply[2] && t_fail "second hung daemon not killed"
  local line
  for line in "${(@f)$(<$T/dt.log)}"; do
    (( ${line%% *} < 0.05 + SLACK )) || t_fail "a redraw blocked ${line%% *}s"
  done
  t_check_output
}

# A daemon that stops reading cannot block the shell on a large request.
test_stuck_big() {
  T_USE_MOCK=1
  T_ENV=(MOCK_MODE=stuck)
  T_POST='_t_big() { BUFFER="echo ${(l:150000::x:)}"; CURSOR=5 }
zle -N _t_big
bindkey "^Xb" _t_big'
  t_spawn || return
  t_send $'\x18b'
  local -F end=$(( EPOCHREALTIME + 5 ))
  while (( EPOCHREALTIME < end )); do
    t_drain
    t_read_dump && [[ $D_BUF == echo\ x## ]] && break
    t_sleep 0.02
  done
  [[ $D_BUF == echo\ x## ]] || t_fail "no redraw after inserting the large buffer"
  (( D_DT < 0.05 + SLACK )) || t_fail "large request blocked ${D_DT}s"
  t_log "large request to a stuck daemon: hook ${D_DT}s"
  t_pids
  t_wait_gone $reply[1] 2 || t_fail "stuck daemon not killed"
  t_check_output
}

# Large requests go out in pieces and still get answered.
test_big_buffer() {
  T_USE_MOCK=1
  T_POST='_t_big() { BUFFER="echo ${(l:100000::x:)}"; CURSOR=5 }
zle -N _t_big
bindkey "^Xb" _t_big'
  t_spawn || return
  t_send $'\x18b'
  local -F end=$(( EPOCHREALTIME + 5 ))
  while (( EPOCHREALTIME < end )); do
    t_drain
    t_read_dump && [[ $D_BUF == echo\ x## ]] && t_has '0 4 *' && break
    t_sleep 0.02
  done
  t_has '0 4 fg=green,bold memo=fast-highlight' || t_fail "large buffer not highlighted"
  t_log "100 kB buffer: hook ${D_DT}s"
  t_check_output
}

# FASTHL_TIMEOUT raises the budget: a slow daemon still highlights.
test_timeout_setting() {
  T_USE_MOCK=1
  T_ENV=(MOCK_MODE=slow)
  T_PRE='FASTHL_TIMEOUT=0.5'
  t_spawn || return
  t_type 'ls'
  t_has '0 2 fg=green,bold memo=fast-highlight' || t_fail "slow answer not applied"
  (( D_DT >= 0.19 )) || t_fail "slow daemon answered in ${D_DT}s?"
  t_check_output
}

# A response to an older request is discarded, never applied.
test_stale() {
  T_USE_MOCK=1
  T_ENV=(MOCK_MODE=stale)
  t_spawn || return
  t_type 'ls'
  t_has '0 2 fg=green,bold memo=fast-highlight' || t_fail "current answer not applied"
  t_has '0 1 *' && t_fail "stale answer applied"
  t_pids
  t_alive $reply[1] || t_fail "daemon killed over a stale frame"
  t_check_output
}

# An E answer means no highlighting for that state, but the daemon stays.
test_error_response() {
  T_USE_MOCK=1
  T_ENV=(MOCK_MODE=error)
  t_spawn || return
  t_type 'ls'
  t_ours && t_fail "entries applied for an E answer"
  t_pids
  t_alive $reply[1] || t_fail "daemon killed over an E answer"
  (( ${#reply} == 1 )) || t_fail "daemon restarted over an E answer"
  t_check_output
}

# Exiting the shell stops the daemon and removes the FIFO directory.
test_exit() {
  t_spawn || return
  t_type 'ls'
  t_pids
  local pid=$reply[1]
  t_alive $pid || t_fail "daemon not running"
  local -a dirs=($T/fasthl.*(N))
  (( ${#dirs} == 0 )) || t_fail "FIFO directory left after the handshake: $dirs"
  t_clear
  t_send $'exit\r'
  t_wait_exit 5 || t_fail "shell did not exit"
  t_wait_gone $pid 2 || t_fail "daemon still running after exit"
  dirs=($T/fasthl.*(N))
  (( ${#dirs} == 0 )) || t_fail "FIFO directory left after exit: $dirs"
  t_check_output
}

# A shell killed without running zshexit leaves no daemon behind.
test_exit_killed() {
  t_spawn || return
  t_type 'ls'
  t_pids
  local pid=$reply[1] spid=$(<$T/shell.pid)
  kill -9 $spid
  t_wait_gone $pid 3 || t_fail "daemon still running after the shell was killed"
}

# Character offsets with multibyte text.
test_multibyte() {
  t_spawn || return
  t_type 'echo "世界" $HOME'
  t_has '5 9 *memo=fast-highlight' || t_fail "double-quoted span not at characters 5-9"
  t_has '10 15 *memo=fast-highlight' || t_fail "parameter span not at characters 10-15"
  if t_is_mock; then
    t_has '5 9 fg=yellow memo=fast-highlight' || t_fail "wrong style for the string"
    t_log_has 'H <-> buf=echo "世界" $HOME cur=15 cwd=- opt=u pre=-' ||
      t_fail "cursor not sent in characters"
  fi
  t_check_output
}

# The MULTIBYTE option does not change how ZLE counts, so offsets stay in characters.
test_nomultibyte_option() {
  T_POST='unsetopt multibyte'
  t_spawn || return
  t_type 'echo "世界" $HOME'
  t_has '5 9 *memo=fast-highlight' || t_fail "double-quoted span not at characters 5-9"
  t_has '10 15 *memo=fast-highlight' || t_fail "parameter span not at characters 10-15"
  if t_is_mock; then
    t_log_has 'H <-> buf=echo "世界" $HOME cur=15 cwd=- opt=u pre=-' ||
      t_fail "request not in characters"
  fi
  t_check_output
}

# In a non-UTF-8 locale ZLE counts bytes and the request asks for byte offsets.
test_bytes_c_locale() {
  T_USE_MOCK=1
  T_LANG=C
  t_spawn || return
  # ZLE in the C locale replaces non-ASCII input, so only ASCII can be typed here.
  t_type 'echo "a" $x'
  t_has '5 8 fg=yellow memo=fast-highlight' || t_fail "double-quoted span missing"
  t_has '9 11 fg=cyan memo=fast-highlight' || t_fail "parameter span missing"
  t_log_has 'H <-> buf=echo "a" $x cur=11 cwd=- opt= pre=-' ||
    t_fail "u option sent in the C locale"
  t_check_output
}

# Option letters follow the user's options.
test_options() {
  T_USE_MOCK=1
  T_POST='setopt interactivecomments autocd extendedglob kshglob'
  t_spawn || return
  t_type 'ls'
  t_log_has 'H <-> buf=ls cur=2 cwd=* opt=ucaek pre=-' || t_fail "option letters wrong"
  t_check_output
}

# Continuation lines send PREBUFFER.
test_prebuffer() {
  T_USE_MOCK=1
  t_spawn || return
  t_run 'for x in a; do'
  t_type 'echo'
  t_log_has 'H <-> buf=echo cur=4 cwd=- opt=u pre=for x in a; do\\n' || t_fail "pre not sent"
  t_has '0 4 fg=green,bold memo=fast-highlight' || t_fail "continuation line not highlighted"
  t_run 'done'
  t_check_output
}

# The zsh 5.8 code path (no memo): exact entries are tracked and removed, others' kept.
# ZLE shifts entry offsets on every insertion and deletion, so exact strings alone would not
# find them again; each state must show exactly the current entries.
test_no_memo() {
  T_USE_MOCK=1
  T_POST='_fasthl_memo=0; _t_foreign=1'
  t_spawn || return
  t_type 'ls foo'
  t_has '0 2 fg=green,bold' || t_fail "no command span"
  t_has '*memo=fast-highlight*' && t_fail "memo used on the 5.8 path"
  local buf=$D_BUF
  while [[ -n $buf ]]; do
    buf=${buf%?}
    t_send $'\x7f'
    t_wait_dump "$buf" 5 || { t_fail "no redraw for ${(qq)buf}"; break }
    t_count 'P0 1 underline memo=t-foreign'
    (( REPLY == 1 )) || t_fail "foreign entry count $REPLY at ${(qq)buf}"
    local -a want=()
    [[ -n $buf ]] && want=("0 ${#${buf%% *}} fg=green,bold")
    [[ ${(j:|:)${D_RH:#*memo=t-foreign}} == ${(j:|:)want} ]] ||
      t_fail "at ${(qq)buf}: entries ${(qq)D_RH}"
  done
  t_type 'x y'
  [[ ${(j:|:)${D_RH:#*memo=t-foreign}} == '0 1 fg=green,bold' ]] ||
    t_fail "after retyping: entries ${(qq)D_RH}"
  t_check_output
}

# With memo, other plugins' entries are kept too.
test_foreign_entries() {
  T_POST='_t_foreign=1'
  t_spawn || return
  t_type 'ls'
  t_type ' a'
  t_count '*memo=t-foreign'
  (( REPLY == 1 )) || t_fail "foreign entry count $REPLY"
  t_count '0 2 *memo=fast-highlight'
  (( REPLY == 1 )) || t_fail "own entries duplicated ($REPLY)"
  t_check_output
}

# precmd sends only the parts of the shell state that changed; cwd changes are sent.
test_state_sync() {
  T_USE_MOCK=1
  t_spawn || return
  t_run 'myfunc() { : }'
  t_log_has 'S <-> func' || t_fail "new function not sent as a func-only update"
  t_log_has 'S-func *myfunc*' || t_fail "myfunc missing from func list"
  t_run 'alias zz=ls'
  t_log_has 'S <-> alias' || t_fail "new alias not sent as an alias-only update"
  t_log_has 'S-alias *zz*' || t_fail "zz missing from alias list"
  t_log_count 'S *'
  local -i before=$REPLY
  t_run 'true'
  t_log_count 'S *'
  (( REPLY == before )) || t_fail "state sent although nothing changed"
  t_run 'cd /'
  t_type 'x'
  t_log_has 'H <-> buf=x cur=1 cwd=/ opt=u pre=-' || t_fail "new cwd not sent"
  t_check_output
}

# The hook leaves $?, $_, $REPLY, and $MATCH alone.
test_preserves_specials() {
  t_spawn || return
  t_run 'false'
  t_run 'echo st=$?'
  t_output_has '*st=1*' || t_fail '$? changed by the plugin'
  t_run 'echo hello world'
  t_run 'echo last=$_'
  t_output_has '*last=world*' || t_fail '$_ changed by the plugin'
  t_run 'REPLY=keep; [[ abc =~ b ]]'
  t_type 'ls'
  t_clear
  t_run 'echo r=$REPLY m=$MATCH'
  t_output_has '*r=keep m=b*' || t_fail '$REPLY or $MATCH changed by the plugin'
  t_check_output
}

# fasthl-disable stops everything; fasthl-enable brings it back.
test_disable_enable() {
  t_spawn || return
  t_type 'ls'
  t_ours || t_fail "no entries before disabling"
  t_pids
  local pid=$reply[1]
  t_clear
  t_run 'fasthl-disable'
  t_wait_gone $pid 2 || t_fail "daemon still running after fasthl-disable"
  t_type 'ls'
  t_ours && t_fail "entries while disabled"
  t_clear
  # Re-enabling appends the plugin's hook after the dump hook; move the dump hook last again.
  local hook=add-zle-hook-widget
  t_run "fasthl-enable; $hook -d line-pre-redraw _t_post; $hook line-pre-redraw _t_post"
  t_wait_ready
  t_send 'ls'
  t_wait_dump 'ls' 5 '0 2 *memo=fast-highlight' || t_fail "no entries after fasthl-enable"
  t_pids
  (( ${#reply} == 2 )) && t_alive $reply[2] || t_fail "no new daemon after fasthl-enable"
  t_check_output
}

# fasthl-reload restarts the daemon and keeps style overrides made after sourcing.
test_reload() {
  T_USE_MOCK=1
  T_POST="FASTHL_STYLES[command]='fg=red'"
  t_spawn || return
  t_type 'ls $x'
  t_has '0 2 fg=red memo=fast-highlight' || t_fail "override not applied"
  t_has '3 5 fg=cyan memo=fast-highlight' || t_fail "theme style not applied"
  t_clear
  t_run 'fasthl-reload'
  t_wait_ready
  t_type 'ls $x'
  t_has '0 2 fg=red memo=fast-highlight' || t_fail "override lost on reload"
  t_has '3 5 fg=cyan memo=fast-highlight' || t_fail "theme style lost on reload"
  t_pids
  (( ${#reply} == 2 )) || t_fail "reload did not restart the daemon"
  t_wait_gone $reply[1] 2 || t_fail "old daemon still running after reload"
  t_check_output
}

# Without `styles` output the built-in table is used.
test_fallback_styles() {
  T_USE_MOCK=1
  T_ENV=(MOCK_NO_STYLES=1)
  t_spawn || return
  t_type 'ls'
  t_has '0 2 fg=green memo=fast-highlight' || t_fail "built-in style not used"
  t_check_output
}

# Without a binary the plugin does nothing and prints nothing.
test_no_binary() {
  T_NO_WAIT_READY=1
  T_PRE='FASTHL_BIN=/nonexistent/fast-highlight'
  t_spawn || return
  t_type 'ls'
  t_ours && t_fail "entries without a binary"
  t_clear
  t_run 'fasthl-reload'
  t_pids
  (( ${#reply} == 0 )) || t_fail "a daemon was started"
  t_check_output
}

# Job control on (the interactive default), daemon crashing on every request: typing and
# commands keep working and nothing reaches the terminal.
test_no_output_on_failures() {
  T_USE_MOCK=1
  T_ENV=(MOCK_MODE=crash)
  T_POST='setopt monitor notify'
  t_spawn || return
  local -F end=$(( EPOCHREALTIME + 2.5 ))
  while (( EPOCHREALTIME < end )); do
    t_type 'ab'
    t_sleep 0.2
    t_clear
  done
  t_pids
  (( ${#reply} >= 3 )) || t_fail "expected at least 3 restarts, saw ${#reply}"
  t_run 'echo ok-$((6*7))'
  t_output_has '*ok-42*' || t_fail "command did not run"
  t_send $'exit\r'
  t_wait_exit 5 || t_fail "shell did not exit"
  local -a dirs=($T/fasthl.*(N))
  (( ${#dirs} == 0 )) || t_fail "FIFO directories left behind: $dirs"
  t_check_output
}

# Round-trip latency per keystroke, as measured inside the shell.
test_latency() {
  t_spawn || return
  t_type 'git commit -m "message" --amend ~/src'
  local -a dts=(${(n)${(f)"$(<$T/dt.log)"}%% *})
  local -F max=$dts[-1] med=$dts[$(( (${#dts} + 1) / 2 ))]
  t_log "keystrokes: ${#dts}, median ${med}s, max ${max}s"
  (( max < 0.05 )) || t_fail "a keystroke took ${max}s"
  t_check_output
}

# Sourcing the plugin twice changes nothing.
test_double_source() {
  T_POST='source $FHT_PLUGIN'
  t_spawn || return
  t_type 'ls'
  t_count '0 2 *memo=fast-highlight'
  (( REPLY == 1 )) || t_fail "own entries duplicated ($REPLY)"
  t_pids
  (( ${#reply} == 1 )) || t_fail "expected 1 daemon, saw ${#reply}"
  t_check_output
}

# A daemon slower to start than the precmd wait is picked up by the zle -F handler, which
# sends the shell state before any keystroke.
test_slow_start() {
  T_USE_MOCK=1
  T_ENV=(MOCK_START_DELAY=0.4)
  T_NO_WAIT_READY=1
  t_spawn || return
  local -F end=$(( EPOCHREALTIME + 5 ))
  until t_log_has 'S <-> *' || (( EPOCHREALTIME > end )); do t_sleep 0.02; done
  t_log_has 'S <-> alias,*' || t_fail "state not sent without a keystroke"
  t_type 'ls'
  t_has '0 2 fg=green,bold memo=fast-highlight' || t_fail "no highlighting after a slow start"
  t_pids
  (( ${#reply} == 1 )) || t_fail "slow start treated as a failure (${#reply} daemons)"
  t_check_output
}

# A daemon that dies at once (writing to stderr) is retried with backoff, silently.
test_die_at_start() {
  T_USE_MOCK=1
  T_ENV=(MOCK_MODE=die)
  T_NO_WAIT_READY=1
  t_spawn || return
  local -F end=$(( EPOCHREALTIME + 2 ))
  while (( EPOCHREALTIME < end )); do
    t_type 'a'
    t_sleep 0.1
  done
  t_ours && t_fail "entries without a daemon"
  t_pids
  (( ${#reply} >= 2 && ${#reply} <= 5 )) || t_fail "unexpected number of start attempts: ${#reply}"
  local line
  for line in "${(@f)$(<$T/dt.log)}"; do
    (( ${line%% *} < 0.05 + SLACK )) || t_fail "a redraw blocked ${line%% *}s"
  done
  t_check_output
}

# `exec zsh` replaces the shell without zshexit; the old daemon still goes away.
test_exec_shell() {
  t_spawn || return
  t_type 'ls'
  t_pids
  local pid=$reply[1]
  t_clear
  # The new shell starts its dump counter afresh.
  zf_rm -f $T/dump
  D_N=0
  t_send "exec ${(q)ZSH_UNDER_TEST} -d -i"$'\r'
  t_wait_dump '' 10 || t_fail "no prompt from the new shell"
  t_wait_ready
  t_wait_gone $pid 3 || t_fail "old daemon still running after exec"
  t_type 'ls'
  t_has '0 2 *memo=fast-highlight' || t_fail "no highlighting in the new shell"
  t_check_output
}

# Plugin functions run in a subshell do not touch the parent's daemon.
test_subshell() {
  t_spawn || return
  t_type 'ls'
  t_pids
  local pid=$reply[1]
  t_clear
  t_run '( fasthl-reload; fasthl-disable; exit ); print sub-$?'
  t_output_has '*sub-1*' || t_fail "plugin functions did not refuse to run in a subshell"
  t_alive $pid || t_fail "subshell stopped the parent's daemon"
  t_type 'ls'
  t_has '0 2 *memo=fast-highlight' || t_fail "highlighting lost after the subshell"
  t_pids
  (( ${#reply} == 1 )) || t_fail "a subshell started a daemon"
  t_check_output
}

# vared and select edit text that is not a command line; nothing is highlighted there.
test_vared() {
  T_USE_MOCK=1
  t_spawn || return
  t_run 'v=ls'
  t_send $'vared v\r'
  t_wait_dump 'ls' 5 || t_fail "vared did not start"
  t_type ' x'
  t_ours && t_fail "vared buffer highlighted"
  t_run ''
  t_type 'ls'
  t_ours || t_fail "no highlighting after vared"
  t_check_output
}

# precmd cost with the completion system loaded (thousands of function names).
test_precmd_cost() {
  T_PRE='typeset -ga _t_pc
_t_pc0() { _t_pct=$EPOCHREALTIME }
precmd_functions=(_t_pc0 $precmd_functions)'
  T_POST='autoload -Uz compinit; compinit -u -d $FHT_DIR/zcompdump
_t_pc1() { print -r -- $(( EPOCHREALTIME - _t_pct )) >>$FHT_DIR/precmd.log }
precmd_functions+=(_t_pc1)'
  t_spawn || return
  t_run 'true'
  t_run 'f1() { : }'
  t_run 'true'
  local -a pc=("${(@f)$(<$T/precmd.log)}")
  t_log "precmd with $(( ${#pc} )) samples: ${(j:s, :)pc}s"
  if t_is_mock; then
    t_log_has 'S <-> func' || t_fail "func change not sent with compinit loaded"
  fi
  local x
  for x in $pc; do
    (( x < 0.05 + SLACK )) || t_fail "precmd took ${x}s"
  done
  t_check_output
}

# The daemon is not a job: `jobs` lists nothing and `wait` returns at once.
test_jobs() {
  t_spawn || return
  t_type 'ls'
  t_clear
  t_run 'wait; print -r -- jobs-${#jobstates}-$(jobs | wc -l)'
  t_output_has '*jobs-0-*0*' || t_fail "the daemon shows up as a job"
  t_check_output
}

# Non-interactive shells and shells without ZLE ignore the plugin, silently.
test_inactive() {
  local out cmd="source ${(q)PLUGIN}; print -r -- loaded=\${+functions[fasthl-reload]}"
  out=$(FASTHL_BIN=$MOCK ${ZSH_UNDER_TEST} -f -c "$cmd" 2>&1)
  [[ $out == 'loaded=0' ]] || t_fail "non-interactive shell: ${(qq)out}"
  out=$(FASTHL_BIN=$MOCK ${ZSH_UNDER_TEST} -f -i -c "$cmd" </dev/null 2>&1)
  [[ $out == 'loaded=0' ]] || t_fail "shell without a terminal: ${(qq)out}"
  # `zsh -i -c` on a terminal, as editors run it to read the environment.
  zpty -b fhtest env FASTHL_BIN=$MOCK ${(q)ZSH_UNDER_TEST} -f -i -c ${(q)cmd}
  t_wait_exit 5 || t_fail "zsh -i -c did not exit"
  t_clean_output
  [[ $REPLY == *loaded=0* ]] || t_fail "zsh -i -c: ${(qq)REPLY}"
}

# --- runner ----------------------------------------------------------------------------------

typeset -a all=(${${(M)${(k)functions}:#test_*}#test_})
all=(${(o)all})
typeset -a selected=("$@")
(( $#selected )) || selected=($all)

typeset -i failed=0 passed=0
typeset name
for name in $selected; do
  if (( ! ${+functions[test_$name]} )); then
    print -r -- "unknown test: $name"
    failed+=1
    continue
  fi
  print -r -- "--- $name"
  # Each test runs in a subshell under a watchdog so a stuck test cannot hang the run.
  (
    t_setup || exit 1
    test_$name
    t_teardown
    (( ${#T_FAILS} == 0 ))
  ) &
  typeset -i tpid=$!
  typeset -F tend=$(( EPOCHREALTIME + 90 ))
  while kill -0 $tpid 2>/dev/null && (( EPOCHREALTIME < tend )); do
    t_sleep 0.05
  done
  if kill -0 $tpid 2>/dev/null; then
    kill -9 $tpid 2>/dev/null
    print -r -- "    FAIL: timed out"
  fi
  if wait $tpid; then
    print -r -- "ok  $name"
    passed+=1
  else
    print -r -- "FAIL $name"
    failed+=1
  fi
done
print -r -- "passed: $passed, failed: $failed"
(( failed == 0 ))
