#!/usr/bin/env bash
# Drive the real bundled app through the SDK's automation harness and assert on
# what actually changes. This is the repeatable version of a session that was
# otherwise twenty manual steps.
#
#   ./scripts/automate-smoke.sh                 # structure + driven interaction
#   ./scripts/automate-smoke.sh --fullscreen    # ...and a real OS fullscreen round-trip
#   ./scripts/automate-smoke.sh --profile       # ...and a 4-pane scheduler profile
#   ./scripts/automate-smoke.sh --churn         # split/close churn profile
#   ./scripts/automate-smoke.sh --phux          # build/attach the real local Phux provider
#   ./scripts/automate-smoke.sh --churn --churn-actions 160
#   ./scripts/automate-smoke.sh --keep          # leave the app running to poke at
#
# It builds the CLI from the PINNED SDK (see build-automation-cli.sh — the
# automation dropbox is fingerprint-guarded, and a CLI from anywhere else will
# refuse this app), packages a bundle with -Dautomation=true, launches it with
# an ISOLATED config and state file so it cannot touch the user's own, waits
# for readiness, and drives it.
#
# WHY EVERY ASSERTION HERE HAS A NEGATIVE CONTROL
# ------------------------------------------------
# `expect_change` asserts the pattern is ABSENT, performs the action, then
# asserts it is PRESENT. The absent half is not ceremony — it is the only
# thing standing between this script and the failure that produced it.
#
# On 2026-08-12 an earlier version of this file carried a confident warning
# that automation input was silently broken. It was not. The assertion used to
# "prove" it counted `role=textbox name="Terminal`, which only ever matches the
# SELECTED tab's rendered pane — so `cmd+t` creating a second TAB could not
# move it, no matter how perfectly the key was delivered. The harness was
# telling the truth; the assertion could not fail for the right reason, and so
# it manufactured a finding. See phux-cockpit-2ml.5.
#
# An assertion never seen to distinguish the two states is not evidence.
# If you add one here, give it its before-half too.
#
# Verified observables for this app (2026-08-12, against the real bundle):
#   new tab            role=tab name="Terminal N
#   rendered pane      role=textbox name="Terminal        (SELECTED tab only)
#   scrollback search  role=group name="Scrollback search
# measures: real-bundle structure, interaction, fullscreen, scheduler cadence, steady repaint, and split/close churn
#
# SERIAL ONLY (phux-cockpit-2ml.10). The identity-staged measurement bundles
# deliberately reuse dev-run's `phux-cockpit-dev` process identity, so two live
# measurement runs would make PID activation ambiguous even though each has a
# private dropbox. scripts/lib/app-instance.sh refuses that state and keeps
# checking the exact process and publisher PID between driven steps; the swap
# that filed the bead happened in the MIDDLE of a sequence that started clean.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
. "${ROOT}/scripts/lib/measure.sh"
WORK="${TMPDIR:-/tmp}/phux-cockpit-smoke.$$"
PROFILE=0
FULLSCREEN=0
CHURN=0
CHURN_ACTIONS=160
PROFILE_PIPELINE_STAGES=(rebuild layout reconcile emit a11y plan patch encode present host_decode host_draw)
PROFILE_REQUIRED_STAGES=("${PROFILE_PIPELINE_STAGES[@]}" interval)
CHURN_REQUIRED_STAGES=(rebuild layout reconcile emit a11y plan patch encode)
KEEP=0
PHUX=0
PHUX_SOCKET=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --profile) PROFILE=1; shift ;;
        --fullscreen) FULLSCREEN=1; shift ;;
        --churn) CHURN=1; shift ;;
        --churn-actions)
            [[ $# -ge 2 ]] || { printf -- '--churn-actions needs a value\n' >&2; exit 2; }
            CHURN_ACTIONS="$2"; shift 2
            ;;
        --keep) KEEP=1; shift ;;
        --phux) PHUX=1; shift ;;
        -h|--help) sed -n '2,45p' "$0"; exit 0 ;;
        *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done

