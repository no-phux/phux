#!/usr/bin/env bash
# Record the real TUI against a disposable server; no agent accounts or TTY needed.
# Usage: bash scripts/record-readme.sh [OUTPUT.gif]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PHUX="${PHUX:-$ROOT/target/debug/phux}"
OUTPUT="${1:-$ROOT/docs/assets/tour.gif}"
[[ -x "$PHUX" ]] || { echo "Build phux first: cargo build --locked -p phux --bin phux" >&2; exit 1; }
PHUX="$(cd "$(dirname "$PHUX")" && pwd)/$(basename "$PHUX")"
for name in ${!PHUX_@}; do unset "$name"; done
WORK="$(mktemp -d "${TMPDIR:-/tmp}/px-tour.XXXXXX")"
SERVER_PID=""
REC_PID=""
cleanup() {
  if [[ -n "$REC_PID" ]]; then kill "$REC_PID" 2>/dev/null || true; wait "$REC_PID" 2>/dev/null || true; fi
  if [[ -n "$SERVER_PID" ]]; then kill "$SERVER_PID" 2>/dev/null || true; wait "$SERVER_PID" 2>/dev/null || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# An empty home and explicit socket keep inherited profiles, hooks, and tokens out.
mkdir -p "$WORK/home" "$WORK/demo"
export HOME="$WORK/home" XDG_CONFIG_HOME="$WORK/config" XDG_DATA_HOME="$WORK/data" XDG_STATE_HOME="$WORK/state"
export PHUX_SOCKET="$WORK/sock" PHUX_PROFILE=readme PHUX_NO_UPDATE_CHECK=1
PATH="$(dirname "$PHUX"):$PATH"
export PATH SHELL=/bin/sh PS1='$ ' ENV=/dev/null
phux() { "$PHUX" "$@"; }
env -i PATH="$PATH" HOME="$HOME" XDG_CONFIG_HOME="$XDG_CONFIG_HOME" \
  XDG_DATA_HOME="$XDG_DATA_HOME" XDG_STATE_HOME="$XDG_STATE_HOME" \
  PHUX_SOCKET="$PHUX_SOCKET" PHUX_PROFILE="$PHUX_PROFILE" PHUX_NO_UPDATE_CHECK=1 \
  SHELL=/bin/sh PS1='$ ' ENV=/dev/null TERM=xterm-256color \
  "$PHUX" server --no-seed --socket "$PHUX_SOCKET" --exit-after-idle 60 >"$WORK/server.log" 2>&1 &
SERVER_PID=$!
for _ in {1..200}; do
  if phux status --json >/dev/null 2>&1; then break; fi
  sleep 0.025
done
phux status --json >/dev/null
pane="@$(phux new --json -s tour -c "$WORK/demo" -- /bin/sh | python3 -c 'import json,sys; print(json.load(sys.stdin)["terminal_id"])')"
other="@$(phux spawn --json --target "$pane" --split horizontal -c "$WORK/demo" -- /bin/sh | python3 -c 'import json,sys; print(json.load(sys.stdin)["terminal_id"])')"
phux resize "$pane" 118x10 >/dev/null
phux resize "$other" 118x10 >/dev/null
phux send-keys "$pane" "clear; printf 'A shell that stays running.\\n'; uname -s" Enter
phux wait --until "$(uname -s)" --timeout 5 "$pane"
phux send-keys "$other" "clear; printf 'Read the same shell from another pane.\\n'" Enter
phux wait --idle 150 --timeout 5 "$other"

phux rec --composited tour --size 1120x400 --duration 8 -o "$WORK/tour.cast" >"$WORK/rec.log" 2>&1 &
REC_PID=$!
sleep 1
phux send-keys "$pane" 'printf "one server, two panes\n"' Enter
sleep 1
phux send-keys "$other" 'phux ls' Enter
sleep 1
phux send-keys "$other" "phux snapshot --tail 3 '$pane'" Enter
wait "$REC_PID"
REC_PID=""
# Trim connection bootstrap time, not content, so the first frame shows the UI.
python3 - "$WORK/tour.cast" <<'PY'
import json, sys
from pathlib import Path
path = Path(sys.argv[1])
header, *events = [json.loads(line) for line in path.read_text().splitlines()]
for event in events:
    event[0] = max(0, event[0] - 0.5)
path.write_text('\n'.join(json.dumps(row) for row in [header, *events]) + '\n')
PY
phux rec --from "$WORK/tour.cast" --fps 4 -o "$OUTPUT" >"$WORK/render.log"
[[ -s "$OUTPUT" ]] || { cat "$WORK/rec.log" >&2; exit 1; }
echo "Recorded $OUTPUT from this checkout's TUI."
