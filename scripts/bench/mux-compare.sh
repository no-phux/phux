#!/usr/bin/env bash
# Black-box performance comparison of terminal multiplexers, driven through a
# private tmux server: attach-to-first-paint, key echo latency, bulk output
# throughput, RSS after a large scrollback, idle CPU, and resize repaint cost.
#
# Every server this script starts is isolated behind its own HOME, XDG_*, and
# socket path, from a scrubbed environment (env -i), and it never touches a
# multiplexer server it did not start.
set -Eeuo pipefail
export LC_ALL=C

if [[ -z ${EPOCHREALTIME:-} ]]; then
  for candidate in /opt/homebrew/bin/bash /usr/local/bin/bash; do
    [[ -x $candidate ]] && exec "$candidate" "$0" "$@"
  done
  printf 'bash 5.0+ is required (EPOCHREALTIME)\n' >&2
  exit 2
fi

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MUX_SELECT=both
BIG_HISTORY=0
PTY_ITERS=60
BH_LINES=60000
BH_COLS=188
BH_PANES=4
PTY_PROBE="$REPO/scripts/bench/pty-echo.py"
UDP_DELAY="$REPO/scripts/bench/udp-delay.py"
RTT_MS=0
PATH_MBIT=0
LOSS_PERCENT=0
LOSS_SEED=0
PHUX_BIN="$REPO/target/release/phux"
HERDR_BIN="${HERDR_BIN:-/opt/homebrew/bin/herdr}"
TMUX_BIN="${TMUX_BIN:-tmux}"
OUT_DIR=""
ATTACH_SAMPLES=5
KEY_SAMPLES=40
SEQ_LINES=300000
COLS=120
ROWS=40
PROMPT='BENCH>'

usage() {
  cat <<'USAGE'
Usage: scripts/bench/mux-compare.sh [options]
  --mux LIST              comma list of lanes, or "both"/"all" (default: both)
                          lanes: phux phux-ws phux-quic herdr
  --phux-bin PATH         phux binary (default: target/release/phux)
  --herdr-bin PATH        herdr binary (default: /opt/homebrew/bin/herdr)
  --out DIR               raw sample output (default: target/bench/mux-compare-<ts>)
  --attach-samples N      attach samples per phase (default: 5)
  --key-samples N         key echo samples (default: 40)
  --seq-lines N           throughput line count (default: 300000)
  --pty-iters N           byte-level echo samples (default: 60)
  --big-history           also run the 4-pane / 60k-line attach scenario
  --rtt-ms N              run the phux-quic lane through a userspace UDP relay
                          that adds N ms of round-trip delay (N/2 each way), so
                          a loopback run can show what costs a round trip
  --path-mbit N           UDP payload Mbit/s per direction (default: unlimited)
  --loss-percent N        independent seeded datagram loss (default: 0)
  --loss-seed N           loss RNG seed (default: 0)
USAGE
}

while (($#)); do
  case "$1" in
    --mux) MUX_SELECT="$2"; shift 2 ;;
    --phux-bin) PHUX_BIN="$2"; shift 2 ;;
    --herdr-bin) HERDR_BIN="$2"; shift 2 ;;
    --out) OUT_DIR="$2"; shift 2 ;;
    --attach-samples) ATTACH_SAMPLES="$2"; shift 2 ;;
    --key-samples) KEY_SAMPLES="$2"; shift 2 ;;
    --seq-lines) SEQ_LINES="$2"; shift 2 ;;
    --pty-iters) PTY_ITERS="$2"; shift 2 ;;
    --big-history) BIG_HISTORY=1; shift ;;
    --rtt-ms) RTT_MS="$2"; shift 2 ;;
    --path-mbit) PATH_MBIT="$2"; shift 2 ;;
    --loss-percent) LOSS_PERCENT="$2"; shift 2 ;;
    --loss-seed) LOSS_SEED="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'unknown option: %s\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
done

[[ -n $OUT_DIR ]] || OUT_DIR="$REPO/target/bench/mux-compare-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$OUT_DIR"

# The run root must stay short: a unix socket path is capped at 104 bytes.
# `mktemp -d` (not a bare pid) so a stale directory from a killed run, or a
# recycled pid, can never be adopted and then `rm -rf`d on exit — the same
# rule scripts/render-bench.sh follows.
RUN="$(mktemp -d /tmp/muxbench-XXXXXX)"
# Loopback listener ports, derived from the pid so parallel runs do not collide.
WS_PORT=$(( 18000 + ($$ % 900) * 2 ))
QUIC_PORT=$(( WS_PORT + 1 ))
# The WAN-simulation relay sits in front of the QUIC listener on its own port.
QUIC_RELAY_PORT=$(( WS_PORT + 900 ))
RELAY_PID=""
TMUX_SOCK="bench-$$"
TMUX=("$TMUX_BIN" -L "$TMUX_SOCK")
SERVER_PIDS=()
COMMAND_LOG="$OUT_DIR/commands.txt"
declare -A RESULT=()
declare -A RAW=()

