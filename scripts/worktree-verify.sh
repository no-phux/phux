#!/usr/bin/env bash
# After a long run, fail (exit 3) when HEAD moved or another session took the
# worktree over since the claim. See scripts/lib/worktree-owner.sh.
# Usage: scripts/worktree-verify.sh [caller-label]
set -euo pipefail
# shellcheck source=lib/worktree-owner.sh
. "$(dirname -- "${BASH_SOURCE[0]}")/lib/worktree-owner.sh"
worktree_owner_verify "$PWD" "${1:-just}"