if (( PROFILE + FULLSCREEN + CHURN > 1 )); then
    printf 'choose one of --fullscreen, --profile, or --churn; each needs an independent run\n' >&2
    exit 2
fi

if [[ "$PHUX" == 1 ]]; then
    command -v phux >/dev/null || { printf 'phux is required for --phux\n' >&2; exit 1; }
    PHUX_SOCKET="$(phux status --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["socket"])')"
    [[ "$PHUX_SOCKET" == /* ]] || { printf 'phux status returned no absolute socket\n' >&2; exit 1; }
fi
if [[ ! "$CHURN_ACTIONS" =~ ^[0-9]+$ ]] \
    || (( CHURN_ACTIONS < 2 || CHURN_ACTIONS > 1000 || CHURN_ACTIONS % 2 != 0 )); then
    printf -- '--churn-actions must be an even integer from 2 through 1000\n' >&2
    exit 2
fi

mkdir -p "$WORK"
APP_PID=""
cleanup() {
    # Waits for the app to actually exit, not just for the signal to be sent:
    # these runs are serial, and returning early makes the NEXT one refuse a
    # machine that is about to be idle. See app_instance_stop.
    if [[ -n "$APP_PID" && "$KEEP" == "0" ]]; then
        app_instance_stop "$APP_PID"
    fi
    if [[ -n "$APP_PID" && "$KEEP" == "1" ]]; then
        measure_print_retained_run "$APP_PID" "$NATIVE" "$MEASURE_DROPBOX"
    fi
    [[ "$KEEP" == "1" ]] || rm -rf "$WORK"
}
trap cleanup EXIT

NATIVE="$("${ROOT}/scripts/build-automation-cli.sh")"
printf 'cli: %s\n' "$NATIVE"

# A pane that repaints continuously, so the profile describes something closer
# to a live TUI than an idle prompt. Uses the `command` config key, which is
# why that key existing is load-bearing here and not just a nicety.
cat >"${WORK}/repaint.sh" <<'REPAINT'
#!/bin/sh
i=0
delay="${1:-}"
while :; do
    printf '\033[H\033[2J'
    n=0
    while [ "$n" -lt 45 ]; do
        printf 'row %03d  frame %06d  %s\n' "$n" "$i" 'the quick brown fox jumps over the lazy dog'
        n=$((n + 1))
    done
    i=$((i + 1))
    # Profile modes pass a delay so an unbounded producer cannot starve the UI
    # thread whose frame/topology work is being measured.
    [ -z "$delay" ] || sleep "$delay"
done
REPAINT
chmod +x "${WORK}/repaint.sh"
if [[ "$CHURN" == 1 ]]; then
    printf 'command = /bin/sh %s 0.01\nfont-size = 13\n' "${WORK}/repaint.sh" >"${WORK}/config"
elif [[ "$PROFILE" == 1 ]]; then
    printf 'command = /bin/sh %s 0.001\nfont-size = 13\n' "${WORK}/repaint.sh" >"${WORK}/config"
else
    printf 'command = /bin/sh %s\nfont-size = 13\n' "${WORK}/repaint.sh" >"${WORK}/config"
fi
if [[ "$PHUX" == 1 ]]; then
    printf 'phux-socket = %s\nphux-session = default\n' "$PHUX_SOCKET" >>"${WORK}/config"
fi

measure_launch_isolated "$WORK" "${WORK}/config" "${WORK}/app.log" "$PHUX"
APP_PID="$MEASURE_APP_PID"

"$NATIVE" automate wait >/dev/null
app_instance_bind "$NATIVE" "$APP_PID"

tab_count() {
    app_instance_snapshot | grep -c 'role=tab name=' || true
}

# A Phux run starts with the ephemeral local terminal, then admits the current
# provider-backed terminal when readiness arrives. Do not drive the app while
# that asynchronous attachment is still changing the tab run.
if [[ "$PHUX" == 1 ]]; then
    deadline=$((SECONDS + 30))
    while :; do
        snapshot="$(app_instance_snapshot)"
        attached_tabs="$(grep -c 'role=tab name=' <<<"$snapshot" || true)"
        if [[ "$attached_tabs" == 2 ]] && grep -q 'role=textbox name=' <<<"$snapshot"; then
            break
        fi
        if [[ "$SECONDS" -ge "$deadline" ]]; then
            printf 'FAILED: real Phux startup did not admit a provider-backed terminal.\n' >&2
            exit 1
        fi
        sleep 0.1
    done
    app_instance_assert
    printf '  ok: Phux inventory admitted exactly one provider terminal (%s tabs total)\n' "$attached_tabs"
fi

# Assert a pattern is absent, run the action, then assert it is present. The
# absent half is the negative control: it proves this assertion can tell the
# two states apart, so a later PRESENT result means the action worked rather
# than meaning the pattern was always there.
expect_change() {
    local what="$1" pattern="$2"; shift 2
    # Re-verify the binding at every step, not just at startup. A sole-instance
    # check that runs once cannot see a sibling that appears afterwards, and
    # that is precisely how phux-cockpit-2ml.10 presented: a sequence that
    # began against our pid and finished against someone else's.
    app_instance_assert || return 1
    if ! "$NATIVE" automate assert --absent "$pattern" >/dev/null 2>&1; then
        printf 'NEGATIVE CONTROL FAILED: %s already matches before the action.\n' "$pattern" >&2
        printf 'This assertion cannot prove %s did anything. Fix the assertion.\n' "$what" >&2
        return 1
    fi
    "$@" >/dev/null
    if ! "$NATIVE" automate assert --timeout-ms 5000 "$pattern" >/dev/null; then
        printf 'FAILED: %s did not produce %s\n' "$what" "$pattern" >&2
        return 1
    fi
    printf '  ok: %s\n' "$what"
}

# Structural assertions. These read published runtime state.
terminal_pattern='role=textbox name="Terminal 1'
[[ "$PHUX" == 1 ]] && terminal_pattern='role=textbox name='
"$NATIVE" automate assert \
    'ready=true' \
    'window @w1' \
    'kind=gpu_surface' \
    "$terminal_pattern" \
    'dispatch_errors=0'
# The terminal must reach glass through the PACKET path, where the AppKit host
# rasterizes with CoreText. A fallback to `pixels` silently moves every glyph
# onto the CPU reference renderer - the app then looks like its own
# screenshots, which is exactly the failure no screenshot can report. See
# docs/RENDER_FIDELITY.md.
"$NATIVE" automate assert 'gpu_present_path=packet'
"$NATIVE" automate assert --absent 'error event='
printf 'structure: ok\n'

printf 'driving interaction...\n'
before_tabs="$(tab_count)"
after_tabs=$((before_tabs + 1))
"$NATIVE" automate widget-key phux-cockpit-canvas cmd+t >/dev/null
deadline=$((SECONDS + 5))
while [[ "$(tab_count)" != "$after_tabs" ]]; do
    if [[ "$SECONDS" -ge "$deadline" ]]; then
        printf 'FAILED: cmd+t expected %s tabs after %s, got %s.\n' \
            "$after_tabs" "$before_tabs" "$(tab_count)" >&2
        exit 1
    fi
    sleep 0.1
done
app_instance_assert
printf '  ok: cmd+t adds exactly one local tab (%s -> %s tabs)\n' "$before_tabs" "$after_tabs"
expect_change 'cmd+f opens scrollback search' 'role=group name="Scrollback search' \
    "$NATIVE" automate widget-key phux-cockpit-canvas cmd+f
"$NATIVE" automate widget-key phux-cockpit-canvas escape >/dev/null
"$NATIVE" automate assert --timeout-ms 5000 --absent 'role=group name="Scrollback search' >/dev/null
printf '  ok: escape closes scrollback search\n'

pane_count() {
    app_instance_snapshot | grep -c 'role=textbox name="Terminal' || true
}

# Run one topology action and prove its exact before/after pane counts. This is
# the churn negative control: the previous count must be observable before the
# key, and the different expected count must become observable afterwards.
expect_pane_transition() {
    local what="$1" before="$2" after="$3" key="$4"
    local observed tab marker
    app_instance_assert || return 1
    observed="$(pane_count)"
    if [[ "$observed" != "$before" ]]; then
        printf 'REFUSING TO DRIVE: %s expected %s panes before the action, got %s.\n' \
            "$what" "$before" "$observed" >&2
        return 1
    fi
    tab="$(app_instance_snapshot \
        | grep -o 'role=textbox name="Terminal [0-9][0-9]*' \
        | head -1 \
        | grep -o '[0-9][0-9]*$' || true)"
    if [[ ! "$tab" =~ ^[0-9]+$ ]]; then
        printf 'REFUSING TO DRIVE: could not derive the selected tab from its pane labels.\n' >&2
        return 1
    fi

    if (( after > before )); then
        marker="role=textbox name=\"Terminal ${tab}\\.${after}"
        "$NATIVE" automate assert --absent "$marker" >/dev/null
    else
        marker="role=textbox name=\"Terminal ${tab}\\.${before}"
        "$NATIVE" automate assert "$marker" >/dev/null
    fi
    "$NATIVE" automate widget-key phux-cockpit-canvas "$key" >/dev/null
    if (( after > before )); then
        "$NATIVE" automate assert --timeout-ms 5000 "$marker" >/dev/null
    else
        "$NATIVE" automate assert --timeout-ms 5000 --absent "$marker" >/dev/null
    fi
    app_instance_assert || return 1
    observed="$(pane_count)"
    if [[ "$observed" != "$after" ]]; then
        printf 'FAILED: %s reached its movable marker but pane count is %s, not %s.\n' \
            "$what" "$observed" "$after" >&2
        return 1
    fi
    printf '  ok: %s (%s -> %s panes)\n' "$what" "$before" "$after"
}

wait_for_profile_floor() {
    local floor="$1"
    shift
    local -a required=("$@")
    local deadline=$((SECONDS + 60)) snapshot
    while :; do
        snapshot="$(app_instance_snapshot)" || return 1
        if measure_require_profile_stages "$snapshot" "$floor" "${required[@]}" 2>/dev/null; then
            printf '%s\n' "$snapshot"
            return 0
        fi
        if [[ "$SECONDS" -ge "$deadline" ]]; then
            measure_require_profile_stages "$snapshot" "$floor" "${required[@]}"
            return 1
        fi
        sleep 1
    done
}

# AXFullScreen is the OS's state for the real NSWindow. The automation
# snapshot has no fullscreen bit, and its CPU reference screenshot does not
# photograph the window, so neither can stand in for this PID-addressed query.
ax_fullscreen_state() {
    osascript -e "tell application \"System Events\" to tell (first process whose unix id is ${APP_INSTANCE_PID}) to get value of attribute \"AXFullScreen\" of window 1" 2>/dev/null
}

wait_for_ax_fullscreen() {
    local wanted="$1"
    local deadline=$((SECONDS + 15)) observed=""
    while :; do
        app_instance_assert || return 1
        observed="$(ax_fullscreen_state || true)"
        [[ "$observed" == "$wanted" ]] && return 0
        if [[ "$SECONDS" -ge "$deadline" ]]; then
            printf 'FAILED: pid %s AXFullScreen did not become %s (last value: %s).\n' \
                "$APP_INSTANCE_PID" "$wanted" "${observed:-unavailable}" >&2
            return 1
        fi
        # Native's polling assertion cannot query AppKit accessibility
        # attributes. This poll is intentionally only for that missing OS bit.
        sleep 0.1
    done
}

snapshot_window_size() {
    app_instance_snapshot \
        | grep '^window @w1 ' \
        | head -1 \
        | grep -o '[0-9][0-9]*x[0-9][0-9]*' \
        | head -1
}

if [[ "$FULLSCREEN" == "1" ]]; then
    printf 'driving the bundled window through fullscreen...\n'
    app_instance_activate
    "$NATIVE" automate assert --timeout-ms 5000 'window @w1.*focused=true' >/dev/null
    app_instance_assert

    initial_fullscreen="$(ax_fullscreen_state || true)"
    initial_size="$(snapshot_window_size || true)"
    if [[ "$initial_fullscreen" != "false" ]]; then
        printf 'REFUSING TO DRIVE: pid %s started with AXFullScreen=%s, not false.\n' \
            "$APP_INSTANCE_PID" "${initial_fullscreen:-unavailable}" >&2
        exit 1
    fi
    if [[ ! "$initial_size" =~ ^[0-9]+x[0-9]+$ ]]; then
        printf 'REFUSING TO DRIVE: the bound snapshot has no readable main-window size.\n' >&2
        exit 1
    fi

    "$NATIVE" automate widget-key phux-cockpit-canvas ctrl+cmd+f >/dev/null
    wait_for_ax_fullscreen true
    "$NATIVE" automate assert --timeout-ms 15000 --absent \
        "window @w1.* ${initial_size}" >/dev/null
    "$NATIVE" automate assert --timeout-ms 5000 'window @w1.*focused=true' >/dev/null
    app_instance_assert
    fullscreen_size="$(snapshot_window_size || true)"
    if [[ ! "$fullscreen_size" =~ ^[0-9]+x[0-9]+$ || "$fullscreen_size" == "$initial_size" ]]; then
        printf 'REFUSING TO REPORT: AXFullScreen became true without a measurable window-size transition.\n' >&2
        exit 1
    fi
    printf '  ok: AXFullScreen false -> true (%s -> %s)\n' "$initial_size" "$fullscreen_size"

    "$NATIVE" automate widget-key phux-cockpit-canvas ctrl+cmd+f >/dev/null
    wait_for_ax_fullscreen false
    "$NATIVE" automate assert --timeout-ms 15000 \
        "window @w1.* ${initial_size}" >/dev/null
    app_instance_assert
    restored_size="$(snapshot_window_size || true)"
    if [[ "$restored_size" != "$initial_size" ]]; then
        printf 'REFUSING TO REPORT: AXFullScreen became false but size restored to %s, not %s.\n' \
            "${restored_size:-unavailable}" "$initial_size" >&2
        exit 1
    fi
    printf '  ok: AXFullScreen true -> false (%s -> %s)\n' "$fullscreen_size" "$restored_size"

    measure_basis fullscreen_transition \
        "identity-staged bundle=${WORK}/Phux Cockpit (measure).app; publisher_pid=${APP_INSTANCE_PID}; frontmost pid and AXFullScreen queried by unix id; no screenshot" \
        './scripts/automate-smoke.sh --fullscreen'
    printf 'OBSERVED fullscreen_transition publisher_pid=%s focused=true ax_fullscreen=false->true->false window_size=%s->%s->%s\n' \
        "$APP_INSTANCE_PID" "$initial_size" "$fullscreen_size" "$restored_size"
fi

if [[ "$PROFILE" == "1" ]]; then
    # jw4's recorded condition is FOUR panes under interaction, not an idle
    # prompt. Split to four before reading anything, or the percentiles
    # describe a near-idle app and mean nothing.
    printf 'building the 4-pane condition...\n'
    expect_pane_transition 'first vertical split' 1 2 cmd+d
    expect_pane_transition 'horizontal split' 2 3 cmd+shift+d
    expect_pane_transition 'second vertical split' 3 4 cmd+d
    panes="$(pane_count)"
    printf '  panes in the selected tab: %s\n' "$panes"
    if [[ "$panes" -lt 4 ]]; then
        printf 'REFUSING TO PROFILE: wanted 4 panes, got %s. Numbers from the\n' "$panes" >&2
        printf 'wrong condition are worse than no numbers. See phux-cockpit-jw4.\n' >&2
        exit 1
    fi

    "$NATIVE" automate profile on >/dev/null 2>&1

    profile_snapshot="$(wait_for_profile_floor "$MEASURE_SAMPLE_FLOOR" "${PROFILE_REQUIRED_STAGES[@]}")"
    printf '\n--- frame profile (4 panes, continuous repaint) ---\n'
    measure_print_frame_profile steady_repaint \
        "4 panes, 45-row continuous repaint, font-size 13; full 128-sample rolling populations for ${PROFILE_REQUIRED_STAGES[*]}" \
        './scripts/automate-smoke.sh --profile' "$profile_snapshot"
    measure_print_scheduler_comparison "$profile_snapshot" "${PROFILE_PIPELINE_STAGES[@]}"
fi

if [[ "$CHURN" == "1" ]]; then
    printf 'profiling %s bounded split/close actions...\n' "$CHURN_ACTIONS"
    [[ "$(pane_count)" == 1 ]] || { printf 'REFUSING TO PROFILE CHURN: expected one starting pane.\n' >&2; exit 1; }
    "$NATIVE" automate profile on >/dev/null
    for (( action = 1; action <= CHURN_ACTIONS; action++ )); do
        if (( action % 2 == 1 )); then
            expect_pane_transition "churn action ${action} split" 1 2 cmd+d
        else
            expect_pane_transition "churn action ${action} close" 2 1 cmd+w
        fi
        (( action % 20 != 0 )) || printf '  proven actions: %s/%s\n' "$action" "$CHURN_ACTIONS"
    done
    [[ "$(pane_count)" == 1 ]] || { printf 'REFUSING TO REPORT: churn did not return to one pane.\n' >&2; exit 1; }
    profile_snapshot="$(wait_for_profile_floor "$MEASURE_SAMPLE_FLOOR" "${CHURN_REQUIRED_STAGES[@]}")"
    printf '\n--- frame profile (split/close churn) ---\n'
    measure_print_frame_profile split_close_churn \
        "${CHURN_ACTIONS} alternating cmd+d/cmd+w actions, each pane transition asserted; required full 128-sample rolling populations for ${CHURN_REQUIRED_STAGES[*]}; paced continuous repaint" \
        "./scripts/automate-smoke.sh --churn --churn-actions ${CHURN_ACTIONS}" "$profile_snapshot"
fi

printf 'verifying focused secondary-window overlays...\n'
expect_change 'cmd+n opens a secondary window' 'window @w2' \
    "$NATIVE" automate widget-key phux-cockpit-canvas cmd+n
"$NATIVE" automate assert --absent 'role=textbox name="Find terminal"' >/dev/null
"$NATIVE" automate widget-key phux-cockpit-canvas-1 cmd+shift+p >/dev/null
deadline=$((SECONDS + 5))
while :; do
    snapshot="$(app_instance_snapshot)"
    switcher_count="$(grep -c 'role=textbox name="Find terminal"' <<<"$snapshot" || true)"
    if [[ "$switcher_count" == 1 ]] \
        && grep -q '@w2/phux-cockpit-canvas-1.*role=textbox name="Find terminal"' <<<"$snapshot"; then
        break
    fi
    if [[ "$SECONDS" -ge "$deadline" ]]; then
        printf 'FAILED: focused secondary switcher appeared %s times or in the wrong window.\n' "$switcher_count" >&2
        exit 1
    fi
    sleep 0.1
done
app_instance_assert
printf '  ok: secondary switcher appears once, in focused window @w2\n'

printf '\nsmoke: ok\n'
