#!/usr/bin/env bash
# Drop this session's worktree lease so the next session needs no takeover.
# See scripts/lib/worktree-owner.sh.
# Usage: scripts/worktree-release.sh [caller-label]
set -euo pipefail
# shellcheck source=lib/worktree-owner.sh
. "$(dirname -- "${BASH_SOURCE[0]}")/lib/worktree-owner.sh"
worktree_owner_release "$PWD" "${1:-just}"
