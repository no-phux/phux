#!/bin/sh
# phux-agent-wrap.sh -- run an agent so its pane carries a `phux.agent/v1`
# identity record (ADR-0040) for its lifetime.
#
# Usage:
#   phux-agent-wrap.sh [--name NAME] [--kind KIND] [--state STATE]
#                      [--target TARGET] [--stream-single-turn]
#                      [--prompt-length CHARS]
#                      -- command [arg...]
#
# The agent runs as a child (not `exec`) so the EXIT trap can clear the
# record. Every value reaches `phux` as its own argv element (no eval), and
# record writes are best-effort: a missing phux or server never blocks launch.
#
# The pane target is resolved once, from --target / PHUX_AGENT_TARGET, else
# @$PHUX_TERMINAL_ID, and reused for both set and clear. It never falls back to
# the focused pane: the exit-time clear would race focus and could delete a
# sibling agent's record. With no target the wrapper writes nothing.
#
# Env overrides: PHUX_AGENT_PHUX_BIN / PHUX_BIN (phux binary), PHUX_AGENT_NAME,
# PHUX_AGENT_KIND, PHUX_AGENT_STATE, PHUX_AGENT_TARGET,
# PHUX_AGENT_STREAM_SINGLE_TURN (1 to enable), PHUX_AGENT_PROMPT_LENGTH.
#
# --stream-single-turn is only for a process whose lifetime is exactly one
# turn (never an interactive TUI): it opens an AgentSession, emits prompt, and
# on exit emits stop (status 0) plus session_end, then closes that exact child.

set -eu

phux_bin=${PHUX_AGENT_PHUX_BIN:-${PHUX_BIN:-phux}}
agent_name=${PHUX_AGENT_NAME:-}
agent_kind=${PHUX_AGENT_KIND:-}
agent_state=${PHUX_AGENT_STATE:-}
agent_target=${PHUX_AGENT_TARGET:-}
stream_single_turn=${PHUX_AGENT_STREAM_SINGLE_TURN:-0}
prompt_length=${PHUX_AGENT_PROMPT_LENGTH:-0}
stream_target=
child_pid=
received_signal=
signal_number=
launching_child=no
cleanup_started=no

usage() {
  printf 'usage: %s [--name NAME] [--kind KIND] [--state STATE] [--target TARGET] [--stream-single-turn] [--prompt-length CHARS] -- command [arg...]\n' "$0" >&2
}

need_value() {
  # $1 flag name, $2 remaining arg count
  if [ "$2" -lt 2 ]; then
    printf '%s: %s requires a value\n' "$0" "$1" >&2
    exit 2
  fi
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --name) need_value "$1" "$#"; agent_name=$2; shift 2 ;;
    --name=*) agent_name=${1#--name=}; shift ;;
    --kind) need_value "$1" "$#"; agent_kind=$2; shift 2 ;;
    --kind=*) agent_kind=${1#--kind=}; shift ;;
    --state) need_value "$1" "$#"; agent_state=$2; shift 2 ;;
    --state=*) agent_state=${1#--state=}; shift ;;
    --target) need_value "$1" "$#"; agent_target=$2; shift 2 ;;
    --target=*) agent_target=${1#--target=}; shift ;;
    --stream-single-turn) stream_single_turn=1; shift ;;
    --prompt-length) need_value "$1" "$#"; prompt_length=$2; shift 2 ;;
    --prompt-length=*) prompt_length=${1#--prompt-length=}; shift ;;
    --) shift; break ;;
    -*) usage; exit 2 ;;
    *) break ;;
  esac
done

if [ "$#" -eq 0 ]; then
  usage
  exit 2
fi

case "$prompt_length" in
  ''|*[!0-9]*)
    printf '%s: --prompt-length must be a non-negative integer\n' "$0" >&2
    exit 2
    ;;
esac

if [ -z "$agent_name" ]; then
  agent_name=$(basename -- "$1")
fi

if [ -z "$agent_target" ] && [ -n "${PHUX_TERMINAL_ID:-}" ]; then
  agent_target="@${PHUX_TERMINAL_ID}"
fi

if [ -z "$agent_target" ]; then
  printf '%s: no pane target (set PHUX_AGENT_TARGET/--target, or run where PHUX_TERMINAL_ID is set); launching %s without a phux.agent record\n' \
    "$0" "$agent_name" >&2
fi

try_phux() {
  "$phux_bin" "$@" >/dev/null 2>&1
}

run_phux() {
  try_phux "$@" || true
}

json_escape() {
  # Keep open-vocabulary provider slugs valid JSON without adding jq/python as
  # wrapper dependencies. Byte-wise encoding preserves UTF-8 and escapes the
  # complete JSON C0 range; shell variables cannot contain NUL.
  LC_ALL=C od -An -v -tu1 | LC_ALL=C awk '
    {
      for (i = 1; i <= NF; i++) {
        byte = $i + 0
        if (byte == 34) printf "\\\""
        else if (byte == 92) printf "\\\\"
        else if (byte == 8) printf "\\b"
        else if (byte == 9) printf "\\t"
        else if (byte == 10) printf "\\n"
        else if (byte == 12) printf "\\f"
        else if (byte == 13) printf "\\r"
        else if (byte < 32) printf "\\u%04x", byte
        else printf "%c", byte
      }
    }'
}

