#!/usr/bin/env bash
# One same-checkout owner for the host library and coordinator artifacts.
set -euo pipefail
ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
PROFILE="${1:-ffi-release}"
case "$PROFILE" in
    ffi-release|ffi-dev) ;;
    *) printf 'error: Cockpit artifacts require an unwind FFI profile (got %s)\n' "$PROFILE" >&2; exit 1 ;;
esac
# Never inherit a sibling worktree's Cargo output directory. Keep the measured
# original sequence: combining packages changes dependency LTO units and makes
# CLI staging rebuild; aligning engine features was slower cold as well.
# See research/2026-09-09-ci-compute-audit.md for all three measured alternatives.
export CARGO_TARGET_DIR="${ROOT}/target"
cd "$ROOT"
CARGO_ARGS=(--locked --manifest-path "${ROOT}/Cargo.toml" --profile "$PROFILE")
cargo rustc "${CARGO_ARGS[@]}" -p phux-client-ffi --lib --crate-type staticlib
cargo build "${CARGO_ARGS[@]}" -p phux
test -s "${ROOT}/target/${PROFILE}/libphux_client_ffi.a"
test -x "${ROOT}/target/${PROFILE}/phux"
