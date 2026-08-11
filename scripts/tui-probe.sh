#!/usr/bin/env bash
# Bounded black-box surface gate for the reference TUI and its asciicast tee.
#
# The probe drives a real phux server and a real `phux attach` inside an
# isolated tmux server. Every assertion leaves its screen, cursor, client log,
# server log, and command transcript under PHUX_SMOKE_ARTIFACT_DIR.
#
# Usage: scripts/tui-probe.sh [COLS] [ROWS]
set -Eeuo pipefail

COLS="${1:-80}"
ROWS="${2:-24}"
OVERALL_TIMEOUT="${PHUX_SMOKE_TIMEOUT_SECS:-90}"

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PHUX_BIN="${PHUX_BIN:-$REPO/target/debug/phux}"
ARTIFACT_ROOT="${PHUX_SMOKE_ARTIFACT_DIR:-$REPO/target/smoke-artifacts}"
RUN_DIR="$ARTIFACT_ROOT/tui-probe-$$"
TMUX_SOCKET="phux-probe-$$"
TMUX=(tmux -L "$TMUX_SOCKET")
PHUX_SOCK="/tmp/phux-tui-probe-$$.sock"
SESSION="probe"
COMPLETE_CAST="$RUN_DIR/complete.cast"
INTERRUPTED_CAST="$RUN_DIR/interrupted.cast"
TRANSCRIPT="$RUN_DIR/transcript.txt"
CLIENT_LOG="$RUN_DIR/client.log"
SERVER_LOG="$RUN_DIR/server.log"
SERVER_PID=""
WATCHDOG_PID=""
STEP=setup
SAW_ONBOARDING=0

mkdir -p "$RUN_DIR"
: >"$TRANSCRIPT"

note() {
  printf '\n=== %s ===\n' "$*" | tee -a "$TRANSCRIPT"
}

record_command() {
  printf '$' >>"$TRANSCRIPT"
  printf ' %q' "$@" >>"$TRANSCRIPT"
  printf '\n' >>"$TRANSCRIPT"
}

run_phux() {
  record_command "$PHUX_BIN" "$@"
  "$PHUX_BIN" "$@" >>"$TRANSCRIPT" 2>&1
}

screen() {
  "${TMUX[@]}" capture-pane -p -t "$1"
}

cursor() {
  "${TMUX[@]}" display-message -p -t "$1" '#{cursor_x},#{cursor_y}'
}

capture() {
  local name="$1"
  local target="${2:-$SESSION}"
  {
    printf 'target=%s\n' "$target"
    printf 'window='
    "${TMUX[@]}" display-message -p -t "$target" '#{window_width}x#{window_height}'
    printf 'cursor='
    cursor "$target"
    printf '%s\n' '--- screen ---'
    screen "$target"
  } >"$RUN_DIR/$name.txt" 2>&1
  cat "$RUN_DIR/$name.txt" >>"$TRANSCRIPT"
}

fail() {
  printf 'FAIL [%s]: %s\n' "$STEP" "$*" | tee -a "$TRANSCRIPT" >&2
  return 1
}

assert_file_contains() {
  local path="$1"
  local needle="$2"
  if ! grep -Fq -- "$needle" "$path"; then
    fail "$path does not contain $needle"
  fi
}

assert_screen_contains() {
  local target="$1"
  local needle="$2"
  local deadline=$((SECONDS + 15))
  while (( SECONDS < deadline )); do
    screen "$target" >"$RUN_DIR/.screen"
    if grep -Fq -- "$needle" "$RUN_DIR/.screen"; then
      return 0
    fi
    sleep 0.05
  done
  capture "timeout-${STEP}" "$target" || true
  fail "screen $target did not paint $needle within 15s"
}

assert_marker_once() {
  local path="$1"
  local needle="$2"
  local count
  count="$(grep -Fo -- "$needle" "$path" | wc -l | tr -d ' ')"
  if [[ "$count" != "1" ]]; then
    fail "$needle appeared $count times in $path (expected exactly once)"
  fi
}

