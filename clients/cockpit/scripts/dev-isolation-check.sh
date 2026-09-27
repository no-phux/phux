#!/usr/bin/env bash
# Run the installed Phux Cockpit and a local build at the same time and prove
# macOS, `pgrep`, System Events and config/layout files cannot confuse them.
#
#   ./scripts/dev-isolation-check.sh              # build, then check
#   ./scripts/dev-isolation-check.sh --no-build   # check what is already built
#
# Every assertion has a negative control: the identity comparison must first
# report a clash on the unstaged bundle, and the config check is paired with a
# run lacking PHUX_COCKPIT_CONFIG/STATE that must reach the "real" config (a
# fake HOME/XDG under scratch). Your actual config and state are fingerprinted
# and must not move. Serial only (refuses if any instance is running).
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
. "${ROOT}/scripts/lib/dev-app.sh"

WORK="${TMPDIR:-/tmp}/phux-cockpit-isolation.$$"
STAGED_APP="${WORK}/staged/Phux Cockpit (dev).app"
PACKAGED_APP="${ROOT}/zig-out/package/phux-cockpit.app"
FAKE_HOME="${WORK}/home"
PIDS=()
FAILURES=0
BUILD=1

# --no-build: check the existing bundle; every property checked belongs to the
# bundle, not the source.
for arg in "$@"; do
    case "$arg" in
        --no-build) BUILD=0 ;;
        -h|--help) sed -n '2,/^set -euo pipefail/{ /^set -euo pipefail/!p; }' "$0"; exit 0 ;;
        *) printf 'unknown argument: %s\n' "$arg" >&2; exit 2 ;;
    esac
done

cleanup() {
    local pid
    for pid in ${PIDS+"${PIDS[@]}"}; do kill "$pid" 2>/dev/null || true; done
    # The marker shells outlive their app when the app is killed rather than
    # asked to quit, and they sleep for five minutes. Matching on the work
    # directory is exact enough to be safe and cannot match this script, whose
    # own argv is a path under scripts/.
    pkill -f "$WORK" 2>/dev/null || true
    rm -rf -- "$WORK"
}
trap cleanup EXIT

ok() { printf '  ok: %s\n' "$*"; }
bad() { printf '  FAILED: %s\n' "$*" >&2; FAILURES=$((FAILURES + 1)); }
section() { printf '\n== %s\n' "$*"; }

# Fingerprint a path for "did this move": its sha256, or the word `absent`.
# Absent is a first-class answer -- this machine has no config file at all, and
# a check that could only compare hashes would have nothing to say about the
# file most at risk.
fingerprint() {
    if [[ -f "$1" ]]; then /usr/bin/shasum -a 256 "$1" | cut -d' ' -f1; else printf 'absent\n'; fi
}

wait_for_file() {
    local path="$1" deadline=$((SECONDS + ${2:-20}))
    while [[ ! -e "$path" ]]; do
        [[ "$SECONDS" -ge "$deadline" ]] && return 1
        sleep 0.2
    done
    return 0
}

[[ "$(uname -s)" == "Darwin" ]] || { printf 'error: macOS only\n' >&2; exit 1; }
[[ -d "$DEV_APP_INSTALLED_BUNDLE" ]] || {
    printf 'error: %s is not installed, so there is nothing to prove a clash against.\n' \
        "$DEV_APP_INSTALLED_BUNDLE" >&2
    exit 1
}
for name in phux-cockpit phux-cockpit-dev; do
    if pgrep -x "$name" >/dev/null; then
        printf 'error: %s is already running (pids: %s).\n' "$name" "$(pgrep -x "$name" | tr '\n' ' ')" >&2
        printf 'This check is serial-only; quit it and re-run. See phux-cockpit-2ml.10.\n' >&2
        exit 1
    fi
done

mkdir -p -- "$WORK" "$FAKE_HOME/.config/phux-cockpit"

# Your real files, by the same precedence the app uses: PHUX_COCKPIT_CONFIG
# beats the dotfile location ($XDG_CONFIG_HOME, else ~/.config), which beats the
# macOS platform location. Recorded now, re-checked at the end.
REAL_CONFIG="${XDG_CONFIG_HOME:-${HOME}/.config}/phux-cockpit/config"
REAL_PLATFORM_CONFIG="${HOME}/Library/Preferences/Phux Cockpit/config"
REAL_STATE="${HOME}/Library/Application Support/Phux Cockpit/State/workspace.state"
REAL_CONFIG_BEFORE="$(fingerprint "$REAL_CONFIG")"
REAL_PLATFORM_CONFIG_BEFORE="$(fingerprint "$REAL_PLATFORM_CONFIG")"
REAL_STATE_BEFORE="$(fingerprint "$REAL_STATE")"

