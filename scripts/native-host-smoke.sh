#!/usr/bin/env bash
# Bounded production native-host smoke against a real phux server.
# Requires the cockpit already built with -Dautomation=true -Dphux-enabled=true.
set -Eeuo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COCKPIT_DIR="${PHUX_COCKPIT_DIR:-/Users/phall/workspace/phux-native-spike/cockpit}"
NATIVE_APP="${PHUX_NATIVE_APP:-$COCKPIT_DIR/zig-out/bin/terminal}"
NATIVE_CLI="${PHUX_NATIVE_CLI:-/Users/phall/workspace/phux-native-spike/ref/native-sdk/zig-out/bin/native}"
PHUX_BIN="${PHUX_BIN:-$REPO/target/debug/phux}"
ARTIFACT_ROOT="${PHUX_SMOKE_ARTIFACT_DIR:-$REPO/target/smoke-artifacts}"
RUN_DIR="$ARTIFACT_ROOT/native-host-$$"
AUTOMATION_DIR="$COCKPIT_DIR/.zig-cache/native-sdk-automation"
PHUX_SOCK="/tmp/phux-native-host-smoke-$$.sock"
SESSION="native-smoke"
TRANSCRIPT="$RUN_DIR/transcript.txt"
SERVER_PID="" APP_PID="" WATCHDOG_PID="" STEP=setup

mkdir -p "$RUN_DIR" "$AUTOMATION_DIR"
: >"$TRANSCRIPT"
note() { printf '\n=== %s ===\n' "$*" | tee -a "$TRANSCRIPT"; }
record_command() { printf '$' >>"$TRANSCRIPT"; printf ' %q' "$@" >>"$TRANSCRIPT"; printf '\n' >>"$TRANSCRIPT"; }
fail() { printf 'FAIL [%s]: %s\n' "$STEP" "$*" | tee -a "$TRANSCRIPT" >&2; return 1; }
run_phux() { record_command "$PHUX_BIN" "$@"; "$PHUX_BIN" "$@" >>"$TRANSCRIPT" 2>&1; }
run_native() {
  record_command "$NATIVE_CLI" automate "$@"
  (cd "$COCKPIT_DIR" && "$NATIVE_CLI" automate "$@") >>"$TRANSCRIPT" 2>&1
}
collect_artifacts() {
  local path
  for path in snapshot.txt accessibility.txt windows.txt screenshot-terminal-canvas.png provenance.txt bridge-response.txt; do
    [[ ! -e "$AUTOMATION_DIR/$path" ]] || cp "$AUTOMATION_DIR/$path" "$RUN_DIR/$path" 2>/dev/null || true
  done
  for path in "$AUTOMATION_DIR"/command*.txt; do
    [[ ! -e "$path" ]] || cp "$path" "$RUN_DIR/" 2>/dev/null || true
  done
}
cleanup() {
  [[ -z "$WATCHDOG_PID" ]] || kill "$WATCHDOG_PID" 2>/dev/null || true
  if [[ -n "$APP_PID" ]]; then kill "$APP_PID" 2>/dev/null || true; wait "$APP_PID" 2>/dev/null || true; fi
  if [[ -n "$SERVER_PID" ]]; then kill "$SERVER_PID" 2>/dev/null || true; wait "$SERVER_PID" 2>/dev/null || true; fi
  rm -f "$PHUX_SOCK"
}
on_exit() {
  local status=$?
  trap - ERR EXIT INT TERM
  collect_artifacts
  if (( status != 0 )); then
    printf 'step=%s\nserver_pid=%s\napp_pid=%s\nsocket=%s\n' "$STEP" "$SERVER_PID" "$APP_PID" "$PHUX_SOCK" >"$RUN_DIR/failure.txt"
    printf 'native smoke artifacts: %s\n' "$RUN_DIR" >&2
  fi
  cleanup
  exit "$status"
}
trap on_exit EXIT
trap 'exit 130' INT
trap 'exit 124' TERM
( sleep "${PHUX_NATIVE_SMOKE_TIMEOUT_SECS:-75}"; printf 'native smoke overall timeout\n' >"$RUN_DIR/deadline.txt"; kill -TERM "$$" ) &
WATCHDOG_PID=$!

