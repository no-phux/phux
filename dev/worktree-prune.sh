#!/usr/bin/env bash
# Reclaim disk from idle linked worktrees of this repository.
#
# Agent sessions leave worktrees behind, and each one keeps its build output.
# For every linked worktree that is idle (no file touched for --idle-hours, no
# live lease from dev/lib/worktree-owner.sh, no process working inside it,
# not locked):
#
#   - a clean worktree on a branch whose work has landed is removed with
#     `git worktree remove` (never --force) and its local branch deleted.
#     Landed means HEAD is an ancestor of the base ref, or merging it into the
#     base would change nothing, or GitHub finds a merged PR that contains
#     HEAD (a squash merge, under any branch name, pushed or not);
#   - any other idle worktree only loses its build output: `target` (mbx clean
#     for an mbx-managed link, rm for a real directory) and ignored
#     node_modules directories.
#
# Dirty worktrees, work that has not landed, and detached checkouts are
# reported and left alone; `git worktree lock` opts a
# worktree out entirely. The primary checkout and the caller's worktree are
# never touched. Dry run by default; --apply acts.
#
# Usage: dev/worktree-prune.sh [--apply] [--outputs-only] [--idle-hours N]
#   --apply         act instead of reporting
#   --outputs-only  never remove a worktree; only prune build output
#   --idle-hours N  minimum quiet time before a worktree counts as idle (6)
# Environment: PHUX_PRUNE_BASE (default origin/main), PHUX_WORKTREE_LEASE_SECONDS.
set -euo pipefail

# shellcheck source=lib/worktree-owner.sh
. "$(dirname -- "${BASH_SOURCE[0]}")/lib/worktree-owner.sh"

APPLY=0
REMOVE=1
IDLE_HOURS=6
BASE="${PHUX_PRUNE_BASE:-origin/main}"
LEASE_TTL="${PHUX_WORKTREE_LEASE_SECONDS:-21600}"
FREED_KB=0
KEPT=()

usage() { sed -n '/^# Usage:/,/^# Environment:/s/^# \{0,1\}//p' "${BASH_SOURCE[0]}"; }

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --apply) APPLY=1 ;;
            --outputs-only) REMOVE=0 ;;
            --idle-hours) IDLE_HOURS="${2:?--idle-hours needs a value}"; shift ;;
            -h | --help) usage; exit 0 ;;
            *) printf 'worktree-prune: unknown argument: %s\n' "$1" >&2; usage >&2; exit 2 ;;
        esac
        shift
    done
    [[ "$IDLE_HOURS" =~ ^[0-9]+$ ]] || { echo "worktree-prune: --idle-hours must be a whole number" >&2; exit 2; }
}

say() { printf '%-9s %s%s\n' "$1" "$2" "${3:+  ($3)}"; }
# Disk use in KiB without following links, except a `target` link, which is
# counted through to the mbx-managed directory it names.
size_kb() {
    local kb link=0
    kb="$(du -sk -- "$1" 2>/dev/null | awk '{print $1}')"
    if [[ -L "$1/target" ]]; then link="$(du -skL -- "$1/target" 2>/dev/null | awk '{print $1}')"; fi
    if [[ -L "$1" ]]; then kb="$(du -skL -- "$1" 2>/dev/null | awk '{print $1}')"; fi
    echo $((${kb:-0} + ${link:-0}))
}
human() { awk -v k="$1" 'BEGIN { printf "%.1fG", k / 1048576 }'; }

# Working directories of every process we can see, one per line.
process_cwds() {
    if command -v lsof >/dev/null 2>&1; then
        lsof -a -d cwd -Fn 2>/dev/null | sed -n 's/^n//p'
    elif [[ -d /proc ]]; then
        local link
        for link in /proc/[0-9]*/cwd; do readlink "$link" 2>/dev/null || true; done
    fi
}

