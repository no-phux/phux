#!/bin/sh
# phux-agent.sh — ergonomic shell library for driving phux programmatically.
#
# Harness-agnostic: any agent or script can source this, not just Muse.
# Source it, don't execute it:
#   . ./phux-agent.sh
#
# Then:
#   phux_init            # ensure phux CLI + server (idempotent, auto-starts)
#   s=$(phux_spawn s1)   # spawn ephemeral session, prints name
#   phux_run "$s" "make -j"  # run a command, prints its output
#   phux_wait "$s" "done"    # wait for text with finite timeout
#   phux_snap "$s"           # snapshot pane text
#   phux_ls                  # list sessions, one name per line
#   phux_kill "$s"           # kill session, no prompt
#   phux_do job "cmd"        # spawn, run, snapshot, kill — one shot
#
# Every function prints its result on stdout and diagnostics on stderr.
# Every wait carries a finite timeout (default 20s, override with
# PHUX_AGENT_TIMEOUT). Requires: phux, jq on PATH.
# Verified against phux 0.108.1 CLI JSON shapes; see SKILL.md for the
# raw-CLI reference when the library does not cover an operation.

PHUX_AGENT_TIMEOUT=${PHUX_AGENT_TIMEOUT:-20}

_phux_err() { printf 'phux-agent: error: %s\n' "$1" >&2; }
_phux_ok() { printf 'phux-agent: ok: %s\n' "$1" >&2; }

# Ensure the phux binary and a running server. Starts the server if needed.
# Idempotent: safe to call on every use. The server's `no_server` contract
# is intentional (status is a read-only probe), so agents own startup.
phux_init() {
  command -v phux >/dev/null 2>&1 || { _phux_err "phux not on PATH"; return 1; }
  command -v jq >/dev/null 2>&1 || { _phux_err "jq not on PATH"; return 1; }
  if [ "$(phux status --json 2>/dev/null | jq -r '.running // false')" = "true" ]; then
    return 0
  fi
  _phux_ok "starting phux server"
  # Detached: the server outlives this shell.
  phux server >/dev/null 2>&1 &
  # Wait up to 10s for the socket.
  i=0
  while [ "$i" -lt 20 ]; do
    if [ "$(phux status --json 2>/dev/null | jq -r '.running // false')" = "true" ]; then
      _phux_ok "server running"
      return 0
    fi
    sleep 0.5
    i=$((i + 1))
  done
  _phux_err "server did not start"
  return 1
}

# Spawn an ephemeral session. Prints the session name.
# Ephemeral: ends with its last process. Use for scratch work.
phux_spawn() {
  [ "$#" -eq 1 ] || { _phux_err "usage: phux_spawn <name>"; return 2; }
  phux_init || return 1
  phux new --json -s "$1" --ephemeral 2>/dev/null | jq -r '.session // empty' || {
    _phux_err "could not spawn session $1"
    return 1
  }
}

# Run one command in a target. Usage: phux_run <target> <command> [timeout]
# Prints the command's output; the function exit status is the command's.
phux_run() {
  [ "$#" -ge 2 ] || { _phux_err "usage: phux_run <target> <command> [timeout]"; return 2; }
  _t=$1; _c=$2; _to=${3:-$PHUX_AGENT_TIMEOUT}
  phux_init || return 1
  _out=$(phux run --json --timeout "$_to" "$_t" "$_c" 2>/dev/null) || {
    _phux_err "run failed"
    return 1
  }
  printf '%s\n' "$_out" | jq -r '.output // empty'
  _code=$(printf '%s\n' "$_out" | jq -r '.exit_code // 1')
  return "$_code"
}

# Snapshot a target's pane. Prints the visible text lines.
phux_snap() {
  [ "$#" -eq 1 ] || { _phux_err "usage: phux_snap <target>"; return 2; }
  phux_init || return 1
  phux snapshot --json "$1" 2>/dev/null | jq -r '.lines | join("\n")'
}

# Wait for text to appear. Usage: phux_wait <target> <text> [timeout]
# Succeeds when the text is observed; fails on timeout.
phux_wait() {
  [ "$#" -ge 2 ] || { _phux_err "usage: phux_wait <target> <text> [timeout]"; return 2; }
  _t=$1; _x=$2; _to=${3:-$PHUX_AGENT_TIMEOUT}
  phux_init || return 1
  phux wait --json --timeout "$_to" --until "$_x" "$_t" >/dev/null 2>&1
}

# List sessions. Prints one name per line.
phux_ls() {
  phux_init || return 1
  phux ls --json 2>/dev/null | jq -r '.sessions[]?.name // empty'
}

# Kill a session. No confirmation prompt.
phux_kill() {
  [ "$#" -eq 1 ] || { _phux_err "usage: phux_kill <target>"; return 2; }
  phux_init || return 1
  phux kill --yes "$1" >/dev/null 2>&1
}

# Full drive cycle for one command. Usage: phux_do <name> <command> [timeout]
# Spawns, runs, snapshots, kills. Prints the snapshot.
phux_do() {
  [ "$#" -ge 2 ] || { _phux_err "usage: phux_do <name> <command> [timeout]"; return 2; }
  _n=$1; _c=$2; _to=${3:-$PHUX_AGENT_TIMEOUT}
  _s=$(phux_spawn "$_n") || return 1
  # shellcheck disable=SC2064
  trap "phux_kill '$_s' >/dev/null 2>&1" EXIT INT TERM
  phux_run "$_s" "$_c" "$_to" >/dev/null || { _phux_err "run failed"; return 1; }
  phux_snap "$_s"
  phux_kill "$_s" >/dev/null 2>&1
  trap - EXIT INT TERM
}
