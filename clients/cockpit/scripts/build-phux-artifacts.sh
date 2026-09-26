#!/usr/bin/env bash
# One same-checkout owner for the host library and coordinator artifacts.
set -euo pipefail
ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
ONLY=both
if [[ "${1:-}" == "--only" ]]; then
    ONLY="${2:?error: --only requires staticlib or cli}"
    shift 2
fi
PROFILE="${1:-ffi-release}"
case "$PROFILE" in
    ffi-release|ffi-dev) ;;
    *) printf 'error: Cockpit artifacts require an unwind FFI profile (got %s)\n' "$PROFILE" >&2; exit 1 ;;
esac
case "$ONLY" in
    both|staticlib|cli) ;;
    *) printf 'error: --only expects staticlib or cli (got %s)\n' "$ONLY" >&2; exit 1 ;;
esac
# Never inherit a sibling worktree's Cargo output directory. Keep the measured
# original sequence: combining packages changes dependency LTO units and makes
# CLI staging rebuild; aligning engine features was slower cold as well.
# See research/2026-09-09-ci-compute-audit.md for all three measured alternatives.
# shellcheck source=clients/cockpit/scripts/native-cargo-target.sh
source "${ROOT}/clients/cockpit/scripts/native-cargo-target.sh"
phux_native_cargo_setup "$ROOT" "$PROFILE"
if [[ "$PROFILE" == "ffi-release" ]]; then
    export RUSTFLAGS="-C target-cpu=apple-m1"
    export LIBGHOSTTY_VT_SYS_CPU=baseline
fi
cd "$ROOT"
if [[ "$ONLY" != "cli" ]]; then
    cargo rustc "${CARGO_ARGS[@]}" -p phux-client-ffi --lib --crate-type staticlib
    test -s "${PHUX_CARGO_OUTPUT}/libphux_client_ffi.a"
    phux_publish_native_artifact "${PHUX_CARGO_OUTPUT}/libphux_client_ffi.a" "${ROOT}/target/${PROFILE}/libphux_client_ffi.a"
fi
if [[ "$ONLY" != "staticlib" ]]; then
    cargo build "${CARGO_ARGS[@]}" -p phux
    test -x "${PHUX_CARGO_OUTPUT}/phux"
    phux_publish_native_artifact "${PHUX_CARGO_OUTPUT}/phux" "${ROOT}/target/${PROFILE}/phux"
fi