cleanup() {
  "${TMUX[@]}" kill-server 2>/dev/null || true
  [[ -n $RELAY_PID ]] && kill "$RELAY_PID" 2>/dev/null
  RELAY_PID=""
  local pid signal
  for signal in TERM KILL; do
    for pid in "${SERVER_PIDS[@]:-}"; do
      [[ -n $pid ]] && kill "-$signal" "$pid" 2>/dev/null || true
    done
    sleep 0.3
  done
  rm -rf "$RUN"
}
trap cleanup EXIT

log_cmd() { printf '%s\n' "$*" >>"$COMMAND_LOG"; }

now_ms() { local s=${EPOCHREALTIME}; printf '%s' "$(( ${s%.*} * 1000 + 10#${s#*.} / 1000 ))"; }

# Sorted percentile over the numeric arguments; p is 0-100.
pct() {
  local p=$1; shift
  local sorted=()
  mapfile -t sorted < <(printf '%s\n' "$@" | sort -n)
  (($#)) || { printf 'n/a'; return; }
  printf '%s' "${sorted[$(( (p * ($# - 1) + 50) / 100 ))]}"
}

mean() {
  local sum=0 v
  (($#)) || { printf 'n/a'; return; }
  for v in "$@"; do sum=$((sum + v)); done
  printf '%s' $((sum / $#))
}

# ps reports accumulated CPU as [hh:]mm:ss.ff; normalise it to milliseconds.
cpu_ms() {
  local pid=${1:-} raw h m s
  [[ -n $pid ]] || { printf 0; return; }
  raw=$(ps -o time= -p "$pid" 2>/dev/null | tr -d ' ') || true
  [[ -n $raw ]] || { printf 0; return; }
  IFS=: read -r h m s <<<"$raw"
  if [[ -z $s ]]; then s=$m; m=$h; h=0; fi
  printf '%s' "$(awk -v h="$h" -v m="$m" -v s="$s" 'BEGIN{printf "%d", (h*3600+m*60+s)*1000}')"
}

# Both tolerate an empty pid so callers need no guards.
rss_kb() { ps -o rss= -p "${1:-0}" 2>/dev/null | tr -d ' ' || printf 0; }
pcpu() { ps -o %cpu= -p "${1:-0}" 2>/dev/null | tr -d ' ' || printf 0; }

capture() { "${TMUX[@]}" capture-pane -p -t "$1" 2>/dev/null || true; }

# Poll a pane until NEEDLE is on screen; echo elapsed ms since START_MS, or -1.
poll_until() {
  local session=$1 needle=$2 start=$3 timeout=$4 nap=$5
  while (( $(now_ms) - start < timeout )); do
    if capture "$session" | grep -Fq -- "$needle"; then
      printf '%s' "$(( $(now_ms) - start ))"
      return 0
    fi
    sleep "$nap"
  done
  printf '%s' -1
  return 1
}

wait_gone() {
  local pid=$1 deadline=$(( SECONDS + 15 ))
  while kill -0 "$pid" 2>/dev/null && (( SECONDS < deadline )); do sleep 0.05; done
}

client_pid() {
  local session=$1 pane
  pane=$("${TMUX[@]}" display-message -p -t "$session" '#{pane_pid}' 2>/dev/null || true)
  [[ -n $pane ]] || { printf ''; return; }
  pgrep -P "$pane" 2>/dev/null | head -n 1
}

# --- multiplexer definitions -------------------------------------------------
# Each mux writes an attach wrapper script and answers to the same verbs.
MUX=""            # current lane name
FAMILY=""         # phux, herdr or tmux
MUX_HOME=""       # isolated root for the current lane
MUX_ATTACH=""     # path to the attach wrapper
MUX_ARGV=""       # attach command line, shared by tmux and the pty probe
MUX_SERVER_PID=""
MUX_DEBUG_LOG=0   # start the next server with handshake-level logging
MUX_DETACH=()     # tmux send-keys arguments, one key per element

BASE_ENV=(env -i PATH=/usr/bin:/bin:/usr/sbin:/sbin TERM=xterm-256color LANG=en_US.UTF-8)

prepare_dirs() {
  MUX_HOME="$RUN/$1"
  mkdir -p "$MUX_HOME"/{home,cfg,state,cache,data,run}
  chmod 700 "$MUX_HOME/run"
  printf "PS1='%s '\n" "$PROMPT" >"$MUX_HOME/shrc"
}

# Environment shared by every isolated process of a mux: scrubbed, redirected,
# and carrying the distinctive prompt used as the paint marker.
iso_env() {
  printf '%s' "PATH=/usr/bin:/bin:/usr/sbin:/sbin TERM=xterm-256color LANG=en_US.UTF-8 \
HOME=$MUX_HOME/home XDG_CONFIG_HOME=$MUX_HOME/cfg XDG_STATE_HOME=$MUX_HOME/state \
XDG_CACHE_HOME=$MUX_HOME/cache XDG_DATA_HOME=$MUX_HOME/data XDG_RUNTIME_DIR=$MUX_HOME/run \
SHELL=/bin/sh ENV=$MUX_HOME/shrc PS1='$PROMPT ' PHUX_PROFILE=muxbench HERDR_DISABLE_SOUND=1"
}

write_attach_wrapper() {
  # A bare shell writes its prompt to stderr, so the baseline keeps stderr on
  # the pane; a multiplexer client paints through its own PTY, so its stderr is
  # diverted to a log where it cannot pollute the screen.
  local err="2>>\"$MUX_HOME/client.log\""
  [[ $MUX == tmux ]] && err=""
  cat >"$MUX_ATTACH" <<EOF
#!/bin/sh
env -i $(iso_env) $1 $err
echo "ATTACH_EXIT=\$?"
sleep 900
EOF
  chmod +x "$MUX_ATTACH"
  log_cmd "# attach wrapper ($MUX): env -i <isolated env> $1"
}

start_server() {
  case "$FAMILY" in
    phux)
      rm -f "$MUX_HOME/mux.sock"
      local listeners="--listen 127.0.0.1:$WS_PORT --quic 127.0.0.1:$QUIC_PORT"
      local filter=""
      # Handshake accounting needs frame-level server logs; the measured runs
      # stay at the default level so logging never taxes a timing.
      (( MUX_DEBUG_LOG )) && filter="RUST_LOG=phux_server=debug"
      log_cmd "env -i <isolated env> $filter $PHUX_BIN server --socket $MUX_HOME/mux.sock --session bench $listeners --exit-after-idle 900"
      { eval "exec ${BASE_ENV[*]} $(iso_env) $filter \"\$PHUX_BIN\" server --socket \"\$MUX_HOME/mux.sock\" \
        --session bench $listeners --exit-after-idle 900" >"$MUX_HOME/server.log" 2>&1; } &
      MUX_SERVER_PID=$!
      local deadline=$((SECONDS + 30))
      while [[ ! -S "$MUX_HOME/mux.sock" ]] && (( SECONDS < deadline )); do sleep 0.05; done
      [[ -S "$MUX_HOME/mux.sock" ]] || { printf 'phux server never bound its socket\n' >&2; return 1; }
      ;;
    herdr)
      log_cmd "env -i <isolated env> $HERDR_BIN server"
      : 
      { eval "exec ${BASE_ENV[*]} $(iso_env) \"\$HERDR_BIN\" server" >"$MUX_HOME/server.log" 2>&1; } &
      MUX_SERVER_PID=$!
      local sock="$MUX_HOME/cfg/herdr/herdr.sock" deadline=$((SECONDS + 30))
      while [[ ! -S $sock ]] && (( SECONDS < deadline )); do sleep 0.05; done
      [[ -S $sock ]] || { printf 'herdr server never bound its socket\n' >&2; return 1; }
      seed_herdr
      ;;
  esac
  SERVER_PIDS+=("$MUX_SERVER_PID")
}

stop_server() {
  [[ -n $MUX_SERVER_PID ]] || return 0
  kill "$MUX_SERVER_PID" 2>/dev/null || true
  wait_gone "$MUX_SERVER_PID"
  kill -9 "$MUX_SERVER_PID" 2>/dev/null || true
  MUX_SERVER_PID=""
}

# herdr starts with no workspace; phux pre-seeds its session at server start.
seed_herdr() {
  local panes
  panes=$(eval "${BASE_ENV[*]} $(iso_env) \"\$HERDR_BIN\" pane list" 2>/dev/null || true)
  if [[ $panes != *'"pane_id"'* ]]; then
    log_cmd "env -i <isolated env> $HERDR_BIN workspace create --cwd $MUX_HOME --label bench --focus"
    eval "${BASE_ENV[*]} $(iso_env) \"\$HERDR_BIN\" workspace create --cwd \"\$MUX_HOME\" \
      --label bench --focus" >>"$MUX_HOME/seed.log" 2>&1 || true
    sleep 0.5
  fi
}

# The relay is started once per quic lane run and torn down with the lane, so a
# lane that is not measuring latency pays nothing for the flag existing.
start_relay() {
  local ready="$RUN/relay-ready"
  rm -f "$ready"
  log_cmd "python3 scripts/bench/udp-delay.py --listen 127.0.0.1:$QUIC_RELAY_PORT --to 127.0.0.1:$QUIC_PORT --delay-ms $(awk -v r="$RTT_MS" 'BEGIN{printf "%g", r/2}') --mbit $PATH_MBIT --loss-percent $LOSS_PERCENT --seed $LOSS_SEED --metrics-file $OUT_DIR/relay-metrics.json"
  python3 "$UDP_DELAY" --listen "127.0.0.1:$QUIC_RELAY_PORT" --to "127.0.0.1:$QUIC_PORT" \
    --delay-ms "$(awk -v r="$RTT_MS" 'BEGIN{printf "%g", r/2}')" --ready-file "$ready" \
    --mbit "$PATH_MBIT" --loss-percent "$LOSS_PERCENT" --seed "$LOSS_SEED" \
    --metrics-file "$OUT_DIR/relay-metrics.json" \
    >>"$OUT_DIR/relay.log" 2>&1 &
  RELAY_PID=$!
  local deadline=$((SECONDS + 10))
  while [[ ! -f $ready ]] && (( SECONDS < deadline )); do sleep 0.05; done
  [[ -f $ready ]] || { printf 'udp relay never bound\n' >&2; return 1; }
}

stop_relay() {
  [[ -n $RELAY_PID ]] || return 0
  kill "$RELAY_PID" 2>/dev/null || true
  local status=0
  wait "$RELAY_PID" || status=$?
  RELAY_PID=""
  if (( status != 0 )); then
    printf 'invalid shaped-path run: relay exited %s; see %s/relay.log\n' "$status" "$OUT_DIR" >&2
    return "$status"
  fi
}

path_shaping_enabled() {
  awk -v r="$RTT_MS" -v b="$PATH_MBIT" -v l="$LOSS_PERCENT" \
    'BEGIN { exit !(r != 0 || b != 0 || l != 0) }'
}

# Detach keys are each multiplexer's documented binding: phux C-a d, herdr C-b q.
# The three phux lanes differ only in the transport the client dials.
setup_mux() {
  MUX="$1"
  prepare_dirs "$MUX"
  MUX_ATTACH="$MUX_HOME/attach.sh"
  case "$MUX" in
    phux) FAMILY=phux; MUX_DETACH=(C-a d)
      MUX_ARGV="$PHUX_BIN attach --socket $MUX_HOME/mux.sock bench" ;;
    phux-ws) FAMILY=phux; MUX_DETACH=(C-a d)
      MUX_ARGV="$PHUX_BIN attach --ws ws://127.0.0.1:$WS_PORT bench" ;;
    phux-quic) FAMILY=phux; MUX_DETACH=(C-a d)
      local port=$QUIC_PORT
      # Requested shaping puts the relay between client and server; the
      # server still binds its own port and never learns it is being shaped.
      if path_shaping_enabled; then
        start_relay || exit 2
        port=$QUIC_RELAY_PORT
      fi
      MUX_ARGV="$PHUX_BIN attach --quic 127.0.0.1:$port bench" ;;
    herdr) FAMILY=herdr; MUX_DETACH=(C-b q); MUX_ARGV="$HERDR_BIN" ;;
    tmux) FAMILY=tmux; MUX_DETACH=(); MUX_ARGV="/bin/sh" ;;
  esac
  write_attach_wrapper "$MUX_ARGV"
}

