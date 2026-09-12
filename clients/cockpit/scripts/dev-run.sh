#!/usr/bin/env bash
# Build this checkout and run it, as a real app, in a way that cannot be
# confused with the Phux Cockpit in /Applications.
#
#   ./scripts/dev-run.sh
#
# That is the whole command. Everything below is optional.
#
# WHY THIS EXISTS
# ---------------
# On 2026-08-12 it surfaced that three days of rendering bug reports had been
# filed against /Applications/Phux Cockpit.app -- version 0.7.1, built
# 2026-08-09 -- while `main` was 0.8.0. Nothing merged in those three days had
# ever been in front of the person reporting the bugs. Both apps are called
# "Phux Cockpit", both put the same icon in the same Dock, and until now the
# only local run was a bare binary out of `zig-out/bin` that shared the
# installed app's bundle identity, its process name, and its state file. There
# was no cheap way to be SURE which one you were looking at, so the expensive
# way -- three days -- happened instead.
#
# This command makes the answer obvious: the dev build's Dock tile and
# application menu read "Phux Cockpit (dev)", its process is `phux-cockpit-dev`
# rather than `phux-cockpit`, and its config, workspace layout and automation
# dropbox live under `.dev-run/` in this repo. Run both at once if you like;
# nothing they own is shared. scripts/lib/dev-app.sh explains the four separate
# mechanisms that takes, and `./scripts/dev-isolation-check.sh` drives both apps
# at once and proves it.
#
# OPTIONS
#   --debug              Debug build instead of ReleaseSafe. Faster to compile,
#                        but it is NOT what ships: scripts/package-macos.sh
#                        builds ReleaseSafe, so timing-shaped questions (frame
#                        pacing, input latency) need the default.
#   --automation         Build with -Dautomation=true and print how to drive it.
#   --config PATH        Use this config file instead of .dev-run/config. The
#                        app WRITES to whatever file it is given (a theme choice
#                        from the settings surface), so naming your real config
#                        here means a dev build can edit it.
#   --fresh              Delete the dev home first: config, workspace layout,
#                        dropbox. A clean first-launch.
#   --detach             Print the pid and exit instead of staying attached.
#   --no-build           Restage the existing package. Skips zig entirely;
#                        build configuration is unknown, flags cannot change it.
#   --phux               -Dphux-enabled=true, with the FFI directories the build
#                        already knows how to find.
#   --ffi-profile NAME   Cargo FFI profile directory (default ffi-release).
#                        Use ffi-dev after building it via just cockpit-ffi.
#   --measure-first-frame Build with automation, enable the SDK's launch/GPU
#                        phase stamps, bind the first nonblank snapshot to the
#                        launched pid, and fail if its 150ms budget is missed.
#
# The foreground run is the default because it makes quitting unambiguous:
# ctrl-c here ends the app, and closing the app ends this script. A dev build
# you forgot was running is a dev build that will confuse the next launch.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
. "${ROOT}/scripts/lib/dev-app.sh"
. "${ROOT}/scripts/lib/app-instance.sh"

DEV_HOME="${PHUX_COCKPIT_DEV_HOME:-${ROOT}/.dev-run}"
STAGED_APP="${ROOT}/zig-out/dev/Phux Cockpit (dev).app"
OPTIMIZE="ReleaseSafe"
AUTOMATION=0
CONFIG=""
FRESH=0
DETACH=0
BUILD=1
PHUX=0
FFI_PROFILE="ffi-release"
MEASURE_FIRST_FRAME=0
BUILD_DURATION_NS="skipped"
LAUNCH_STARTED_NS=0

wall_ns() {
    /usr/bin/python3 -c 'import time; print(time.time_ns())'
}

snapshot_field() {
    local line="$1" name="$2"
    printf '%s\n' "$line" | tr ' ' '\n' | sed -n "s/^${name}=//p" | head -1
}

launch_lap_wall_ns() {
    local name="$1"
    sed -n "s/^native-sdk: launch ${name} wall_ns=\\([0-9][0-9]*\\)$/\\1/p" "$LOG" | head -1
}