set_record() {
  [ -n "$agent_target" ] || return 0
  set -- agent set "$agent_target" --name "$agent_name"
  if [ -n "$agent_kind" ]; then
    set -- "$@" --kind "$agent_kind"
  fi
  if [ -n "$agent_state" ]; then
    set -- "$@" --state "$agent_state"
  fi
  run_phux "$@"
}

# Invoked indirectly through the EXIT trap below.
# shellcheck disable=SC2329
clear_record() {
  [ -n "$agent_target" ] || return 0
  run_phux agent clear "$agent_target"
}

start_stream() {
  [ "$stream_single_turn" = 1 ] || return 0
  [ -n "$agent_target" ] || return 0
  provider=${agent_kind:-$agent_name}
  provider_json=$(printf '%s' "$provider" | json_escape)
  stream_target=$("$phux_bin" agent session open "$agent_target" --provider "$provider" 2>/dev/null) || {
    stream_target=
    return 0
  }
  # Accept only an exact `@N` selector; never fall back to the parent pane.
  case "$stream_target" in
    @*[!0-9]*|'@'|'') stream_target=; return 0 ;;
    @*) ;;
    *) stream_target=; return 0 ;;
  esac
  run_phux agent emit "$stream_target" --type session_start \
    --data "{\"provider\":\"$provider_json\"}"
  run_phux agent emit "$stream_target" --type prompt \
    --data "{\"length\":$prompt_length}"
}

# Invoked through the EXIT-trap call graph.
# shellcheck disable=SC2329
failure_reason() {
  if [ -n "$received_signal" ]; then
    printf 'signal_%s\n' "$received_signal"
    return
  fi
  case "$1" in
    126) printf 'command_not_executable\n' ;;
    127) printf 'command_not_found\n' ;;
    *) printf 'exit_status_%s\n' "$1" ;;
  esac
}

# Invoked through the EXIT trap below.
# shellcheck disable=SC2329
finish_stream() {
  status=$1
  [ -n "$stream_target" ] || return 0
  if [ "$status" -eq 0 ] && [ -z "$received_signal" ]; then
    # Keep `done` visible briefly; session_end must precede close.
    run_phux agent emit "$stream_target" --type stop
    sleep 1
    reason=completed
  else
    reason=$(failure_reason "$status")
  fi
  run_phux agent emit "$stream_target" --type session_end \
    --data "{\"reason\":\"$reason\"}"
  run_phux agent session close "$stream_target"
  stream_target=
}

# Invoked through the signal traps below.
# shellcheck disable=SC2329
forward_signal() {
  signal_name=$1
  signal_number=$2
  received_signal=$signal_name
  if [ -z "$child_pid" ]; then
    [ "$launching_child" = yes ] && return
    exit $((128 + signal_number))
  fi
  kill -s "$signal_name" "$child_pid" 2>/dev/null || true
}

run_child() {
  # macOS sh reports a missing background command as 1; preserve 127.
  case "$1" in
    */*) [ -e "$1" ] || return 127 ;;
    *) command -v "$1" >/dev/null 2>&1 || return 127 ;;
  esac
  # Background jobs get /dev/null stdin in non-interactive sh; keep the TTY.
  launching_child=yes
  "$@" <&0 &
  child_pid=$!
  launching_child=no
  # A signal caught between fork and `$!` was only recorded; forward it now.
  if [ -n "$received_signal" ]; then
    kill -s "$received_signal" "$child_pid" 2>/dev/null || true
  fi
  child_status=0
  while :; do
    wait "$child_pid" || child_status=$?
    if [ -z "$received_signal" ] || ! kill -0 "$child_pid" 2>/dev/null; then
      break
    fi
  done
  child_pid=
  if [ -n "$received_signal" ]; then
    child_status=$((128 + signal_number))
  fi
  return "$child_status"
}

# Invoked indirectly through the EXIT trap below.
# shellcheck disable=SC2329
cleanup() {
  exit_status=$1
  [ "$cleanup_started" = no ] || return 0
  cleanup_started=yes
  # A second signal must not interrupt session close or identity cleanup.
  trap '' INT TERM HUP QUIT
  finish_stream "$exit_status"
  clear_record
}

# Signals forward to the child; run_child reaps it, then EXIT cleans up once.
trap 'cleanup "$?"' EXIT
trap 'forward_signal INT 2' INT
trap 'forward_signal TERM 15' TERM
trap 'forward_signal HUP 1' HUP
trap 'forward_signal QUIT 3' QUIT

set_record
start_stream

status=0
run_child "$@" || status=$?
exit "$status"