# --- measurement primitives --------------------------------------------------
SESSION=run

# Launch the attach wrapper in our private tmux and time the first prompt paint.
timed_attach() {
  local start
  "${TMUX[@]}" kill-session -t "$SESSION" 2>/dev/null || true
  start=$(now_ms)
  "${TMUX[@]}" new-session -d -s "$SESSION" -x "$COLS" -y "$ROWS" "$MUX_ATTACH"
  poll_until "$SESSION" "$PROMPT" "$start" 30000 0.005 || true
}

detach_session() {
  local key
  for key in "${MUX_DETACH[@]:-}"; do
    [[ -n $key ]] || continue
    "${TMUX[@]}" send-keys -t "$SESSION" "$key"
    sleep 0.05
  done
  ((${#MUX_DETACH[@]})) && { poll_until "$SESSION" ATTACH_EXIT "$(now_ms)" 10000 0.02 >/dev/null || true; }
  "${TMUX[@]}" kill-session -t "$SESSION" 2>/dev/null || true
}

# A fresh profile shows a first-run overlay that eats keystrokes, and herdr
# paints its prompt while that overlay is still up, so prove the pane really
# reaches the shell by round-tripping a probe command before anything is timed.
settle_pane() {
  local token needle attempt
  token="S$RANDOM$RANDOM"
  needle="READY_$token"
  for ((attempt = 0; attempt < 8; attempt++)); do
    "${TMUX[@]}" send-keys -t "$SESSION" Escape
    sleep 0.1
    "${TMUX[@]}" send-keys -t "$SESSION" C-u
    "${TMUX[@]}" send-keys -t "$SESSION" "echo R\"\"EADY_$token" Enter
    if poll_until "$SESSION" "$needle" "$(now_ms)" 2500 0.02 >/dev/null; then
      "${TMUX[@]}" send-keys -t "$SESSION" C-u
      sleep 0.2
      return 0
    fi
    "${TMUX[@]}" send-keys -t "$SESSION" Enter
    sleep 0.2
  done
  printf 'warning: %s pane never answered a probe command\n' "$MUX" >&2
  return 1
}

measure_attach() {
  local phase=$1 samples=() i t
  for ((i = 0; i < ATTACH_SAMPLES; i++)); do
    if [[ $phase == cold ]]; then
      stop_server
      start_server
    fi
    t=$(timed_attach)
    samples+=("$t")
    detach_session
  done
  RAW["$MUX:attach_$phase"]=$(IFS=,; printf '%s' "${samples[*]}")
  RESULT["$MUX:attach_$phase"]=$(pct 50 "${samples[@]}")
}

# Type one letter and poll at 1ms until it lands next to the prompt.
measure_keys() {
  local samples=() i letter start t
  for ((i = 0; i < KEY_SAMPLES; i++)); do
    letter=$(printf "\\$(printf '%03o' $((97 + i % 26)))")
    # The clock starts once send-keys has returned: tmux has already handed the
    # byte to the pane, so what is left is echo plus the capture poll floor.
    "${TMUX[@]}" send-keys -t "$SESSION" "$letter"
    start=$(now_ms)
    t=$(poll_until "$SESSION" "$PROMPT $letter" "$start" 5000 0.001) || true
    (( t >= 0 )) && samples+=("$t")
    "${TMUX[@]}" send-keys -t "$SESSION" C-u
    sleep 0.05
  done
  RAW["$MUX:key"]=$(IFS=,; printf '%s' "${samples[*]}")
  RESULT["$MUX:key_p50"]=$(pct 50 "${samples[@]}")
  RESULT["$MUX:key_p90"]=$(pct 90 "${samples[@]}")
  RESULT["$MUX:key_p99"]=$(pct 99 "${samples[@]}")
}

# A lane that lost its client mid-run would silently measure nothing, so prove
# a live client is attached before any metric that depends on one.
ensure_attached() {
  [[ $FAMILY == tmux ]] && return 0
  [[ -n $(client_pid "$SESSION") ]] && return 0
  printf 'warning: %s lost its client; re-attaching\n' "$MUX" >&2
  timed_attach >/dev/null
  settle_pane || true
}

measure_throughput() {
  local cpid token needle start elapsed sc0 cc0 sc1 cc1
  ensure_attached
  cpid=$(client_pid "$SESSION")
  token="T$RANDOM$RANDOM"
  needle="DONE_$token"
  sc0=$(cpu_ms "$MUX_SERVER_PID"); cc0=$(cpu_ms "$cpid")
  start=$(now_ms)
  # The literal quotes keep the echoed command line from matching the marker.
  "${TMUX[@]}" send-keys -t "$SESSION" "seq 1 $SEQ_LINES; echo D\"\"ONE_$token" Enter
  elapsed=$(poll_until "$SESSION" "$needle" "$start" 120000 0.005) || true
  sc1=$(cpu_ms "$MUX_SERVER_PID"); cc1=$(cpu_ms "$cpid")
  (( elapsed < 0 )) && elapsed="timeout>120s"
  RESULT["$MUX:throughput_ms"]=$elapsed
  RESULT["$MUX:cpu_server_ms"]=$((sc1 - sc0))
  RESULT["$MUX:cpu_client_ms"]=$((cc1 - cc0))
  RESULT["$MUX:rss_server_kb"]=$(rss_kb "$MUX_SERVER_PID")
  RESULT["$MUX:rss_client_kb"]=$(rss_kb "$cpid")
  "${TMUX[@]}" send-keys -t "$SESSION" C-u
  sleep 0.2
}

# ps %cpu on macOS is a decaying average, so also take an exact cputime delta.
measure_idle() {
  local cpid samples=() i s0 c0 s1 c1 t0 t1
  cpid=$(client_pid "$SESSION")
  sleep 3
  s0=$(cpu_ms "$MUX_SERVER_PID"); c0=$(cpu_ms "$cpid"); t0=$(now_ms)
  for ((i = 0; i < 10; i++)); do
    samples+=("$(awk -v a="$(pcpu "$MUX_SERVER_PID")" -v b="$(pcpu "$cpid")" \
      'BEGIN{printf "%d", (a+b)*100}')")
    sleep 0.5
  done
  t1=$(now_ms)
  s1=$(cpu_ms "$MUX_SERVER_PID"); c1=$(cpu_ms "$cpid")
  RAW["$MUX:idle_pcpu_x100"]=$(IFS=,; printf '%s' "${samples[*]}")
  RESULT["$MUX:idle_pcpu"]=$(awk -v v="$(mean "${samples[@]}")" 'BEGIN{printf "%.2f", v/100}')
  RESULT["$MUX:idle_cputime_pct"]=$(awk -v d="$(( (s1 - s0) + (c1 - c0) ))" -v w="$((t1 - t0))" \
    'BEGIN{if (w<=0) print "n/a"; else printf "%.2f", 100*d/w}')
}

# Repaint is "settled" once two captures 30ms apart are byte-identical.
wait_stable() {
  local start=$1 prev="" cur deadline=$((SECONDS + 5))
  while (( SECONDS < deadline )); do
    cur=$(capture "$SESSION")
    if [[ -n $prev && $cur == "$prev" ]]; then
      printf '%s' "$(( $(now_ms) - start ))"
      return 0
    fi
    prev=$cur
    sleep 0.03
  done
  printf '%s' -1
}

resize_to() {
  local start
  start=$(now_ms)
  "${TMUX[@]}" resize-window -t "$SESSION" -x "$1" -y "$2"
  wait_stable "$start"
}

measure_resize() {
  local down up
  ensure_attached
  down=$(resize_to 100 30)
  sleep 0.3
  up=$(resize_to "$COLS" "$ROWS")
  [[ $down == -1 ]] && down=unsettled
  [[ $up == -1 ]] && up=unsettled
  RESULT["$MUX:resize_ms"]="$down/$up"
}


# --- byte-level echo, handshake accounting, big-history scenario -------------

# Run the pty probe against this lane's attach command. No tmux, no screen
# scrape: the probe owns the pty, so its floor is the pty round trip.
run_pty_probe() {
  local label=$1 out=$2 iters=$3 cols=$4 rows=$5 extra=${6:-}
  log_cmd "env -i <isolated env> python3 scripts/bench/pty-echo.py --label $label --iters $iters --cols $cols --rows $rows $extra -- $MUX_ARGV"
  eval "${BASE_ENV[*]} $(iso_env) python3 \"\$PTY_PROBE\" --label \"\$label\" --iters $iters \
    --cols $cols --rows $rows --json \"\$out\" $extra -- $MUX_ARGV" >>"$MUX_HOME/pty.log" 2>&1 || true
}

# Pull one numeric field out of a probe result, or "n/a".
probe_field() {
  python3 - "$1" "$2" <<'PY' 2>/dev/null || printf 'n/a'
import json, sys
try:
    value = json.load(open(sys.argv[1])).get(sys.argv[2])
except Exception:
    value = None
print("n/a" if value is None else value)
PY
}

measure_pty_echo() {
  local out="$MUX_HOME/pty-echo.json"
  run_pty_probe "$MUX" "$out" "$PTY_ITERS" "$COLS" "$ROWS"
  cp -f "$out" "$OUT_DIR/$MUX-pty-echo.json" 2>/dev/null || true
  local key
  for key in p50 p90 p99 max; do
    RESULT["$MUX:pty_$key"]=$(probe_field "$out" "${key}_us")
  done
  RAW["$MUX:pty_us"]=$(python3 -c 'import json,sys;print(",".join(str(v) for v in json.load(open(sys.argv[1])).get("samples_us",[])))' "$out" 2>/dev/null || true)
}

# One attach against a debug-logging server, then read the ordered request
# frames out of the server log with times relative to the connection.
measure_handshake() {
  [[ $FAMILY == phux ]] || return 0
  stop_server
  MUX_DEBUG_LOG=1
  start_server
  timed_attach >/dev/null
  settle_pane || true
  detach_session
  MUX_DEBUG_LOG=0
  local out="$OUT_DIR/$MUX-handshake.txt"
  python3 - "$MUX_HOME/server.log" "$MUX" >"$out" <<'PY' 2>/dev/null || true
import re, sys
WANTED = ("client connected", "HELLO", "ATTACH with", "handle_attach",
          "SUBSCRIBE_EVENTS", "SUBSCRIBE_METADATA", "GET_METADATA")
ansi = re.compile(r"\x1b\[[0-9;]*m")
rows, base = [], None
for line in open(sys.argv[1], errors="replace"):
    line = ansi.sub("", line).rstrip()
    if not any(w in line for w in WANTED):
        continue
    if "handle_attach" in line and "close" not in line:
        continue
    stamp = line.split("Z", 1)[0]
    try:
        h, m, sec = stamp.split("T")[1].split(":")
        t = (int(h) * 3600 + int(m) * 60 + float(sec)) * 1000.0
    except Exception:
        continue
    if base is None:
        base = t
    label = next(w for w in WANTED if w in line)
    if label == "handle_attach":
        label = "ATTACHED (attach handler close)"
    rows.append((t - base, label, line.split(": ", 2)[-1][:110]))
    if len(rows) >= 24:
        break
print("lane=%s  frames before and around first paint" % sys.argv[2])
print("%-9s %-32s %s" % ("t+ms", "frame", "detail"))
for dt, label, detail in rows:
    print("%-9.3f %-32s %s" % (dt, label, detail))
pre = [r for r in rows if r[1] in ("client connected", "HELLO", "ATTACH with",
                                   "ATTACHED (attach handler close)")]
post = [r for r in rows if r not in pre]
print()
print("pre-paint request/reply pairs: %d (HELLO/HELLO_OK, ATTACH/ATTACHED)"
      % len([r for r in pre if r[1] in ("HELLO", "ATTACH with")]))
print("post-paint requests: %d" % len(post))
if rows:
    print("connect to ATTACHED: %.3f ms" % max(
        [r[0] for r in rows if r[1].startswith("ATTACHED")] or [0.0]))
PY
  RESULT["$MUX:handshake_pairs"]=$(grep -o 'pre-paint request/reply pairs: [0-9]*' "$out" 2>/dev/null | grep -o '[0-9]*$' || printf 'n/a')
  RESULT["$MUX:handshake_ms"]=$(grep -o 'connect to ATTACHED: [0-9.]*' "$out" 2>/dev/null | grep -o '[0-9.]*$' || printf 'n/a')
  stop_server
}

# --- big-history scenario ----------------------------------------------------
# Four panes, each holding more scrollback than the history limit keeps, at the
# width the user actually runs. This is the shape the empty-session numbers miss.

# --socket must precede the positionals: send-keys takes variadic keys and
# would otherwise swallow the flag and dial the default socket.
phux_ctl() {
  eval "${BASE_ENV[*]} $(iso_env) \"\$PHUX_BIN\" \"\$1\" --socket \"\$MUX_HOME/mux.sock\" \"\${@:2}\""
}

fill_phux_history() {
  local i name
  for ((i = 1; i <= BH_PANES; i++)); do
    name="bench"
    (( i > 1 )) && name="bench$i"
    (( i > 1 )) && phux_ctl new -s "$name" --json >>"$MUX_HOME/bh.log" 2>&1
    phux_ctl resize "$name" "${BH_COLS}x${ROWS}" >>"$MUX_HOME/bh.log" 2>&1 || true
    phux_ctl send-keys "$name" "seq 1 $BH_LINES; echo F\"\"ILLED_$name" Enter >>"$MUX_HOME/bh.log" 2>&1
  done
  for ((i = 1; i <= BH_PANES; i++)); do
    name="bench"
    (( i > 1 )) && name="bench$i"
    phux_ctl wait --until "FILLED_$name" --timeout 180 "$name" >>"$MUX_HOME/bh.log" 2>&1 || true
  done
}

herdr_ctl() { eval "${BASE_ENV[*]} $(iso_env) \"\$HERDR_BIN\" \"\$@\"" ; }

fill_herdr_history() {
  local i pane panes=()
  for ((i = 2; i <= BH_PANES; i++)); do
    herdr_ctl tab create --workspace w1 --label "bench$i" --focus >>"$MUX_HOME/bh.log" 2>&1 || true
  done
  mapfile -t panes < <(herdr_ctl pane list 2>/dev/null |
    python3 -c 'import json,sys;print("\n".join(p["pane_id"] for p in json.load(sys.stdin)["result"]["panes"]))' 2>/dev/null || true)
  for pane in "${panes[@]}"; do
    [[ -n $pane ]] || continue
    herdr_ctl pane send-text "$pane" "seq 1 $BH_LINES; echo F\"\"ILLED_${pane//:/_}" >>"$MUX_HOME/bh.log" 2>&1 || true
    herdr_ctl pane send-keys "$pane" enter >>"$MUX_HOME/bh.log" 2>&1 || true
  done
  for pane in "${panes[@]}"; do
    [[ -n $pane ]] || continue
    herdr_ctl pane wait-output --match "FILLED_${pane//:/_}" --timeout 180000 "$pane" >>"$MUX_HOME/bh.log" 2>&1 || true
  done
  RESULT["$MUX:bh_panes"]=${#panes[@]}
}

run_big_history() {
  setup_mux "$1"
  printf 'measuring %s big-history ...\n' "$1" >&2
  start_server
  # Three resident-memory readings tell the story a single number hides: a
  # server nobody has touched, the same server holding the history, and the
  # same server once a client has attached to it.
  RESULT["$MUX:bh_rss_fresh_kb"]=$(rss_kb "$MUX_SERVER_PID")
  timed_attach >/dev/null
  settle_pane || true
  detach_session
  case "$FAMILY" in
    phux) fill_phux_history; RESULT["$MUX:bh_panes"]=$BH_PANES ;;
    herdr) fill_herdr_history ;;
  esac
  sleep 1
  RESULT["$MUX:bh_rss_kb"]=$(rss_kb "$MUX_SERVER_PID")
  local samples=() i t
  for ((i = 0; i < ATTACH_SAMPLES; i++)); do
    t=$(timed_attach)
    samples+=("$t")
    (( i == ATTACH_SAMPLES - 1 )) && RESULT["$MUX:bh_rss_attached_kb"]=$(rss_kb "$MUX_SERVER_PID")
    detach_session
  done
  RESULT["$MUX:bh_rss_detached_kb"]=$(rss_kb "$MUX_SERVER_PID")
  RAW["$MUX:bh_attach"]=$(IFS=,; printf '%s' "${samples[*]}")
  RESULT["$MUX:bh_attach_med"]=$(pct 50 "${samples[@]}")
  RESULT["$MUX:bh_attach_max"]=$(pct 100 "${samples[@]}")
  local out="$MUX_HOME/bh-pty.json" second
  # A second client attaches mid-probe: phux dials another session, herdr opens
  # a second view of the same server, which is that product's equivalent.
  if [[ $FAMILY == phux ]]; then
    second="$PHUX_BIN attach --socket $MUX_HOME/mux.sock bench2"
  else
    second="$HERDR_BIN"
  fi
  run_pty_probe "$MUX-bh" "$out" "$PTY_ITERS" "$BH_COLS" "$ROWS" \
    "--settle-timeout 8 --attach-timeout 60 --interfere-at $((PTY_ITERS / 2)) --interfere '$second'"
  cp -f "$out" "$OUT_DIR/$MUX-bh-pty-echo.json" 2>/dev/null || true
  RESULT["$MUX:bh_pty_p50"]=$(probe_field "$out" p50_us)
  RESULT["$MUX:bh_pty_p99"]=$(probe_field "$out" p99_us)
  RESULT["$MUX:bh_pty_spike"]=$(probe_field "$out" interference_max_us)
  RAW["$MUX:bh_pty_us"]=$(python3 -c 'import json,sys;print(",".join(str(v) for v in json.load(open(sys.argv[1])).get("samples_us",[])))' "$out" 2>/dev/null || true)
  cp -f "$MUX_HOME/server.log" "$OUT_DIR/$1-bh-server.log" 2>/dev/null || true
  stop_server
}

run_mux() {
  setup_mux "$1"
  printf 'measuring %s ...\n' "$1" >&2
  if [[ $1 != tmux ]]; then
    start_server
    # Warm-up attach: absorbs first-run onboarding so it is not timed.
    timed_attach >/dev/null
    settle_pane || true
    detach_session
    measure_attach cold
    measure_attach warm
  fi
  measure_pty_echo
  timed_attach >/dev/null
  settle_pane || true
  measure_keys
  if [[ $1 != tmux ]]; then
    measure_throughput
    measure_idle
    measure_resize
  fi
  cp -f "$MUX_HOME/server.log" "$OUT_DIR/$1-server.log" 2>/dev/null || true
  detach_session
  measure_handshake
  stop_server
  stop_relay
}

cell() { local key=$1; printf '%s' "${RESULT[$key]:-n/a}"; }

write_json() {
  local first=1 key
  {
    printf '{\n  "generated": "%s",\n  "host": "%s",\n' \
      "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$(uname -mrs)"
    printf '  "cols": %s,\n  "rows": %s,\n  "seq_lines": %s,\n  "results": {\n' \
      "$COLS" "$ROWS" "$SEQ_LINES"
    for key in "${!RESULT[@]}"; do
      ((first)) || printf ',\n'; first=0
      printf '    "%s": "%s"' "$key" "${RESULT[$key]}"
    done
    printf '\n  },\n  "raw": {\n'; first=1
    for key in "${!RAW[@]}"; do
      ((first)) || printf ',\n'; first=0
      printf '    "%s": [%s]' "$key" "${RAW[$key]}"
    done
    printf '\n  }\n}\n'
  } >"$OUT_DIR/samples.json"
}

report() {
  local muxes=("$@")
  local heads; heads=$(IFS='|'; printf '%s' "${muxes[*]}"); heads=${heads//|/ | }
  printf '\n| Metric | %s | tmux (baseline) |\n' "$heads"
  printf '|---|%s---|\n' "$(printf -- '---|%.0s' "${muxes[@]}")"
  local row
  for row in \
    "Attach to first prompt, cold (ms, median):attach_cold" \
    "Attach to first prompt, warm (ms, median):attach_warm" \
    "PTY echo p50 (us):pty_p50" \
    "PTY echo p90 (us):pty_p90" \
    "PTY echo p99 (us):pty_p99" \
    "PTY echo max (us):pty_max" \
    "Screen-scrape echo p50 (ms, sanity):key_p50" \
    "Screen-scrape echo p99 (ms, sanity):key_p99" \
    "Handshake pairs before first paint:handshake_pairs" \
    "Connect to ATTACHED (ms):handshake_ms" \
    "seq 1 $SEQ_LINES wall (ms):throughput_ms" \
    "Server CPU over that run (ms):cpu_server_ms" \
    "Client CPU over that run (ms):cpu_client_ms" \
    "Server RSS after (KB):rss_server_kb" \
    "Client RSS after (KB):rss_client_kb" \
    "Idle CPU, ps %cpu mean (%):idle_pcpu" \
    "Idle CPU, cputime delta (%):idle_cputime_pct" \
    "Resize shrink/grow repaint (ms):resize_ms" \
    "Big-history: panes filled:bh_panes" \
    "Big-history: server RSS, fresh (KB):bh_rss_fresh_kb" \
    "Big-history: server RSS, history loaded (KB):bh_rss_kb" \
    "Big-history: server RSS, client attached (KB):bh_rss_attached_kb" \
    "Big-history: server RSS, after detach (KB):bh_rss_detached_kb" \
    "Big-history: warm re-attach median (ms):bh_attach_med" \
    "Big-history: warm re-attach max (ms):bh_attach_max" \
    "Big-history: PTY echo p50 (us):bh_pty_p50" \
    "Big-history: PTY echo p99 (us):bh_pty_p99" \
    "Big-history: echo max during 2nd attach (us):bh_pty_spike"; do
    local label=${row%:*} key=${row##*:} m line
    line="| $label |"
    for m in "${muxes[@]}"; do line+=" $(cell "$m:$key") |"; done
    line+=" $(cell "tmux:$key") |"
    printf '%s\n' "$line"
  done
  printf '\nRaw samples: %s\nCommands used: %s\n' "$OUT_DIR/samples.json" "$COMMAND_LOG"
}

main() {
  local bin
  command -v "$TMUX_BIN" >/dev/null || { printf 'tmux is required\n' >&2; exit 2; }
  : >"$COMMAND_LOG"
  log_cmd "# private tmux server: ${TMUX[*]}"
  log_cmd "# pane geometry: ${COLS}x${ROWS}"
  local muxes=() m
  case "$MUX_SELECT" in
    both) muxes=(phux herdr) ;;
    all) muxes=(phux phux-ws phux-quic herdr) ;;
    *) IFS=, read -r -a muxes <<<"$MUX_SELECT" ;;
  esac
  for m in "${muxes[@]}"; do
    case "$m" in
      phux|phux-ws|phux-quic) bin="$PHUX_BIN" ;;
      herdr) bin="$HERDR_BIN" ;;
      *) printf 'bad lane: %s\n' "$m" >&2; exit 2 ;;
    esac
    [[ -x $bin ]] || { printf 'missing %s\n' "$bin" >&2; exit 2; }
  done
  [[ -f $PTY_PROBE ]] || { printf 'missing %s\n' "$PTY_PROBE" >&2; exit 2; }
  for m in "${muxes[@]}"; do run_mux "$m"; done
  run_mux tmux
  if (( BIG_HISTORY )); then
    for m in "${muxes[@]}"; do
      [[ $m == phux || $m == herdr ]] && run_big_history "$m"
    done
  fi
  write_json
  report "${muxes[@]}"
}

main "$@"