wait_for_socket() {
  local deadline=$((SECONDS + 30))
  while (( SECONDS < deadline )); do
    if [[ -S "$PHUX_SOCK" ]]; then
      return 0
    fi
    if [[ -n "$SERVER_PID" ]] && ! kill -0 "$SERVER_PID" 2>/dev/null; then
      fail "real server exited before binding $PHUX_SOCK"
    fi
    sleep 0.05
  done
  fail "real server did not bind $PHUX_SOCK within 30s"
}

collect_failure() {
  {
    printf 'step=%s\n' "$STEP"
    printf 'socket=%s\n' "$PHUX_SOCK"
    printf 'server_pid=%s\n' "$SERVER_PID"
    printf 'tmux_socket=%s\n' "$TMUX_SOCKET"
  } >"$RUN_DIR/failure.txt"
  "${TMUX[@]}" list-sessions >>"$RUN_DIR/failure.txt" 2>&1 || true
  "${TMUX[@]}" list-panes -a -F '#{session_name}:#{window_index}.#{pane_index} #{pane_pid} #{pane_dead} #{pane_current_command}' \
    >>"$RUN_DIR/failure.txt" 2>&1 || true
  while IFS= read -r target; do
    [[ -n "$target" ]] || continue
    capture "failure-${target//[:.]/-}" "$target" || true
  done < <("${TMUX[@]}" list-panes -a -F '#{session_name}:#{window_index}.#{pane_index}' 2>/dev/null || true)
}

cleanup() {
  if [[ -n "$WATCHDOG_PID" ]]; then
    kill "$WATCHDOG_PID" 2>/dev/null || true
  fi
  "${TMUX[@]}" kill-server 2>/dev/null || true
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -f "$PHUX_SOCK"
}

on_exit() {
  local status=$?
  trap - ERR EXIT INT TERM
  if (( status != 0 )); then
    collect_failure
    printf 'surface artifacts: %s\n' "$RUN_DIR" >&2
  fi
  cleanup
  exit "$status"
}

trap on_exit EXIT
trap 'exit 130' INT
trap 'exit 124' TERM

(
  sleep "$OVERALL_TIMEOUT"
  printf 'overall timeout after %ss\n' "$OVERALL_TIMEOUT" >"$RUN_DIR/deadline.txt"
  kill -TERM "$$"
) &
WATCHDOG_PID=$!

[[ -x "$PHUX_BIN" ]] || fail "missing executable $PHUX_BIN"
command -v tmux >/dev/null || fail "tmux is required"
command -v python3 >/dev/null || fail "python3 is required"

STEP=server-start
note "start isolated real server"
rm -f "$PHUX_SOCK"
RUST_LOG="${RUST_LOG:-info}" "$PHUX_BIN" server \
  --socket "$PHUX_SOCK" \
  --session "$SESSION" \
  --exit-after-idle 120 \
  >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!
wait_for_socket

STEP=seed-history
note "seed scrollback and bottom marker"
SEED_COMMAND='i=1; while [ "$i" -le 50000 ]; do printf "HISTORY-%05d\r\n" "$i"; i=$((i+1)); done; printf "READY-VISIBLE-MARKER\r\n"'
run_phux send-keys --socket "$PHUX_SOCK" "$SESSION" "$SEED_COMMAND" Enter
run_phux wait --socket "$PHUX_SOCK" --until READY-VISIBLE-MARKER --timeout 30 "$SESSION"

HOST_WRAPPER="$RUN_DIR/host-wrapper.sh"
cat >"$HOST_WRAPPER" <<'HOST_WRAPPER'
#!/bin/sh
set -u
before="$(stty -g)"
printf '%s\n' "$before" >"$PHUX_PROBE_MODE_BEFORE"
RUST_LOG=trace "$PHUX_PROBE_BIN" attach \
  --socket "$PHUX_PROBE_SOCKET" \
  "$PHUX_PROBE_SESSION" \
  --rec "$PHUX_PROBE_CAST" \
  2>"$PHUX_PROBE_CLIENT_LOG"
status=$?
after="$(stty -g)"
printf '%s\n' "$after" >"$PHUX_PROBE_MODE_AFTER"
if [ "$before" = "$after" ]; then
  printf 'HOST-TERMINAL-RESTORED\n'
else
  printf 'HOST-TERMINAL-NOT-RESTORED before=%s after=%s\n' "$before" "$after"
