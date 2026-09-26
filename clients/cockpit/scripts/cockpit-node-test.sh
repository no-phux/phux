#!/usr/bin/env bash
# Canonical noninteractive Node gate for Cockpit's TypeScript model tests.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

npm ci --ignore-scripts --no-audit --no-fund
exec node --import ./src/tests/navigation-loader.mjs --test \
    ./src/tests/*.test.mjs ./src/keybindings.test.ts
