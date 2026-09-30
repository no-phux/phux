#!/usr/bin/env bash
# PID-bound presentation and navigation journey over the isolated dev bundle.
# It never opens, activates, inspects, or kills /Applications/Phux Cockpit.app.
# --shots DIR saves reference-renderer PNGs of each navigation step there.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
export PHUX_COCKPIT_PROCESS_NAME=phux-cockpit-dev
# shellcheck source=scripts/lib/app-instance.sh disable=SC1091
. "${ROOT}/scripts/lib/app-instance.sh"

EXPECT_KNOWN_RED=0
KEEP=0
NO_BUILD=0
SHOTS=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --expect-known-red) EXPECT_KNOWN_RED=1 ;;
        --keep) KEEP=1 ;;
        --no-build) NO_BUILD=1 ;;
        --shots) shift; SHOTS="${1:?--shots needs a directory}" ;;
        -h|--help)
            sed -n '2,5p' "$0"
            printf 'usage: %s [--expect-known-red] [--no-build] [--keep] [--shots DIR]\n' "$0"
            exit 0
            ;;
        *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
    shift
done

WORK="$(mktemp -d "${TMPDIR:-/private/tmp}/cockpit-presentation.XXXXXX")"
DEV_HOME="${WORK}/dev-home"
launch_log="${WORK}/dev-run.log"
APP_PID=""
STOPPED_PID=""
CLEANUP_DONE=0

reported_pid() {
    [[ -f "$launch_log" ]] || return 0
    sed -n 's/^pid \([0-9][0-9]*\), log .*/\1/p' "$launch_log" | tail -1
}

stop_owned_pid() {
    local pid="$1"
    [[ "$pid" =~ ^[0-9]+$ && "$pid" != "$STOPPED_PID" ]] || return 0
    if app_instance_pids | grep -Fxq "$pid"; then
        app_instance_stop "$pid"
        STOPPED_PID="$pid"
    fi
}

