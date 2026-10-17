#!/usr/bin/env bash
# Hermetic coverage for the per-worktree agent lease (worktree-owner.sh), its
# entrypoint (worktree-claim.sh), and its wiring into the agent-facing just
# recipes: a scratch repo with a linked worktree, every lease rule exercised
# against it, then the real `worktree-claim` recipe run through just.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/worktree-owner-test.XXXXXX")"
WORK="$(cd "$WORK" && pwd -P)"
trap 'rm -rf -- "$WORK"' EXIT
FAILURES=0

ok() { printf '  ok: %s\n' "$*"; }
bad() { printf '  FAILED: %s\n' "$*" >&2; FAILURES=$((FAILURES + 1)); }

# A clean environment per call: only the identity variables a case sets.
run_as() {
    local owner="$1"; shift
    env -u CLAUDE_CODE_SESSION_ID -u OPENCODE_SESSION_ID -u CODEX_THREAD_ID \
        -u PHUX_WORKTREE_OWNER -u PHUX_WORKTREE_TAKEOVER -u GITHUB_ACTIONS \
        -u PHUX_WORKTREE_LEASE_SECONDS \
        ${owner:+CLAUDE_CODE_SESSION_ID="$owner"} "$@"
}

# ------------------------------------------------------------- fixture
REPO="${WORK}/repo"
mkdir -p "${REPO}/scripts/lib"
cp "${ROOT}/scripts/worktree-claim.sh" "${REPO}/scripts/worktree-claim.sh"
cp "${ROOT}/scripts/lib/worktree-owner.sh" "${REPO}/scripts/lib/worktree-owner.sh"
git -C "$REPO" init -q -b main
git -C "$REPO" -c user.name=t -c user.email=t@t add -A
git -C "$REPO" -c user.name=t -c user.email=t@t commit -q -m fixture
git -C "$REPO" worktree add -q -b sibling "${WORK}/sibling"
SIBLING="${WORK}/sibling"
LEASE="$(git -C "$REPO" rev-parse --absolute-git-dir)/phux-worktree-owner"
SIBLING_LEASE="$(git -C "$SIBLING" rev-parse --absolute-git-dir)/phux-worktree-owner"

claim() {
    # claim <owner> <worktree dir> [env assignments...]
    local owner="$1" dir="$2"; shift 2
    (cd "$dir" && run_as "$owner" env "$@" bash ./scripts/worktree-claim.sh test) >"${WORK}/out.log" 2>&1
}

printf '== worktree lease\n'

# A human shell (no session id) is never checked and leaves no lease.
if claim "" "$REPO" && [[ ! -e "$LEASE" ]]; then ok "no session id: passes, no lease"; else bad "no session id should pass without a lease"; fi

# First agent claims the tree.
if claim session-a "$REPO" && grep -qx 'owner=session-a' "$LEASE"; then ok "first session claims the lease"; else bad "first session should claim"; fi

# The same agent keeps working.
if claim session-a "$REPO"; then ok "lease holder reclaims"; else bad "lease holder should reclaim"; fi

# A second agent is refused, and is told who holds it.
status=0; claim session-b "$REPO" || status=$?
if [[ "$status" == 3 ]] && grep -q 'held by:     session-a' "${WORK}/out.log"; then
    ok "another session is refused (exit 3)"
else
    bad "another session should be refused (status=$status)"; cat "${WORK}/out.log" >&2
fi
if grep -qx 'owner=session-a' "$LEASE"; then ok "refusal leaves the lease untouched"; else bad "refusal must not move the lease"; fi

# Claiming from a subdirectory resolves the same worktree lease.
mkdir -p "${REPO}/scripts/lib/nested"
status=0; (cd "${REPO}/scripts/lib/nested" && run_as session-b bash ../../worktree-claim.sh test) >"${WORK}/out.log" 2>&1 || status=$?
if [[ "$status" == 3 ]]; then ok "a subdirectory shares the worktree lease"; else bad "a subdirectory should hit the same lease (status=$status)"; fi

# A sibling worktree has its own lease.
if claim session-b "$SIBLING" && grep -qx 'owner=session-b' "$SIBLING_LEASE" && grep -qx 'owner=session-a' "$LEASE"; then
    ok "sibling worktree leases independently"
else
    bad "sibling worktree should lease independently"
fi

# Deliberate handoff.
if claim session-b "$REPO" PHUX_WORKTREE_TAKEOVER=1 && grep -qx 'owner=session-b' "$LEASE"; then ok "PHUX_WORKTREE_TAKEOVER=1 hands the lease over"; else bad "takeover should hand the lease over"; fi

# A stale lease is taken over with a notice.
sed -i.bak 's/^claimed_at=.*/claimed_at=1/' "$LEASE"
if claim session-c "$REPO" && grep -qx 'owner=session-c' "$LEASE" && grep -q 'taking over the worktree lease from session-b' "${WORK}/out.log"; then
    ok "stale lease is taken over with a notice"
else
    bad "stale lease should be taken over"; cat "${WORK}/out.log" >&2
fi

# CI is never checked and never moves the lease.
if claim session-d "$REPO" GITHUB_ACTIONS=true && grep -qx 'owner=session-c' "$LEASE"; then ok "CI ignores the lease"; else bad "CI should ignore the lease"; fi

# PHUX_WORKTREE_OWNER outranks the harness id.
status=0; claim session-x "$REPO" PHUX_WORKTREE_OWNER=session-c || status=$?
if [[ "$status" == 0 ]]; then ok "PHUX_WORKTREE_OWNER names the owner explicitly"; else bad "PHUX_WORKTREE_OWNER should win (status=$status)"; fi

# Other harness ids count as sessions too.
status=0; claim "" "$REPO" CODEX_THREAD_ID=thread-z || status=$?
if [[ "$status" == 3 ]]; then ok "CODEX_THREAD_ID is a session id"; else bad "CODEX_THREAD_ID should be refused (status=$status)"; fi

# ------------------------------------------------------- just wiring
printf '== just wiring\n'
if ! command -v just >/dev/null 2>&1; then
    bad "just is required to check the recipe wiring"
else
    for recipe in ci check test install-dev desktop-test desktop-app; do
        if just --justfile "${ROOT}/justfile" --dump --dump-format json \
            | jq -e --arg r "$recipe" '.recipes[$r].dependencies | any(.recipe == "worktree-claim")' >/dev/null; then
            ok "just ${recipe} claims the worktree first"
        else
            bad "just ${recipe} must depend on worktree-claim"
        fi
    done

    # The real recipe, run in the scratch worktree: refusal surfaces as exit 3.
    status=0
    run_as session-y just --justfile "${ROOT}/justfile" --working-directory "$REPO" worktree-claim >"${WORK}/out.log" 2>&1 || status=$?
    if [[ "$status" == 3 ]] && grep -q 'REFUSING' "${WORK}/out.log"; then
        ok "just worktree-claim refuses a foreign session with exit 3"
    else
        bad "just worktree-claim should refuse with exit 3 (status=$status)"; cat "${WORK}/out.log" >&2
    fi
    if run_as session-c just --justfile "${ROOT}/justfile" --working-directory "$REPO" worktree-claim >"${WORK}/out.log" 2>&1; then
        ok "just worktree-claim passes for the holder"
    else
        bad "just worktree-claim should pass for the holder"; cat "${WORK}/out.log" >&2
    fi
fi

if [[ "$FAILURES" -ne 0 ]]; then
    printf '%s worktree lease check(s) failed\n' "$FAILURES" >&2
    exit 1
fi
printf 'worktree lease: all checks passed\n'
