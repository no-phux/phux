# shellcheck shell=bash
# Per-worktree ownership lease for agent sessions.
#
# Two agent sessions sharing one git worktree produced builds and guard runs
# whose exit code, source-root line and cwd were all correct while the commit
# or the file contents underneath belonged to the other session
# (phux-cockpit-tnt, phux-cockpit-6w9). Documentation alone did not stop it,
# so the build and dev-run wrappers take a lease before touching the tree:
#
#   - The lease lives in the worktree's OWN git dir (`git rev-parse
#     --git-dir`), so sibling worktrees never share it and it is never tracked.
#   - The owner is the agent session id: PHUX_WORKTREE_OWNER if set, else
#     CLAUDE_CODE_SESSION_ID, else OPENCODE_SESSION_ID, else CODEX_THREAD_ID.
#     A plain human shell has none of these and is never checked.
#   - A live lease held by a different session refuses the run (exit 3). The
#     lease is refreshed on every claim and goes stale after
#     PHUX_WORKTREE_LEASE_SECONDS (default 6h); a stale lease is taken over
#     with a notice. PHUX_WORKTREE_TAKEOVER=1 takes over a live one, for a
#     deliberate handoff.
#   - CI (GITHUB_ACTIONS) has one checkout per job and is never checked.

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

# worktree_owner_claim <dir inside the worktree> <caller name>
worktree_owner_claim() {
    local dir="$1" caller="$2"
    local owner git_dir lease now held_owner held_at held_by age ttl
    [[ -z "${GITHUB_ACTIONS:-}" ]] || return 0
    owner="$(worktree_owner_id)" || return 0
    git_dir="$(git -C "$dir" rev-parse --absolute-git-dir 2>/dev/null)" || return 0
    lease="${git_dir}/phux-worktree-owner"
    now="$(date +%s)"
    ttl="${PHUX_WORKTREE_LEASE_SECONDS:-21600}"

    if [[ -f "$lease" ]]; then
        held_owner="$(sed -n 's/^owner=//p' "$lease" | head -1)"
        held_at="$(sed -n 's/^claimed_at=//p' "$lease" | head -1)"
        held_by="$(sed -n 's/^caller=//p' "$lease" | head -1)"
        [[ "$held_at" =~ ^[0-9]+$ ]] || held_at=0
        age=$((now - held_at))
        if [[ -n "$held_owner" && "$held_owner" != "$owner" ]]; then
            if [[ "$age" -lt "$ttl" && "${PHUX_WORKTREE_TAKEOVER:-0}" != "1" ]]; then
                printf '%s: REFUSING: this worktree is leased to another agent session.\n' "$caller" >&2
                printf '  worktree:    %s\n' "$(git -C "$dir" rev-parse --show-toplevel 2>/dev/null)" >&2
                printf '  held by:     %s (via %s, %ss ago)\n' "$held_owner" "${held_by:-?}" "$age" >&2
                printf '  you are:     %s\n' "$owner" >&2
                printf '  Work in your own worktree (git worktree add ...). For a deliberate\n' >&2
                printf '  handoff, rerun with PHUX_WORKTREE_TAKEOVER=1. Lease file: %s\n' "$lease" >&2
                return 3
            fi
            printf '%s: taking over the worktree lease from %s (%ss old)\n' "$caller" "$held_owner" "$age" >&2
        fi
    fi

    printf 'owner=%s\nclaimed_at=%s\ncaller=%s\n' "$owner" "$now" "$caller" >"${lease}.tmp.$$"
    mv -f "${lease}.tmp.$$" "$lease"
}
