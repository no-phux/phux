#!/usr/bin/env bash
# Measure SDK paint-table bind points at N=1/2/4/8 panes (Hybrid C).
#
#   ./scripts/measure-paint-ceiling.sh
# measures: SDK paint-table bind points at 1/2/4/8 panes (Hybrid C / pkg3b)
#
# Headless. Does not bump the Native SDK pin. Live macOS PTY rss is
# scripts/drive-shell-ceiling.sh and cannot run here.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
. "${ROOT}/scripts/lib/measure.sh"

measure_basis paint-ceiling \
    "zig build test -Dplatform=null -Dmeasure=true (pin ad3f0fae, Hybrid C signed)" \
    "./scripts/measure-paint-ceiling.sh"

if [[ "$(uname -s)" != Darwin ]]; then
    printf 'note: drive-shell-ceiling.sh skipped (macOS live .app only); paint bind is the cell store.\n'
fi

cd "$ROOT"
# -Dmeasure=true makes Zig print a fake `failed command:` line on a green
# run; the exit code is the verdict. See src/tests/measured.zig.
set +e
./scripts/zig-build.sh test -Dplatform=null -Dmeasure=true --summary all
status=$?
set -e
exit "$status"
