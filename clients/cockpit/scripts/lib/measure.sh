# shellcheck shell=bash
# Shared measurement conventions. Source this file; do not execute it.

[[ -n "${MEASURE_LIB_SOURCED:-}" ]] && return 0
MEASURE_LIB_SOURCED=1

MEASURE_LIB_DIR="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
MEASURE_ROOT="$(CDPATH='' cd -- "${MEASURE_LIB_DIR}/../.." && pwd)"

# shellcheck source=scripts/lib/zon.sh
source "${MEASURE_ROOT}/scripts/lib/zon.sh"
# shellcheck source=scripts/lib/dev-app.sh
source "${MEASURE_ROOT}/scripts/lib/dev-app.sh"
# shellcheck source=scripts/lib/app-instance.sh
source "${MEASURE_ROOT}/scripts/lib/app-instance.sh"

# The pinned SDK keeps a 128-entry rolling ring PER STAGE. Snapshot `_n` is the
# lifetime total, while each percentile's actual population is min(_n, 128).
# A floor above this cap is a false claim and is refused by the helper below.
MEASURE_SAMPLE_CAP=128
MEASURE_SAMPLE_FLOOR=128

# Print enough context to reproduce and compare a number without changing the
# existing MEASURED lines cited by documentation and issues.
measure_basis() {
    local name="$1" basis="$2" derive="$3"
    printf 'MEASURED-BASIS %s host=%s when=%s basis="%s" derive="%s"\n' \
        "$name" "$(uname -m)-$(sw_vers -productVersion 2>/dev/null || printf unknown)" \
        "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$basis" "$derive"
}

measure_require_sample_floor() {
    local label="$1" count="${2:-}" floor="${3:-$MEASURE_SAMPLE_FLOOR}" population
    if [[ ! "$floor" =~ ^[0-9]+$ ]] || (( floor > MEASURE_SAMPLE_CAP )); then
        printf 'REFUSING TO REPORT: %s sample floor %s exceeds the SDK rolling cap of %s.\n' \
            "$label" "${floor:-<missing>}" "$MEASURE_SAMPLE_CAP" >&2
        return 1
    fi
    if [[ ! "$count" =~ ^[0-9]+$ ]]; then
        printf 'REFUSING TO REPORT: %s sample count is %s, not a number.\n' \
            "$label" "${count:-<missing>}" >&2
        return 1
    fi
    population=$((count < MEASURE_SAMPLE_CAP ? count : MEASURE_SAMPLE_CAP))
    if (( population < floor )); then
        printf 'REFUSING TO REPORT: %s percentile population is %s (lifetime_n=%s), below the floor of %s.\n' \
            "$label" "$population" "$count" "$floor" >&2
        printf 'A percentile over that few samples is not evidence.\n' >&2
        return 1
    fi
}

# Require every named stage to be present and independently populated. Churn
# uses this before printing anything, so a full host_draw ring cannot stand in
# for absent topology work.
measure_require_profile_stages() {
    local snapshot="$1" floor="$2"
    shift 2
    local profile stage token count failed=0
    profile="$(printf '%s\n' "$snapshot" | grep -o 'frame_profile.*' | head -1)"
    if [[ -z "$profile" ]]; then
        printf 'REFUSING TO REPORT: snapshot has no frame_profile.\n' >&2
        return 1
    fi
    for stage in "$@"; do
        count=""
        for token in $profile; do
            case "$token" in
                "${stage}_n="*) count="${token##*=}"; break ;;
            esac
        done
        if ! measure_require_sample_floor "$stage" "$count" "$floor"; then
            failed=1
        fi
    done
    (( failed == 0 ))
}

# Validate every stage independently before printing its percentiles. This
# keeps a busy host_draw counter from laundering a near-empty plan/rebuild
# stage, while still reporting stages that have enough evidence.
measure_print_frame_profile() {
    local name="$1" basis="$2" derive="$3" snapshot="$4"
    local profile token label count population found=0 reportable=0 allowed='|'
    profile="$(printf '%s\n' "$snapshot" | grep -o 'frame_profile.*' | head -1)"
    if [[ -z "$profile" ]]; then
        printf 'REFUSING TO REPORT: snapshot has no frame_profile.\n' >&2
        return 1
    fi
    for token in $profile; do
        case "$token" in
            *_n=*)
                label="${token%%_n=*}"
                count="${token##*=}"
                found=$((found + 1))
                if measure_require_sample_floor "$label" "$count"; then
                    allowed="${allowed}${label}|"
                    reportable=$((reportable + 1))
                fi
                ;;
        esac
    done
    if (( found == 0 )); then
        printf 'REFUSING TO REPORT: frame_profile has no stage sample counts.\n' >&2
        return 1
    fi
    if (( reportable == 0 )); then
        printf 'REFUSING TO REPORT: no frame-profile stage meets the sample floor.\n' >&2
        return 1
    fi
    measure_basis "$name" "$basis" "$derive"
    for token in $profile; do
        case "$token" in
            *_n=*)
                label="${token%%_n=*}"
                count="${token##*=}"
                population=$((count < MEASURE_SAMPLE_CAP ? count : MEASURE_SAMPLE_CAP))
                printf '%s\n%s_population_n=%s\n' "$token" "$label" "$population"
                ;;
            *_p50_us=*|*_p90_us=*|*_max_us=*)
                label="${token%%_p[59]0_us=*}"
                label="${label%%_max_us=*}"
                [[ "$allowed" == *"|${label}|"* ]] && printf '%s\n' "$token"
                ;;
        esac
    done
    return 0
}

