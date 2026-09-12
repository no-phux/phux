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
# shellcheck source=clients/cockpit/scripts/native-cargo-target.sh
source "${ROOT}/clients/cockpit/scripts/native-cargo-target.sh"
phux_native_cargo_setup "$ROOT" "$PROFILE"
cd "$ROOT"
cargo rustc "${CARGO_ARGS[@]}" -p phux-client-ffi --lib --crate-type staticlib
cargo build "${CARGO_ARGS[@]}" -p phux
test -s "${PHUX_CARGO_OUTPUT}/libphux_client_ffi.a"
test -x "${PHUX_CARGO_OUTPUT}/phux"
for artifact in libphux_client_ffi.a phux; do
    phux_publish_native_artifact "${PHUX_CARGO_OUTPUT}/${artifact}" "${ROOT}/target/${PROFILE}/${artifact}"
done
