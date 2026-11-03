#!/usr/bin/env bash
# One tick of the Rune upstream follower (scripts/rune-upstream/README.md).
# Keeps its phux session and worktree alive, then starts a fresh OpenCode run
# in that session unless the last run is still going or a steerer holds it.
# install.sh copies this into RUNE_UPSTREAM_HOME/bin and schedules it.
# Bash 3.2: launchd runs it with /bin/bash.
set -euo pipefail

home="${RUNE_UPSTREAM_HOME:?run scripts/rune-upstream/install.sh}"
repo="${RUNE_UPSTREAM_REPO:?run scripts/rune-upstream/install.sh}"
session="${RUNE_UPSTREAM_SESSION:-rune-upstream}"
max_run_hours="${RUNE_UPSTREAM_MAX_RUN_HOURS:-10}"
shell="${RUNE_UPSTREAM_SHELL:-${SHELL:-/bin/zsh}}"
wt="$home/phux"
run_tag=rune-upstream-run
hold_tag=hold

log() { printf '%s tick: %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*"; }

ensure_worktree() {
    [[ -e "$wt/.git" ]] && return 0
    if [[ -e "$wt" ]]; then
        log "$wt exists but is not a worktree; move it aside"
        return 1
    fi
    # A worktree deleted by hand stays registered until pruned.
    git -C "$repo" worktree prune
    git -C "$repo" fetch -q origin main
    git -C "$repo" worktree add -q --detach "$wt" origin/main
    log "created worktree $wt"
}

# A run ends parked on a clean, detached origin/main. A branch whose remote
# is gone was merged; any other branch, or a dirty tree, is a run's unfinished
# work, left for the next run to pick up.
refresh_worktree() {
    local branch
    git -C "$wt" fetch -q --prune origin
    [[ -z "$(git -C "$wt" status --porcelain)" ]] || return 0
    if branch="$(git -C "$wt" symbolic-ref -q --short HEAD)"; then
        [[ "$(git -C "$wt" for-each-ref --format='%(upstream:track)' "refs/heads/$branch")" == "[gone]" ]] || return 0
    fi
    if [[ "$(git -C "$wt" rev-parse HEAD)" != "$(git -C "$wt" rev-parse origin/main)" ]]; then
        git -C "$wt" switch -q --detach origin/main
    fi
}

# The session's panes and their tags; none when the session is gone. Any
# other failure stops the tick rather than mistaking a live session for empty.
session_panes() {
    local err out
    err="$(mktemp)"
    if out="$(phux tag ls --json "$session" 2>"$err")"; then
        rm -f "$err"
        printf '%s\n' "$out"
    elif grep -q '"no_such_target"' "$err"; then
        rm -f "$err"
        printf '{"terminals":[]}\n'
    else
        log "cannot list session $session: $(cat "$err")"
        rm -f "$err"
        return 1
    fi
}

tagged() {
    jq -r --arg t "$1" '[.terminals[] | select(.tags | index($t))] | last | .terminal // empty'
}

# Skip when a pane is held, or the run is busy and not yet presumed stuck.
should_skip() {
    local run="$1" held="$2" show state started age_h
    if [[ -n "$held" ]]; then
        log "$held is tagged #$hold_tag; skipping"
        return 0
    fi
    [[ -n "$run" ]] || return 1
    if ! show="$(phux agent show --json "$run")"; then
        log "cannot read the state of $run; skipping"
        return 0
    fi
    state="$(jq -r '.agents[0].state // "unknown"' <<<"$show")"
    case "$state" in
        working | blocked) ;;
        *) return 1 ;;
    esac
    started="$(cat "$home/run-started" 2>/dev/null || echo 0)"
    age_h=$((($(date +%s) - started) / 3600))
    if ((age_h < max_run_hours)); then
        log "run $run is $state after ${age_h}h; skipping"
        return 0
    fi
    log "run $run is still $state after ${age_h}h; replacing it"
    return 1
}

session_exists() {
    phux ls --json | jq -e --arg s "$session" 'any(.sessions[]; .name == $s)' >/dev/null
}

launch_run() {
    local target="$1" prompt cmd out id
    prompt="Scheduled rune-upstream tick $(date '+%Y-%m-%dT%H:%M%z'). You are the rune-upstream agent: do one run of .opencode/agents/rune-upstream.md."
    cmd=("$shell" -lic "exec opencode --auto --prompt $(printf '%q' "$prompt")")
    if [[ -n "$target" ]]; then
        out="$(phux spawn --json --target "$target" --cwd "$wt" -- "${cmd[@]}")"
    else
        out="$(phux new --json -s "$session" -c "$wt" -- "${cmd[@]}")"
    fi
    id="$(jq -r '.terminal_id // empty' <<<"$out")"
    [[ "$id" =~ ^[0-9]+$ ]] || { log "no pane id in: $out"; return 1; }
    # An untagged run would be invisible to the next tick, which would start a
    # second agent in the same worktree.
    if ! phux tag add "@$id" "$run_tag" >/dev/null; then
        phux kill --yes "@$id" >/dev/null || true
        log "could not tag @$id; closed it"
        return 1
    fi
    date +%s >"$home/run-started"
    log "started run @$id in session $session"
}

main() {
    local panes run held any
    phux server --ensure >/dev/null
    ensure_worktree

    panes="$(session_panes)"
    run="$(tagged "$run_tag" <<<"$panes")"
    held="$(tagged "$hold_tag" <<<"$panes")"
    if should_skip "$run" "$held"; then
        return 0
    fi

    # The old run goes before the worktree moves under it, and its lease with it.
    if [[ -n "$run" ]] && ! phux kill --yes "$run" >/dev/null; then
        log "could not close the old run $run; it may have exited"
    fi
    rm -f "$(git -C "$wt" rev-parse --absolute-git-dir)/phux-worktree-owner"
    refresh_worktree

    any="$(session_panes | jq -r '.terminals[0].terminal // empty')"
    if [[ -z "$any" ]] && session_exists; then
        # An empty persistent session has no pane to place a run beside.
        phux kill --yes "$session" >/dev/null
    fi
    launch_run "$any"
}

main "$@"
