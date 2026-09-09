#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
Usage:
  soak-macos-app.sh --app PATH [options]

Required:
  --app PATH                 macOS application bundle to soak

Options:
  --cycles COUNT             launch/termination cycles (default: 10)
  --expected-shells COUNT    coordinator-seeded shells expected after
                             launch (default: 1)
  --startup-timeout SECONDS  time to wait for the coordinator and its
                             shells (default: 20)
  --shutdown-timeout SECONDS time to wait for each clean shutdown
                             (default: 10)
  --cli PATH                 Phux CLI used to stop the coordinator
                             (default: the bundle's Contents/MacOS/phux)
  --artifacts PATH           failure diagnostics directory
                             (default: ./soak-artifacts)
  -h, --help                 show this help

The bundle executable is launched directly. Each cycle gets an isolated HOME,
XDG directories, ZDOTDIR with a controlled .zshrc, and its own PHUX_SOCKET, so
a developer's real coordinator is never touched. No Accessibility permission
is needed.

What one cycle proves (the shipping app's process-lifecycle contract):

  1. A fresh configured launch owns no shell itself. It starts a coordinator
     beside the bundled CLI (`phux server --ensure`), and that coordinator's
     seeded shell runs under the controlled environment.
  2. The app reaps its startup helper: no zombie child survives startup.
  3. SIGTERM ends the app inside the shutdown budget while the coordinator
     and its shell keep running. Durable work outlives the window.
  4. `phux kill --server` through the bundled CLI ends the coordinator, its
     shell, and its socket inside the shutdown budget, leaving no zombie or
     orphan behind.
EOF
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

require_value() {
    [[ "$#" -ge 2 && -n "$2" ]] || die "$1 requires a value"
}

valid_path() {
    [[ -n "$1" && "$1" != *[[:cntrl:]]* ]]
}

APP=''
CLI=''
CYCLES=10
EXPECTED_SHELLS=1
STARTUP_TIMEOUT=20
SHUTDOWN_TIMEOUT=10
ARTIFACTS="${PWD}/soak-artifacts"

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --app)
            require_value "$@"
            APP="$2"
            shift 2
            ;;
        --cli)
            require_value "$@"
            CLI="$2"
            shift 2
            ;;
        --cycles)
            require_value "$@"
            CYCLES="$2"
            shift 2
            ;;
        --expected-shells)
            require_value "$@"
            EXPECTED_SHELLS="$2"
            shift 2
            ;;
        --startup-timeout)
            require_value "$@"
            STARTUP_TIMEOUT="$2"
            shift 2
            ;;
        --shutdown-timeout)
            require_value "$@"
            SHUTDOWN_TIMEOUT="$2"
            shift 2
            ;;
        --artifacts)
            require_value "$@"
            ARTIFACTS="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "unknown argument: $1"
            ;;
    esac
done

[[ -n "${APP}" ]] || die '--app is required'
valid_path "${APP}" || die '--app must be a printable path'
valid_path "${ARTIFACTS}" || die '--artifacts must be a printable path'
[[ -z "${CLI}" ]] || valid_path "${CLI}" || die '--cli must be a printable path'
[[ "${CYCLES}" =~ ^[1-9][0-9]*$ ]] || die '--cycles must be a positive integer'
[[ "${EXPECTED_SHELLS}" =~ ^[1-9][0-9]*$ ]] || die '--expected-shells must be a positive integer'
[[ "${STARTUP_TIMEOUT}" =~ ^[1-9][0-9]*$ ]] ||
    die '--startup-timeout must be a positive integer'
[[ "${SHUTDOWN_TIMEOUT}" =~ ^[1-9][0-9]*$ ]] ||
    die '--shutdown-timeout must be a positive integer'

[[ "$(/usr/bin/uname -s)" == 'Darwin' ]] || die 'this soak test requires macOS'
for tool in /bin/cp /bin/kill /bin/mkdir /bin/ps /bin/rm /bin/sleep \
    /usr/bin/mktemp /usr/bin/pgrep /usr/bin/plutil; do
    [[ -x "${tool}" ]] || die "required macOS tool not found: ${tool}"
