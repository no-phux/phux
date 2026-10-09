#!/usr/bin/env bash
# Canonical noninteractive Node gate for Cockpit's TypeScript model tests.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

npm ci --ignore-scripts --no-audit --no-fund
# The loader needs Node 24: Node 26 rejects stripTypeScriptTypes mode
# "transform". mise.toml pins 24. CI's setup-node job has no mise and
# already puts Node 24 on PATH.
if command -v mise >/dev/null 2>&1; then
    exec mise exec -- node --import ./src/tests/navigation-loader.mjs --test \
        ./src/tests/*.test.mjs ./src/keybindings.test.ts
fi
exec node --import ./src/tests/navigation-loader.mjs --test \
    ./src/tests/*.test.mjs ./src/keybindings.test.ts
