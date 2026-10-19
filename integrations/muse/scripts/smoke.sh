#!/bin/sh
# End-to-end drive test: proves Muse can use phux, via the general
# phux-agent.sh library (dogfooding). Spawns, runs, waits, snapshots, kills.
# Exits 0 on success.
set -u

_LIBDIR=$(dirname "$0")
# shellcheck source=../../../.agents/lib/phux-agent.sh
. "$_LIBDIR/../../../.agents/lib/phux-agent.sh"

fail() { printf 'muse-smoke: error: %s\n' "$1" >&2; exit 1; }
ok() { printf 'muse-smoke: ok: %s\n' "$1"; }

MARKER="phux-muse-smoke-ok"
[ "$#" -eq 1 ] || fail "usage: smoke.sh /absolute/path/to/isolated-test.sock"
case "$1" in /*) ;; *) fail "test socket path must be absolute" ;; esac
# Do not mistake an inherited production PHUX_SOCKET for test opt-in.
PHUX_SOCKET=$1
export PHUX_SOCKET
s=$(phux_spawn "muse-smoke-$$") || fail "could not spawn session"
ok "spawned pane $s"
trap 'phux_kill "$s" >/dev/null || :' 0
trap 'exit 130' INT
trap 'exit 143' TERM

out=$(phux_run "$s" "printf '%s\n' '$MARKER'") || fail "run failed"
case "$out" in
  *"$MARKER"*) ok "command output contains marker" ;;
  *) fail "command output missing marker" ;;
esac

phux_wait "$s" "$MARKER" || fail "wait for marker timed out"
ok "marker observed via wait"

case "$(phux_snap "$s")" in
  *"$MARKER"*) ok "marker confirmed in snapshot" ;;
  *) fail "marker missing from snapshot" ;;
esac

phux_kill "$s" || fail "kill failed"
trap - 0 INT TERM
ok "smoke passed: Muse can drive phux"