[[ -x "$PHUX_BIN" ]] || fail "missing phux executable $PHUX_BIN"
[[ -x "$NATIVE_APP" ]] || fail "missing automation-enabled cockpit $NATIVE_APP; build with -Dautomation=true -Dphux-enabled=true and the current FFI"
[[ -x "$NATIVE_CLI" ]] || fail "missing native-sdk automation CLI $NATIVE_CLI"
command -v python3 >/dev/null || fail "python3 is required"

STEP=server-start
note "start isolated real server"
rm -f "$PHUX_SOCK"
RUST_LOG="${RUST_LOG:-info}" "$PHUX_BIN" server --socket "$PHUX_SOCK" --session "$SESSION" --exit-after-idle 120 >"$RUN_DIR/server.log" 2>&1 &
SERVER_PID=$!
deadline=$((SECONDS + 30))
while [[ ! -S "$PHUX_SOCK" ]]; do
  (( SECONDS < deadline )) || fail "server did not bind $PHUX_SOCK within 30s"
  kill -0 "$SERVER_PID" 2>/dev/null || fail "server exited before binding"
  sleep 0.05
done

STEP=seed-panes
note "seed two real PTY panes"
run_phux send-keys --socket "$PHUX_SOCK" @1 "printf 'NATIVE-PANE-ONE\\r\\n'" Enter
run_phux wait --socket "$PHUX_SOCK" --until NATIVE-PANE-ONE --timeout 15 @1
record_command "$PHUX_BIN" spawn --socket "$PHUX_SOCK" --json
SPAWN_JSON="$("$PHUX_BIN" spawn --socket "$PHUX_SOCK" --json 2>>"$TRANSCRIPT")"
printf '%s\n' "$SPAWN_JSON" >"$RUN_DIR/spawn.json"
PANE_TWO="$(python3 -c 'import json,sys; print("@" + str(json.load(sys.stdin)["terminal_id"]))' <<<"$SPAWN_JSON")"
run_phux send-keys --socket "$PHUX_SOCK" "$PANE_TWO" "printf 'NATIVE-PANE-TWO\\r\\n'" Enter
run_phux wait --socket "$PHUX_SOCK" --until NATIVE-PANE-TWO --timeout 15 "$PANE_TWO"

STEP=native-attach
note "launch production cockpit and observe both rendered panes"
rm -f "$AUTOMATION_DIR/snapshot.txt" "$AUTOMATION_DIR/accessibility.txt" "$AUTOMATION_DIR/windows.txt" "$AUTOMATION_DIR/screenshot-terminal-canvas.png" "$AUTOMATION_DIR"/command*.txt
record_command "$NATIVE_APP" "unix://$PHUX_SOCK" "$SESSION" smoke
(cd "$COCKPIT_DIR" && "$NATIVE_APP" "unix://$PHUX_SOCK" "$SESSION" smoke) >"$RUN_DIR/native-app.log" 2>&1 &
APP_PID=$!
run_native wait
run_native assert --timeout-ms 30000 'ready=true' 'gpu_nonblank=true' 'view @w1/terminal-canvas' '1\* live' '2 live' 'NATIVE-PANE-ONE' 'NATIVE-PANE-TWO'
record_command "$NATIVE_CLI" automate snapshot
(cd "$COCKPIT_DIR" && "$NATIVE_CLI" automate snapshot) >"$RUN_DIR/attached.snapshot.txt" 2>>"$TRANSCRIPT"

