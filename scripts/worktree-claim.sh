#!/usr/bin/env bash
# Lease the current worktree to the running agent session, or refuse (exit 3)
# when another live session holds it. See scripts/lib/worktree-owner.sh.
# Usage: scripts/worktree-claim.sh [caller-label]
set -euo pipefail
# shellcheck source=lib/worktree-owner.sh
. "$(dirname -- "${BASH_SOURCE[0]}")/lib/worktree-owner.sh"
# shellcheck source=lib/mbx-target.sh
. "$(dirname -- "${BASH_SOURCE[0]}")/lib/mbx-target.sh"
worktree_owner_claim "$PWD" "${1:-just}"
# mbx GC can delete the cache directory this symlink names. Cargo then
# fails with "Not a directory". Recreate that directory only.
mbx_heal_target "$PWD" || true