measure_first_frame() {
    local snapshot_path="${automation_dir}/snapshot.txt"
    local snapshot="" deadline=$((SECONDS + 20))
    local canvas_line publisher latency budget exceeded observed_ns
    local runner_main scene_loaded first_recorded plan_done draw_trace present_us
    local process_to_runner scene_to_first_record first_record_to_plan useful_schedule present_ns useful_glass
    local -a live

    while true; do
        if [[ -s "$snapshot_path" ]]; then
            snapshot="$(cat "$snapshot_path")"
            if [[ "$snapshot" == *"publisher_pid=${DEV_APP_PID}"* &&
                  "$snapshot" == *"view @w1/phux-cockpit-canvas kind=gpu_surface"* &&
                  "$snapshot" == *"gpu_nonblank=true"* ]]; then
                break
            fi
        fi
        if [[ "$SECONDS" -ge "$deadline" ]]; then
            printf 'first-frame: timed out waiting for pid %s to publish a nonblank canvas snapshot\n' "$DEV_APP_PID" >&2
            return 1
        fi
        sleep 0.01
    done

    IFS=$'\n' read -r -d '' -a live < <(app_instance_pids && printf '\0') || true
    app_instance_check_only "$DEV_APP_PID" "${live[@]}" || return 1
    app_instance_check_snapshot "$DEV_APP_PID" "$snapshot" || return 1
    canvas_line="$(printf '%s\n' "$snapshot" | grep -F 'view @w1/phux-cockpit-canvas kind=gpu_surface' | head -1)"
    publisher="$(snapshot_field "$(printf '%s\n' "$snapshot" | head -1)" publisher_pid)"
    latency="$(snapshot_field "$canvas_line" gpu_first_frame_latency_ns)"
    budget="$(snapshot_field "$canvas_line" gpu_first_frame_latency_budget_ns)"
    exceeded="$(snapshot_field "$canvas_line" gpu_first_frame_latency_budget_exceeded)"
    runner_main="$(launch_lap_wall_ns runner_main)"
    scene_loaded="$(launch_lap_wall_ns scene_loaded)"
    first_recorded="$(launch_lap_wall_ns first_present_recorded)"
    plan_done="$(launch_lap_wall_ns first_plan_done)"
    draw_trace="$(sed -n '/^native-sdk: gpu draw-trace /{p;q;}' "$LOG")"
    present_us="$(snapshot_field "$draw_trace" present_us)"
    observed_ns="$(wall_ns)"

    for value in "$publisher" "$latency" "$budget" "$exceeded" "$runner_main" "$scene_loaded" "$first_recorded" "$plan_done" "$present_us"; do
        if [[ ! "$value" =~ ^[0-9]+$ ]]; then
            printf 'first-frame: required timing provenance is missing; log=%s snapshot=%s\n' "$LOG" "$snapshot_path" >&2
            return 1
        fi
    done

    process_to_runner=$((runner_main - LAUNCH_STARTED_NS))
    scene_to_first_record=$((first_recorded - scene_loaded))
    first_record_to_plan=$((plan_done - first_recorded))
    # The pinned SDK records gpu_first_frame_latency at the request/completion
    # event before UiApp builds the startup frame. Add the measured plan tail
    # rather than relabeling that field as useful-frame latency, then add the
    # host's separately reported draw/upload/present duration for glass.
    useful_schedule=$((latency + first_record_to_plan))
    present_ns=$((present_us * 1000))
    useful_glass=$((useful_schedule + present_ns))

    printf '\n'
    printf 'MEASURED-BASIS first_frame host=%s-%s optimize=%s phux=%s publisher_pid=%s\n' \
        "$(uname -m)" "$(sw_vers -productVersion)" "$OPTIMIZE" "$PHUX" "$publisher"
    printf 'MEASURED first_frame build_duration_ns=%s process_spawn_wall_ns=%s process_to_runner_ns=%s\n' \
        "$BUILD_DURATION_NS" "$LAUNCH_STARTED_NS" "$process_to_runner"
    printf 'MEASURED first_frame scene_loaded_wall_ns=%s sdk_first_present_recorded_wall_ns=%s scene_to_sdk_record_ns=%s\n' \
        "$scene_loaded" "$first_recorded" "$scene_to_first_record"
    printf 'MEASURED first_frame first_plan_done_wall_ns=%s sdk_record_to_plan_done_ns=%s useful_frame_schedulable_ns=%s\n' \
        "$plan_done" "$first_record_to_plan" "$useful_schedule"
    printf 'MEASURED first_frame gpu_first_frame_latency_ns=%s gpu_first_frame_latency_budget_ns=%s gpu_first_frame_latency_budget_exceeded=%s\n' \
        "$latency" "$budget" "$exceeded"
    printf 'MEASURED first_frame host_present_us=%s useful_frame_to_glass_ns=%s process_to_nonblank_snapshot_observed_upper_bound_ns=%s\n' \
        "$present_us" "$useful_glass" "$((observed_ns - LAUNCH_STARTED_NS))"

    if (( latency > budget || useful_glass > budget )); then
        printf 'first-frame: useful frame missed %s ns to glass (sdk=%s, schedulable=%s, to_glass=%s)\n' \
            "$budget" "$latency" "$useful_schedule" "$useful_glass" >&2
        return 1
    fi
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --debug) OPTIMIZE="Debug" ;;
        --automation) AUTOMATION=1 ;;
        --config) CONFIG="${2:?--config needs a path}"; shift ;;
        --fresh) FRESH=1 ;;
        --detach) DETACH=1 ;;
        --no-build) BUILD=0 ;;
        --phux) PHUX=1 ;;
        --ffi-profile) FFI_PROFILE="${2:?--ffi-profile needs a profile}"; shift ;;
        --measure-first-frame) MEASURE_FIRST_FRAME=1; AUTOMATION=1 ;;
        -h|--help) sed -n '2,/^set -euo pipefail/{ /^set -euo pipefail/!p; }' "$0"; exit 0 ;;
        *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
    shift
