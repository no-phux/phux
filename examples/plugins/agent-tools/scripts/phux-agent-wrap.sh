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
#                      -- command [arg...]
#
# Everything after `--` is the real agent argv, run in the foreground so
# its TTY / job-control semantics are untouched. On start the wrapper
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
# the whole point of the trap. Running the agent as a foreground child and
# forwarding its exit status is the only way to guarantee cleanup.
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
#   PHUX_AGENT_STREAM_SINGLE_TURN   `1` to emit prompt -> stop around command
#   PHUX_TERMINAL_ID                pane wire id; used as target `@N` when
#                                   no explicit --target/PHUX_AGENT_TARGET
#
# State note: normal interactive agents still use screen/title detection.
# `--stream-single-turn` is only for a command whose process lifetime is one
# complete turn: it opens an AgentSession, emits `prompt` before execution,
# then `stop` at exit. It must not be used for a long-lived interactive TUI.

set -eu

phux_bin=${PHUX_AGENT_PHUX_BIN:-${PHUX_BIN:-phux}}
agent_name=${PHUX_AGENT_NAME:-}
agent_kind=${PHUX_AGENT_KIND:-}
agent_state=${PHUX_AGENT_STATE:-}
agent_target=${PHUX_AGENT_TARGET:-}
stream_single_turn=${PHUX_AGENT_STREAM_SINGLE_TURN:-0}
stream_opened=no

usage() {
  printf 'usage: %s [--name NAME] [--kind KIND] [--state STATE] [--target TARGET] [--stream-single-turn] -- command [arg...]\n' "$0" >&2
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
    --) shift; break ;;
    -*) usage; exit 2 ;;
    *) break ;;
  esac
done

if [ "$#" -eq 0 ]; then
  usage
  exit 2
fi

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
  if try_phux agent session open "$agent_target" --provider "$provider"; then
    stream_opened=yes
    run_phux agent emit "$agent_target" --type session_start
    run_phux agent emit "$agent_target" --type prompt
  fi
}

# Invoked indirectly through cleanup's EXIT-trap path.
# shellcheck disable=SC2329
finish_stream() {
  [ "$stream_opened" = yes ] || return 0
  # Keep `done` observable for a bounded grace before closing the child. A
  # `session_end` would retract it immediately, defeating an edge-triggered
  # waiter; never closing would leak a done session when this wrapper is run
  # manually inside a long-lived shell pane.
  run_phux agent emit "$agent_target" --type stop
  sleep 1
  run_phux agent session close "$agent_target"
  stream_opened=no
}

# Invoked indirectly through the EXIT trap below.
# shellcheck disable=SC2329
cleanup() {
  # A second signal during the done grace must not interrupt session close or
  # identity cleanup. The first signal already selected this exit path.
  trap '' INT TERM HUP QUIT
  finish_stream
  clear_record
}

# Clear on any exit path. Signal traps re-raise through `exit`, which then
# runs the EXIT trap exactly once.
trap 'cleanup' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
trap 'exit 131' QUIT

set_record
start_stream

status=0
"$@" || status=$?
exit "$status"