printf 'your files, before:\n'
printf '  %-64s %s\n' "$REAL_CONFIG" "$REAL_CONFIG_BEFORE"
printf '  %-64s %s\n' "$REAL_PLATFORM_CONFIG" "$REAL_PLATFORM_CONFIG_BEFORE"
printf '  %-64s %s\n' "$REAL_STATE" "$REAL_STATE_BEFORE"

if [[ "$BUILD" == "1" ]]; then
    printf '\nbuilding: zig build package -Doptimize=ReleaseSafe\n'
    ( cd "$ROOT" && zig build package -Doptimize=ReleaseSafe >/dev/null )
else
    printf '\n--no-build: checking the bundle already in zig-out/package (built %s)\n' \
        "$(date -r "${PACKAGED_APP}/Contents/MacOS/$(dev_app_plist_value "$PACKAGED_APP" CFBundleExecutable)" '+%Y-%m-%d %H:%M')"
fi

# ---------------------------------------------------------------- identity

# Report every field on which two bundles are the SAME app to macOS. Prints one
# line per clash and returns 1 when there are none, so both the "must clash" and
# the "must not" direction read the same way.
identity_clashes() {
    local left="$1" right="$2" key found=1
    for key in CFBundleIdentifier CFBundleExecutable CFBundleName; do
        local left_value right_value
        left_value="$(dev_app_plist_value "$left" "$key")"
        right_value="$(dev_app_plist_value "$right" "$key")"
        if [[ "$left_value" == "$right_value" ]]; then
            printf '    %s: both "%s"\n' "$key" "$left_value"
            found=0
        fi
    done
    return "$found"
}

section "identity"
printf '  installed:  %s\n' "$(dev_app_identity "$DEV_APP_INSTALLED_BUNDLE")"
printf '  packaged:   %s\n' "$(dev_app_identity "$PACKAGED_APP")"

# NEGATIVE CONTROL. `zig build package` is the local build everyone reaches for,
# and it is identical to the installed app in all three fields. If this ever
# stops clashing, the comparison below has stopped being able to see a clash and
# its clean result means nothing.
printf '  negative control -- packaged vs installed, must clash on all three:\n'
clash_report="$(identity_clashes "$PACKAGED_APP" "$DEV_APP_INSTALLED_BUNDLE" || true)"
printf '%s\n' "$clash_report"
clash_count="$(printf '%s' "$clash_report" | grep -c . || true)"
if [[ "$clash_count" == "3" ]]; then
    ok 'a plain `zig build package` bundle IS the installed app to LaunchServices'
else
    bad "expected the packaged bundle to clash on all 3 identity fields, saw ${clash_count}"
fi

EXECUTABLE="$(dev_app_stage "$PACKAGED_APP" "$STAGED_APP")"
printf '  staged:     %s\n' "$(dev_app_identity "$STAGED_APP")"
if identity_clashes "$STAGED_APP" "$DEV_APP_INSTALLED_BUNDLE"; then
    bad 'the staged dev bundle still shares an identity field with the installed app'
else
    ok 'staged dev bundle shares no identity field with the installed app'
fi

# ------------------------------------------------------------------- live

section "both apps running at once"

INSTALLED_EXECUTABLE="${DEV_APP_INSTALLED_BUNDLE}/Contents/MacOS/$(dev_app_plist_value "$DEV_APP_INSTALLED_BUNDLE" CFBundleExecutable)"
mkdir -p -- "${WORK}/installed-cwd" "${WORK}/dev-cwd"

# The installed app gets the fake HOME and its own scratch config/state too. It
# is whatever version is installed -- 0.7.1 here -- and this script has no
# business assuming which seams that build honours. HOME and XDG_CONFIG_HOME
# cover the ones it cannot not honour.
env -C "${WORK}/installed-cwd" \
    HOME="$FAKE_HOME" XDG_CONFIG_HOME="${FAKE_HOME}/.config" \
    PHUX_COCKPIT_CONFIG="${WORK}/installed-config" \
    PHUX_COCKPIT_STATE="${WORK}/installed.state" \
    "$INSTALLED_EXECUTABLE" >"${WORK}/installed.log" 2>&1 &
INSTALLED_PID=$!
PIDS+=("$INSTALLED_PID")

env -C "${WORK}/dev-cwd" \
    HOME="$FAKE_HOME" XDG_CONFIG_HOME="${FAKE_HOME}/.config" \
    PHUX_COCKPIT_CONFIG="${WORK}/dev-config" \
    PHUX_COCKPIT_STATE="${WORK}/dev.state" \
    "$EXECUTABLE" >"${WORK}/dev.log" 2>&1 &
DEV_PID=$!
PIDS+=("$DEV_PID")

