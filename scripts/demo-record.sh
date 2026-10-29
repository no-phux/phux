#!/usr/bin/env bash
# Record a shell demonstrating phux's own recording and rendering commands.
# Uses a private server and intermediates; no TTY or external renderer needed.
# Usage: bash scripts/demo-record.sh [OUTPUT_DIRECTORY]
# The default output directory is docs/assets; use separate directories for
# concurrent takes. Existing committed casts are also test fixtures.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

PHUX="${PHUX:-$REPO/target/debug/phux}"
[ -x "$PHUX" ] || { echo "Build phux first: cargo build --locked -p phux --bin phux" >&2; exit 1; }
PHUX="$(cd "$(dirname "$PHUX")" && pwd)/$(basename "$PHUX")"
for name in ${!PHUX_@}; do unset "$name"; done
BIN_DIR="$(dirname "$PHUX")"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/px-rec.XXXXXX")"
SOCKET="$WORK/sock"
STAGE=recdemo   # the pane we film
SUBJECT=work    # the pane that pane records

OUTPUT="${1:-$REPO/docs/assets}"
mkdir -p "$OUTPUT"
OUTPUT="$(cd "$OUTPUT" && pwd)"
CAST="$OUTPUT/recording-demo.cast"
GIF="$OUTPUT/recording-demo.gif"

# Hard budget: GitHub serves the whole file before the first frame paints,
# and repo history is permanent, so the check is a gate and not a warning.
MAX_BYTES=1048576

# Collapse long pauses while leaving time to read each command.
IDLE_LIMIT=2.5

# Backstop if the shell dies before its cleanup trap can run.
SERVER_IDLE_LIMIT=300

SERVER_PID=""
REC_PID=""
cleanup() {
  if [ -n "$REC_PID" ]; then kill "$REC_PID" 2>/dev/null || true; wait "$REC_PID" 2>/dev/null || true; fi
  if [ -n "$SERVER_PID" ]; then kill "$SERVER_PID" 2>/dev/null || true; wait "$SERVER_PID" 2>/dev/null || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -p "$WORK/home"
export HOME="$WORK/home" XDG_CONFIG_HOME="$WORK/config" XDG_DATA_HOME="$WORK/data" XDG_STATE_HOME="$WORK/state"
export PHUX_SOCKET="$SOCKET" PHUX_PROFILE=record-demo PHUX_NO_UPDATE_CHECK=1
env -i PATH="$BIN_DIR:/usr/bin:/bin" HOME="$HOME" \
  XDG_CONFIG_HOME="$XDG_CONFIG_HOME" XDG_DATA_HOME="$XDG_DATA_HOME" XDG_STATE_HOME="$XDG_STATE_HOME" \
  PHUX_SOCKET="$SOCKET" PHUX_PROFILE="$PHUX_PROFILE" PHUX_NO_UPDATE_CHECK=1 \
  SHELL=/bin/sh PS1='$ ' ENV=/dev/null TERM=xterm-256color \
  "$PHUX" server --session "$SUBJECT" --socket "$SOCKET" \
  --exit-after-idle "$SERVER_IDLE_LIMIT" >/dev/null 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 100); do
  [ -S "$SOCKET" ] && break
  sleep 0.1
done
if [ ! -S "$SOCKET" ]; then
  echo "phux server did not bind $SOCKET within 10s" >&2
  exit 1
fi
echo "server up on $SOCKET (pid $SERVER_PID), session \"$SUBJECT\" seeded"

"$PHUX" new --json --socket "$SOCKET" -s "$STAGE" -c "$WORK" -- /bin/sh >/dev/null
echo "created session \"$STAGE\" (the pane on camera)"

# Wipe the setup lines: the recording opens on a bare prompt.
"$PHUX" send-keys --socket "$SOCKET" "$STAGE" "clear" Enter
sleep 1

# Give the subject pane something worth observing. `phux rec` is a pure
# observer, so a quiescent pane records as two frames and the progress
# counter never ticks — the demo would be a still image of itself. Six
# seconds of output at 1 Hz is what makes beat 2 move.
"$PHUX" send-keys --socket "$SOCKET" "$SUBJECT" \
  "for i in 1 2 3 4 5 6 7 8; do printf '  \033[32mok\033[0m   step %d/8 built\n' \$i; sleep 1; done" \
  Enter

# --- the take -----------------------------------------------------------
# The host recorder runs in the background and the beats are sent against
# a wall clock. `--duration` is the only stop condition, so it must cover
# every beat plus the hold at the end; it is the budget the schedule below
# is written against.
mkdir -p "$(dirname "$CAST")"
"$PHUX" rec "$STAGE" --socket "$SOCKET" -o "$CAST" --duration 25 >/dev/null 2>&1 &
REC_PID=$!
sleep 0.7

beat() {
  "$PHUX" send-keys --socket "$SOCKET" "$STAGE" "$1" Enter
  sleep "$2"
}

# Narration is a shell comment, not simulated program output.
say() {
  "$PHUX" send-keys --socket "$SOCKET" "$STAGE" "# $1" Enter
  sleep 0.6
}

# 1. the session list — establishes that there are two live panes.
say "two live panes on this server"
beat "phux ls" 2.5
# 2. capture: the observer runs for 6s against the OTHER live pane, its
#    progress counter ticking as that pane paints. Naming the property
#    on camera is the point — it is the one people do not expect.
say "record one of them. a pure observer: no attach, no resize"
beat "phux rec $SUBJECT -o inner.cast --duration 6" 8.5
# 3. render: the same binary turns the cast into a GIF, in process.
say "render it to a GIF with the same binary. no vhs, no agg, no ffmpeg"
beat "phux rec --from inner.cast -o inner.gif --fps 10" 3
# 4. two real files on disk.
say "two real files"
beat "ls -lh inner.cast inner.gif" 3
# 5. a bare Enter. Every pause is clamped to IDLE_LIMIT, so the payoff
#    frame would otherwise flash past in one clamp before the loop
#    restarts; an empty line buys a second clamp on the same screen.
beat "" 3

wait "$REC_PID"
REC_PID=""
[ -s "$WORK/inner.cast" ] && [ -s "$WORK/inner.gif" ] || {
  echo "the recorded commands did not produce both private intermediate files" >&2
  exit 1
}
echo "captured $CAST ($(wc -c <"$CAST" | tr -d ' ') bytes)"

# --- render -------------------------------------------------------------
# Same binary, no external tools. --idle-limit collapses the dead air
# between beats so the GIF reads at conversational speed without the
# schedule above having to be tight.
"$PHUX" rec --from "$CAST" -o "$GIF" --fps 10 --idle-limit "$IDLE_LIMIT"
BYTES="$(wc -c <"$GIF" | tr -d ' ')"
echo "rendered $GIF ($BYTES bytes)"

if [ "$BYTES" -gt "$MAX_BYTES" ]; then
  echo "GIF is $BYTES bytes, over the $MAX_BYTES budget; shorten the take" >&2
  exit 1
fi
echo "under the ${MAX_BYTES}-byte budget by $((MAX_BYTES - BYTES)) bytes"
echo
echo "Both files are committed assets. Review the GIF before committing:"
echo "  open $GIF"
