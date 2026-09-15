#!/usr/bin/env bash
# Hermetic regression coverage for the detached dev launcher.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/dev-run-test.XXXXXX")"
RUNNER_PID=""

cleanup() {
    local pid
    for pid_file in "${WORK}/runner.pid" "${WORK}/osascript.pid" "${WORK}/app.pid"; do
        [[ -f "$pid_file" ]] || continue
        pid="$(cat "$pid_file")"
        [[ "$pid" =~ ^[0-9]+$ ]] || continue
        kill -KILL "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
    rm -rf -- "$WORK"
}
trap cleanup EXIT

mkdir -p "${WORK}/fixture/scripts/lib" "${WORK}/bin" "${WORK}/home"
cp "${ROOT}/scripts/dev-run.sh" "${WORK}/fixture/scripts/dev-run.sh"
chmod +x "${WORK}/fixture/scripts/dev-run.sh"

cat >"${WORK}/fixture/scripts/lib/dev-app.sh" <<'LIB'
DEV_APP_INSTALLED_BUNDLE=/fixture/does-not-exist.app

dev_app_home_init() {
    mkdir -p -- "$1/dev.phux.cockpit"
    : >"$1/config"
}

dev_app_state_path() {
    printf '%s/dev.phux.cockpit/workspace.state\n' "$1"
}

dev_app_stage() {
    printf '%s\n' "$FIXTURE_EXECUTABLE"
}

dev_app_identity() {
    printf 'dev.fixture phux-cockpit-dev Fixture\n'
}

dev_app_launch() {
    local log="$4"
    /bin/sleep 30 >"$log" 2>&1 &
    DEV_APP_PID=$!
    printf '%s\n' "$DEV_APP_PID" >"$FIXTURE_APP_PID_FILE"
}

dev_app_wait_named() {
    return 0
}
LIB
: >"${WORK}/fixture/scripts/lib/app-instance.sh"

cat >"${WORK}/bin/osascript" <<'SCRIPT'
#!/bin/sh
printf '%s\n' "$$" >"$FIXTURE_OSASCRIPT_PID_FILE"
exec /bin/sleep 30
SCRIPT
chmod +x "${WORK}/bin/osascript"

printf '#!/bin/sh\nexit 0\n' >"${WORK}/bin/fake-app"
chmod +x "${WORK}/bin/fake-app"

export FIXTURE_APP_PID_FILE="${WORK}/app.pid"
export FIXTURE_EXECUTABLE="${WORK}/bin/fake-app"
export FIXTURE_OSASCRIPT_PID_FILE="${WORK}/osascript.pid"
export PHUX_COCKPIT_DEV_HOME="${WORK}/home"
export PHUX_COCKPIT_FRONT_TIMEOUT_SECONDS=1
PATH="${WORK}/bin:${PATH}"
export PATH

# Historical RED on ae6dc1a2: the mock System Events request never returned,
# so the launcher was still alive after this test's three-second ceiling even
# though it had already printed a healthy app pid. The production 15-second
# deadline surrounded a synchronous osascript call and could not interrupt it.
"${WORK}/fixture/scripts/dev-run.sh" --no-build --detach >"${WORK}/output" 2>&1 &
RUNNER_PID=$!
printf '%s\n' "$RUNNER_PID" >"${WORK}/runner.pid"

for ((attempt = 0; attempt < 60; attempt++)); do
    kill -0 "$RUNNER_PID" 2>/dev/null || break
    /bin/sleep 0.05
done

if kill -0 "$RUNNER_PID" 2>/dev/null; then
    printf 'FAIL: dev-run --detach stayed blocked after its fronting deadline\n' >&2
    printf '%s\n' 'output:' >&2
    cat "${WORK}/output" >&2
    exit 1
fi
wait "$RUNNER_PID"
RUNNER_PID=""
rm -f -- "${WORK}/runner.pid"

grep -Fq 'note: could not front the window in 1s' "${WORK}/output"
grep -Fq 'detached. kill ' "${WORK}/output"

app_pid="$(cat "${WORK}/app.pid")"
if ! kill -0 "$app_pid" 2>/dev/null; then
    printf 'FAIL: --detach did not leave the launched app running\n' >&2
    exit 1
fi

osascript_pid="$(cat "${WORK}/osascript.pid")"
if kill -0 "$osascript_pid" 2>/dev/null; then
    printf 'FAIL: timed-out osascript process %s was left running\n' "$osascript_pid" >&2
    exit 1
fi
rm -f -- "${WORK}/osascript.pid"

printf 'ok: detached launch bounds and reaps a blocked fronting attempt\n'
