#!/usr/bin/env bash
# Canonical shipping inputs shared by PR compilation and release packaging.
set -euo pipefail
ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
exec ./scripts/zig-build.sh "$@" \
    -Dtarget=aarch64-macos -Doptimize=ReleaseSafe -Dphux-enabled=true
