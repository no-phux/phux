# shellcheck shell=bash
# Per-worktree ownership lease for agent sessions.
#
# Two agent sessions sharing one git worktree produced builds and test runs
# whose exit code, source-root line and cwd were all correct while the commit
# or the file contents underneath belonged to the other session
# (phux-cockpit-tnt). Documentation alone did not stop it, so every just recipe
# that builds, tests or formats the tree takes a lease through
# scripts/worktree-claim.sh first (scripts/lib/worktree-owner_test.sh holds the
# wired list and fails when one loses its claim):
#
#   - The lease lives in the worktree's OWN git dir (`git rev-parse
#     --absolute-git-dir`), so sibling worktrees never share it and it is
#     never tracked.
#   - The owner is the agent session id: PHUX_WORKTREE_OWNER if set, else
#     CLAUDE_CODE_SESSION_ID, else OPENCODE_SESSION_ID, else CODEX_THREAD_ID.
#     A plain human shell has none of these and is never checked.
#   - A live lease held by a different session refuses the run (exit 3). The
#     lease is refreshed on every claim and goes stale after
#     PHUX_WORKTREE_LEASE_SECONDS (default 6h); a stale lease is taken over
#     with a notice. `just worktree-release` drops the caller's own lease when
#     a session hands the worktree on; PHUX_WORKTREE_TAKEOVER=1 takes over a
#     live one, for a deliberate handoff the previous session did not release.
#   - The claim records HEAD. The long runs (`test`, `e2e`) re-check it
#     after success through scripts/worktree-verify.sh and fail (exit 3) when
#     HEAD moved or the lease changed hands mid-run, because their green no
#     longer describes one commit. This is detection, not prevention: a
#     session that edits files or switches branches without running a guarded
#     recipe is never stopped, and uncommitted edits under a run are not seen.
#   - CI (GITHUB_ACTIONS) has one checkout per job and is never checked.

# worktree_owner_id: print the running agent session id, or fail for a human.
worktree_owner_id() {
    local id
    for id in "${PHUX_WORKTREE_OWNER:-}" "${CLAUDE_CODE_SESSION_ID:-}" "${OPENCODE_SESSION_ID:-}" "${CODEX_THREAD_ID:-}"; do
        if [[ -n "$id" ]]; then
            printf '%s\n' "$id"
            return 0
        fi
    done
    return 1
}

# worktree_owner_lease <dir inside the worktree>: print the lease file path.
worktree_owner_lease() {
    local git_dir
    git_dir="$(git -C "$1" rev-parse --absolute-git-dir 2>/dev/null)" || return 1
    printf '%s/phux-worktree-owner\n' "$git_dir"
}

# worktree_owner_field <lease file> <key>
worktree_owner_field() {
    sed -n "s/^$2=//p" "$1" | head -1
}

# worktree_owner_head <dir inside the worktree>: HEAD's commit, or empty.
worktree_owner_head() {
    git -C "$1" rev-parse -q --verify HEAD 2>/dev/null || true
}

# worktree_owner_claim <dir inside the worktree> <caller name>
worktree_owner_claim() {
    local dir="$1" caller="$2"
    local owner lease now held_owner held_at held_by age ttl
    [[ -z "${GITHUB_ACTIONS:-}" ]] || return 0
    owner="$(worktree_owner_id)" || return 0
    lease="$(worktree_owner_lease "$dir")" || return 0
    now="$(date +%s)"
    ttl="${PHUX_WORKTREE_LEASE_SECONDS:-21600}"

    if [[ -f "$lease" ]]; then
        held_owner="$(worktree_owner_field "$lease" owner)"
        held_at="$(worktree_owner_field "$lease" claimed_at)"
        held_by="$(worktree_owner_field "$lease" caller)"
        [[ "$held_at" =~ ^[0-9]+$ ]] || held_at=0
        age=$((now - held_at))
        if [[ -n "$held_owner" && "$held_owner" != "$owner" ]]; then
            if [[ "$age" -lt "$ttl" && "${PHUX_WORKTREE_TAKEOVER:-0}" != "1" ]]; then
                printf '%s: REFUSING: this worktree is leased to another agent session.\n' "$caller" >&2
                printf '  worktree:    %s\n' "$(git -C "$dir" rev-parse --show-toplevel 2>/dev/null)" >&2
                printf '  held by:     %s (via %s, %ss ago)\n' "$held_owner" "${held_by:-?}" "$age" >&2
                printf '  you are:     %s\n' "$owner" >&2
                printf '  Work in your own worktree (git worktree add ...). For a deliberate\n' >&2
                printf '  handoff, the holder runs just worktree-release, or rerun with\n' >&2
                printf '  PHUX_WORKTREE_TAKEOVER=1. Lease file: %s\n' "$lease" >&2
                return 3
            fi
            printf '%s: taking over the worktree lease from %s (%ss old)\n' "$caller" "$held_owner" "$age" >&2
        fi
    fi

    printf 'owner=%s\nclaimed_at=%s\ncaller=%s\nhead=%s\n' \
        "$owner" "$now" "$caller" "$(worktree_owner_head "$dir")" >"${lease}.tmp.$$"
    mv -f "${lease}.tmp.$$" "$lease"
}

# worktree_owner_verify <dir inside the worktree> <caller name>
# After a long run: fail (exit 3) when the lease changed hands or HEAD moved
# since this session's claim, so a green that straddled two commits is not
# reported as one.
worktree_owner_verify() {
    local dir="$1" caller="$2"
    local owner lease held_owner held_head head
    [[ -z "${GITHUB_ACTIONS:-}" ]] || return 0
    owner="$(worktree_owner_id)" || return 0
    lease="$(worktree_owner_lease "$dir")" || return 0
    if [[ ! -f "$lease" ]]; then
        printf '%s: REFUSING the result: the worktree lease was released during the run.\n' "$caller" >&2
        return 3
    fi
    held_owner="$(worktree_owner_field "$lease" owner)"
    if [[ "$held_owner" != "$owner" ]]; then
        printf '%s: REFUSING the result: session %s took this worktree over during the run (you are %s).\n' \
            "$caller" "$held_owner" "$owner" >&2
        return 3
    fi
    held_head="$(worktree_owner_field "$lease" head)"
    head="$(worktree_owner_head "$dir")"
    if [[ "$held_head" != "$head" ]]; then
        printf '%s: REFUSING the result: HEAD moved from %s to %s during the run,\n' \
            "$caller" "${held_head:-none}" "${head:-none}" >&2
        printf '  so it does not describe one commit. Rerun on a quiet worktree.\n' >&2
        return 3
    fi
}

# worktree_owner_release <dir inside the worktree> <caller name>
# Drop the lease when the running session holds it, so the next session in
# this worktree needs no takeover. Another session's lease is left in place.
worktree_owner_release() {
    local dir="$1" caller="$2"
    local owner lease held_owner
    owner="$(worktree_owner_id)" || return 0
    lease="$(worktree_owner_lease "$dir")" || return 0
    [[ -f "$lease" ]] || return 0
    held_owner="$(worktree_owner_field "$lease" owner)"
    if [[ "$held_owner" != "$owner" ]]; then
        printf '%s: the lease is held by %s, not this session (%s); left in place.\n' \
            "$caller" "$held_owner" "$owner" >&2
        return 3
    fi
    rm -f -- "$lease"
    printf '%s: released the worktree lease.\n' "$caller" >&2
}