done

[[ -d "${APP}" ]] || die "application bundle not found: ${APP}"
PLIST="${APP}/Contents/Info.plist"
[[ -f "${PLIST}" ]] || die "Info.plist not found: ${PLIST}"
EXECUTABLE_NAME="$(/usr/bin/plutil -extract CFBundleExecutable raw -o - "${PLIST}" 2>/dev/null)" ||
    die 'Info.plist has no CFBundleExecutable'
[[ -n "${EXECUTABLE_NAME}" && "${EXECUTABLE_NAME}" != */* ]] ||
    die 'CFBundleExecutable must be a file name'
EXECUTABLE="${APP}/Contents/MacOS/${EXECUTABLE_NAME}"
[[ -f "${EXECUTABLE}" && -x "${EXECUTABLE}" ]] ||
    die "bundle executable is missing or not executable: ${EXECUTABLE}"
# The app discovers the coordinator CLI beside its own executable; the soak
# stops the coordinator through that same binary unless told otherwise.
[[ -n "${CLI}" ]] || CLI="${APP}/Contents/MacOS/phux"
[[ -f "${CLI}" && -x "${CLI}" ]] ||
    die "Phux CLI is missing or not executable: ${CLI}"
if [[ -e "${ARTIFACTS}" && ! -d "${ARTIFACTS}" ]]; then
    die "artifacts path is not a directory: ${ARTIFACTS}"
fi

# sockaddr_un leaves 104 bytes for a path on macOS; per-cycle sockets live in
# a short /tmp directory rather than under TMPDIR's deep per-user folder.
MAX_SOCKET_BYTES=100
ANOMALY_POLLS=5

RUN_DIR=''
SOCKET_DIR=''
CURRENT_CYCLE=0
CURRENT_CYCLE_DIR=''
CURRENT_PID_FILE=''
CURRENT_SOCKET=''
CURRENT_APP_PID=''
CURRENT_APP_IDENTITY=''
COORDINATOR_PID=''
COORDINATOR_IDENTITY=''
TRACKED_SHELL_PIDS=()
TRACKED_SHELL_IDENTITIES=()
RECORDED_SHELL_PIDS=()
PID_FILE_MALFORMED='false'
LAST_ARTIFACT=''

process_identity() {
    /bin/ps -ww -p "$1" -o lstart= -o command= 2>/dev/null
}

process_state() {
    /bin/ps -p "$1" -o state= 2>/dev/null | /usr/bin/tr -d '[:space:]'
}

process_ppid() {
    /bin/ps -p "$1" -o ppid= 2>/dev/null | /usr/bin/tr -d '[:space:]'
}

process_comm() {
    local comm

    comm="$(/bin/ps -p "$1" -o comm= 2>/dev/null)" || return 1
    comm="${comm//[[:space:]]/}"
    comm="${comm##*/}"
    comm="${comm#-}"
    printf '%s\n' "${comm}"
}

identity_matches() {
    local pid="$1"
    local expected="$2"
    local identity

    [[ -n "${pid}" && -n "${expected}" ]] || return 1
    identity="$(process_identity "${pid}" || true)"
    [[ -n "${identity}" && "${identity}" == "${expected}" ]]
}

app_identity_matches() {
    identity_matches "${CURRENT_APP_PID}" "${CURRENT_APP_IDENTITY}"
}

coordinator_identity_matches() {
    identity_matches "${COORDINATOR_PID}" "${COORDINATOR_IDENTITY}"
}

is_zombie() {
    [[ "$(process_state "$1" || true)" == Z* ]]
}

shell_is_tracked() {
    local candidate="$1"
    local pid

    for pid in "${TRACKED_SHELL_PIDS[@]:-}"; do
        [[ "${pid}" == "${candidate}" ]] && return 0
    done
    return 1
}

