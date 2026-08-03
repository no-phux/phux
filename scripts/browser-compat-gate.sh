#!/bin/sh
# Real Chrome -> WebSocket -> ServerRuntime -> PTY compatibility gate.
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
STAMP=$(date -u +%Y%m%dT%H%M%SZ)
ARTIFACT_DIR=${PHUX_BROWSER_ARTIFACT_DIR:-"$ROOT/target/browser-compat/$STAMP-$$"}
mkdir -p "$ARTIFACT_DIR"
SERVER_PID=
cleanup() {
    if [ -n "$SERVER_PID" ]; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT HUP INT TERM
fail() {
    status=$1
    ( set -C; printf 'status=%s\nartifact_dir=%s\n' "$status" "$ARTIFACT_DIR" > "$ARTIFACT_DIR/first-failure.txt" ) 2>/dev/null || true
    exit "$status"
}
cd "$ROOT"
PHUX_WS_ADDR=127.0.0.1:47654 cargo run -p phux-server --example ws_demo_server \
    >"$ARTIFACT_DIR/server.stdout.log" 2>"$ARTIFACT_DIR/server.stderr.log" &
SERVER_PID=$!
count=0
while ! grep -q 'ws-demo-server listening' "$ARTIFACT_DIR/server.stderr.log" 2>/dev/null; do
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then fail 1; fi
    count=$((count + 1))
    if [ "$count" -ge 600 ]; then fail 1; fi
    sleep 0.05
done
# The banner is emitted immediately before ServerRuntime binds. Give the real
# listener one bounded scheduling turn; browser tests retain their own 6s
# assertion deadline and are never retried.
sleep 0.1
cd "$ROOT/clients/phux-web"
wasm-pack test --headless --chrome >"$ARTIFACT_DIR/chrome.log" 2>&1 || fail "$?"
printf '{"status":"passed","artifact_dir":"%s"}\n' "$ARTIFACT_DIR" > "$ARTIFACT_DIR/success.json"
printf '%s\n' "$ARTIFACT_DIR"
