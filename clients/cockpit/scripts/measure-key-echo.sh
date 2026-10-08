#!/usr/bin/env bash
# Measure keystroke to echo on the coordinator path: a text key leaving the
# Phux host until the next grid damage of the same terminal lands back in it.
#
#   ./scripts/measure-key-echo.sh [--keys N] [--max-p99-us US] [--keep]
# measures: keystroke to echoed grid damage on the coordinator path (p50/p90/p99)
#
# The number is the round trip through the FFI, the socket, the coordinator's
# PTY, the shell's echo, and the published grid, measured by the host's own
# clock (host.zig EchoProbe, on when PHUX_COCKPIT_KEY_ECHO is set). What is
# left to glass is the SDK's paint and present, which the same run reports
# beside it from the frame profile when its ring is full. The app runs
# identity-staged with its own HOME, config, state, and socket, so a
# developer's coordinator is never touched; the bundled CLI starts and stops
# the coordinator the run attaches to.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
. "${ROOT}/scripts/lib/measure.sh"

KEYS=160
# The pinned ceiling; docs/MEASUREMENT.md "Key echo" names the run it came from.
MAX_P99_US=20000
KEEP=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --keys) shift; KEYS="${1:?--keys needs a count}" ;;
        --max-p99-us) shift; MAX_P99_US="${1:?--max-p99-us needs microseconds}" ;;
        --keep) KEEP=1 ;;
        -h|--help) sed -n '2,5p' "$0"; exit 0 ;;
        *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
    shift
done
if [[ ! "$KEYS" =~ ^[0-9]+$ ]] || (( KEYS < MEASURE_SAMPLE_FLOOR || KEYS > 2000 )); then
    printf -- '--keys must be an integer from %s through 2000\n' "$MEASURE_SAMPLE_FLOOR" >&2
    exit 2
fi
[[ "$(uname -s)" == Darwin ]] || { printf 'macOS live .app only\n' >&2; exit 1; }

WORK="${TMPDIR:-/tmp}/phux-cockpit-key-echo.$$"
mkdir -p "$WORK/.config" "$WORK/.local/state" "$WORK/.cache"
SOCKET="${WORK}/phux.sock"
APP_PID=""
STAGED_CLI=""

cleanup() {
    if [[ -n "$APP_PID" && "$KEEP" == 0 ]]; then
        app_instance_stop "$APP_PID"
    fi
    if [[ -n "$STAGED_CLI" && -S "$SOCKET" ]]; then
        "$STAGED_CLI" --socket "$SOCKET" kill --server >/dev/null 2>&1 || true
    fi
    if [[ -n "$APP_PID" && "$KEEP" == 1 ]]; then
        measure_print_retained_run "$APP_PID" "$NATIVE" "$MEASURE_DROPBOX"
        printf 'retained log: %s\n' "${WORK}/app.log"
    fi
    [[ "$KEEP" == 1 ]] || rm -rf "$WORK"
}
trap cleanup EXIT

NATIVE="$("${ROOT}/scripts/build-automation-cli.sh")"
printf 'building the iteration FFI...\n'
bash "${ROOT}/scripts/build-phux-artifacts.sh" ffi-dev >/dev/null
export MEASURE_PACKAGE_ARGS="-Dphux-client-ffi-profile=ffi-dev"

# PHUX_SOCKET outranks the config file (startup.zig), so the socket rides the
# environment too: a developer's own PHUX_SOCKET must never be the one measured.
# No phux-session: the fresh coordinator seeds one session, named after its
# working directory, and an unset session attaches to the current one.
printf 'phux-socket = %s\nfont-size = 13\n' "$SOCKET" >"${WORK}/config"
measure_launch_isolated "$WORK" "${WORK}/config" "${WORK}/app.log" 1 \
    PHUX_COCKPIT_KEY_ECHO=1 HOME="$WORK" PHUX_SOCKET="$SOCKET" PHUX_SESSION= PHUX_REMOTE= \
    XDG_CONFIG_HOME="${WORK}/.config" XDG_STATE_HOME="${WORK}/.local/state" \
    XDG_CACHE_HOME="${WORK}/.cache" XDG_RUNTIME_DIR="$WORK"
APP_PID="$MEASURE_APP_PID"
STAGED_CLI="${WORK}/Phux Cockpit (measure).app/Contents/MacOS/phux"