track_shell() {
    local pid="$1"
    local identity

    [[ "${pid}" =~ ^[1-9][0-9]*$ ]] || return 1
    shell_is_tracked "${pid}" && return 0
    [[ "$(process_comm "${pid}" || true)" == 'zsh' ]] || return 1
    identity="$(process_identity "${pid}" || true)"
    [[ -n "${identity}" ]] || return 1
    TRACKED_SHELL_PIDS+=("${pid}")
    TRACKED_SHELL_IDENTITIES+=("${identity}")
}

tracked_shell_identity_matches() {
    identity_matches "${TRACKED_SHELL_PIDS[$1]}" "${TRACKED_SHELL_IDENTITIES[$1]}"
}

# Coordinator-owned shells are found by identity, never by walking the tree:
# the coordinator is a detached daemon, so its children are not the app's.
load_recorded_shells() {
    local pid extra seen
    local existing

    RECORDED_SHELL_PIDS=()
    PID_FILE_MALFORMED='false'
    [[ -f "${CURRENT_PID_FILE}" ]] || return 0
    while IFS=' ' read -r pid extra; do
        if [[ ! "${pid}" =~ ^[1-9][0-9]*$ || -n "${extra:-}" ]]; then
            PID_FILE_MALFORMED='true'
            continue
        fi
        seen='false'
        for existing in "${RECORDED_SHELL_PIDS[@]:-}"; do
            [[ "${existing}" == "${pid}" ]] && seen='true'
        done
        if [[ "${seen}" == 'true' ]]; then
            PID_FILE_MALFORMED='true'
        else
            RECORDED_SHELL_PIDS+=("${pid}")
        fi
    done < "${CURRENT_PID_FILE}"
}

# The daemon's argv names the socket the app asked for, which is what ties it
# to this cycle. The app's own `--socket PATH server --ensure` helper and the
# soak's `--socket PATH kill --server` order the words differently.
find_coordinator() {
    local pids

    pids="$(/usr/bin/pgrep -f -- "server --socket ${CURRENT_SOCKET}( |$)" 2>/dev/null || true)"
    [[ -n "${pids}" ]] || return 1
    [[ "${pids}" != *$'\n'* ]] || return 2
    printf '%s\n' "${pids}"
}

adopt_coordinator() {
    local pid

    [[ -z "${COORDINATOR_PID}" ]] || return 0
    pid="$(find_coordinator)" || {
        [[ "$?" -ne 2 ]] || fail_cycle "more than one coordinator claims socket ${CURRENT_SOCKET}"
        return 1
    }
    [[ "$(process_comm "${pid}" || true)" == 'phux' ]] ||
        fail_cycle "coordinator ${pid} is not a phux process"
    COORDINATOR_PID="${pid}"
    COORDINATOR_IDENTITY="$(process_identity "${pid}" || true)"
    [[ -n "${COORDINATOR_IDENTITY}" ]] || fail_cycle "could not establish identity for coordinator ${pid}"
}

app_children_are_healthy() {
    local pid

    while IFS= read -r pid; do
        pid="${pid//[[:space:]]/}"
        [[ -n "${pid}" ]] || continue
        is_zombie "${pid}" && fail_cycle "app left child ${pid} as a zombie during startup"
        [[ "$(process_comm "${pid}" || true)" != 'zsh' ]] ||
            fail_cycle "app spawned a direct shell ${pid} on a coordinator-configured launch"
    done < <(/usr/bin/pgrep -P "${CURRENT_APP_PID}" 2>/dev/null || true)
}

