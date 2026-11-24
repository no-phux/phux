#!/usr/bin/env bash
# The per-worktree agent lease (scripts/lib/worktree-owner.sh):
#   claim    lease the current worktree to the running agent session, or refuse
#            (exit 3) when another live session holds it
#   verify   after a long run, fail (exit 3) when HEAD moved or another session
#            took the worktree over since the claim
#   release  drop this session's lease so the next session needs no takeover
# Usage: scripts/worktree-lease.sh claim|verify|release [caller-label]
set -euo pipefail
here="$(dirname -- "${BASH_SOURCE[0]}")"
# shellcheck source=lib/worktree-owner.sh
. "$here/lib/worktree-owner.sh"
op="${1:-}"
label="${2:-just}"
case "$op" in
    claim)
        # shellcheck source=lib/mbx-target.sh
        . "$here/lib/mbx-target.sh"
        worktree_owner_claim "$PWD" "$label"
        # mbx GC can delete the cache directory this symlink names. Cargo then
        # fails with "Not a directory". Recreate that directory only.
        mbx_heal_target "$PWD" || true
        ;;
    verify) worktree_owner_verify "$PWD" "$label" ;;
    release) worktree_owner_release "$PWD" "$label" ;;
    *)
        echo "usage: scripts/worktree-lease.sh claim|verify|release [caller-label]" >&2
        exit 2
        ;;
esac
