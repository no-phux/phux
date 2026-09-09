#!/usr/bin/env bash
# One same-checkout producer with aligned host-library/coordinator engine features.
set -euo pipefail
ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
PROFILE="${1:-ffi-release}"
case "$PROFILE" in
    ffi-release|ffi-dev) ;;
    *) printf 'error: Cockpit artifacts require an unwind FFI profile (got %s)\n' "$PROFILE" >&2; exit 1 ;;
esac
# Never inherit a sibling worktree's Cargo output directory. Align the native
# engine with the CLI, including kitty-graphics/PNG, so Ghostty builds once.
# Keep staticlib-only: a combined cargo build emits rlib/cdylib too, changing
# dependency LTO units and forcing the later CLI freshness check to rebuild.
export CARGO_TARGET_DIR="${ROOT}/target"
CARGO_ARGS=(--locked --manifest-path "${ROOT}/Cargo.toml" --profile "$PROFILE")
cargo rustc "${CARGO_ARGS[@]}" -p phux-client-ffi --lib --crate-type staticlib \
    --features phux-protocol/server
cargo build "${CARGO_ARGS[@]}" -p phux
test -s "${ROOT}/target/${PROFILE}/libphux_client_ffi.a"
test -x "${ROOT}/target/${PROFILE}/phux"