fi
printf '%s\n' "$status" >"$PHUX_PROBE_ATTACH_STATUS"
sleep 60
HOST_WRAPPER
chmod +x "$HOST_WRAPPER"

STEP=attach-visible-before-history
note "attach and require newest marker before progressive history settles"
printf -v ATTACH_COMMAND \
  'env PHUX_PROBE_BIN=%q PHUX_PROBE_SOCKET=%q PHUX_PROBE_SESSION=%q PHUX_PROBE_CAST=%q PHUX_PROBE_CLIENT_LOG=%q PHUX_PROBE_MODE_BEFORE=%q PHUX_PROBE_MODE_AFTER=%q PHUX_PROBE_ATTACH_STATUS=%q %q' \
  "$PHUX_BIN" \
  "$PHUX_SOCK" \
  "$SESSION" \
  "$COMPLETE_CAST" \
  "$CLIENT_LOG" \
  "$RUN_DIR/mode.before" \
  "$RUN_DIR/mode.after" \
  "$RUN_DIR/attach.status" \
  "$HOST_WRAPPER"
record_command "${TMUX[@]}" new-session -d -s "$SESSION" -x "$COLS" -y "$ROWS" "$ATTACH_COMMAND"
"${TMUX[@]}" new-session -d -s "$SESSION" -x "$COLS" -y "$ROWS" "$ATTACH_COMMAND"
deadline=$((SECONDS + 15))
while (( SECONDS < deadline )); do
  screen "$SESSION" >"$RUN_DIR/.screen"
  if grep -Fq -- 'Your session is live' "$RUN_DIR/.screen"; then
    SAW_ONBOARDING=1
    capture first-use
    "${TMUX[@]}" send-keys -t "$SESSION" \
      "clear; printf 'ONBOARDING-PASSTHROUGH\\r\\nREADY-VISIBLE-MARKER\\r\\n'" Enter
    assert_screen_contains "$SESSION" ONBOARDING-PASSTHROUGH
    break
  fi
  if grep -Fq -- READY-VISIBLE-MARKER "$RUN_DIR/.screen"; then
    break
  fi
  sleep 0.05
done
assert_screen_contains "$SESSION" READY-VISIBLE-MARKER
capture attach-visible
assert_marker_once "$RUN_DIR/attach-visible.txt" READY-VISIBLE-MARKER
if (( SAW_ONBOARDING == 1 )); then
  assert_marker_once "$RUN_DIR/attach-visible.txt" ONBOARDING-PASSTHROUGH
  STEP=first-use-return
  note "first detach reassures, then the same session returns"
  "${TMUX[@]}" send-keys -t "$SESSION" C-a
  "${TMUX[@]}" send-keys -t "$SESSION" d
  assert_screen_contains "$SESSION" HOST-TERMINAL-RESTORED
  assert_file_contains "$CLIENT_LOG" 'phux: session still running; run `phux` when you want to come back'
  cp "$CLIENT_LOG" "$RUN_DIR/first-detach.log"
  "${TMUX[@]}" kill-session -t "$SESSION"
  "${TMUX[@]}" new-session -d -s "$SESSION" -x "$COLS" -y "$ROWS" "$ATTACH_COMMAND"
  assert_screen_contains "$SESSION" 'Welcome back - this is the session you left running'
  assert_screen_contains "$SESSION" READY-VISIBLE-MARKER
  capture first-return
fi

HISTORY_PAGES_AT_MARKER="$(grep -c 'history_page' "$CLIENT_LOG" 2>/dev/null || true)"
STEP=typing-during-history
note "type through the attached client while history pages are in flight"
"${TMUX[@]}" send-keys -t "$SESSION" "printf 'TYPED-DURING-HISTORY\\r\\n'" Enter
assert_screen_contains "$SESSION" TYPED-DURING-HISTORY
deadline=$((SECONDS + 15))
while (( SECONDS < deadline )); do
  HISTORY_PAGES_AFTER_MARKER="$(grep -c 'history_page' "$CLIENT_LOG" 2>/dev/null || true)"
  if (( HISTORY_PAGES_AFTER_MARKER > HISTORY_PAGES_AT_MARKER )); then
    break
  fi
  sleep 0.02
