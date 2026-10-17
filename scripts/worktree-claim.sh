#!/usr/bin/env bash
# Lease the current worktree to the running agent session, or refuse (exit 3)
# when another live session holds it. See scripts/lib/worktree-owner.sh.
# Usage: scripts/worktree-claim.sh [caller-label]
set -euo pipefail
# shellcheck source=lib/worktree-owner.sh
. "$(dirname -- "${BASH_SOURCE[0]}")/lib/worktree-owner.sh"
worktree_owner_claim "$PWD" "${1:-just}"
