#!/usr/bin/env bash
# Render-cost bench for the attach client (phux-l96p.3).
#
# Drives an ISOLATED phux server and a real `phux attach` inside an isolated
# tmux, floods the focused pane with `seq 1 300000`, and reports what the paint
# path cost: client CPU time, bytes written to the glass (via the `--rec`
# asciicast tee, which is exactly the byte stream the terminal received), and
# idle CPU over a quiet window at a prompt.
#
# Never touches the user's own server: its own HOME/XDG_*, its own
# PHUX_PROFILE, its own socket, its own tmux server.
#
# Usage: scripts/render-bench.sh /path/to/phux [LABEL] [COLS] [ROWS]
set -Eeuo pipefail

PHUX_BIN="${1:?usage: render-bench.sh /path/to/phux [LABEL] [COLS] [ROWS]}"
LABEL="${2:-run}"
COLS="${3:-120}"
ROWS="${4:-40}"
LINES_N="${PHUX_BENCH_LINES:-300000}"
DRIP_N="${PHUX_BENCH_DRIP:-10000}"
DRIP_DELAY="${PHUX_BENCH_DRIP_DELAY:-0.001}"
IDLE_SECS="${PHUX_BENCH_IDLE_SECS:-10}"

RUN_DIR="$(mktemp -d "/tmp/phux-render-bench-XXXXXX")"
TMUX_SOCKET="phux-bench-$$"
TMUX=(tmux -L "$TMUX_SOCKET")
PHUX_SOCK="$RUN_DIR/phux.sock"
SESSION="bench"
CAST="$RUN_DIR/glass.cast"
CLIENT_LOG="$RUN_DIR/client.log"
SERVER_LOG="$RUN_DIR/server.log"
SERVER_PID=""

mkdir -p "$RUN_DIR"/{home,config,state,cache,data,runtime}
chmod 700 "$RUN_DIR/runtime"
export HOME="$RUN_DIR/home"
export XDG_CONFIG_HOME="$RUN_DIR/config"
export XDG_STATE_HOME="$RUN_DIR/state"
export XDG_CACHE_HOME="$RUN_DIR/cache"
export XDG_DATA_HOME="$RUN_DIR/data"
export XDG_RUNTIME_DIR="$RUN_DIR/runtime"
export PHUX_PROFILE="render-bench-$$"