done

if [[ "$BUILD" == "0" && "$MEASURE_FIRST_FRAME" == "1" ]]; then
    printf 'error: --measure-first-frame requires a build to establish its optimize/provider/automation configuration\n' >&2
    exit 2
fi

[[ "$(uname -s)" == "Darwin" ]] || { printf 'error: macOS only\n' >&2; exit 1; }

if [[ "$FRESH" == "1" ]]; then
    rm -rf -- "$DEV_HOME"
    printf 'fresh: removed %s\n' "$DEV_HOME"
fi
dev_app_home_init "$DEV_HOME"
CONFIG="${CONFIG:-${DEV_HOME}/config}"

if [[ "$BUILD" == "1" ]]; then
    if [[ "$MEASURE_FIRST_FRAME" == "1" ]]; then
        build_started_ns="$(wall_ns)"
    fi
    build_args=(package "-Doptimize=${OPTIMIZE}")
    [[ "$AUTOMATION" == "1" ]] && build_args+=(-Dautomation=true)
    [[ "$PHUX" == "1" ]] && build_args+=(-Dphux-enabled=true)
    build_args+=("-Dphux-client-ffi-profile=${FFI_PROFILE}")
    printf 'building: zig-build.sh %s\n' "${build_args[*]}"
    # A cold ReleaseSafe package build measured 1m41s on this machine, and an
    # unchanged rebuild 1.9s (`time zig build package -Doptimize=ReleaseSafe`,
    # 2026-08-12, M-series). The build is not the friction this command exists
    # to remove, so it is not worth defaulting to a Debug binary to avoid it.
    ( cd "$ROOT" && ./scripts/zig-build.sh "${build_args[@]}" )
    if [[ "$MEASURE_FIRST_FRAME" == "1" ]]; then
        BUILD_DURATION_NS=$(( $(wall_ns) - build_started_ns ))
    fi
fi

EXECUTABLE="$(dev_app_stage "${ROOT}/zig-out/package/phux-cockpit.app" "$STAGED_APP")"

read -r staged_id staged_executable staged_name <<<"$(dev_app_identity "$STAGED_APP")"
APP_INSTANCE_NAME="$staged_executable"
printf '\n'
printf 'running:  %s\n' "$STAGED_APP"
if [[ "$BUILD" == "1" ]]; then
    printf '  build:      %s%s\n' "$OPTIMIZE" "$([[ "$AUTOMATION" == "1" ]] && printf ', automation' || true)"
else
    printf '  build:      existing package; optimize/provider/automation/FFI profile UNKNOWN\n'
    printf '              build flags have no effect with --no-build; verify the live publisher\n'
