#!/usr/bin/env bash
# Hermetic coverage for the per-worktree agent lease (worktree-owner.sh) and
# its wiring into zig-build.sh: a scratch repo with a linked worktree, a fake
# `zig` that records whether a build actually started, and every lease rule
# exercised against it.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/worktree-owner-test.XXXXXX")"
trap 'rm -rf -- "$WORK"' EXIT
FAILURES=0

ok() { printf '  ok: %s\n' "$*"; }
bad() { printf '  FAILED: %s\n' "$*" >&2; FAILURES=$((FAILURES + 1)); }

# A clean environment per call: only the identity variables a case sets.
run_as() {
    local owner="$1"; shift
    env -u CLAUDE_CODE_SESSION_ID -u OPENCODE_SESSION_ID -u CODEX_THREAD_ID \
        -u PHUX_WORKTREE_OWNER -u PHUX_WORKTREE_TAKEOVER -u GITHUB_ACTIONS \
        ${owner:+CLAUDE_CODE_SESSION_ID="$owner"} \
        PATH="${WORK}/bin:${PATH}" ZIG_LOG="${WORK}/zig.log" "$@"
}

# ------------------------------------------------------------- fixture
mkdir -p "${WORK}/bin"
cat >"${WORK}/bin/zig" <<'ZIG'
#!/bin/sh
printf 'zig %s\n' "$*" >>"$ZIG_LOG"
ZIG
chmod +x "${WORK}/bin/zig"

REPO="${WORK}/repo"
mkdir -p "${REPO}/clients/cockpit/scripts/lib"
cp "${ROOT}/scripts/zig-build.sh" "${REPO}/clients/cockpit/scripts/zig-build.sh"
cp "${ROOT}/scripts/lib/worktree-owner.sh" "${REPO}/clients/cockpit/scripts/lib/worktree-owner.sh"
git -C "$REPO" init -q -b main
git -C "$REPO" -c user.name=t -c user.email=t@t add -A
git -C "$REPO" -c user.name=t -c user.email=t@t commit -q -m fixture
git -C "$REPO" worktree add -q -b sibling "${WORK}/sibling"
COCKPIT="${REPO}/clients/cockpit"
SIBLING="${WORK}/sibling/clients/cockpit"
LEASE="$(git -C "$REPO" rev-parse --absolute-git-dir)/phux-worktree-owner"
SIBLING_LEASE="$(git -C "${WORK}/sibling" rev-parse --absolute-git-dir)/phux-worktree-owner"

build() {
    # build <owner> <cockpit dir> [env assignments...]
    local owner="$1" dir="$2"; shift 2
    : >"${WORK}/zig.log"
    (cd "$dir" && run_as "$owner" env "$@" PHUX_ZIG_BUILD_TIMEOUT=0 ./scripts/zig-build.sh test) >"${WORK}/out.log" 2>&1
}
built() { grep -q '^zig build' "${WORK}/zig.log"; }

printf '== worktree lease\n'

# A human shell (no session id) is never checked and leaves no lease.
if build "" "$COCKPIT" && built && [[ ! -e "$LEASE" ]]; then ok "no session id: builds, no lease"; else bad "no session id should build without a lease"; fi

# First agent claims the tree.
if build session-a "$COCKPIT" && built && grep -qx 'owner=session-a' "$LEASE"; then ok "first session claims the lease"; else bad "first session should claim"; fi

# The same agent keeps building.
if build session-a "$COCKPIT" && built; then ok "lease holder rebuilds"; else bad "lease holder should rebuild"; fi

# A second agent is refused before zig runs, and is told who holds it.
status=0; build session-b "$COCKPIT" || status=$?
if [[ "$status" == 3 ]] && ! built && grep -q 'held by:     session-a' "${WORK}/out.log"; then
    ok "another session is refused (exit 3) before zig starts"
else
    bad "another session should be refused before zig (status=$status)"; cat "${WORK}/out.log" >&2
fi
if grep -qx 'owner=session-a' "$LEASE"; then ok "refusal leaves the lease untouched"; else bad "refusal must not move the lease"; fi

# A sibling worktree has its own lease.
if build session-b "$SIBLING" && built && grep -qx 'owner=session-b' "$SIBLING_LEASE"; then ok "sibling worktree leases independently"; else bad "sibling worktree should lease independently"; fi

# Deliberate handoff.
if build session-b "$COCKPIT" PHUX_WORKTREE_TAKEOVER=1 && built && grep -qx 'owner=session-b' "$LEASE"; then ok "PHUX_WORKTREE_TAKEOVER=1 hands the lease over"; else bad "takeover should hand the lease over"; fi

# A stale lease is taken over with a notice.
sed -i.bak 's/^claimed_at=.*/claimed_at=1/' "$LEASE"
if build session-c "$COCKPIT" && built && grep -qx 'owner=session-c' "$LEASE" && grep -q 'taking over the worktree lease from session-b' "${WORK}/out.log"; then
    ok "stale lease is taken over with a notice"
else
    bad "stale lease should be taken over"
fi

# CI is never checked.
if build session-d "$COCKPIT" GITHUB_ACTIONS=true && built && grep -qx 'owner=session-c' "$LEASE"; then ok "CI ignores the lease"; else bad "CI should ignore the lease"; fi

# PHUX_WORKTREE_OWNER outranks the harness id.
status=0; build session-x "$COCKPIT" PHUX_WORKTREE_OWNER=session-c || status=$?
if [[ "$status" == 0 ]] && built; then ok "PHUX_WORKTREE_OWNER names the owner explicitly"; else bad "PHUX_WORKTREE_OWNER should win (status=$status)"; fi

if [[ "$FAILURES" -ne 0 ]]; then
    printf '%s worktree lease check(s) failed\n' "$FAILURES" >&2
    exit 1
fi
printf 'worktree lease: all checks passed\n'