recorded_shells_are_ready() {
    local pid

    load_recorded_shells
    [[ "${PID_FILE_MALFORMED}" == 'false' ]] ||
        fail_cycle 'controlled .zshrc produced malformed or duplicate shell PID records'
    [[ "${#RECORDED_SHELL_PIDS[@]}" -le "${EXPECTED_SHELLS}" ]] ||
        fail_cycle "coordinator seeded too many shells (${#RECORDED_SHELL_PIDS[@]}, expected ${EXPECTED_SHELLS})"
    [[ "${#RECORDED_SHELL_PIDS[@]}" -eq "${EXPECTED_SHELLS}" ]] || return 1
    for pid in "${RECORDED_SHELL_PIDS[@]}"; do
        [[ -n "$(process_state "${pid}" || true)" ]] || fail_cycle "recorded shell ${pid} is missing"
        is_zombie "${pid}" && fail_cycle "recorded shell ${pid} is a zombie"
        [[ "$(process_ppid "${pid}" || true)" == "${COORDINATOR_PID}" ]] ||
            fail_cycle "recorded shell ${pid} is not a child of coordinator ${COORDINATOR_PID}"
        track_shell "${pid}" || fail_cycle "could not establish identity for recorded shell ${pid}"
    done
}

startup_status() {
    printf 'coordinator=%s recorded=%s' \
        "${COORDINATOR_PID:-none}" "${#RECORDED_SHELL_PIDS[@]}"
}

await_startup() {
    local deadline=$((SECONDS + STARTUP_TIMEOUT))

    while true; do
        if ! app_identity_matches; then
            wait "${CURRENT_APP_PID}" 2>/dev/null || true
            fail_cycle "app exited before its coordinator became ready ($(startup_status))"
        fi
        is_zombie "${CURRENT_APP_PID}" && fail_cycle 'app became a zombie during startup'
        app_children_are_healthy
        if adopt_coordinator && recorded_shells_are_ready; then
            return 0
        fi
        [[ "${SECONDS}" -lt "${deadline}" ]] ||
            fail_cycle "startup timed out ($(startup_status), expected ${EXPECTED_SHELLS} shells)"
        /bin/sleep 0.1
    done
}

await_app_exit() {
    local deadline=$((SECONDS + SHUTDOWN_TIMEOUT))

    /bin/kill -TERM "${CURRENT_APP_PID}" || fail_cycle 'could not terminate the exact app PID'
    while app_identity_matches && ! is_zombie "${CURRENT_APP_PID}"; do
        [[ "${SECONDS}" -lt "${deadline}" ]] ||
            fail_cycle 'shutdown timed out before the app exited'
        /bin/sleep 0.1
    done
    wait "${CURRENT_APP_PID}" 2>/dev/null || true
    [[ -z "$(process_identity "${CURRENT_APP_PID}" || true)" ]] ||
        fail_cycle 'app PID still exists after shutdown'
}

