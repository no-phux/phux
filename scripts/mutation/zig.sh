#!/usr/bin/env bash
# Scoped Cockpit mutation testing; see docs/TESTING_MUTATIONS.md.
set -euo pipefail
ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
exec python3 "$ROOT/scripts/mutation/zig.py" "$@"