done
printf 'history_pages_at_marker=%s\nhistory_pages_after_marker=%s\n' \
  "$HISTORY_PAGES_AT_MARKER" "${HISTORY_PAGES_AFTER_MARKER:-0}" \
  >"$RUN_DIR/history-order.txt"
(( ${HISTORY_PAGES_AFTER_MARKER:-0} > HISTORY_PAGES_AT_MARKER )) \
  || fail "no progressive history page arrived after the newest marker was already visible"
capture typed-during-history
assert_marker_once "$RUN_DIR/typed-during-history.txt" TYPED-DURING-HISTORY

STEP=pageup-anchor
note "PageUp keeps its document anchor while live output arrives"
"${TMUX[@]}" send-keys -t "$SESSION" C-a
"${TMUX[@]}" send-keys -t "$SESSION" "["
"${TMUX[@]}" send-keys -t "$SESSION" PageUp
sleep 0.15
capture pageup-before
grep -Eo 'HISTORY-[0-9]{5}' "$RUN_DIR/pageup-before.txt" >"$RUN_DIR/pageup-anchor.before" || true
[[ -s "$RUN_DIR/pageup-anchor.before" ]] || fail "PageUp did not expose seeded history"
run_phux send-keys --socket "$PHUX_SOCK" "$SESSION" "printf 'LIVE-WHILE-PAGED\\r\\n'" Enter
sleep 0.2
capture pageup-after-live
grep -Eo 'HISTORY-[0-9]{5}' "$RUN_DIR/pageup-after-live.txt" >"$RUN_DIR/pageup-anchor.after" || true
cmp -s "$RUN_DIR/pageup-anchor.before" "$RUN_DIR/pageup-anchor.after" \
  || fail "PageUp document anchor jumped when live output arrived"
"${TMUX[@]}" send-keys -t "$SESSION" Escape
assert_screen_contains "$SESSION" LIVE-WHILE-PAGED

STEP=resize-split-resync
note "resize, split, and repaint stay exact"
"${TMUX[@]}" resize-window -t "$SESSION" -x 96 -y 28
sleep 0.2
[[ "$("${TMUX[@]}" display-message -p -t "$SESSION" '#{window_width}x#{window_height}')" == "96x28" ]] \
  || fail "tmux host did not reach 96x28"
"${TMUX[@]}" send-keys -t "$SESSION" C-a
"${TMUX[@]}" send-keys -t "$SESSION" "%"
sleep 0.2
"${TMUX[@]}" send-keys -t "$SESSION" "printf 'SPLIT-PANE-MARKER\\r\\n'" Enter
assert_screen_contains "$SESSION" SPLIT-PANE-MARKER
"${TMUX[@]}" resize-window -t "$SESSION" -x "$COLS" -y "$ROWS"
sleep 0.2
capture resize-split-resync
[[ "$("${TMUX[@]}" display-message -p -t "$SESSION" '#{window_width}x#{window_height}')" == "${COLS}x${ROWS}" ]] \
  || fail "tmux host did not return to ${COLS}x${ROWS}"
assert_marker_once "$RUN_DIR/resize-split-resync.txt" READY-VISIBLE-MARKER
assert_marker_once "$RUN_DIR/resize-split-resync.txt" SPLIT-PANE-MARKER

STEP=host-restoration
note "clean detach restores the host terminal exactly"
"${TMUX[@]}" send-keys -t "$SESSION" C-a
"${TMUX[@]}" send-keys -t "$SESSION" d
assert_screen_contains "$SESSION" HOST-TERMINAL-RESTORED
capture host-restored
cmp -s "$RUN_DIR/mode.before" "$RUN_DIR/mode.after" \
  || fail "stty mode differs after detach"
[[ "$(cat "$RUN_DIR/attach.status")" == "0" ]] || fail "attach returned non-zero"

STEP=interrupted-recording
note "interrupt a second live recording after a visible marker"
INTERRUPT="interrupt"
"${TMUX[@]}" new-session -d -s "$INTERRUPT" -x "$COLS" -y "$ROWS" \
  "RUST_LOG=trace '$PHUX_BIN' attach --socket '$PHUX_SOCK' '$SESSION' --rec '$INTERRUPTED_CAST' 2>'$RUN_DIR/interrupted-client.log'"
