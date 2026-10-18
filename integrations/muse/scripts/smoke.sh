#!/bin/sh
# End-to-end drive test: proves Muse can use phux, via the muse-lib.sh
# library itself (dogfooding). Spawns, runs, waits, snapshots, kills.
# Exits 0 on success.
set -u

_LIBDIR=$(dirname "$0")
# shellcheck source=scripts/muse-lib.sh
. "$_LIBDIR/muse-lib.sh"

fail() { printf 'muse-smoke: error: %s\n' "$1" >&2; exit 1; }
ok() { printf 'muse-smoke: ok: %s\n' "$1"; }

MARKER="phux-muse-smoke-ok"
s=$(mspawn "muse-smoke-$$") || fail "could not spawn session"
ok "spawned session $s"
# shellcheck disable=SC2064
trap "mkill '$s' >/dev/null 2>&1" EXIT INT TERM

out=$(mrun "$s" "printf '%s\n' '$MARKER'") || fail "run failed"
case "$out" in
  *"$MARKER"*) ok "command output contains marker" ;;
  *) fail "command output missing marker" ;;
esac

mwait "$s" "$MARKER" || fail "wait for marker timed out"
ok "marker observed via wait"

case "$(msnap "$s")" in
  *"$MARKER"*) ok "marker confirmed in snapshot" ;;
  *) fail "marker missing from snapshot" ;;
esac

mkill "$s" || fail "kill failed"
trap - EXIT INT TERM
ok "smoke passed: Muse can drive phux"
