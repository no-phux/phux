#!/usr/bin/env bash
# demo-record.sh — regenerate docs/assets/recording-demo.{cast,gif}.
#
# The meta-demo: phux records a terminal in which phux is recording a
# terminal, and the GIF you end up with was rendered by the same binary.
# Nothing outside this repo touches the pipeline — no asciinema, no vhs,
# no agg, no ffmpeg, no ImageMagick (ADR-0060).
#
# Unlike scripts/demo-setup.sh — which stages a session and hands the
# keyboard back to a human, because Beat 1 of the README demo needs real
# pixels from a graphics-capable terminal — this one runs end to end with
# no TTY anywhere. The pane is driven with `phux send-keys` and observed
# with `phux rec`, both of which are headless by construction. That is
# the point: the asset is reproducible, so a rendering change can be
# re-verified by re-running this instead of by staging a take.
#
# Everything happens on a PRIVATE server on a private socket, torn down
# on exit. Your own sessions are never touched, and a second run cannot
# collide with the first.
#
# Usage: scripts/demo-record.sh
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Prefer a local build: the whole claim is that THIS tree's binary made
# the asset, so an older phux on PATH would be the wrong answer here
# (the opposite of demo-setup.sh, which is happy with any phux).
if [ -x "$REPO/target/release/phux" ]; then
  PHUX="$REPO/target/release/phux"
elif [ -x "$REPO/target/debug/phux" ]; then
  PHUX="$REPO/target/debug/phux"
else
  echo "no phux build found; run 'cargo build --bin phux' first" >&2
  exit 1
fi
PHUX_DIR="$(dirname "$PHUX")"

# macOS caps sun_path at 104 bytes, so the socket lives at the root of
# /tmp rather than under a mktemp -d that could be arbitrarily deep.
SOCKET="/tmp/phux-demo-record-$$.sock"
STAGE=recdemo   # the pane we film
SUBJECT=work    # the pane that pane records

CAST="$REPO/docs/assets/recording-demo.cast"
GIF="$REPO/docs/assets/recording-demo.gif"

# Hard budget: this lands in a README-adjacent doc and GitHub serves the
# whole file before the first frame paints. docs/demo.md allows 2 MB; we
# hold half that, and repo history is permanent, so the check is a gate
# and not a warning.
MAX_BYTES=1048576

# Every pause longer than this collapses to it at render time. It doubles
# as the readability floor: it is exactly how long the final ten-line
# frame stays up before the animation loops.
IDLE_LIMIT=2.5

# Lifetime for the private server, as a backstop UNDER the trap below
# (ADR-0063). The trap is still the primary cleanup — it reclaims the
# socket the instant the script ends — but a trap cannot run if this
# script is SIGKILLed or the machine's shell is torn out from under it,
# and what leaks then is a daemon holding two live PTYs. The server exits
# on its own once nothing has talked to it for this long.
#
# Sized far above any gap in the schedule below: the longest stretch with
# no client connected is a few seconds between beats, and the take itself
# keeps `phux rec` connected for its whole 25s duration. A whole run is
# well under a minute, so this can only fire after the script is gone.
SERVER_IDLE_LIMIT=300

SERVER_PID=""
cleanup() {
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true
  rm -f "$SOCKET"
}
trap cleanup EXIT

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

"$PHUX" new --json --socket "$SOCKET" -s "$STAGE" >/dev/null
echo "created session \"$STAGE\" (the pane on camera)"

# Replace the seed shell with a hermetic one. `env -i` is deliberate:
# whoever regenerates this must get the same frames as the last person,
# and a starship/oh-my-zsh prompt would paint their hostname, git state,
# and a two-line prompt into a committed asset. PATH carries exactly the
# build dir plus the system bins, so the recorded commands read `phux`
# rather than a 90-column absolute path, and PHUX_SOCKET points the
# in-pane phux at this private server instead of the operator's.
"$PHUX" send-keys --socket "$SOCKET" "$STAGE" \
  "exec env -i TERM=xterm-256color HOME=$HOME PATH=$PHUX_DIR:/usr/bin:/bin PHUX_SOCKET=$SOCKET PS1='$ ' /bin/sh" \
  Enter
sleep 1
# A colored payoff line was tried here and removed on the evidence. Two
# attempts (a 256-color PS1, then an `ok()` helper printing a green
# U+2713) both arrived scrambled: the escape introducer survived but its
# parameter bytes were reordered into the tail, and the helper's nested
# quoting left /bin/sh sitting on a PS2 continuation prompt, so the take
# recorded a screen of `>` with no command output at all.
#
# It is NOT a phux bug — `send-keys` round-trips multi-byte UTF-8 and SGR
# escapes intact when they are the payload of a typed command (verified
# directly: `printf 'chevron:❯ check:✓ CJK:世界'` renders correctly). It
# is this hermetic /bin/sh re-expanding backslash escapes inside a prompt
# string and a function body. Since the asset is regenerated by hand and
# reviewed by eye, a legible monochrome capture is worth more than a
# colored one that is one quoting accident away from recording garbage.
# Color in the demo would have to come from the recorded program, not
# from the shell wrapping it.

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

# A typed `# comment` is echoed by the pane's shell and does nothing else,
# which is the cheapest possible narration: no helper function to explain,
# no wrapper command in frame, and it reads the way a person actually
# demonstrates a tool. It also fills the frame — the first cut of this
# asset used four bare commands and left the bottom 60 percent of every
# frame black.
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
beat "phux rec $SUBJECT -o /tmp/inner.cast --duration 6" 8.5
# 3. render: the same binary turns the cast into a GIF, in process.
say "render it to a GIF with the same binary. no vhs, no agg, no ffmpeg"
beat "phux rec --from /tmp/inner.cast -o /tmp/inner.gif --fps 10" 3
# 4. two real files on disk.
say "two real files"
beat "ls -lh /tmp/inner.cast /tmp/inner.gif" 3
# 5. a bare Enter. Every pause is clamped to IDLE_LIMIT, so the payoff
#    frame would otherwise flash past in one clamp before the loop
#    restarts; an empty line buys a second clamp on the same screen.
beat "" 3

wait "$REC_PID"
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