assert_screen_contains "$INTERRUPT" READY-VISIBLE-MARKER
"${TMUX[@]}" send-keys -t "$INTERRUPT" "printf 'INTERRUPTED-CAST-MARKER\\r\\n'" Enter
assert_screen_contains "$INTERRUPT" INTERRUPTED-CAST-MARKER
capture interrupted-before-kill "$INTERRUPT"
INTERRUPT_PID="$("${TMUX[@]}" display-message -p -t "$INTERRUPT" '#{pane_pid}')"
kill -KILL "$INTERRUPT_PID"
sleep 0.2
"${TMUX[@]}" kill-session -t "$INTERRUPT" 2>/dev/null || true
[[ -s "$INTERRUPTED_CAST" ]] || fail "interrupted attach left no playable cast prefix"

validate_cast() {
  local cast="$1"
  local marker="$2"
  local completion="$3"
  python3 - "$cast" "$marker" "$completion" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
marker = sys.argv[2]
expect_complete = sys.argv[3] == "complete"
raw = path.read_bytes()
text = raw.decode("utf-8")
lines = text.splitlines()
assert lines, f"{path}: empty asciicast"
header = json.loads(lines[0])
assert header["version"] in (2, 3), header
assert ("duration" in header) == expect_complete, header
events = [json.loads(line) for line in lines[1:] if line.strip()]
assert events, f"{path}: no events"
assert all(event[1] in ("o", "r", "m", "x") for event in events), events
output = "".join(event[2] for event in events if event[1] == "o")
assert marker in output, f"{path}: missing {marker}"
assert "ghostty.snapshot" not in output
assert "\x00" not in output
assert "\ufffd" not in output
allowed = {7, 8, 9, 10, 13, 27}
bad = sorted({ord(char) for char in output if ord(char) < 32 and ord(char) not in allowed})
assert not bad, f"{path}: non-VT control bytes {bad}"
summary = {
    "path": str(path),
    "complete": expect_complete,
    "events": len(events),
    "output_chars": len(output),
    "portable_vt_only": True,
}
path.with_suffix(path.suffix + ".summary.json").write_text(json.dumps(summary, sort_keys=True) + "\n")
PY
}

STEP=cast-portability
note "assert complete and interrupted casts contain portable VT only"
validate_cast "$COMPLETE_CAST" TYPED-DURING-HISTORY complete
validate_cast "$INTERRUPTED_CAST" INTERRUPTED-CAST-MARKER interrupted

replay_cast() {
  local cast="$1"
  local marker="$2"
  local name="$3"
  local reply pane snapshot
  record_command "$PHUX_BIN" play --socket "$PHUX_SOCK" --json --speed 100 --idle-limit 0.05 "$cast" "$SESSION"
  reply="$("$PHUX_BIN" play --socket "$PHUX_SOCK" --json --speed 100 --idle-limit 0.05 "$cast" "$SESSION" 2>>"$TRANSCRIPT")"
  printf '%s\n' "$reply" >"$RUN_DIR/$name-play.json"
  pane="$(python3 -c 'import json,sys; print("@" + str(json.load(sys.stdin)["terminal_id"]))' <<<"$reply")"
  run_phux wait --socket "$PHUX_SOCK" --until "$marker" --timeout 15 "$pane"
  record_command "$PHUX_BIN" snapshot --socket "$PHUX_SOCK" --json "$pane"
  snapshot="$("$PHUX_BIN" snapshot --socket "$PHUX_SOCK" --json "$pane" 2>>"$TRANSCRIPT")"
  printf '%s\n' "$snapshot" >"$RUN_DIR/$name-replay-snapshot.json"
  assert_file_contains "$RUN_DIR/$name-replay-snapshot.json" "$marker"
}

STEP=complete-replay
note "replay complete recording through a real server pane"
replay_cast "$COMPLETE_CAST" TYPED-DURING-HISTORY complete

STEP=interrupted-replay
note "replay interrupted recording prefix through a real server pane"
replay_cast "$INTERRUPTED_CAST" INTERRUPTED-CAST-MARKER interrupted

STEP=done
note "PASS"
printf 'surface artifacts: %s\n' "$RUN_DIR"
