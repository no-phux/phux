#!/usr/bin/env bash
# Hermetic tests for measurement policy and launcher composition.
set -uo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/lib/measure.sh
source "${ROOT}/scripts/lib/measure.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
pass=0
fail=0

ok() { pass=$((pass + 1)); }
bad() { printf 'FAIL: %s\n' "$1" >&2; fail=$((fail + 1)); }
check() { if "$@"; then ok; else bad "$*"; fi; }

check measure_require_sample_floor host_draw 128 128
check measure_require_sample_floor host_draw 900 128
if measure_require_sample_floor rebuild 127 128 >/dev/null 2>&1; then
    bad 'sample below floor was accepted'
else
    ok
fi
if measure_require_sample_floor rebuild missing 128 >/dev/null 2>&1; then
    bad 'non-numeric sample count was accepted'
else
    ok
fi
if measure_require_sample_floor rebuild 900 129 > /dev/null 2>"${WORK}/cap.err"; then
    bad 'floor above rolling cap was accepted'
elif grep -q 'exceeds the SDK rolling cap of 128' "${WORK}/cap.err"; then
    ok
else
    bad 'floor above rolling cap was not explained'
fi
# Watched red with MEASURE_SAMPLE_CAP raised to 1024: the test accepted the
# impossible 129-sample floor and reported the wrong populations.

good_profile='frame_profile host_draw_n=310 host_draw_p50_us=4 host_draw_p90_us=8 host_draw_max_us=11 rebuild_n=180 rebuild_p50_us=2 rebuild_p90_us=3 rebuild_max_us=5'
if output="$(measure_print_frame_profile test basis derive "$good_profile")"; then
    if [[ "$output" == *'MEASURED-BASIS test '* \
        && "$output" == *'rebuild_p90_us=3'* \
        && "$output" == *'host_draw_max_us=11'* \
        && "$output" == *'host_draw_population_n=128'* \
        && "$output" == *'rebuild_population_n=128'* ]]; then
        ok
    else
        bad 'valid profile did not use common output'
    fi
else
    bad 'valid profile was refused'
fi
low_profile='frame_profile host_draw_n=310 host_draw_p50_us=4 host_draw_p90_us=8 rebuild_n=12 rebuild_p50_us=2 rebuild_p90_us=3'
# Watched red with the per-stage output filter disabled: the test printed
# rebuild_p90_us=3 and failed "under-sampled stage percentile ... refused".
if low_output="$(measure_print_frame_profile test basis derive "$low_profile" 2>"${WORK}/low.err")"; then
    if [[ "$low_output" == *'host_draw_p90_us=8'* && "$low_output" != *'rebuild_p90_us=3'* ]] \
        && grep -q 'rebuild percentile population is 12' "${WORK}/low.err"; then
        ok
    else
        bad 'under-sampled stage percentile was not independently refused'
    fi
else
    bad 'one low stage hid independently reportable stages'
fi

required_profile='frame_profile rebuild_n=140 rebuild_p50_us=1 rebuild_p90_us=2 layout_n=128 layout_p50_us=1 layout_p90_us=2 plan_n=127 plan_p50_us=1 plan_p90_us=2'
# Watched red with the required-stage loop reduced to its first stage: both the
# under-populated `plan` and an absent named stage were accepted.
if measure_require_profile_stages "$required_profile" 128 rebuild layout plan >/dev/null 2>"${WORK}/required.err"; then
    bad 'under-populated required topology stage was accepted'
elif grep -q 'plan percentile population is 127' "${WORK}/required.err"; then
    ok
else
    bad 'required topology-stage refusal did not name the stage population'
fi
if measure_require_profile_stages "$required_profile" 128 rebuild absent >/dev/null 2>"${WORK}/absent.err"; then
    bad 'absent required topology stage was accepted'
elif grep -q 'absent sample count is <missing>' "${WORK}/absent.err"; then
    ok
else
    bad 'absent topology-stage refusal did not name the stage'
fi

