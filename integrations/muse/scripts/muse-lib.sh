#!/bin/sh
# muse-lib.sh — ergonomic shell library for driving phux as Muse.
#
# Source it, don't execute it:
#   . ./scripts/muse-lib.sh
#
# Then:
#   muse_init            # ensure phux + server (idempotent)
#   s=$(mspawn mysess)   # spawn ephemeral session, prints name
#   mrun "$s" "make -j"  # run a command (default timeout 20s)
#   mwait "$s" "done"    # wait for text with finite timeout
#   msnap "$s"           # snapshot pane
#   mkill "$s"           # kill session (no prompt)
#
# Every function prints its result on stdout and diagnostics on stderr.
# Every wait carries a finite timeout. Requires: phux, jq on PATH.
# shellcheck disable=SC2317  # functions are the API; sourced, not executed

MUSE_DEFAULT_TIMEOUT=20

_muse_err() { printf 'muse: error: %s\n' "$1" >&2; }
_muse_ok() { printf 'muse: ok: %s\n' "$1" >&2; }

# Ensure the phux binary and a running server. Starts the server if needed.
# Idempotent: safe to call on every use.
muse_init() {
  command -v phux >/dev/null 2>&1 || { _muse_err "phux not on PATH"; return 1; }
  command -v jq >/dev/null 2>&1 || { _muse_err "jq not on PATH"; return 1; }
  if [ "$(phux status --json 2>/dev/null | jq -r '.running // false')" = "true" ]; then
    return 0
  fi
  _muse_ok "starting phux server"
  # Detached: the server outlives this shell.
  phux server >/dev/null 2>&1 &
  # Wait up to 10s for the socket.
  i=0
  while [ "$i" -lt 20 ]; do
    if [ "$(phux status --json 2>/dev/null | jq -r '.running // false')" = "true" ]; then
      _muse_ok "server running"
      return 0
    fi
    sleep 0.5
    i=$((i + 1))
  done
  _muse_err "server did not start"
  return 1
}

# Spawn an ephemeral session. Prints the session name.
mspawn() {
  [ "$#" -eq 1 ] || { _muse_err "usage: mspawn <name>"; return 2; }
  muse_init || return 1
  phux new --json -s "$1" --ephemeral 2>/dev/null | jq -r '.session // empty' || {
    _muse_err "could not spawn session $1"
    return 1
  }
}

# Run one command in a target. Usage: mrun <target> <command> [timeout]
# Prints the command's output; the function exit status is the command's.
mrun() {
  [ "$#" -ge 2 ] || { _muse_err "usage: mrun <target> <command> [timeout]"; return 2; }
  _t=$1; _c=$2; _to=${3:-$MUSE_DEFAULT_TIMEOUT}
  muse_init || return 1
  _out=$(phux run --json --timeout "$_to" "$_t" "$_c" 2>/dev/null) || {
    _muse_err "run failed"
    return 1
  }
  printf '%s\n' "$_out" | jq -r '.output // empty'
  _code=$(printf '%s\n' "$_out" | jq -r '.exit_code // 1')
  return "$_code"
}

# Snapshot a target's pane. Prints the visible text.
msnap() {
  [ "$#" -eq 1 ] || { _muse_err "usage: msnap <target>"; return 2; }
  muse_init || return 1
  phux snapshot --json "$1" 2>/dev/null | jq -r '.lines | join("\n")'
}

# Wait for text to appear. Usage: mwait <target> <text> [timeout]
# Succeeds when the text is observed; fails on timeout.
mwait() {
  [ "$#" -ge 2 ] || { _muse_err "usage: mwait <target> <text> [timeout]"; return 2; }
  _t=$1; _x=$2; _to=${3:-$MUSE_DEFAULT_TIMEOUT}
  muse_init || return 1
  phux wait --json --timeout "$_to" --until "$_x" "$_t" >/dev/null 2>&1
}

# List sessions. Prints one name per line.
mls() {
  muse_init || return 1
  phux ls --json 2>/dev/null | jq -r '.sessions[]?.name // empty'
}

# Kill a session. No confirmation prompt.
mkill() {
  [ "$#" -eq 1 ] || { _muse_err "usage: mkill <target>"; return 2; }
  muse_init || return 1
  phux kill --yes "$1" >/dev/null 2>&1
}

# Full drive cycle for one command. Usage: mdo <name> <command> [timeout]
# Spawns, runs, waits for exit marker, snapshots, kills. Prints snapshot.
mdo() {
  [ "$#" -ge 2 ] || { _muse_err "usage: mdo <name> <command> [timeout]"; return 2; }
  _n=$1; _c=$2; _to=${3:-$MUSE_DEFAULT_TIMEOUT}
  _s=$(mspawn "$_n") || return 1
  # shellcheck disable=SC2064
  trap "mkill '$_s' >/dev/null 2>&1" EXIT INT TERM
  mrun "$_s" "$_c" "$_to" >/dev/null || { _muse_err "run failed"; return 1; }
  msnap "$_s"
  mkill "$_s" >/dev/null 2>&1
  trap - EXIT INT TERM
}