"$NATIVE" automate wait >/dev/null
app_instance_bind "$NATIVE" "$APP_PID"

# The configured launch owns no shell; wait for the coordinator's terminal.
deadline=$((SECONDS + 30))
while :; do
    snapshot="$(app_instance_snapshot)"
    if [[ "$(grep -c 'role=tab name=' <<<"$snapshot" || true)" == 1 ]] &&
        grep -q 'role=textbox name=' <<<"$snapshot"; then
        break
    fi
    if [[ "$SECONDS" -ge "$deadline" ]]; then
        printf 'FAILED: the coordinator did not admit a terminal within 30s.\n' >&2
        exit 1
    fi
    sleep 0.1
done
app_instance_assert
# Typed keys reach the engine only while the app is active (ts_engine onKey
# refuses them unfocused), so activate by PID the way the fullscreen smoke
# does. This needs a logged-in GUI session and, once, macOS Automation
# permission for the invoking terminal to control System Events.
if ! app_instance_activate 15; then
    printf 'FAILED: could not activate the measured app. Grant the invoking terminal\n' >&2
    printf 'Automation permission for System Events (System Settings > Privacy & Security\n' >&2
    printf '> Automation) and rerun.\n' >&2
    exit 1
fi
"$NATIVE" automate assert --timeout-ms 5000 'window @w1.*focused=true' >/dev/null
# Let the shell reach its prompt before the first key, and prove the probe
# is live: no sample may exist before a key is sent.
sleep 1
[[ "$(grep -c 'key_echo_us=' "${WORK}/app.log" || true)" == 0 ]] ||
    { printf 'FAILED: key echo samples exist before any key was sent.\n' >&2; exit 1; }

# One letter per widget-key, each its own keystroke, with room for the echo
# so the probe (one key in flight) sees every key rather than every Nth.
sent=0
while (( sent < KEYS )); do
    "$NATIVE" automate widget-key phux-cockpit-canvas x >/dev/null
    sent=$((sent + 1))
    if (( sent % 60 == 0 )); then
        "$NATIVE" automate widget-key phux-cockpit-canvas ctrl+u >/dev/null
    fi
    sleep 0.03
done
app_instance_assert
sleep 1

samples="$(grep -o 'key_echo_us=[0-9]*' "${WORK}/app.log" | cut -d= -f2 || true)"
count="$(printf '%s\n' "$samples" | grep -c . || true)"
measure_require_sample_floor key_echo "$count" "$MEASURE_SAMPLE_FLOOR"

stats="$(printf '%s\n' "$samples" | python3 -c '
import sys
v = sorted(int(x) for x in sys.stdin.read().split())
def pct(p):
    return v[min(len(v) - 1, int(round(p * (len(v) - 1))))]
print(f"key_echo_n={len(v)}")
print(f"key_echo_p50_us={pct(0.50)}")
print(f"key_echo_p90_us={pct(0.90)}")
print(f"key_echo_p99_us={pct(0.99)}")
print(f"key_echo_max_us={v[-1]}")
')"

measure_basis key-echo \
    "identity-staged bundle activated by pid with PHUX_COCKPIT_KEY_ECHO=1; isolated coordinator at ${SOCKET##*/} started by the bundled CLI; ${KEYS} single-letter widget-key presses 30ms apart; sample = host sendKey to the same terminal's next damage" \
    "./scripts/measure-key-echo.sh --keys ${KEYS}"
printf '%s\n' "$stats"

# The SDK's own present stage, from the same run, when its ring is full.
snapshot="$(app_instance_snapshot)"
if measure_require_profile_stages "$snapshot" "$MEASURE_SAMPLE_FLOOR" present 2>/dev/null; then
    printf '%s\n' "$snapshot" | grep -o 'frame_profile.*' | tr ' ' '\n' | grep '^present_' || true
else
    printf 'present stage: ring below the sample floor in this run; see automate-smoke --profile\n'
fi

p99="$(printf '%s\n' "$stats" | sed -n 's/^key_echo_p99_us=//p')"
if (( p99 > MAX_P99_US )); then
    printf 'FAILED: key echo p99 %sus exceeds the pinned ceiling of %sus.\n' "$p99" "$MAX_P99_US" >&2
    exit 1
fi
printf 'ok: key echo p99 %sus within the pinned ceiling of %sus\n' "$p99" "$MAX_P99_US"
