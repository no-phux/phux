#!/bin/sh
# Deterministic, non-retried release interoperability gate.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
STAMP=$(date -u +%Y%m%dT%H%M%SZ)
ARTIFACT_DIR=${PHUX_RELEASE_ARTIFACT_DIR:-"$ROOT/target/release-interop/$STAMP-$$"}
CURRENT_PHUX=${PHUX_CURRENT_BIN:-"$ROOT/target/release/phux"}
PHUX_BROWSER_GATE=${PHUX_BROWSER_GATE:-"PHUX_BROWSER_ARTIFACT_DIR='$ARTIFACT_DIR/browser' '$ROOT/scripts/browser-compat-gate.sh'"}
PHUX_NATIVE_APP_GATE=${PHUX_NATIVE_APP_GATE:-"PHUX_SMOKE_ARTIFACT_DIR='$ARTIFACT_DIR/native' '$ROOT/scripts/native-host-smoke.sh'"}
: "${PHUX_PREVIOUS_BIN:?set PHUX_PREVIOUS_BIN to the previous released phux executable}"
: "${PHUX_MACOS_ARM64_CHECKPOINT:?set PHUX_MACOS_ARM64_CHECKPOINT to the release fixture}"
: "${PHUX_LINUX_X86_64_CHECKPOINT:?set PHUX_LINUX_X86_64_CHECKPOINT to the release fixture}"
: "${PHUX_CHECKPOINT_FIXTURE_VERIFY:?set PHUX_CHECKPOINT_FIXTURE_VERIFY to the C/Rust fixture verifier command}"

mkdir -p "$ARTIFACT_DIR/runtime"
CURRENT_PID=
PREVIOUS_PID=
cleanup() {
    for pid in "$CURRENT_PID" "$PREVIOUS_PID"; do
        if [ -n "$pid" ]; then
            kill "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
        fi
    done
    rm -f "$ARTIFACT_DIR/runtime/"*.sock
}
trap cleanup EXIT HUP INT TERM

fail() {
    step=$1
    status=$2
    # The first failure is immutable even if cleanup or a wrapper reports again.
    ( set -C; printf 'step=%s\nstatus=%s\nartifact_dir=%s\n' "$step" "$status" "$ARTIFACT_DIR" > "$ARTIFACT_DIR/first-failure.txt" ) 2>/dev/null || true
    exit "$status"
}

run_step() {
    name=$1
    shift
    log="$ARTIFACT_DIR/$name.log"
    printf '%s\n' "$*" > "$ARTIFACT_DIR/$name.command"
    "$@" >"$log" 2>&1 || fail "$name" "$?"
}

run_shell_step() {
    name=$1
    command=$2
    log="$ARTIFACT_DIR/$name.log"
    printf '%s\n' "$command" > "$ARTIFACT_DIR/$name.command"
    /bin/sh -c "$command" >"$log" 2>&1 || fail "$name" "$?"
}

wait_socket() {
    socket=$1
    pid=$2
    count=0
    while [ ! -S "$socket" ]; do
        if ! kill -0 "$pid" 2>/dev/null; then return 1; fi
        count=$((count + 1))
        if [ "$count" -ge 300 ]; then return 1; fi
        sleep 0.05
    done
}

compat_pair() {
    name=$1
    server_bin=$2
    client_bin=$3
    socket="$ARTIFACT_DIR/runtime/$name.sock"
    server_log="$ARTIFACT_DIR/$name-server.log"
    "$server_bin" --socket "$socket" server --session interop >"$server_log" 2>&1 &
    pid=$!
    if [ "$name" = current-server-previous-client ]; then CURRENT_PID=$pid; else PREVIOUS_PID=$pid; fi
    wait_socket "$socket" "$pid" || fail "$name-server-start" 1
    run_step "$name" "$client_bin" --socket "$socket" ls --json
    run_step "$name-rendered-attach" "$client_bin" --socket "$socket" snapshot interop --rendered --json
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    if [ "$name" = current-server-previous-client ]; then CURRENT_PID=; else PREVIOUS_PID=; fi
}

[ -x "$CURRENT_PHUX" ] || fail current-binary-missing 1
[ -x "$PHUX_PREVIOUS_BIN" ] || fail previous-binary-missing 1
[ -f "$PHUX_MACOS_ARM64_CHECKPOINT" ] || fail macos-arm64-fixture-missing 1
[ -f "$PHUX_LINUX_X86_64_CHECKPOINT" ] || fail linux-x86_64-fixture-missing 1

compat_pair current-server-previous-client "$CURRENT_PHUX" "$PHUX_PREVIOUS_BIN"
compat_pair previous-server-current-client "$PHUX_PREVIOUS_BIN" "$CURRENT_PHUX"

# The verifier is intentionally external to phux framing: checkpoint bytes are
# opaque here. It must decode each architecture's artifact and exchange them in
# both directions through the C and Rust bindings.
run_shell_step checkpoint-exchange "$PHUX_CHECKPOINT_FIXTURE_VERIFY \"$PHUX_MACOS_ARM64_CHECKPOINT\" \"$PHUX_LINUX_X86_64_CHECKPOINT\""

cd "$ROOT"
# Ordinary cargo integration tests: no nextest retry profile and no shell retry.
run_step uds cargo test -p phux-server --test concurrent_attach_l2
run_step wss cargo test -p phux-client --test ws_dial wss_with_pinned_cert_sends_bearer_token
run_step quic cargo test -p phux-client --test quic_dial
run_step relay cargo test -p phux-server --test relay_e2e
run_step warm-fullscreen-eight-client-reconnect cargo test -p phux-server --test release_bootstrap_milestones
run_step tui-record-play-surface env PHUX_SMOKE_ARTIFACT_DIR="$ARTIFACT_DIR" bash scripts/tui-probe.sh 80 24
run_shell_step browser "$PHUX_BROWSER_GATE"
run_shell_step native-app "$PHUX_NATIVE_APP_GATE"
run_step recording cargo test -p phux --test rec_e2e
run_step playback cargo test -p phux --test play_e2e

printf '{"status":"passed","artifact_dir":"%s"}\n' "$ARTIFACT_DIR" > "$ARTIFACT_DIR/success.json"
printf '%s\n' "$ARTIFACT_DIR"