# Durable work must not end with the window that displayed it.
assert_coordinator_survived() {
    local index pid

    coordinator_identity_matches || fail_cycle 'coordinator did not survive the app exiting'
    is_zombie "${COORDINATOR_PID}" && fail_cycle 'coordinator became a zombie when the app exited'
    for ((index = 0; index < ${#TRACKED_SHELL_PIDS[@]}; index++)); do
        pid="${TRACKED_SHELL_PIDS[${index}]}"
        tracked_shell_identity_matches "${index}" ||
            fail_cycle "coordinator shell ${pid} did not survive the app exiting"
        is_zombie "${pid}" && fail_cycle "coordinator shell ${pid} became a zombie when the app exited"
        [[ "$(process_ppid "${pid}" || true)" == "${COORDINATOR_PID}" ]] ||
            fail_cycle "coordinator shell ${pid} was orphaned when the app exited"
    done
}

# A process can be observed briefly between exit and reap. Report a zombie or
# orphan only when it persists across several polls.
shell_anomaly() {
    local pid="$1"

    if is_zombie "${pid}"; then
        printf 'tracked shell %s remained a zombie during coordinator shutdown\n' "${pid}"
    elif [[ "$(process_ppid "${pid}" || true)" != "${COORDINATOR_PID}" ]]; then
        printf 'tracked shell %s remained an orphan during coordinator shutdown\n' "${pid}"
    fi
}

tracked_shells_present() {
    local index anomaly
    local present='false'

    for ((index = 0; index < ${#TRACKED_SHELL_PIDS[@]}; index++)); do
        tracked_shell_identity_matches "${index}" || continue
        present='true'
        anomaly="$(shell_anomaly "${TRACKED_SHELL_PIDS[${index}]}")"
        if [[ -z "${anomaly}" ]]; then
            SHELL_ANOMALY_COUNTS[index]=0
            continue
        fi
        SHELL_ANOMALY_COUNTS[index]=$((SHELL_ANOMALY_COUNTS[index] + 1))
        [[ "${SHELL_ANOMALY_COUNTS[${index}]}" -lt "${ANOMALY_POLLS}" ]] || fail_cycle "${anomaly}"
    done
    [[ "${present}" == 'true' ]]
}

coordinator_present() {
    coordinator_identity_matches || return 1
    if is_zombie "${COORDINATOR_PID}"; then
        COORDINATOR_ANOMALY_COUNT=$((COORDINATOR_ANOMALY_COUNT + 1))
        [[ "${COORDINATOR_ANOMALY_COUNT}" -lt "${ANOMALY_POLLS}" ]] ||
            fail_cycle 'coordinator remained a zombie during its shutdown'
    fi
    return 0
}

stop_coordinator() {
    local deadline=$((SECONDS + SHUTDOWN_TIMEOUT))
    local index

    SHELL_ANOMALY_COUNTS=()
    for ((index = 0; index < ${#TRACKED_SHELL_PIDS[@]}; index++)); do
        SHELL_ANOMALY_COUNTS[index]=0
    done
    COORDINATOR_ANOMALY_COUNT=0
    "${CLI}" --socket "${CURRENT_SOCKET}" kill --server \
        > "${CURRENT_CYCLE_DIR}/kill-server.stdout" \
        2> "${CURRENT_CYCLE_DIR}/kill-server.stderr" ||
        fail_cycle "phux kill --server failed with status $?"
    while coordinator_present || tracked_shells_present || [[ -S "${CURRENT_SOCKET}" ]]; do
        [[ "${SECONDS}" -lt "${deadline}" ]] ||
            fail_cycle 'shutdown timed out before the coordinator, its shells, and its socket disappeared'
        /bin/sleep 0.1
    done
}

assert_nothing_remains() {
    local index

    [[ -z "$(process_identity "${COORDINATOR_PID}" || true)" ]] ||
        fail_cycle 'coordinator PID still exists after its shutdown'
    for ((index = 0; index < ${#TRACKED_SHELL_PIDS[@]}; index++)); do
        tracked_shell_identity_matches "${index}" &&
            fail_cycle "tracked shell ${TRACKED_SHELL_PIDS[${index}]} still exists after shutdown"
    done
    ! find_coordinator >/dev/null || fail_cycle "a coordinator still claims socket ${CURRENT_SOCKET}"
}

terminate_pid() {
    local pid="$1"
    local expected="$2"
    local deadline

    identity_matches "${pid}" "${expected}" || return 0
    /bin/kill -TERM "${pid}" 2>/dev/null || true
    deadline=$((SECONDS + 2))
    while identity_matches "${pid}" "${expected}" && [[ "${SECONDS}" -lt "${deadline}" ]]; do
        /bin/sleep 0.1
    done
    identity_matches "${pid}" "${expected}" && /bin/kill -KILL "${pid}" 2>/dev/null || true
}

cleanup_owned_processes() {
    local index

    terminate_pid "${CURRENT_APP_PID}" "${CURRENT_APP_IDENTITY}"
    if [[ -n "${CURRENT_APP_PID}" ]]; then
        wait "${CURRENT_APP_PID}" 2>/dev/null || true
    fi
    if [[ -z "${COORDINATOR_PID}" ]]; then
        adopt_coordinator_quietly
    fi
    if coordinator_identity_matches; then
        "${CLI}" --socket "${CURRENT_SOCKET}" kill --server >/dev/null 2>&1 || true
        terminate_pid "${COORDINATOR_PID}" "${COORDINATOR_IDENTITY}"
    fi
    for ((index = 0; index < ${#TRACKED_SHELL_PIDS[@]}; index++)); do
        terminate_pid "${TRACKED_SHELL_PIDS[${index}]}" "${TRACKED_SHELL_IDENTITIES[${index}]}"
    done
    CURRENT_APP_PID=''
    CURRENT_APP_IDENTITY=''
    COORDINATOR_PID=''
    COORDINATOR_IDENTITY=''
}

# Cleanup runs from failure paths, so it must not call fail_cycle recursively.
adopt_coordinator_quietly() {
    local pid

    [[ -n "${CURRENT_SOCKET}" ]] || return 0
    pid="$(find_coordinator)" || return 0
    COORDINATOR_PID="${pid}"
    COORDINATOR_IDENTITY="$(process_identity "${pid}" || true)"
}

collect_diagnostics() {
    local reason="$1"
    local failure_dir

    /bin/mkdir -p -- "${ARTIFACTS}" || return 1
    failure_dir="$(/usr/bin/mktemp -d "${ARTIFACTS%/}/cycle-${CURRENT_CYCLE}-failure.XXXXXX")" ||
        return 1
    {
        printf 'cycle=%s\n' "${CURRENT_CYCLE}"
        printf 'reason=%s\n' "${reason}"
        printf 'app=%s\n' "${APP}"
        printf 'executable=%s\n' "${EXECUTABLE}"
        printf 'cli=%s\n' "${CLI}"
        printf 'socket=%s\n' "${CURRENT_SOCKET}"
        printf 'app_pid=%s\n' "${CURRENT_APP_PID}"
        printf 'coordinator_pid=%s\n' "${COORDINATOR_PID}"
        printf 'tracked_shell_pids='
        printf '%s ' "${TRACKED_SHELL_PIDS[@]:-}"
        printf '\n'
    } > "${CURRENT_CYCLE_DIR}/failure.txt"
    /bin/ps -axo pid,ppid,pgid,sess,state,lstart,command \
        > "${CURRENT_CYCLE_DIR}/processes.txt" 2>&1 || true
    /bin/cp -R -- "${CURRENT_CYCLE_DIR}/." "${failure_dir}/" || return 1
    LAST_ARTIFACT="${failure_dir}"
}

on_exit() {
    cleanup_owned_processes || true
    if [[ -n "${RUN_DIR}" && -d "${RUN_DIR}" ]]; then
        /bin/rm -rf -- "${RUN_DIR}"
    fi
    if [[ -n "${SOCKET_DIR}" && -d "${SOCKET_DIR}" ]]; then
        /bin/rm -rf -- "${SOCKET_DIR}"
    fi
}

on_signal() {
    local signal="$1"
    local status="$2"

    if [[ -n "${CURRENT_CYCLE_DIR}" && -d "${CURRENT_CYCLE_DIR}" ]]; then
        collect_diagnostics "interrupted by ${signal}" || true
    fi
    exit "${status}"
}

fail_cycle() {
    local reason="$1"

    if ! collect_diagnostics "${reason}"; then
        printf 'error: cycle %s: %s (diagnostic collection failed)\n' \
            "${CURRENT_CYCLE}" "${reason}" >&2
    else
        printf 'error: cycle %s: %s\n' "${CURRENT_CYCLE}" "${reason}" >&2
        printf 'diagnostics: %s\n' "${LAST_ARTIFACT}" >&2
    fi
    cleanup_owned_processes
    exit 1
}

trap on_exit EXIT
trap 'on_signal HUP 129' HUP
trap 'on_signal INT 130' INT
trap 'on_signal TERM 143' TERM

RUN_DIR="$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/soak-macos-app.XXXXXX")" ||
    die 'could not create temporary run directory'
SOCKET_DIR="$(/usr/bin/mktemp -d /tmp/soak-app.XXXXXX)" ||
    die 'could not create temporary socket directory'

prepare_cycle() {
    local home_dir="$1"

    /bin/mkdir -p -- "${home_dir}/.config" "${home_dir}/.local/state" "${home_dir}/.cache"
    cat > "${home_dir}/.zshrc" <<'EOF'
# Controlled by soak-macos-app.sh; do not load user startup files.
unset HISTFILE
PROMPT='soak%# '
RPROMPT=''
print -r -- "$$" >> "${SOAK_SHELL_PID_FILE:?}"
EOF
    : > "${CURRENT_PID_FILE}"
    [[ "${#CURRENT_SOCKET}" -le "${MAX_SOCKET_BYTES}" ]] ||
        die "socket path is too long for sockaddr_un: ${CURRENT_SOCKET}"
}

# Every location the app or the coordinator resolves from the environment
# points into the cycle: config, state, caches, and the socket. A developer's
# runtime dir, session choice, and Cockpit overrides are unset rather than
# blanked so neither binary sees an empty value it has to interpret.
launch_app() {
    local home_dir="$1"

    /usr/bin/env -u XDG_RUNTIME_DIR -u PHUX_SESSION \
        -u PHUX_COCKPIT_CONFIG -u PHUX_COCKPIT_STATE -u PHUX_COCKPIT_TABS \
        HOME="${home_dir}" \
        ZDOTDIR="${home_dir}" \
        XDG_CONFIG_HOME="${home_dir}/.config" \
        XDG_STATE_HOME="${home_dir}/.local/state" \
        XDG_CACHE_HOME="${home_dir}/.cache" \
        PHUX_SOCKET="${CURRENT_SOCKET}" \
        SOAK_SHELL_PID_FILE="${CURRENT_PID_FILE}" \
        "${EXECUTABLE}" \
        > "${CURRENT_CYCLE_DIR}/app.stdout" \
        2> "${CURRENT_CYCLE_DIR}/app.stderr" &
    CURRENT_APP_PID=$!
    CURRENT_APP_IDENTITY="$(process_identity "${CURRENT_APP_PID}" || true)"
    [[ -n "${CURRENT_APP_IDENTITY}" ]] || fail_cycle 'app exited immediately after launch'
}

run_cycle() {
    local home_dir="${CURRENT_CYCLE_DIR}/home"
    local app_pid

    prepare_cycle "${home_dir}"
    launch_app "${home_dir}"
    await_startup
    app_pid="${CURRENT_APP_PID}"
    await_app_exit
    /bin/sleep 1
    assert_coordinator_survived
    stop_coordinator
    assert_nothing_remains
    printf 'cycle %s/%s: ok (app pid %s; coordinator %s; shells %s)\n' \
        "${CURRENT_CYCLE}" "${CYCLES}" "${app_pid}" "${COORDINATOR_PID}" "${TRACKED_SHELL_PIDS[*]}"
}

printf 'soak: app=%s cli=%s cycles=%s startup=%ss shutdown=%ss\n' \
    "${APP}" "${CLI}" "${CYCLES}" "${STARTUP_TIMEOUT}" "${SHUTDOWN_TIMEOUT}"

for ((CURRENT_CYCLE = 1; CURRENT_CYCLE <= CYCLES; CURRENT_CYCLE++)); do
    CURRENT_CYCLE_DIR="${RUN_DIR}/cycle-${CURRENT_CYCLE}"
    CURRENT_PID_FILE="${CURRENT_CYCLE_DIR}/shell-pids.txt"
    CURRENT_SOCKET="${SOCKET_DIR}/c${CURRENT_CYCLE}.sock"
    CURRENT_APP_PID=''
    CURRENT_APP_IDENTITY=''
    COORDINATOR_PID=''
    COORDINATOR_IDENTITY=''
    TRACKED_SHELL_PIDS=()
    TRACKED_SHELL_IDENTITIES=()
    /bin/mkdir -p -- "${CURRENT_CYCLE_DIR}"
    run_cycle
    CURRENT_APP_PID=''
    CURRENT_APP_IDENTITY=''
    COORDINATOR_PID=''
    COORDINATOR_IDENTITY=''
done

printf 'soak passed: %s/%s cycles\n' "${CYCLES}" "${CYCLES}"