fi
BINARY_SHA="$(shasum -a 256 "$EXECUTABLE" | cut -d ' ' -f 1)"
printf '  binary sha: %s\n' "$BINARY_SHA"
printf '  bundle id:  %s\n' "$staged_id"
printf '  process:    %s      (pgrep -x %s)\n' "$staged_executable" "$staged_executable"
printf '  menu name:  %s\n' "$staged_name"
printf '  config:     %s\n' "$CONFIG"
printf '  state:      %s\n' "$(dev_app_state_path "$DEV_HOME")"
if [[ -d "$DEV_APP_INSTALLED_BUNDLE" ]]; then
    read -r installed_id installed_executable _ <<<"$(dev_app_identity "$DEV_APP_INSTALLED_BUNDLE")"
    printf '  not:        %s (%s, %s, v%s)\n' \
        "$DEV_APP_INSTALLED_BUNDLE" "$installed_id" "$installed_executable" \
        "$(dev_app_plist_value "$DEV_APP_INSTALLED_BUNDLE" CFBundleShortVersionString)"
fi
printf '\n'

LOG="${DEV_HOME}/app.log"
launch_env=()
if [[ "$MEASURE_FIRST_FRAME" == "1" ]]; then
    automation_dir="${DEV_HOME}/.zig-cache/native-sdk-automation"
    rm -f -- "${automation_dir}/snapshot.txt"
    LAUNCH_STARTED_NS="$(wall_ns)"
    launch_env+=(NATIVE_SDK_WINDOW_TIMING=1 NATIVE_SDK_GPU_DRAW_TRACE=1)
fi
dev_app_launch "$EXECUTABLE" "$DEV_HOME" "$CONFIG" "$LOG" "${launch_env[@]}"
printf 'pid %s, log %s\n' "$DEV_APP_PID" "$LOG"

cleanup() {
    [[ "$DETACH" == "1" ]] && return 0
    kill "$DEV_APP_PID" 2>/dev/null || true
}
trap cleanup EXIT

# Fail here rather than leaving a half-started app: if the name does not resolve
# to exactly this pid, every later `pgrep -x` and every System Events activation
# in this session is already ambiguous.
dev_app_wait_named "$staged_executable" "$DEV_APP_PID"
if [[ "$MEASURE_FIRST_FRAME" == "1" ]] && ! measure_first_frame; then
    # A failed detached measurement must not leave the failed subject running.
    DETACH=0
    exit 1
fi

# Front it by name. This is the activation phux-cockpit-2ml.10 records as
# unreliable-by-name with two instances up -- which is exactly what the renamed
# executable fixes: "phux-cockpit-dev" cannot resolve to the installed app.
#
# Retried, because the process exists (pgrep sees it) several seconds before
# System Events does, and a single attempt right after launch fails on a
# perfectly healthy app -- it reported "needs Accessibility permission" on a
# machine that had the grant. Failure after the deadline is still not fatal: a
# machine without the grant cannot be activated this way at all, and the app is
# already running.
front_deadline=$((SECONDS + 15))
until osascript -e "tell application \"System Events\" to set frontmost of process \"${staged_executable}\" to true" >/dev/null 2>&1; do
    if [[ "$SECONDS" -ge "$front_deadline" ]]; then
        printf 'note: could not front the window in 15s (System Events may need an Accessibility grant)\n'
        break
    fi
    sleep 0.5
done

if [[ "$AUTOMATION" == "1" && "$BUILD" == "1" ]]; then
    printf '\nautomation is on. The dropbox is resolved against the app CWD, which\n'
    printf 'is the dev home, so drive it from there:\n\n'
    printf '  eval "$(%s/scripts/build-automation-cli.sh --export)"\n' "$ROOT"
    printf '  (cd %s && "$NATIVE" automate wait && "$NATIVE" automate assert '"'"'ready=true'"'"')\n\n' "$DEV_HOME"
fi

if [[ "$DETACH" == "1" ]]; then
    printf 'detached. kill %s to stop it.\n' "$DEV_APP_PID"
    exit 0
fi

printf 'attached. ctrl-c to quit.\n'
status=0
wait "$DEV_APP_PID" || status=$?
printf 'app exited (status %s). log: %s\n' "$status" "$LOG"
exit "$status"
