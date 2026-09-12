#!/usr/bin/env bash
# Build the matching coordinator CLI, never resolve an installed phux on PATH.
set -euo pipefail
ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
PROFILE="${1:?Cargo profile required}"
DESTINATION="${2:?destination required}"
CLI="${ROOT}/target/${PROFILE}/phux"
# Pin the output tree too: external CARGO_TARGET_DIR must not silently substitute
# a sibling worktree's stale binary for this checkout's build.
# A CI candidate can omit Cargo fingerprints. Recheck its exact input identity
# and BOTH output hashes at this consumption boundary; the flag alone is no proof.
if [[ "$PROFILE" == "ffi-release" && "${PHUX_CI_ARTIFACTS_VERIFIED:-}" == "true" ]]; then
    # A miss here fails the graph: its parallel links may already be consuming
    # the FFI archive, so repairing a candidate belongs before Zig starts.
    (cd "$ROOT" && python3 scripts/ci/cockpit_artifacts.py verify)
else
    # shellcheck source=clients/cockpit/scripts/native-cargo-target.sh
    source "${ROOT}/clients/cockpit/scripts/native-cargo-target.sh"
    phux_native_cargo_setup "$ROOT" "$PROFILE"
    # This step runs beside Zig links. Build only the CLI here so it cannot
    # replace the FFI archive being consumed by a concurrent linker. Its command
    # matches the final build in the producer, including Cargo's LTO unit mode.
    cargo build "${CARGO_ARGS[@]}" -p phux
    CLI="${PHUX_CARGO_OUTPUT}/phux"
fi
bash "${ROOT}/clients/cockpit/scripts/stage-phux-cli.sh" "$CLI" "$DESTINATION"
