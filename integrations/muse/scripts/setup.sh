#!/bin/sh
# Idempotent environment check for the Muse integration.
# Verifies the toolchain and proves the server is drivable.
# Exits 0 when Muse can use phux, non-zero with a hint otherwise.
set -u

_LIBDIR=$(dirname "$0")
# shellcheck source=../../.agents/lib/phux-agent.sh
. "$_LIBDIR/../../../.agents/lib/phux-agent.sh"

fail() { printf 'muse-setup: error: %s\n' "$1" >&2; exit 1; }
ok() { printf 'muse-setup: ok: %s\n' "$1"; }

phux_init || fail "phux not ready (need phux and jq on PATH, and a startable server)"
ok "phux $(phux --version 2>/dev/null | head -1), server running"

# Pinned tool versions this integration was validated against.
# Warn, do not fail: the CLI contract matters more than the toolchain.
check_version() {
  tool=$1; want=$2; got=$3
  case "$got" in
    *"$want"*) ok "$tool $got" ;;
    *) printf 'muse-setup: warn: %s is %s, integration built against %s\n' "$tool" "$got" "$want" >&2 ;;
  esac
}

command -v rustc >/dev/null 2>&1 && check_version rustc "1.99.0" "$(rustc --version 2>/dev/null)"
command -v just >/dev/null 2>&1 && check_version just "1.58.0" "$(just --version 2>/dev/null)"

ok "Muse can drive phux"