printf '  installed pid %s, dev pid %s\n' "$INSTALLED_PID" "$DEV_PID"

# The whole point, in two lines: each name resolves to exactly one pid, and it
# is the pid we launched. This is what an automation script is really asking
# when it says `pgrep -x phux-cockpit`.
if dev_app_wait_named phux-cockpit "$INSTALLED_PID"; then
    ok "pgrep -x phux-cockpit          -> ${INSTALLED_PID} (the installed app, only)"
else
    bad 'pgrep -x phux-cockpit did not resolve to exactly the installed pid'
fi
if dev_app_wait_named phux-cockpit-dev "$DEV_PID"; then
    ok "pgrep -x phux-cockpit-dev      -> ${DEV_PID} (the dev build, only)"
else
    bad 'pgrep -x phux-cockpit-dev did not resolve to exactly the dev pid'
fi

# LaunchServices' bundle id for the live process, polled: it registers a
# process after pgrep sees it (reading early returns NULL for both).
registered_bundle_id() {
    local raw
    raw="$(lsappinfo info -only bundleid "$1" 2>/dev/null || true)"
    # Only a quoted value is an answer; `[ NULL ]` means not yet registered.
    printf '%s' "$raw" | sed -nE 's/.*="([^"]+)".*/\1/p'
}
wait_registered_bundle_id() {
    local pid="$1" deadline=$((SECONDS + 30)) value
    while :; do
        value="$(registered_bundle_id "$pid")"
        if [[ -n "$value" ]]; then printf '%s\n' "$value"; return 0; fi
        [[ "$SECONDS" -ge "$deadline" ]] && return 1
        sleep 0.5
    done
}
installed_registered="$(wait_registered_bundle_id "$INSTALLED_PID" || true)"
dev_registered="$(wait_registered_bundle_id "$DEV_PID" || true)"
printf '  lsappinfo: installed=%s dev=%s\n' "${installed_registered:-<none>}" "${dev_registered:-<none>}"
if [[ -n "$installed_registered" && "$installed_registered" != "$dev_registered" ]]; then
    ok 'LaunchServices registered the two live processes under different bundle ids'
else
    bad 'LaunchServices sees one bundle id for both processes'
fi

# System Events process list (skipped, not passed, without the grant).
# Exact membership, since "phux-cockpit" prefixes "phux-cockpit-dev"; polled
# because a process appears there only once it has a UI session.
system_events_phux() {
    osascript -e 'tell application "System Events" to get name of every process whose name contains "phux"' 2>/dev/null |
        tr ',' '\n' | sed 's/^ *//; s/ *$//' | grep -v '^$' || true
}
events_deadline=$((SECONDS + 30))
while :; do
    events_list="$(system_events_phux)"
    if printf '%s\n' "$events_list" | grep -qx 'phux-cockpit-dev' &&
       printf '%s\n' "$events_list" | grep -qx 'phux-cockpit'; then
        break
    fi
    [[ "$SECONDS" -ge "$events_deadline" ]] && break
    sleep 0.5
done
printf '  System Events sees: %s\n' "$(printf '%s' "$events_list" | tr '\n' ' ')"
if [[ -z "$events_list" ]] && ! osascript -e 'tell application "System Events" to get name of first process' >/dev/null 2>&1; then
    # No answer at all AND the surface itself refuses: a missing Accessibility
    # grant, which is a skip. Saying so is the difference between evidence and
    # an empty result reported as a pass.
    printf '  SKIPPED: System Events refused (needs an Accessibility grant). Not a pass.\n'
elif printf '%s\n' "$events_list" | grep -qx 'phux-cockpit-dev' &&
     printf '%s\n' "$events_list" | grep -qx 'phux-cockpit'; then
    ok 'System Events can name each instance separately'
else
    bad "System Events did not list both process names distinctly: ${events_list}"
fi

for pid in "$INSTALLED_PID" "$DEV_PID"; do kill "$pid" 2>/dev/null || true; done
PIDS=()
sleep 2

# --------------------------------------------------- config and state files

section "config and state"

# A config whose `command` key launches a marker script instead of a shell: the
# marker file appearing is proof the app READ that config, observable without
# the automation harness and without a screenshot.
write_marker_config() {
    local config="$1" script="$2" marker="$3"
    cat >"$script" <<EOF
#!/bin/sh
touch "${marker}"
sleep 300
EOF
    chmod +x "$script"
    printf 'command = /bin/sh %s\n' "$script" >"$config"
}

# A FRESH fake home for this phase. The live phase above ran an older binary
# under the previous one, and this phase's assertions are about files that must
# not exist yet -- an artifact left behind by an older build's idea of the same
# seams would read here as a failure of this run.
rm -rf -- "$FAKE_HOME"
mkdir -p -- "${FAKE_HOME}/.config/phux-cockpit"

