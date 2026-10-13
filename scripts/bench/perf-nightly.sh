#!/usr/bin/env bash
# Advisory wall-time samples. A crash fails the run. No sample is a budget.
#
# Usage: scripts/bench/perf-nightly.sh PHUX_BIN OUT_DIR
# OUT_DIR is created. The mux-compare subdirectory must not already exist;
# this script refuses to overwrite evidence it did not just create.
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: scripts/bench/perf-nightly.sh PHUX_BIN OUT_DIR" >&2
  exit 2
fi

PHUX_BIN=$1
OUT=$2
case $PHUX_BIN in
  /*) ;;
  *) PHUX_BIN="$(pwd)/$PHUX_BIN" ;;
esac
if [[ ! -x $PHUX_BIN ]]; then
  echo "perf-nightly: phux binary is not executable: $PHUX_BIN" >&2
  exit 1
fi
if [[ -e $OUT ]]; then
  echo "perf-nightly: output directory already exists: $OUT" >&2
  exit 2
fi
command -v tmux >/dev/null 2>&1 || { echo "perf-nightly: tmux is required" >&2; exit 1; }

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
mkdir -p "$OUT"

bash scripts/bench/mux-compare.sh \
  --mux phux \
  --phux-bin "$PHUX_BIN" \
  --out "$OUT/mux" \
  --attach-samples 2 \
  --key-samples 5 \
  --seq-lines 20000 \
  --pty-iters 5

bash scripts/bench/tui-load.sh "$PHUX_BIN" nightly-none none 80 24 5 \
  > "$OUT/tui-load.txt"

scale_one() {
  local count="$1"
  local home sock start end elapsed i
  home="$(mktemp -d /tmp/phux-perf-scale-XXXX)"
  mkdir -p "$home/state" "$home/config/phux" "$home/run"
  : > "$home/config/phux/config.toml"
  sock="$home/run/mux.sock"
  env -i \
    PATH="/usr/bin:/bin:/usr/sbin:/sbin" \
    HOME="$home" \
    XDG_STATE_HOME="$home/state" \
    XDG_CONFIG_HOME="$home/config" \
    XDG_RUNTIME_DIR="$home/run" \
    TERM=xterm-256color \
    LANG=C \
    PHUX_PROFILE=perf-nightly \
    "$PHUX_BIN" server --socket "$sock" --session seed --exit-after-idle 120 \
    >"$home/server.out" 2>&1 &
  local srv=$!
  cleanup() {
    env -i \
      PATH="/usr/bin:/bin:/usr/sbin:/sbin" \
      HOME="$home" \
      XDG_STATE_HOME="$home/state" \
      XDG_CONFIG_HOME="$home/config" \
      XDG_RUNTIME_DIR="$home/run" \
      PHUX_PROFILE=perf-nightly \
      "$PHUX_BIN" kill --socket "$sock" --server >/dev/null 2>&1 || kill "$srv" 2>/dev/null || true
    wait "$srv" 2>/dev/null || true
  }
  trap cleanup RETURN
  local ready=0
  for _ in $(seq 1 100); do
    if [[ -S $sock ]]; then
      ready=1
      break
    fi
    sleep 0.05
  done
  if [[ $ready -ne 1 ]]; then
    echo "perf-nightly: server never bound $sock" >&2
    cat "$home/server.out" >&2 || true
    return 1
  fi
  start="${EPOCHREALTIME}"
  for i in $(seq 1 "$count"); do
    env -i \
      PATH="/usr/bin:/bin:/usr/sbin:/sbin" \
      HOME="$home" \
      XDG_STATE_HOME="$home/state" \
      XDG_CONFIG_HOME="$home/config" \
      XDG_RUNTIME_DIR="$home/run" \
      TERM=xterm-256color \
      LANG=C \
      PHUX_PROFILE=perf-nightly \
      "$PHUX_BIN" new --json -s "scale${i}" --socket "$sock" \
      >"$home/new-${i}.json"
  done
  end="${EPOCHREALTIME}"
  elapsed="$(awk -v s="$start" -v e="$end" 'BEGIN { printf "%.6f", e - s }')"
  printf '{"sessions":%s,"seconds":%s}\n' "$count" "$elapsed"
}

{
  echo '{"runs":['
  scale_one 1
  printf ','
  scale_one 10
  printf ','
  scale_one 50
  echo ']}'
} > "$OUT/scaling.json"

echo "perf-nightly: wrote $OUT"
