#!/usr/bin/env bash
# Bind a live-app automation run to ONE process, and refuse - loudly - every
# state in which the run would otherwise measure someone else.
#
# Source it; it defines functions and no side effects:
#
#     . "${ROOT}/scripts/lib/app-instance.sh"
#     app_instance_require_free                 # before launching
#     app_pid=$!                                # ...launch...
#     app_instance_bind "$NATIVE" "$app_pid"    # after `automate wait`
#     snap="$(app_instance_snapshot)"           # guarded on every read
#     app_instance_activate                     # pid-targeted, never by name
#
# WHY THIS EXISTS
# ---------------
# phux-cockpit-2ml.10. Live-app automation on this repo is serial-only, and
# both ways it goes wrong are SILENT - they do not produce an error, they
# produce a clean transcript about the wrong process.
#
#   1. ACTIVATION IS GLOBAL AND NAME-BASED. The only reliable way to front
#      this app is System Events (`tell application "Phux Cockpit" to
#      activate` does NOT reliably work for a bundle launched by its inner
#      binary), and System Events targets a process BY NAME. With two
#      instances live, `process "phux-cockpit"` resolves to an arbitrary one,
#      so a test needing OS focus measures a window it never launched and says
#      nothing about it.
#
#   2. INSTANCE CONFUSION IS NOT REPORTED. On 2026-08-12 one instance logged
#      `event=stop` and `window_closed` mid-sequence while a DIFFERENT pid
#      kept answering the dropbox. The snapshot came back with a `ready=true`
#      header and ZERO widget lines. Nothing failed. A test reads that as "the
#      UI vanished" - a product bug - when it actually means "you are talking
#      to a corpse". That is worse than a crash: it manufactures a finding.
#
# So the guard is three separate refusals, because they have three different
# causes and a reader who sees the wrong one goes looking in the wrong place:
# a second instance is live; the dropbox publisher is not our pid; the tree is
# empty. Each prints what it saw and what to do about it.
#
# WHY A SHARED FILE AND NOT A COPY PER SCRIPT
# --------------------------------------------
# There were three copies before this file, and they had already drifted:
# drive-shell-ceiling.sh and drive-backing-scale.sh each refused a
# pre-existing instance and each asserted publisher_pid ONCE at startup;
# automate-smoke.sh - the script most likely to be run by hand, mid-swarm -
# did neither. None of the three could catch the mid-sequence swap in (2),
# because none of them looked again after the first assertion. One definition
# means the next script gets the guard by sourcing it rather than by
# remembering to reimplement it.
#
# WHAT THIS DOES NOT DO
# ---------------------
# It does not make concurrent runs work. The automation dropbox is per-user,
# not per-process: a second bundle steals the channel no matter how carefully
# each side checks. This turns a silent wrong answer into a refusal, which is
# the whole of the available fix short of a per-instance dropbox.
#
# A locally built app run through scripts/dev-run.sh takes the process name
# `phux-cockpit-dev`, which removes the dev-versus-installed collision by
# construction (phux-cockpit-g2x). It does NOT remove the case this guard is
# for - two agents each driving their own bundle, both named `phux-cockpit`.
# Set PHUX_COCKPIT_PROCESS_NAME to point the guard at a differently-named
# build; the default is what `zig build package` produces.

# The executable name `pgrep -x` and System Events both match on. Overridable
# for a build staged under another identity (scripts/dev-run.sh).
APP_INSTANCE_NAME="${PHUX_COCKPIT_PROCESS_NAME:-phux-cockpit}"

# Set by app_instance_bind. Read by every guarded call after it.
APP_INSTANCE_PID=""
APP_INSTANCE_NATIVE=""

# Every live pid carrying our process name, one per line, newest last.
#
# `pgrep -x` and not `pgrep -f`: `-f` matches against the whole command line,
# which includes the command line of the shell running the pgrep itself, so it
# self-matches and reports a hit every single time. That is not a hypothetical
# - it is why a wait loop written with `-f` can never exit. `-x` matches the
# executable name, which cannot self-match from a shell.
app_instance_pids() {
    pgrep -x "$APP_INSTANCE_NAME" 2>/dev/null || true
}

# Decisions take their inputs as arguments rather than reading the machine.