# shellcheck disable=SC2329 # invoked by the EXIT trap below
cleanup() {
    local status=$? pid="$APP_PID"
    (( CLEANUP_DONE == 0 )) || return "$status"
    CLEANUP_DONE=1
    trap - INT TERM EXIT
    if [[ -z "$pid" ]]; then
        pid="$(reported_pid)"
    fi
    if [[ "$KEEP" == 0 ]]; then
        stop_owned_pid "$pid"
    fi
    if [[ "$KEEP" == 0 ]]; then
        rm -rf -- "$WORK"
    else
        printf 'retained isolated run: %s (pid %s)\n' "$WORK" "$pid"
    fi
    return "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# The dev executable has a distinct process name and bundle id. The private
# cwd supplies its own automation dropbox; config and workspace state also live
# under WORK. A concurrently installed app named phux-cockpit is irrelevant.
app_instance_require_free
run_args=(--debug --automation --fresh --detach)
(( NO_BUILD == 0 )) || run_args+=(--no-build)
set +e
# The detached app inherits the launcher's stdout. A pipe to tee would stay
# open for the app's entire lifetime, preventing us from ever driving it.
PHUX_COCKPIT_DEV_HOME="$DEV_HOME" "${ROOT}/scripts/dev-run.sh" "${run_args[@]}" >"$launch_log" 2>&1
launch_status=$?
set -e
cat "$launch_log"
APP_PID="$(reported_pid)"
if (( launch_status != 0 )); then
    stop_owned_pid "$APP_PID"
    printf 'FAILED: dev-run exited %s; transcript: %s\n' \
        "$launch_status" "$launch_log" >&2
    exit 1
fi
if [[ ! "$APP_PID" =~ ^[0-9]+$ ]]; then
    printf 'FAILED: dev-run did not report a usable pid; transcript: %s\n' "$launch_log" >&2
    exit 1
fi

NATIVE="$("${ROOT}/scripts/build-automation-cli.sh")"
(cd "$DEV_HOME" && "$NATIVE" automate wait >/dev/null)
(cd "$DEV_HOME" && "$NATIVE" automate assert --timeout-ms 30000 \
    "publisher_pid=${APP_PID}" 'markup_watch=armed' 'ready=true' >/dev/null)
# app-instance.sh invokes its native command directly. Keep those guarded reads
# in this run's private dropbox without changing the harness process's cwd.
# shellcheck disable=SC2329 # invoked indirectly by app-instance.sh
app_native() {
    (cd "$DEV_HOME" && "$NATIVE" "$@")
}
app_instance_bind app_native "$APP_PID"

snapshot="$(app_instance_snapshot)"
if ! grep -q "publisher_pid=${APP_PID} .*markup_watch=armed" <<<"$snapshot"; then
    printf 'FAILED: Debug dev pid %s is not the armed automation publisher.\n' "$APP_PID" >&2
    printf 'Expected publisher_pid=%s and markup_watch=armed in the same header.\n' "$APP_PID" >&2
    exit 1
fi
printf '  ok: pid %s exclusively owns an armed markup watcher\n' "$APP_PID"

widget() {
    local role="$1" name="$2" snapshot line count
    snapshot="$(app_instance_snapshot)" || return 1
    count="$(grep -Fc "role=${role} name=\"${name}\"" <<<"$snapshot" || true)"
    if [[ "$count" != 1 ]]; then
        printf 'FAILED: expected one role=%s name=%q, found %s.\n' "$role" "$name" "$count" >&2
        return 1
    fi
    line="$(grep -F "role=${role} name=\"${name}\"" <<<"$snapshot")"
    if [[ "$line" =~ @w[0-9]+/([^#[:space:]]+)#([0-9]+) ]]; then
        printf '%s %s\n' "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}"
        return 0
    fi
    printf 'FAILED: could not parse widget identity from: %s\n' "$line" >&2
    return 1
}

click_named() {
    local role="$1" name="$2" view id
    local target
    target="$(widget "$role" "$name")" || return 1
    read -r view id <<<"$target"
    app_instance_assert
    (cd "$DEV_HOME" && "$NATIVE" automate widget-click "$view" "$id" >/dev/null)
}

(cd "$DEV_HOME" && "$NATIVE" automate assert --absent 'name="Agent inspection details"' >/dev/null)
click_named button 'Workspace actions'
click_named menuitem 'Inspect agents'
(cd "$DEV_HOME" && "$NATIVE" automate assert --timeout-ms 5000 'name="Agent inspection details"' >/dev/null)
printf '  ok: Agents opens the inspector\n'

# A horizontal drag finishes with the automation pointer parked over the
# shipping scroll surface. Hovered state proves only that the pointer reached
# the real control; rendered rest/hover fill equality is the deterministic
# semantic_theme Zig recipe contract named by cockpit-state-inventory.mjs.
passive_snapshot="${WORK}/passive-hover.snapshot"
passive_target="$(widget group 'Agent inspection details')"
read -r passive_view passive_id <<<"$passive_target"
(cd "$DEV_HOME" && "$NATIVE" automate widget-drag "$passive_view" "$passive_id" 0.25 0.75 0.5 0.5 >/dev/null)
app_instance_snapshot >"$passive_snapshot"
node "${ROOT}/scripts/cockpit-state-inventory.mjs" --check-pointer-target "$passive_snapshot" \
    --target-role group --target-name 'Agent inspection details'

# Negative control: the navigator search is not present while Agents owns the
# slot. Invoke the shipping menu command rather than clicking the toolbar button
# behind the modal sheet: pointer hit-testing correctly gives that click to the
# sheet's light-dismiss layer, so it cannot prove a Sessions transition.
(cd "$DEV_HOME" && "$NATIVE" automate assert --absent 'name="Search navigator"' >/dev/null)
(cd "$DEV_HOME" && "$NATIVE" automate native-command navigator.sessions "$passive_view" >/dev/null)
if (cd "$DEV_HOME" && "$NATIVE" automate assert --timeout-ms 5000 \
    'name="Search navigator"' >/dev/null); then
    :
else
    (cd "$DEV_HOME" && "$NATIVE" automate assert --timeout-ms 5000 \
        'name="Agent inspection details"' >/dev/null)
fi
snapshot="$(app_instance_snapshot)"

transition_red=0
if grep -q 'name="Search navigator"' <<<"$snapshot" \
    && ! grep -q 'name="Agent inspection details"' <<<"$snapshot"; then
    :
elif grep -q 'name="Agent inspection details"' <<<"$snapshot" \
    && ! grep -q 'name="Search navigator"' <<<"$snapshot"; then
    transition_red=1
else
    printf 'FAILED: Agents -> Sessions reached neither the accepted nor known-red presentation.\n' >&2
    printf 'Expected exactly one of Search navigator and Agent inspection details.\n' >&2
    exit 1
fi

if [[ "$EXPECT_KNOWN_RED" == 1 ]]; then
    if [[ "$transition_red" == 1 ]]; then
        printf 'EXPECTED FAILURE: Sessions retained the Agents inspector and projected no navigator.\n'
        printf 'Strict gate: %s\n' "$0"
        exit 0
    fi
    printf 'UNEXPECTED PASS: Agents -> Sessions now replaces inspector.\n' >&2
    printf 'Remove --expect-known-red and promote the strict integration gate.\n' >&2
    exit 1
fi

if [[ "$transition_red" == 1 ]]; then
    printf 'FAILED: Agents -> Sessions did not replace inspector in pid %s.\n' "$APP_PID" >&2
    printf 'Expected Search navigator present and Agent inspection details absent.\n' >&2
fi
(( transition_red == 0 ))
printf 'PASS: live Agents -> Sessions replacement holds in pid %s\n' "$APP_PID"

# ---------------------------------------------------------------- navigation
# Where am I, what is open, and where are my machines: each step asserts the
# destination absent, acts through visible chrome, then asserts it present.

automate() {
    (cd "$DEV_HOME" && "$NATIVE" automate "$@")
}

shot() {
    [[ -n "$SHOTS" ]] || return 0
    local view="$1" name="$2"
    mkdir -p -- "$SHOTS"
    automate screenshot "$view" >/dev/null
    cp -f -- "${DEV_HOME}/.zig-cache/native-sdk-automation/screenshot-${view}.png" "${SHOTS}/${name}.png"
}

# Click the one widget with ROLE and NAME inside OS window W (w1, w2, ...).
click_in() {
    local window="$1" view="$2" role="$3" name="$4" snapshot matches count
    snapshot="$(app_instance_snapshot)" || return 1
    matches="$(grep -F "@${window}/" <<<"$snapshot" | grep -F "role=${role} name=\"${name}\"" || true)"
    count="$(grep -c . <<<"$matches" || true)"
    if [[ "$count" != 1 ]]; then
        printf 'FAILED: expected one role=%s name=%q in %s, found %s.\n' "$role" "$name" "$window" "$count" >&2
        return 1
    fi
    if [[ ! "$matches" =~ \#([0-9]+)\ role= ]]; then
        printf 'FAILED: could not parse widget identity from: %s\n' "$matches" >&2
        return 1
    fi
    app_instance_assert
    automate widget-click "$view" "${BASH_REMATCH[1]}" >/dev/null
}

# Leave the Sessions navigator from the step above.
click_in w1 phux-cockpit-canvas button 'Close navigator'
automate assert --absent --timeout-ms 5000 'role=dialog name="Navigator"' >/dev/null

# 1. The header names this window's session and machine at a fixed width.
automate assert --timeout-ms 5000 \
    '@w1/[^ ]* role=button name="Session and machine" bounds=\([0-9.]+,[0-9.]+ 216x32\)' >/dev/null
shot phux-cockpit-canvas nav-01-header
printf '  ok: header location trigger names session and machine\n'

# 2. Its menu reaches Machines and Sessions without shortcut knowledge.
automate assert --absent 'role=menuitem name="Machines…"' >/dev/null
click_in w1 phux-cockpit-canvas button 'Session and machine'
automate assert --timeout-ms 5000 'role=menuitem name="Machines…"' \
    'role=menuitem name="Sessions…"' 'role=menuitem name="Connect to Host…"' >/dev/null
shot phux-cockpit-canvas nav-02-location-menu
click_in w1 phux-cockpit-canvas menuitem 'Machines…'
automate assert --timeout-ms 5000 'role=tab name="Machines" [^\n]*state=\[selected\]' \
    'role=button name="Add Machine…"' >/dev/null
shot phux-cockpit-canvas nav-03-machines
printf '  ok: location menu -> Machines lists saved machines with Add Machine\n'
click_in w1 phux-cockpit-canvas button 'Close navigator'
automate assert --absent --timeout-ms 5000 'role=dialog name="Navigator"' >/dev/null

# 3. A second window gets a distinguishable OS title.
automate assert --absent '^window @w2 ' >/dev/null
automate native-command window.new phux-cockpit-canvas >/dev/null
automate assert --timeout-ms 10000 '^window @w2 "Phux Cockpit — Window 2"' >/dev/null
printf '  ok: New Window is titled "Phux Cockpit — Window 2"\n'

# 4. Workspace actions -> Show all windows lists both windows as a hierarchy,
#    with this window and its shown tab marked current.
automate assert --timeout-ms 10000 '@w2/[^ ]* role=button name="Workspace actions"' >/dev/null
click_in w2 phux-cockpit-canvas-1 button 'Workspace actions'
click_in w2 phux-cockpit-canvas-1 menuitem 'Show all windows…'
automate assert --timeout-ms 5000 '@w2/[^ ]* role=tab name="Windows" [^\n]*state=\[selected\]' \
    '@w2/[^ ]* role=listitem name="Window 1"' '@w2/[^ ]* role=listitem name="Window 2"' \
    '@w2/[^ ]* role=text name="Tab 1 of 1 · Window 2 · ' >/dev/null
current="$(app_instance_snapshot | grep -F '@w2/' | grep -Fc 'role=text name="Current"' || true)"
if [[ "$current" != 2 ]]; then
    printf 'FAILED: expected Window 2 and its shown tab marked Current, found %s marks.\n' "$current" >&2
    exit 1
fi
shot phux-cockpit-canvas-1 nav-04-windows
printf '  ok: Show all windows lists both windows; Window 2 and its tab are current\n'

# 5. Choosing Window 1 brings existing work forward without a new window.
click_in w2 phux-cockpit-canvas-1 listitem 'Window 1'
automate assert --absent --timeout-ms 5000 '@w2/[^ ]* role=dialog name="Navigator"' >/dev/null
automate assert --absent '^window @w3 ' >/dev/null
printf '  ok: activating Window 1 closes the overview and opens nothing new\n'
printf 'PASS: live navigation journey holds in pid %s\n' "$APP_PID"
