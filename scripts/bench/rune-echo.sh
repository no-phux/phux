#!/usr/bin/env bash
# Rune keystroke-echo hops against an isolated server.
#
# Starts a server on a temp socket (its own HOME and a scrubbed environment;
# never the user's), then runs phux-rune headless with --realtime and a
# --script that types KEYS keystrokes GAP_MS apart into the shell, a letter
# then Backspace, so the line stays short. Both processes append to one
# PHUX_ECHO_HOPS trace, and scripts/bench/echo-hops.py prints p50/p99 per
# hop and end to end (stdin->stdout: key event to the first frame drawn
# that shows the echo).
#
# Headless frames are paced at FPS rather than by a display: the end to end
# here is the client and server work, not vsync or the compositor. FPS
# defaults to 1000, which renders as soon as the last frame is done.
#
# --window (macOS) opens a real window instead and posts the keystrokes to
# it with post-keys.swift, which needs the Accessibility permission. The
# window takes focus for the run; `stdout` is then the frame's present.
#
# Usage: rune-echo.sh [--window] PHUX_BIN RUNE_BIN [KEYS] [GAP_MS] [FPS]
# PHUX_ECHO_FAST=0 is passed through for a before/after of the server's
# fast paths. Prints the trace's path and keeps the scratch directory.
set -euo pipefail
WINDOW=0
if [[ ${1:-} == --window ]]; then WINDOW=1; shift; fi
PHUX_BIN=$1; RUNE_BIN=$2; KEYS=${3:-200}; GAP_MS=${4:-120}; FPS=${5:-1000}
HERE=$(cd "$(dirname "$0")" && pwd)
abs() { case $1 in /*) echo "$1" ;; *) echo "$(pwd)/$1" ;; esac; }
PHUX_BIN=$(abs "$PHUX_BIN"); RUNE_BIN=$(abs "$RUNE_BIN")
# Under /tmp: a Unix socket path has to stay short.
H=$(mktemp -d /tmp/rune-echo.XXXXXX)
mkdir -p "$H/home" "$H/prefs" "$H/state" "$H/config/phux" "$H/run"
: > "$H/config/phux/config.toml"
printf "PS1='BENCH> '\n" > "$H/shrc"
TRACE="$H/hops.log"
ISO=(env -i PATH=/usr/bin:/bin:/usr/sbin:/sbin HOME="$H/home" TMPDIR="$H" TERM=xterm-256color
    LANG=en_US.UTF-8 XDG_STATE_HOME="$H/state" XDG_CONFIG_HOME="$H/config" XDG_RUNTIME_DIR="$H/run"
    SHELL=/bin/sh ENV="$H/shrc" PS1='BENCH> ' PHUX_PROFILE=rune-echo RUNE_PREFS_DIR="$H/prefs"
    PHUX_ECHO_HOPS="$TRACE" ${PHUX_ECHO_FAST:+PHUX_ECHO_FAST="$PHUX_ECHO_FAST"}
    ${WGPU_BACKEND:+WGPU_BACKEND="$WGPU_BACKEND"} ${VK_DRIVER_FILES:+VK_DRIVER_FILES="$VK_DRIVER_FILES"})
SOCK="$H/s"
"${ISO[@]}" "$PHUX_BIN" --socket "$SOCK" server --session bench >"$H/server.log" 2>&1 &
SRV=$!
trap 'kill "$SRV" 2>/dev/null || true; wait "$SRV" 2>/dev/null || true' EXIT
for _ in $(seq 1 150); do [[ -S $SOCK ]] && break; sleep 0.1; done
[[ -S $SOCK ]] || { echo "server never listened: $(cat "$H/server.log")" >&2; exit 1; }

if [[ $WINDOW == 1 ]]; then
    swiftc -O "$HERE/post-keys.swift" -o "$H/post-keys"
    "${ISO[@]}" "$RUNE_BIN" --socket "$SOCK" --session bench >"$H/rune.log" 2>&1 &
    RUNE=$!
    trap 'kill "$RUNE" "$SRV" 2>/dev/null || true; wait "$RUNE" "$SRV" 2>/dev/null || true' EXIT
    # Attach and draw the prompt first.
    sleep 3
    "$H/post-keys" "$RUNE" "$KEYS" "$GAP_MS"
    sleep 0.5
    python3 "$HERE/echo-hops.py" "$TRACE"
    echo "trace: $TRACE"
    exit 0
fi

# Three seconds to attach and draw the prompt, then the keystrokes.
python3 - "$KEYS" "$GAP_MS" > "$H/script.json" <<'PY'
import json, sys
keys, gap = int(sys.argv[1]), int(sys.argv[2]) / 1000
events = []
for k in range(keys):
    t = round(3 + k * gap, 4)
    events.append({"t": t, "type": "type", "text": "a"} if k % 2 == 0
                  else {"t": t, "type": "key", "name": "Backspace"})
events.append({"t": round(3 + keys * gap + 0.5, 4), "type": "mark", "name": "done"})
print(json.dumps(events))
PY
AT=$(python3 -c "print(3 + $KEYS * $GAP_MS / 1000 + 0.5)")
"${ISO[@]}" "$RUNE_BIN" --socket "$SOCK" --session bench --script "$H/script.json" \
    --realtime --fps "$FPS" --snap "$H/last.png" --at "$AT" --size 1100x720 >"$H/rune.log" 2>&1 \
    || { echo "phux-rune failed: $(tail -20 "$H/rune.log")" >&2; exit 1; }
python3 "$HERE/echo-hops.py" "$TRACE"
echo "trace: $TRACE"
