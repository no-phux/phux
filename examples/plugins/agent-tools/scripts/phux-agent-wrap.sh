#!/bin/sh
#
# phux-agent-wrap.sh (phux-r82.11) — make a terminal agent self-identify.
#
# Wrap a real agent command so that, the moment it launches inside a
# phux pane, the pane gets a first-class `phux.agent/v1` L3 record
# (ADR-0040) instead of relying on the OSC-title substring heuristic.
# The record carries the agent's name and kind, so the TUI sidebar and
# any fleet view show a declared identity that a plain `claude`/`codex`
# session never announces.
#
# Usage:
#   phux-agent-wrap.sh [--name NAME] [--kind KIND] [--state STATE]
#                      [--target TARGET] [--stream-single-turn]
#                      [--prompt-length CHARS]
#                      -- command [arg...]
#
# Everything after `--` is the real agent argv. The child retains the wrapper's
# stdin/stdout/stderr while the wrapper waits and forwards signals. On start it
# writes the record; on exit (normal, signal, or agent failure) it clears
# it via a trap, so an un-launched pane never shows a stale agent.
#
# Design constraints (see phux CLAUDE.md / AGENTS.md):
#   - POSIX sh, no bashisms, no new dependencies.
#   - No shell injection: every value is passed as its own quoted argv
#     element to `phux`; nothing is ever routed through `eval` or `sh -c`.
#   - Best-effort identity: if `phux` is missing or no server is up, the
#     record write fails silently and the agent still launches. Losing the
#     sidebar label must never stop the agent from running.
#
# We deliberately do NOT `exec` the agent: a trap on EXIT cannot fire
# after `exec` replaces this process, and clearing the record on exit is
# the whole point of the trap. Supervising the agent as a child, preserving its
# TTY streams, and forwarding its outcome is the only way to guarantee cleanup.
#
# Pane targeting is REQUIRED and resolved exactly once, up front, then
# reused verbatim for both the launch-time `set` and the exit-time `clear`.
# We never let `phux agent set/clear` fall back to whatever pane happens to
# be FOCUSED at CLI-run time: focus moves freely, and the exit-time clear
# fires at an arbitrary later moment, so a focused-pane guess would race —
# in a multi-pane / fleet run the clear would delete a *different*, still-
# running agent's record and leave this pane's record stale. If we cannot
# resolve which pane we are running in, we write nothing at all (best-
# effort no-op) and still launch the agent; a missing sidebar label is
# always safer than corrupting a sibling pane's identity.
#
# The pane target comes from, in order: `--target` / PHUX_AGENT_TARGET, or
# else PHUX_TERMINAL_ID (the pane's wire id, used as the `@N` selector).
# PHUX_TERMINAL_ID is the automatic path: phux exposes it to hook children
# today, and once the server also injects it into spawned pane processes
# (see the README follow-up) a wrapped agent self-targets with no config.
# Until then, a launcher that knows the pane must pass PHUX_AGENT_TARGET /
# --target for the record to be written.
#
# Overrides (env):
#   PHUX_AGENT_PHUX_BIN / PHUX_BIN  path to the `phux` binary (default `phux`)
#   PHUX_AGENT_NAME                 default --name
#   PHUX_AGENT_KIND                 default --kind
#   PHUX_AGENT_STATE                default --state (see note below)
#   PHUX_AGENT_TARGET               default --target (pane selector)
#   PHUX_AGENT_STREAM_SINGLE_TURN   `1` to emit a one-turn lifecycle
#   PHUX_AGENT_PROMPT_LENGTH        prompt character count for that lifecycle
#   PHUX_TERMINAL_ID                pane wire id; used as target `@N` when
#                                   no explicit --target/PHUX_AGENT_TARGET
#
# State note: normal interactive agents still use screen/title detection.
# `--stream-single-turn` is only for a command whose process lifetime is one
# complete turn: it opens an AgentSession and emits `prompt` before execution.
# Exit 0 emits `stop`; every outcome emits terminal `session_end` and closes the
# exact child returned by `session open`. It must not wrap an interactive TUI.

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

# Fall back to the launched command's basename as the agent name, so the
# wrapper is still useful when invoked with a bare `-- command`.
if [ -z "$agent_name" ]; then
  agent_name=$(basename -- "$1")
fi

# Resolve the pane target exactly once, here, so `set` (launch) and `clear`
# (exit) always act on the SAME pane. Never guess the focused pane: if no
# explicit target is given, fall back to the pane's own wire id
# (PHUX_TERMINAL_ID) as the `@N` selector, and if that is also absent leave
# the target empty — in which case we deliberately skip the record writes.
if [ -z "$agent_target" ] && [ -n "${PHUX_TERMINAL_ID:-}" ]; then
  agent_target="@${PHUX_TERMINAL_ID}"
fi

if [ -z "$agent_target" ]; then
  printf '%s: no pane target (set PHUX_AGENT_TARGET/--target, or run where PHUX_TERMINAL_ID is set); launching %s without a phux.agent record\n' \
    "$0" "$agent_name" >&2
fi

# Run `phux` with the given argv, best-effort: never let a missing binary
# or absent server abort the agent launch or the cleanup. Positional
# params here are local to the function, so the caller's agent argv ($@)
# is preserved across these calls.
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
  # No resolved pane target => do not write. Writing here would target the
  # focused pane, which may be a different agent's pane.
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
  # Only clear the exact pane we set at launch. With no target we would
  # otherwise clear whichever pane is focused at exit time — very likely a
  # different, still-running agent's record. Skipping is the safe default.
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
  # `open` prints the exact local AgentSession selector. Reject any diagnostic
  # or malformed output rather than falling back to the parent pane, whose
  # child lookup can be ambiguous as sessions appear concurrently.
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
    # Keep `done` observable briefly, then terminate the record grammar before
    # closing. Closing without `session_end` leaves an incomplete event log.
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
  # macOS's POSIX-mode sh reports a missing asynchronous command as 1 rather
  # than the standard command-not-found status. Detect lookup failure before
  # forking so the wrapper preserves the portable 127 outcome.
  case "$1" in
    */*) [ -e "$1" ] || return 127 ;;
    *) command -v "$1" >/dev/null 2>&1 || return 127 ;;
  esac
  # An asynchronous command would otherwise inherit /dev/null in a
  # non-interactive POSIX shell. Preserve stdin explicitly so interactive
  # agents retain their TTY while the wrapper remains able to handle signals.
  launching_child=yes
  "$@" <&0 &
  child_pid=$!
  launching_child=no
  # A trap can run in the tiny interval after fork but before `$!` is stored.
  # It records the signal without exiting; forward it now that the child is
  # addressable, then follow the ordinary wait/reap path.
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
  # A second signal during the done grace must not interrupt session close or
  # identity cleanup. The first signal already selected this exit path.
  trap '' INT TERM HUP QUIT
  finish_stream "$exit_status"
  clear_record
}

# Clear on every exit path. Signal handlers forward to the recorded child and
# let `run_child` reap it before the EXIT trap performs cleanup exactly once.
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
