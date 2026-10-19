#!/bin/sh
# Idempotent environment check for the Muse integration.
# Verifies runtime dependencies and an already running server, read-only.
# Exits 0 when Muse can use phux, non-zero with a hint otherwise.
set -u

_LIBDIR=$(dirname "$0")
# shellcheck source=../../../.agents/lib/phux-agent.sh
. "$_LIBDIR/../../../.agents/lib/phux-agent.sh"

fail() { printf 'muse-setup: error: %s\n' "$1" >&2; exit 1; }
ok() { printf 'muse-setup: ok: %s\n' "$1"; }

phux_init || fail "phux not ready (need phux and jq on PATH, and a running server)"
ok "phux $(phux --version 2>/dev/null | head -1), server running"

ok "Muse can drive phux"
