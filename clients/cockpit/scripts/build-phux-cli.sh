#!/usr/bin/env bash
# Build the matching coordinator CLI, never resolve an installed phux on PATH.
set -euo pipefail
ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
PROFILE="${1:?Cargo profile required}"
DESTINATION="${2:?destination required}"
# Pin the output tree too: external CARGO_TARGET_DIR must not silently substitute
# a sibling worktree's stale binary for this checkout's build.
CARGO_TARGET_DIR="${ROOT}/target" cargo build --locked --manifest-path "${ROOT}/Cargo.toml" --profile "$PROFILE" -p phux
bash "${ROOT}/clients/cockpit/scripts/stage-phux-cli.sh" "${ROOT}/target/${PROFILE}/phux" "$DESTINATION"