has_process_inside() {
    local wt="$1" cwd
    while IFS= read -r cwd; do
        [[ "$cwd" == "$wt" || "$cwd" == "$wt"/* ]] && return 0
    done <<<"$CWDS"
    return 1
}

# A lease younger than its TTL means a session still owns the worktree.
has_live_lease() {
    local lease held_at
    lease="$(worktree_owner_lease "$1")" || return 1
    [[ -f "$lease" ]] || return 1
    held_at="$(worktree_owner_field "$lease" claimed_at)"
    [[ "$held_at" =~ ^[0-9]+$ ]] || return 1
    (($(date +%s) - held_at < LEASE_TTL))
}

touched_recently() {
    local wt="$1" minutes=$((IDLE_HOURS * 60)) git_dir
    git_dir="$(git -C "$wt" rev-parse --absolute-git-dir 2>/dev/null)" || return 0
    [[ -n "$(find "$wt" \( -name target -o -name node_modules \) -prune -o -mmin "-$minutes" -print -quit 2>/dev/null)" ]] && return 0
    # HEAD and its reflog move on commit, checkout and reset; the index is
    # left out because a plain `git status` (this script's own) rewrites it.
    [[ -n "$(find "$git_dir/HEAD" "$git_dir/logs/HEAD" -mmin "-$minutes" -print -quit 2>/dev/null)" ]]
}

# Why a worktree is busy, or nothing when it is idle.
busy_reason() {
    local wt="$1" locked="$2"
    if [[ "$locked" == 1 ]]; then echo "locked"
    elif has_process_inside "$wt"; then echo "a process is working inside it"
    elif has_live_lease "$wt"; then echo "an agent session holds its lease"
    elif touched_recently "$wt"; then echo "touched in the last ${IDLE_HOURS}h"
    fi
}

is_dirty() { [[ -n "$(git -C "$1" --no-optional-locks status --porcelain --untracked-files=normal 2>/dev/null)" ]]; }

# The merged PR that contains `head`, by number: GitHub's commit search finds
# it whatever the branch was called and whether this clone ever pushed it.
merged_pr_containing() {
    command -v gh >/dev/null 2>&1 || return 1
    local number
    number="$(gh pr list --state merged --search "$1" --json number --jq '.[0].number // empty' 2>/dev/null)" || return 1
    [[ -n "$number" ]] && echo "$number"
}

# How the worktree's work reached the base, or nothing when it has not.
# Detached checkouts (baselines, tooling) never qualify.
landed_how() {
    local wt="$1" branch="$2" head merged pr
    [[ -n "$branch" ]] || return 0
    head="$(git -C "$wt" rev-parse HEAD 2>/dev/null)" || return 0
    if git merge-base --is-ancestor "$head" "$BASE" 2>/dev/null; then echo "ancestor"; return; fi
    # A squash or cherry-pick already in the base leaves a merge with
    # nothing to add.
    merged="$(git merge-tree --write-tree "$BASE" "$head" 2>/dev/null | head -n 1)" || merged=""
    if [[ -n "$merged" && "$merged" == "$(git rev-parse "$BASE^{tree}")" ]]; then echo "already in $BASE"; return; fi
    if pr="$(merged_pr_containing "$head")"; then echo "PR #$pr"; fi
}

unpushed_count() { git -C "$1" rev-list --count HEAD --not --remotes 2>/dev/null || echo "?"; }

prune_target() {
    local wt="$1" kb
    [[ -e "$wt/target" ]] || return 0
    kb="$(size_kb "$wt/target")"
    if [[ -L "$wt/target" ]]; then
        command -v mbx >/dev/null 2>&1 || return 0
        say prune "$wt/target" "mbx-managed, $(human "$kb")"
        if [[ "$APPLY" == 1 ]]; then mbx clean "$wt" >/dev/null 2>&1 || true; fi
    else
        git -C "$wt" check-ignore -q target || return 0
        say prune "$wt/target" "$(human "$kb")"
        if [[ "$APPLY" == 1 ]]; then rm -rf -- "$wt/target"; fi
    fi
    FREED_KB=$((FREED_KB + kb))
}

prune_node_modules() {
    local wt="$1" dir kb
    while IFS= read -r dir; do
        git -C "$wt" check-ignore -q -- "$dir" || continue
        kb="$(size_kb "$dir")"
        say prune "$dir" "$(human "$kb")"
        if [[ "$APPLY" == 1 ]]; then rm -rf -- "$dir"; fi
        FREED_KB=$((FREED_KB + kb))
    done < <(find "$wt" -maxdepth 4 -name target -prune -o -name node_modules -type d -prune -print 2>/dev/null)
}

remove_worktree() {
    local wt="$1" branch="$2" how="$3" kb
    kb="$(size_kb "$wt")"
    say remove "$wt" "${branch:-detached}, landed ($how), $(human "$kb")"
    FREED_KB=$((FREED_KB + kb))
    [[ "$APPLY" == 1 ]] || return 0
    if command -v mbx >/dev/null 2>&1; then mbx clean --under "$wt" >/dev/null 2>&1 || true; fi
    git worktree remove "$wt" || { KEPT+=("$wt: git worktree remove failed"); return 0; }
    [[ -n "$branch" ]] || return 0
    # Landed by a squash or another branch, so not an ancestor: -D.
    if [[ "$how" == ancestor ]]; then git branch -q -d "$branch"; else git branch -q -D "$branch"; fi
}

keep_reason() {
    local wt="$1" branch="$2" unpushed
    if is_dirty "$wt"; then echo "uncommitted changes"; return; fi
    if [[ -z "$branch" ]]; then echo "detached checkout"; return; fi
    unpushed="$(unpushed_count "$wt")"
    if [[ "$unpushed" != 0 ]]; then echo "not landed, $unpushed unpushed commit(s)"; return; fi
    echo "not landed in $BASE or a merged PR"
}

handle_worktree() {
    local wt="$1" branch="$2" locked="$3" busy how
    busy="$(busy_reason "$wt" "$locked")"
    if [[ -n "$busy" ]]; then say skip "$wt" "$busy"; return; fi
    if [[ "$REMOVE" == 1 ]] && ! is_dirty "$wt"; then
        how="$(landed_how "$wt" "$branch")"
        if [[ -n "$how" ]]; then remove_worktree "$wt" "$branch" "$how"; return; fi
    fi
    [[ "$REMOVE" == 1 ]] && KEPT+=("$wt (${branch:-detached}): $(keep_reason "$wt" "$branch")")
    prune_target "$wt"
    prune_node_modules "$wt"
}

# Emit "path branch locked", split by the unit separator (\037): a detached
# entry has an empty branch, which a whitespace IFS would collapse.
linked_worktrees() {
    git worktree list --porcelain | awk '
        /^worktree / { if (p != "") print p "\037" b "\037" l; p = substr($0, 10); b = ""; l = 0; n++; if (n == 1) p = "" }
        /^branch refs\/heads\// { b = substr($0, 19) }
        /^locked/ { l = 1 }
        END { if (p != "") print p "\037" b "\037" l }'
}

main() {
    parse_args "$@"
    local self wt branch locked
    self="$(git rev-parse --show-toplevel)"
    CWDS="$(process_cwds)"
    [[ "$APPLY" == 1 ]] && git worktree prune
    while IFS=$'\037' read -r wt branch locked; do
        [[ "$wt" == "$self" ]] && { say skip "$wt" "this worktree"; continue; }
        [[ -d "$wt" ]] || continue
        handle_worktree "$wt" "$branch" "$locked"
    done < <(linked_worktrees)
    if [[ ${#KEPT[@]} -gt 0 ]]; then
        printf '\nKept for you to decide:\n'
        printf '  %s\n' "${KEPT[@]}"
    fi
    if [[ "$APPLY" == 1 ]]; then
        printf '\nFreed about %s.\n' "$(human "$FREED_KB")"
    else
        printf '\nDry run: would free about %s. Rerun with --apply.\n' "$(human "$FREED_KB")"
    fi
}

main "$@"