widget_for_marker() {
  python3 - "$RUN_DIR/attached.snapshot.txt" "$1" <<'PY'
import pathlib, re, sys
text = pathlib.Path(sys.argv[1]).read_text()
match = re.search(r"widget @w1/terminal-canvas#([0-9]+)[^\n]*" + re.escape(sys.argv[2]), text)
if not match:
    raise SystemExit(f"no terminal-canvas widget contains {sys.argv[2]!r}")
print(match.group(1))
PY
}
PANE_ONE_WIDGET="$(widget_for_marker NATIVE-PANE-ONE)"
PANE_TWO_WIDGET="$(widget_for_marker NATIVE-PANE-TWO)"
printf 'pane_one_widget=%s\npane_two_widget=%s\n' "$PANE_ONE_WIDGET" "$PANE_TWO_WIDGET" >"$RUN_DIR/widget-ids.txt"

STEP=native-input
note "focus and type through both real native pane widgets"
run_native widget-click terminal-canvas "$PANE_ONE_WIDGET"
run_native widget-key terminal-canvas a "printf 'NATIVE-INPUT-PANE-ONE\\r\\n'"
run_native widget-key terminal-canvas enter
run_phux wait --socket "$PHUX_SOCK" --until NATIVE-INPUT-PANE-ONE --timeout 15 @1
run_native widget-click terminal-canvas "$PANE_TWO_WIDGET"
run_native widget-key terminal-canvas a "printf 'NATIVE-INPUT-PANE-TWO\\r\\n'"
run_native widget-key terminal-canvas enter
run_phux wait --socket "$PHUX_SOCK" --until NATIVE-INPUT-PANE-TWO --timeout 15 "$PANE_TWO"
run_native assert --timeout-ms 15000 'NATIVE-INPUT-PANE-ONE' 'NATIVE-INPUT-PANE-TWO' '1 live' '2\* live'

STEP=native-resize
note "resize real window and preserve exact two-pane presentation"
run_native resize 1240 720 1
run_native assert --timeout-ms 15000 'gpu_nonblank=true' 'NATIVE-INPUT-PANE-ONE' 'NATIVE-INPUT-PANE-TWO' '1 live' '2\* live'
record_command "$NATIVE_CLI" automate snapshot
(cd "$COCKPIT_DIR" && "$NATIVE_CLI" automate snapshot) >"$RUN_DIR/final.snapshot.txt" 2>>"$TRANSCRIPT"
run_native screenshot terminal-canvas
[[ -s "$AUTOMATION_DIR/screenshot-terminal-canvas.png" ]] || fail "native-sdk produced no terminal screenshot"

STEP=server-projection
note "assert input stayed on the focused canonical pane"
record_command "$PHUX_BIN" snapshot --socket "$PHUX_SOCK" --json @1
"$PHUX_BIN" snapshot --socket "$PHUX_SOCK" --json @1 >"$RUN_DIR/pane-one.json" 2>>"$TRANSCRIPT"
record_command "$PHUX_BIN" snapshot --socket "$PHUX_SOCK" --json "$PANE_TWO"
"$PHUX_BIN" snapshot --socket "$PHUX_SOCK" --json "$PANE_TWO" >"$RUN_DIR/pane-two.json" 2>>"$TRANSCRIPT"
python3 - "$RUN_DIR/pane-one.json" "$RUN_DIR/pane-two.json" <<'PY'
import json, pathlib, sys
one = "\n".join(json.loads(pathlib.Path(sys.argv[1]).read_text())["lines"])
two = "\n".join(json.loads(pathlib.Path(sys.argv[2]).read_text())["lines"])
assert "NATIVE-INPUT-PANE-ONE" in one and "NATIVE-INPUT-PANE-TWO" not in one, one
assert "NATIVE-INPUT-PANE-TWO" in two and "NATIVE-INPUT-PANE-ONE" not in two, two
PY

STEP=done
collect_artifacts
note "PASS"
printf 'native smoke artifacts: %s\n' "$RUN_DIR"