cleanup() {
  "${TMUX[@]}" kill-server 2>/dev/null || true
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

# CPU seconds consumed by $1 so far, as a float.
cpu_secs() {
  ps -p "$1" -o cputime= 2>/dev/null | awk -F: '
    NF==3 { printf "%.2f", $1*3600 + $2*60 + $3; exit }
    NF==2 { printf "%.2f", $1*60 + $2; exit }
    { print "0" }'
}

wait_for_socket() {
  local deadline=$((SECONDS + 30))
  while ((SECONDS < deadline)); do
    [[ -S "$PHUX_SOCK" ]] && return 0
    sleep 0.05
  done
  echo "server did not bind $PHUX_SOCK" >&2
  exit 1
}

wait_for_screen() {
  local needle="$1" deadline=$((SECONDS + ${PHUX_BENCH_WAIT_SECS:-300}))
  while ((SECONDS < deadline)); do
    if "${TMUX[@]}" capture-pane -p -t "$SESSION" 2>/dev/null | grep -Fq -- "$needle"; then
      return 0
    fi
    sleep 0.1
  done
  echo "screen never showed $needle" >&2
  "${TMUX[@]}" capture-pane -p -t "$SESSION" >&2 || true
  exit 1
}

"$PHUX_BIN" server --socket "$PHUX_SOCK" --session "$SESSION" --exit-after-idle 300 \
  >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!
wait_for_socket

# `--rec` is the only way to see the exact byte stream the glass received, but
# its asciicast encoder JSON-escapes every byte (~18x expansion) and that
# encoding lands in the CLIENT's CPU time. Set PHUX_BENCH_NO_REC=1 for a clean
# CPU number; leave it off to measure bytes.
REC_ARGS="--rec $CAST"
if [[ "${PHUX_BENCH_NO_REC:-0}" != "0" ]]; then
  REC_ARGS=""
  : >"$CAST"
fi
ATTACH="env PHUX_RENDER_PROF=${PHUX_RENDER_PROF:-0} PHUX_LOG=$CLIENT_LOG RUST_LOG=${RUST_LOG:-phux=info} \
  $PHUX_BIN attach --socket $PHUX_SOCK $SESSION $REC_ARGS"
"${TMUX[@]}" new-session -d -s "$SESSION" -x "$COLS" -y "$ROWS" "$ATTACH"

# Dismiss the first-use overlay BEFORE measuring: a modal suppresses pane
# paints entirely, which would benchmark the wrong path.
sleep 2
"${TMUX[@]}" send-keys -t "$SESSION" Escape
sleep 1
"${TMUX[@]}" send-keys -t "$SESSION" Escape
sleep 1
"$PHUX_BIN" send-keys --socket "$PHUX_SOCK" "$SESSION" 'echo BENCH-ATTACH-READY' Enter
wait_for_screen 'BENCH-ATTACH-READY'
sleep 1

CLIENT_PID="$("${TMUX[@]}" list-panes -t "$SESSION" -F '#{pane_pid}' | head -1)"
[[ -n "$CLIENT_PID" ]] || { echo "no client pid" >&2; exit 1; }

# Two workloads, because they stress different halves of the paint path.
#
#   flood - `seq 1 N` writes in big PTY-sized chunks, so many lines arrive per
#           inbound frame. This measures per-BYTE render cost.
#   drip  - one flushed write per line, so the server ships roughly one frame
#           per line. This measures per-FRAME cost: layout, chrome compose,
#           cursor tail, flush. It is the workload the paint scheduler exists
#           for, and the one a coalescing drain alone cannot help with.
phase() {
  local name="$1" command="$2" count="$3"
  local cpu_before cast_before t0 t1 cpu_after cast_after
  cpu_before="$(cpu_secs "$CLIENT_PID")"
  cast_before="$(wc -c <"$CAST" | tr -d ' ')"
  t0=$(python3 -c 'import time; print(time.time())')
  "$PHUX_BIN" send-keys --socket "$PHUX_SOCK" "$SESSION" "$command" Enter
  wait_for_screen "BENCH-${name}-DONE"
  sleep 2
  t1=$(python3 -c 'import time; print(time.time())')
  cpu_after="$(cpu_secs "$CLIENT_PID")"
  [[ -n "$cpu_after" ]] || { echo "client $CLIENT_PID died during $name" >&2; exit 1; }
  cast_after="$(wc -c <"$CAST" | tr -d ' ')"
  python3 -c '
import sys
(name, cb, ca, sb, sa, t0, t1, count) = sys.argv[1:]
cpu = float(ca) - float(cb)
byt = int(sa) - int(sb)
wall = float(t1) - float(t0)
n = int(count)
print(f"  {name:<6} lines={n:<7} wall={wall:6.2f}s  cpu={cpu:6.2f}s  "
      f"bytes={byt:>10} ({byt/1e6:6.2f} MB)  bytes/line={byt/n:6.1f}")
' "$name" "$cpu_before" "$cpu_after" "$cast_before" "$cast_after" "$t0" "$t1" "$count"
}

echo "== render-bench [$LABEL] =="
# The marker is assembled at runtime (`printf '%s-DONE' FLOOD`) so the echoed
# command line itself never matches it — otherwise the wait returns the instant
# the shell echoes the command and times nothing.
# The flood is emitted in $FLOOD_CHUNKS bursts with a breath between them.
# At full speed the SERVER's ResourceOutput pump outruns the client, drops
# frames, and asks for an in-band resync that the client's session kernel
# rejects outright ("live sequence gap at N; expected M") -- `phux attach`
# then exits 1. That is a real defect on `main` and it is not in this lane's
# files; chunking keeps the pump inside its window so the paint path is what
# the numbers measure.
FLOOD_CHUNKS="${PHUX_BENCH_FLOOD_CHUNKS:-12}"
FLOOD_EACH=$((LINES_N / FLOOD_CHUNKS))
phase FLOOD "for c in $(seq 1 "$FLOOD_CHUNKS" | tr '\n' ' '); do seq 1 $FLOOD_EACH; sleep 0.3; done; printf 'BENCH-%s-DONE\\n' FLOOD" "$LINES_N"
# The drip is rate-limited on purpose. One flushed line per tick means one
# inbound frame per line, which is the per-FRAME cost this lane is about, and
# a paced producer keeps the server's output pump inside its window so the
# run measures painting rather than the resync defect above.
cat >"$RUN_DIR/drip.py" <<'DRIP_PY'
import sys, time

n = int(sys.argv[1])
delay = float(sys.argv[2])
for i in range(n):
    sys.stdout.write(f"{i}\n")
    sys.stdout.flush()
    time.sleep(delay)
DRIP_PY
phase DRIP "python3 $RUN_DIR/drip.py $DRIP_N $DRIP_DELAY; printf 'BENCH-%s-DONE\\n' DRIP" "$DRIP_N"

# Idle window: a quiet prompt must cost essentially nothing.
cpu_idle_before="$(cpu_secs "$CLIENT_PID")"
sleep "$IDLE_SECS"
cpu_idle_after="$(cpu_secs "$CLIENT_PID")"
python3 -c 'import sys; print(f"  idle   {sys.argv[3]}s at a prompt: cpu={float(sys.argv[2])-float(sys.argv[1]):.2f}s")' \
  "$cpu_idle_before" "$cpu_idle_after" "$IDLE_SECS"

blocks="$(LC_ALL=C grep -c "$(printf '\033')\[?2026h" "$CAST" 2>/dev/null || echo 0)"
echo "  cast lines carrying a DEC 2026 open: $blocks"

echo "artifacts: $RUN_DIR"
if [[ -s "$CLIENT_LOG" ]]; then
  echo "-- render_prof (last 5) --"
  grep -F 'render_prof' "$CLIENT_LOG" | tail -5 || true
fi