# Put the delivery cadence beside the deliberately conservative sum of every
# named stage's p90. `present` contains the macOS host decode/draw call, so the
# sum double-counts nested work on purpose; an interval p90 that still exceeds
# it cannot be explained by the measured synchronous stages. This is an
# arithmetic comparison of independently sampled percentiles, not a synthetic
# whole-frame percentile.
measure_print_scheduler_comparison() {
    local snapshot="$1"
    shift
    local -a pipeline_stages=("$@")
    local profile stage token value
    local stage_p90_sum=0 interval_p50="" interval_p90="" interval_max=""

    measure_require_profile_stages \
        "$snapshot" "$MEASURE_SAMPLE_FLOOR" "${pipeline_stages[@]}" interval || return 1
    profile="$(printf '%s\n' "$snapshot" | grep -o 'frame_profile.*' | head -1)"

    for stage in "${pipeline_stages[@]}"; do
        value=""
        for token in $profile; do
            case "$token" in
                "${stage}_p90_us="*) value="${token##*=}"; break ;;
            esac
        done
        if [[ ! "$value" =~ ^[0-9]+$ ]]; then
            printf 'REFUSING TO COMPARE: %s has no numeric p90 in frame_profile.\n' \
                "$stage" >&2
            return 1
        fi
        stage_p90_sum=$((stage_p90_sum + value))
    done

    for token in $profile; do
        case "$token" in
            interval_p50_us=*) interval_p50="${token##*=}" ;;
            interval_p90_us=*) interval_p90="${token##*=}" ;;
            interval_max_us=*) interval_max="${token##*=}" ;;
        esac
    done
    if [[ ! "$interval_p50" =~ ^[0-9]+$ \
        || ! "$interval_p90" =~ ^[0-9]+$ \
        || ! "$interval_max" =~ ^[0-9]+$ ]]; then
        printf 'REFUSING TO COMPARE: frame_profile has incomplete interval percentiles.\n' >&2
        return 1
    fi

    printf 'scheduler_stage_p90_sum_us=%s\n' "$stage_p90_sum"
    printf 'scheduler_interval_p50_us=%s\n' "$interval_p50"
    printf 'scheduler_interval_p90_us=%s\n' "$interval_p90"
    printf 'scheduler_interval_max_us=%s\n' "$interval_max"
}

measure_raster_comparison_basis() {
    local before_ref="$1" after_ref="$2" before_source="$3" after_source="$4" derive="$5"
    measure_basis host_raster_comparison \
        "PHUX_COCKPIT_SDK_SRC override per side; before=${before_ref} source=${before_source}; after=${after_ref} source=${after_source}" \
        "$derive"
}

measure_raster_worktree_source() {
    local work="$1" side="$2" ref="$3"
    case "$side" in before|after) ;; *) return 2 ;; esac
    printf '%s/%s-%s\n' "$work" "$side" "$ref"
}

# Package, identity-stage, and launch from HOME. The app and CLI both use HOME
# as cwd, giving this run a private automation dropbox. Sets MEASURE_APP_PID and
# replaces NATIVE with a cwd-pinned wrapper for subsequent automation calls.
measure_launch_isolated() {
    local home="$1" config="$2" log="$3"
    local source_app staged_app executable native_real wrapper
    source_app="${MEASURE_ROOT}/zig-out/package/phux-cockpit.app"
    staged_app="${home}/Phux Cockpit (measure).app"
    native_real="${NATIVE:?set NATIVE to the automation CLI before launching}"

    printf 'packaging with automation...\n'
    ( cd "$MEASURE_ROOT" && zig build package -Dautomation=true >/dev/null )
    executable="$(dev_app_stage "$source_app" "$staged_app")"

    # dev_app_stage gives measurements a process identity distinct from the
    # installed app. Refuse another staged dev/measurement process because
    # System Events still targets this name globally.
    # shellcheck disable=SC2034 # consumed by app-instance.sh after sourcing
    APP_INSTANCE_NAME="$(basename -- "$executable")"
    app_instance_require_free

    wrapper="${home}/native-in-measure-home"
    printf '#!/usr/bin/env bash\nexec env -C %q %q "$@"\n' "$home" "$native_real" >"$wrapper"
    chmod +x "$wrapper"
    NATIVE="$wrapper"
    # shellcheck disable=SC2034 # output variable consumed by --keep callers
    MEASURE_DROPBOX="${home}/.zig-cache/native-sdk-automation"

    dev_app_launch "$executable" "$home" "$config" "$log"
    # shellcheck disable=SC2034 # output variable consumed by the caller
    MEASURE_APP_PID="$DEV_APP_PID"
}

measure_print_retained_run() {
    local pid="$1" wrapper="$2" dropbox="$3"
    printf '\nretained app pid: %s\n' "$pid"
    printf 'retained automation command: '
    printf '%q' "$wrapper"
    printf ' automate snapshot\nretained automation dropbox: %s\n' "$dropbox"
}