# Refuse unless the set of live pids is empty. Used before launching.
# Arguments: the live pids (may be none).
app_instance_check_free() {
    [[ $# -eq 0 ]] && return 0
    printf 'REFUSING TO START: %s is already running (pid %s).\n' \
        "$APP_INSTANCE_NAME" "$*" >&2
    printf 'Live-app automation is serial-only (phux-cockpit-2ml.10): the automation\n' >&2
    printf 'dropbox is per-user, so a second bundle steals the channel, and System\n' >&2
    printf 'Events fronts whichever instance it likes.\n' >&2
    printf 'Wait for that run, or end it:  kill %s\n' "$*" >&2
    return 1
}

# Refuse unless EXACTLY the pid we launched is live. Used after launching and
# again mid-sequence: the failure this exists for appeared in the MIDDLE of a
# sequence that started clean, so checking only at startup cannot see it.
# Arguments: expected pid, then the live pids.
app_instance_check_only() {
    local expected="$1"; shift
    local -a live=("$@")

    if [[ ${#live[@]} -eq 0 ]]; then
        printf 'REFUSING: no %s process is alive, but this run is bound to pid %s.\n' \
            "$APP_INSTANCE_NAME" "$expected" >&2
        printf 'The app under test exited. Read its log before believing anything\n' >&2
        printf 'measured after this point.\n' >&2
        return 1
    fi
    if [[ ${#live[@]} -gt 1 ]]; then
        printf 'REFUSING: %d instances of %s are live (pids %s).\n' \
            "${#live[@]}" "$APP_INSTANCE_NAME" "${live[*]}" >&2
        printf 'This run is bound to %s, but activation targets a process BY NAME and\n' "$expected" >&2
        printf 'would front an arbitrary one of these. Any measurement that needs OS\n' >&2
        printf 'focus would silently describe the wrong window (phux-cockpit-2ml.10).\n' >&2
        return 1
    fi
    if [[ "${live[0]}" != "$expected" ]]; then
        printf 'REFUSING: the live %s is pid %s, not the pid this run launched (%s).\n' \
            "$APP_INSTANCE_NAME" "${live[0]}" "$expected" >&2
        printf 'Ours died and someone else took the name. Nothing measured from here\n' >&2
        printf 'describes the build under test.\n' >&2
        return 1
    fi
    return 0
}

# Refuse unless SNAPSHOT was published by EXPECTED pid and describes a UI that
# still exists. Arguments: expected pid, snapshot text.
#
# The three failures are kept apart on purpose. "No header" means the dropbox
# answered nothing; "wrong publisher" means another instance owns the channel;
# "empty tree" means the publisher is ours but its window is gone - and that
# last one is the case that used to be reported as a product bug, because an
# empty widget list is exactly what a healthy app with no UI would print.
app_instance_check_snapshot() {
    local expected="$1" snapshot="$2"
    local publisher windows views

    publisher="$(printf '%s\n' "$snapshot" | grep -o 'publisher_pid=[0-9]*' | head -1 | cut -d= -f2)"
    if [[ -z "$publisher" ]]; then
        printf 'REFUSING: the snapshot has no publisher_pid - it is not a snapshot.\n' >&2
        printf 'The automation dropbox published nothing readable. Do not treat the\n' >&2
        printf 'empty result as a statement about the UI.\n' >&2
        printf -- '--- what came back ---\n%s\n----------------------\n' "$snapshot" >&2
        return 1
    fi
    if [[ "$publisher" != "$expected" ]]; then
        printf 'REFUSING: the dropbox publisher is pid %s, not the pid this run launched (%s).\n' \
            "$publisher" "$expected" >&2
        printf 'A different instance is answering. Every assertion from here would be a\n' >&2
        printf 'perfectly clean transcript about the wrong binary (phux-cockpit-2ml.10).\n' >&2
        return 1
    fi

    # A `ready=true` header with nothing under it is the corpse. Windows and
    # views are the two things every live UI publishes (snapshot.zig writes
    # `window @wN ...` then indented `view @wN/... kind=...` lines), so zero of
    # either means the process is answering but has no UI to describe.
    windows="$(printf '%s\n' "$snapshot" | grep -c '^window @w' || true)"
    views="$(printf '%s\n' "$snapshot" | grep -c 'kind=[a-z_]* role=' || true)"
    if [[ "$windows" -eq 0 || "$views" -eq 0 ]]; then
        printf 'REFUSING: pid %s published a snapshot with %s windows and %s widgets.\n' \
            "$expected" "$windows" "$views" >&2
        printf 'This is NOT "the UI vanished" - it is an instance answering the dropbox\n' >&2
        printf 'with no UI to report, which on 2026-08-12 was an app that had already\n' >&2
        printf 'logged event=stop and window_closed. Read the app log before reading\n' >&2
        printf 'this as a product defect (phux-cockpit-2ml.10).\n' >&2
        printf -- '--- what came back ---\n%s\n----------------------\n' "$snapshot" >&2
        return 1
    fi
    return 0
}

# ---------------------------------------------------------------------------
# The machine-reading wrappers scripts actually call.
# ---------------------------------------------------------------------------

# Before launching: refuse if anything with our process name is already live.
app_instance_require_free() {
    local -a pids
    # `read -a` rather than word-splitting so an empty pgrep gives an empty
    # array instead of one empty-string element that reads as a live pid.
    IFS=$'\n' read -r -d '' -a pids < <(app_instance_pids && printf '\0') || true
    app_instance_check_free ${pids[@]+"${pids[@]}"}
}

# After launching and `automate wait`: bind this run to PID, and prove the
# binding holds right now. Everything after this point is guarded against it.
app_instance_bind() {
    APP_INSTANCE_NATIVE="${1:?the automation CLI path}"
    APP_INSTANCE_PID="${2:?the pid this run launched}"
    app_instance_assert || return 1
    printf 'bound: %s pid %s (sole instance, publishing)\n' \
        "$APP_INSTANCE_NAME" "$APP_INSTANCE_PID"
}

# Re-verify the binding. Cheap enough to call between steps, and the only way
# to catch the mid-sequence swap that startup checks cannot see.
app_instance_assert() {
    local -a pids
    IFS=$'\n' read -r -d '' -a pids < <(app_instance_pids && printf '\0') || true
    app_instance_check_only "$APP_INSTANCE_PID" ${pids[@]+"${pids[@]}"} || return 1
    app_instance_check_snapshot "$APP_INSTANCE_PID" \
        "$("$APP_INSTANCE_NATIVE" automate snapshot 2>/dev/null || true)" || return 1
}

# A snapshot that cannot come back empty or come back from the wrong process.
# Prints the snapshot on stdout, so it is a drop-in for `$NATIVE automate
# snapshot` and every existing `grep -o 'gpu_scale=...'` around it still works.
app_instance_snapshot() {
    local snapshot
    snapshot="$("$APP_INSTANCE_NATIVE" automate snapshot 2>/dev/null || true)"
    app_instance_check_snapshot "$APP_INSTANCE_PID" "$snapshot" || return 1
    printf '%s\n' "$snapshot"
}

# End PID and do not return until the process is actually gone.
#
# `kill` only REQUESTS termination. A cleanup trap that kills and returns
# leaves the app dying in the background, and because these runs are serial the
# very next script starts while it is still listed - so the guard above refuses
# a machine that is, a quarter of a second later, completely idle.
#
# That is not hypothetical: it was measured on 2026-08-12 running
# drive-shell-ceiling.sh straight after automate-smoke.sh. Smoke's trap killed
# without waiting, the ceiling script refused with "phux-cockpit is already
# running (pid 73200)", and by the time the pid was inspected it had exited.
# drive-shell-ceiling.sh had `wait` and automate-smoke.sh did not, which is the
# per-script drift this shared file exists to end.
#
# `wait` handles the common case where the pid is our own child; the poll after
# it covers a pid that is not (an app relaunched by something else), where
# `wait` returns immediately with an error.
app_instance_stop() {
    local pid="${1:-}" timeout_s="${2:-10}" deadline state
    [[ -n "$pid" ]] || return 0
    kill "$pid" 2>/dev/null || true
    deadline=$((SECONDS + timeout_s))
    while kill -0 "$pid" 2>/dev/null; do
        # A terminated child remains killable while it is a zombie; waiting for
        # kill -0 to fail before reaping it turns every clean exit into a false
        # timeout. Break to the unconditional wait below as soon as ps says Z.
        state="$(ps -o stat= -p "$pid" 2>/dev/null | tr -d ' ' || true)"
        [[ "$state" == Z* ]] && break
        if [[ "$SECONDS" -ge "$deadline" ]]; then
            printf 'warning: pid %s did not exit %ss after SIGTERM; sending SIGKILL.\n' \
                "$pid" "$timeout_s" >&2
            kill -9 "$pid" 2>/dev/null || true
            break
        fi
        sleep 0.2
    done
    # This is deliberately unconditional, including the SIGKILL timeout path.
    # Returning before wait leaves our child as a zombie and makes the next
    # serial automation run observe stale process state.
    wait "$pid" 2>/dev/null || true
}

# The unix id of whatever is frontmost right now, per System Events. Empty if
# System Events cannot answer at all.
app_instance_frontmost_pid() {
    osascript -e 'tell application "System Events" to get unix id of first process whose frontmost is true' 2>/dev/null || true
}

# Decide whether activation actually took. Pure; arguments are the pid we
# wanted fronted and the pid that is actually frontmost.
#
# THIS IS THE POINT OF THE ACTIVATION HELPER, and it is worth being explicit
# about why, because the bead assumed the opposite. Measured on this machine
# 2026-08-12, from a non-interactive shell:
#
#   osascript -e 'tell application "System Events" to set frontmost of
#                 process "phux-cockpit" to true'          -> exit 0, no effect
#   osascript -e 'tell application "System Events" to set frontmost of
#                 (first process whose unix id is N) to true' -> exit 0, no effect
#   osascript -e 'tell application "Finder" to activate'   -> exit 0, no effect
#
# Fronting FINDER fails the same way, so this is not a property of this app or
# of how its bundle was launched - it is the environment declining to let a
# background shell change the frontmost app, and osascript reporting success
# anyway. An exit code from osascript is therefore NOT evidence that a window
# was fronted, and any test that treated it as evidence measured whatever
# happened to be in front. Read the frontmost pid back and compare.
app_instance_check_frontmost() {
    local expected="$1" actual="$2"
    if [[ -z "$actual" ]]; then
        printf 'REFUSING: System Events would not say which process is frontmost.\n' >&2
        printf 'Activation cannot be confirmed, so do not run a focus-dependent\n' >&2
        printf 'measurement here.\n' >&2
        return 1
    fi
    if [[ "$actual" != "$expected" ]]; then
        printf 'REFUSING: asked to front pid %s, but pid %s is frontmost.\n' "$expected" "$actual" >&2
        printf 'osascript exits 0 whether or not activation took effect, so this is what\n' >&2
        printf 'a silent no-op looks like. If this machine denies the terminal the\n' >&2
        printf 'Accessibility grant (System Settings > Privacy & Security >\n' >&2
        printf 'Accessibility), no AppleScript form will front anything - including\n' >&2
        printf '"tell application ... to activate". Do NOT proceed with a measurement\n' >&2
        printf 'that needs OS focus (phux-cockpit-2ml.10).\n' >&2
        return 1
    fi
    return 0
}

# Front the bound instance's window, targeting it BY PID, and PROVE it worked.
#
# Targeting by unix id rather than by name is what makes this safe to call with
# siblings around: `process "phux-cockpit"` resolves to an arbitrary instance,
# which is failure mode (1) in the bead. The sole-instance check still runs
# first - if a second instance is live the run is compromised beyond
# activation, because the dropbox is shared too.
#
# Retried to a deadline because System Events does not see a process the
# instant `pgrep` does; it was measured several seconds late here.
app_instance_activate() {
    local deadline_s="${1:-15}"
    app_instance_assert || return 1
    local deadline=$((SECONDS + deadline_s)) front=""
    while :; do
        osascript -e "tell application \"System Events\" to set frontmost of (first process whose unix id is ${APP_INSTANCE_PID}) to true" >/dev/null 2>&1 || true
        front="$(app_instance_frontmost_pid)"
        [[ "$front" == "$APP_INSTANCE_PID" ]] && return 0
        if [[ "$SECONDS" -ge "$deadline" ]]; then
            app_instance_check_frontmost "$APP_INSTANCE_PID" "$front"
            return 1
        fi
        sleep 1
    done
}
