#!/usr/bin/env bash
# Canonical shipping inputs shared by PR compilation and release packaging.
set -euo pipefail
ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
# -Dtrace=off: the shipped app routes stderr into its log file, and the SDK's
# default per-wake runtime.event trace would fill it at kilobytes per second.
exec ./scripts/zig-build.sh "$@" \
    -Dtarget=aarch64-macos -Dcpu=baseline \
    -Doptimize=ReleaseSafe -Dphux-enabled=true -Dtrace=off
