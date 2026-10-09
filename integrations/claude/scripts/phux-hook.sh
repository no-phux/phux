#!/bin/sh
# The plugin's lifecycle hook. One arm per registration in hooks/hooks.json;
# Claude's payload arrives on stdin and is read through `phux agent
# hook-payload`, which prints shell-safe tokens and never the prompt text,
# tool input, or tool output. Conversation text reaches phux only on stdin:
# the transcript entry `phux agent hook-transcript` prints (ADR-0156), and the
# raw payload under `PHUX_AGENT_EMIT_RAW=1`. Best effort and silent: every phux call is
# allowed to fail, and nothing is printed back to Claude.
set -u

[ "$#" -eq 1 ] || exit 2
[ -n "${PHUX_TERMINAL_ID:-}" ] || exit 0

phux=${PHUX_AGENT_PHUX_BIN:-phux}
target="@$PHUX_TERMINAL_ID"

run_phux() {
  "$phux" "$@" >/dev/null 2>&1 || true
}

# Does the server serve AgentSession resources? `phux status --json` lists
# `resource_kinds` under `features` when it does. Probed once per hook.
streams=no
case $("$phux" status --json 2>/dev/null) in
  *'"resource_kinds"'*) streams=yes ;;
esac

# Payload fields as `phux agent hook-payload` prints them: one line of
# shell-safe tokens, `-` when absent. A phux without the helper leaves
# every field absent, and the arms below still do their every-server work.
hook_session=-
hook_event=-
hook_tool=-
hook_kind=-
hook_chars=0
hook_reason=-
hook_source=-
payload=
entry=
# The payload copy and a transcript entry are removed however the hook
# leaves, including Claude killing it at its timeout.
trap 'rm -f ${payload:+"$payload"} ${entry:+"$entry"}' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
if [ ! -t 0 ] && payload=$(mktemp 2>/dev/null); then
  cat > "$payload" 2>/dev/null || :
  fields=$("$phux" agent hook-payload < "$payload" 2>/dev/null) || fields=
  # shellcheck disable=SC2086 # the helper prints tokens with no IFS or glob characters
  set -- "$1" $fields
  if [ "$#" -eq 8 ]; then
    hook_session=$2
    hook_event=$3
    hook_tool=$4
    hook_kind=$5
    hook_chars=$6
    hook_reason=$7
    hook_source=$8
    case "$hook_chars" in *[!0-9]*) hook_chars=0 ;; esac
  fi
  set -- "$1"
fi

# One `agent emit`, its stdin from file $1 (empty: none). A graceful upgrade
# refuses appends from its cut of the stream until the re-exec ("the server
# is upgrading"), with nothing appended, so that refusal alone is resent for
# up to ~3s; every other failure stays best effort and silent.
emit_once() {
  input=${1:-/dev/null}
  shift
  tries=0
  while :; do
    err=$("$phux" agent emit "$target" "$@" < "$input" 2>&1 >/dev/null) && return 0
    case $err in *'server is upgrading'*) ;; *) return 0 ;; esac
    tries=$((tries + 1))
    [ "$tries" -lt 10 ] || return 0
    sleep 0.3 2>/dev/null || return 0
  done
}

emit() {
  [ "$streams" = yes ] || return 0
  if [ "$#" -gt 1 ]; then
    emit_once "" --type "$1" --data "$2"
  else
    emit_once "" --type "$1"
  fi
}

emit_tool() {
  if [ "$hook_tool" != - ]; then
    emit "$1" "{\"tool_name\":\"$hook_tool\"}"
  else
    emit "$1"
  fi
}

# The whole payload, only when opted in, and never after `session_end`.
emit_raw() {
  if [ "${PHUX_AGENT_EMIT_RAW:-0}" = 1 ] && [ -n "$payload" ] && [ -s "$payload" ]; then
    [ "$streams" = yes ] || return 0
    emit_once "$payload" --type provider_raw --data -
  fi
}

# The transcript entry for this hook (ADR-0156): the prompt, a finished tool
# call, or the turn's last reply, as `phux.transcript/v1` data fed on stdin.
# On by default; `PHUX_AGENT_TRANSCRIPT=0` opts out. A phux without the
# helper prints nothing, and nothing is emitted.
emit_transcript() {
  [ "${PHUX_AGENT_TRANSCRIPT:-1}" != 0 ] || return 0
  [ "$streams" = yes ] || return 0
  [ -n "$payload" ] || return 0
  [ -s "$payload" ] || return 0
  entry=$(mktemp 2>/dev/null) || return 0
  if "$phux" agent hook-transcript < "$payload" > "$entry" 2>/dev/null && [ -s "$entry" ]; then
    emit_once "$entry" --type provider_raw --data -
  fi
  rm -f "$entry"
  entry=
}

case "$1" in
  start)
    run_phux agent set "$target" --name claude --kind claude
    if [ "$streams" = yes ] && [ "$hook_session" != - ] && [ "$hook_source" != compact ]; then
      run_phux agent session open "$target" --provider claude --native-id="$hook_session"
      emit session_start
    fi
    emit_raw
    ;;
  working)
    emit prompt "{\"chars\":$hook_chars}"
    emit_transcript
    emit_raw
    ;;
  tool-start)
    emit_tool tool_start
    emit_raw
    ;;
  tool-end)
    if [ "$hook_event" = PostToolUseFailure ] && [ "$hook_tool" != - ]; then
      emit tool_end "{\"tool_name\":\"$hook_tool\",\"ok\":false}"
    else
      emit_tool tool_end
    fi
    emit_transcript
    emit_raw
    ;;
  blocked)
    if [ "$hook_event" = Notification ]; then
      case "$hook_kind" in
        permission_prompt) kind=permission ;;
        elicitation_dialog|elicitation_url_dialog) kind=elicitation ;;
        idle_prompt) kind=idle ;;
        *) kind=$hook_kind ;;
      esac
      emit notification "{\"kind\":\"$kind\"}"
    else
      emit ask
    fi
    emit_raw
    run_phux ask "$target" "Claude needs attention"
    ;;
  done)
    emit_transcript
    emit stop
    emit_raw
    ;;
  clear)
    emit_raw
    if [ "$hook_reason" != - ]; then
      emit session_end "{\"reason\":\"$hook_reason\"}"
    else
      emit session_end
    fi
    [ "$streams" = yes ] && run_phux agent session close "$target"
    run_phux agent clear "$target"
    ;;
  *)
    exit 2
    ;;
esac
exit 0
