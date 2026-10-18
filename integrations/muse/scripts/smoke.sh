#!/bin/sh
# End-to-end drive test: proves Muse can use phux, via the general
# phux-agent.sh library (dogfooding). Spawns, runs, waits, snapshots, kills.
# Exits 0 on success.
set -u

_LIBDIR=$(dirname "$0")
# shellcheck source=../../.agents/lib/phux-agent.sh
. "$_LIBDIR/../../../.agents/lib/phux-agent.sh"

fail() { printf 'muse-smoke: error: %s\n' "$1" >&2; exit 1; }
ok() { printf 'muse-smoke: ok: %s\n' "$1"; }

MARKER="phux-muse-smoke-ok"
s=$(phux_spawn "muse-smoke-$$") || fail "could not spawn session"
ok "spawned session $s"
# shellcheck disable=SC2064
trap "phux_kill '$s' >/dev/null 2>&1" EXIT INT TERM

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
trap - EXIT INT TERM
ok "smoke passed: Muse can drive phux"