REAL_MARKER="${WORK}/marker-real"
DEV_MARKER="${WORK}/marker-dev"
write_marker_config "${FAKE_HOME}/.config/phux-cockpit/config" "${WORK}/real-shell.sh" "$REAL_MARKER"
mkdir -p -- "${WORK}/devhome/${DEV_APP_RUNTIME_ID}"
write_marker_config "${WORK}/devhome/config" "${WORK}/dev-shell.sh" "$DEV_MARKER"
DEV_STATE="$(dev_app_state_path "${WORK}/devhome")"
FAKE_PLATFORM_STATE="${FAKE_HOME}/Library/Application Support/Phux Cockpit/State/workspace.state"

if [[ -e "$REAL_MARKER" || -e "$DEV_MARKER" || -e "$FAKE_PLATFORM_STATE" ]]; then
    printf 'error: the files this phase watches for exist before anything ran\n' >&2
    exit 1
fi

printf '  arm A: launched the way scripts/dev-run.sh launches\n'
env -C "${WORK}/devhome" \
    HOME="$FAKE_HOME" XDG_CONFIG_HOME="${FAKE_HOME}/.config" TMPDIR="${WORK}/devhome" \
    PHUX_COCKPIT_CONFIG="${WORK}/devhome/config" \
    PHUX_COCKPIT_STATE="$DEV_STATE" \
    "$EXECUTABLE" >"${WORK}/arm-a.log" 2>&1 &
ARM_A_PID=$!
PIDS+=("$ARM_A_PID")
if wait_for_file "$DEV_MARKER"; then
    ok 'the dev config was read (its shell ran)'
else
    bad 'the dev config was never read; the rest of arm A proves nothing'
fi
if [[ -e "$REAL_MARKER" ]]; then
    bad 'the dev run read the user-level config'
else
    ok 'the user-level config was NOT read'
fi

# Layout is written on shutdown: settle, kill, then poll for the file.
sleep 3
kill "$ARM_A_PID" 2>/dev/null || true
PIDS=()
if wait_for_file "$DEV_STATE" 20; then
    ok 'workspace layout was written into the dev home'
else
    bad 'no workspace state in the dev home; the state seam did not take'
fi
if [[ -e "$FAKE_PLATFORM_STATE" ]]; then
    bad 'the dev run wrote the platform state file'
else
    ok 'the platform state file was NOT written'
fi

# NEGATIVE CONTROL. Same binary, same fake HOME, PHUX_COCKPIT_CONFIG and
# PHUX_COCKPIT_STATE removed and nothing else. Both files arm A left alone must
# now appear -- which is what makes arm A's two absences mean "isolated" rather
# than "unreachable".
printf '  arm B (negative control): the same run WITHOUT the two variables\n'
rm -f -- "$DEV_MARKER"
mkdir -p -- "${WORK}/ctrlhome"
env -C "${WORK}/ctrlhome" \
    HOME="$FAKE_HOME" XDG_CONFIG_HOME="${FAKE_HOME}/.config" \
    "$EXECUTABLE" >"${WORK}/arm-b.log" 2>&1 &
ARM_B_PID=$!
PIDS+=("$ARM_B_PID")
if wait_for_file "$REAL_MARKER"; then
    ok 'without the variables it reaches the user-level config, as it must'
else
    bad 'even unisolated it never read the user-level config: the marker cannot move, so arm A proved nothing'
fi
sleep 3
kill "$ARM_B_PID" 2>/dev/null || true
PIDS=()
if wait_for_file "$FAKE_PLATFORM_STATE" 20; then
    ok 'without the variables it writes the platform state file, as it must'
else
    bad 'even unisolated it never wrote the platform state file: that absence cannot move either'
fi

# ------------------------------------------------------------------ yours

section "your own files"
for pair in \
    "${REAL_CONFIG}|${REAL_CONFIG_BEFORE}" \
    "${REAL_PLATFORM_CONFIG}|${REAL_PLATFORM_CONFIG_BEFORE}" \
    "${REAL_STATE}|${REAL_STATE_BEFORE}"
do
    path="${pair%%|*}"; before="${pair##*|}"
    after="$(fingerprint "$path")"
    if [[ "$after" == "$before" ]]; then
        ok "unchanged (${before:0:12}): ${path}"
    else
        bad "CHANGED (${before:0:12} -> ${after:0:12}): ${path}"
    fi
done

printf '\n'
if [[ "$FAILURES" == "0" ]]; then
    printf 'isolation: ok\n'
    exit 0
fi
printf 'isolation: %s FAILED assertion(s)\n' "$FAILURES"
exit 1