scheduler_profile='frame_profile rebuild_n=128 rebuild_p50_us=2 rebuild_p90_us=3 rebuild_max_us=5 host_draw_n=256 host_draw_p50_us=4 host_draw_p90_us=8 host_draw_max_us=13 interval_n=256 interval_p50_us=16968 interval_p90_us=36222 interval_max_us=50000'
if scheduler="$(measure_print_scheduler_comparison "$scheduler_profile" rebuild host_draw)"; then
    if [[ "$scheduler" == *'scheduler_stage_p90_sum_us=11'* \
        && "$scheduler" == *'scheduler_interval_p50_us=16968'* \
        && "$scheduler" == *'scheduler_interval_p90_us=36222'* \
        && "$scheduler" == *'scheduler_interval_max_us=50000'* ]]; then
        ok
    else
        bad 'scheduler comparison did not separate stage and interval timing'
    fi
else
    bad 'fully populated scheduler profile was refused'
fi

# Replace only external mechanics. The function under test must still compose
# package -> dev_app_stage -> same-name refusal -> dev_app_launch, and must pin
# the CLI to the launch cwd.
mkdir -p "${WORK}/bin" "${WORK}/home"
printf '#!/bin/sh\nexit 0\n' >"${WORK}/bin/zig"
chmod +x "${WORK}/bin/zig"
PATH="${WORK}/bin:${PATH}"
export PATH
dev_app_stage() {
    printf '%s\n' "$*" >"${WORK}/stage.args"
    printf '%s\n' "${WORK}/home/App.app/Contents/MacOS/phux-cockpit-dev"
}
app_instance_require_free() { printf '%s\n' "$APP_INSTANCE_NAME" >"${WORK}/guard.name"; }
dev_app_launch() {
    printf '%s\n' "$*" >"${WORK}/launch.args"
    DEV_APP_PID=4242
}
NATIVE=/bin/echo
measure_launch_isolated "${WORK}/home" "${WORK}/home/config" "${WORK}/home/app.log"
if [[ "$MEASURE_APP_PID" == 4242 ]]; then ok; else bad 'launcher did not return dev_app pid'; fi
if [[ "$(cat "${WORK}/guard.name")" == phux-cockpit-dev ]]; then ok; else bad 'guard did not use staged process identity'; fi
if grep -q 'Phux Cockpit (measure).app' "${WORK}/stage.args"; then ok; else bad 'launcher did not identity-stage bundle'; fi
if grep -q "${WORK}/home ${WORK}/home/config ${WORK}/home/app.log" "${WORK}/launch.args"; then
    ok
else
    bad 'launcher did not delegate home/config/log to dev_app_launch'
fi
if [[ "$NATIVE" == "${WORK}/home/native-in-measure-home" ]]; then ok; else bad 'automation CLI was not cwd-pinned'; fi
if grep -q 'exec env -C' "$NATIVE"; then ok; else bad 'CLI wrapper does not enter isolated dropbox cwd'; fi
if [[ "$MEASURE_DROPBOX" == "${WORK}/home/.zig-cache/native-sdk-automation" ]]; then ok; else bad 'launcher did not expose exact dropbox'; fi

retained="$(measure_print_retained_run 4242 "$NATIVE" "$MEASURE_DROPBOX")"
if [[ "$retained" == *"retained automation command: ${NATIVE} automate snapshot"* \
    && "$retained" == *"retained automation dropbox: ${MEASURE_DROPBOX}"* ]]; then
    ok
else
    bad 'retained-run output omitted the exact wrapper command or dropbox'
fi

raster_basis="$(measure_raster_comparison_basis before-sha after-sha /work/before /work/after './scripts/host-raster-compare.sh before after')"
if [[ "$raster_basis" == *'before=before-sha source=/work/before'* \
    && "$raster_basis" == *'after=after-sha source=/work/after'* \
    && "$raster_basis" == *'PHUX_COCKPIT_SDK_SRC override per side'* ]]; then
    ok
else
    bad 'raster comparison basis omitted refs or override sources'
fi

equal_before="$(measure_raster_worktree_source /work before same-sha)"
equal_after="$(measure_raster_worktree_source /work after same-sha)"
# Watched red when `side` was removed from the path: both resolved to
# /work/same-sha and the second worktree collided with the first.
if [[ "$equal_before" == /work/before-same-sha \
    && "$equal_after" == /work/after-same-sha \
    && "$equal_before" != "$equal_after" ]]; then
    ok
else
    bad 'equivalent raster refs did not receive distinct worktree paths'
fi

catalog="$("${ROOT}/scripts/measure.sh")"
if [[ "$catalog" == *'automate-smoke'* && "$catalog" == *'host-raster-check'* ]]; then
    ok
else
    bad 'catalog did not discover tagged measurements'
fi

printf '%s passed, %s failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
