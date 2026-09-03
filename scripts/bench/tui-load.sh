#!/usr/bin/env bash
# Keystroke echo latency under a TUI-style redraw load.
#
# Starts an isolated phux server (its own HOME, XDG dirs, socket, and a
# scrubbed environment; never the user's), optionally spawns a sibling pane
# running scripts/bench/flood.py beside the probe pane, then runs
# scripts/bench/pty-echo.py against `phux attach` at the requested size. The
# probe measures one typed byte from write(2) on the master until it comes
# back, so its floor is the pty and the process under test, not a screen
# scrape. While it runs, the server and the attach client are `sample`d for
# four seconds (readable when PHUX_BIN keeps symbols: the `profiling` cargo
# profile) and their CPU time is recorded.
#
# Usage: tui-load.sh PHUX_BIN LABEL FLOOD [COLS] [ROWS] [ITERS]
#   FLOOD  none     quiet baseline
#          spinner  one line redrawn at 10 Hz in the sibling pane
#          full     the whole sibling pane repainted at 30 fps
# Prints the pty-echo JSON (p50/p90/p99/max in microseconds) and the
# artifacts directory holding echo.json, server-sample.txt, client-sample.txt.
set -euo pipefail
PHUX_BIN=$1; LABEL=$2; FLOOD=$3; COLS=${4:-188}; ROWS=${5:-48}; ITERS=${6:-60}
HERE=$(cd "$(dirname "$0")" && pwd)
case $PHUX_BIN in /*) ;; *) PHUX_BIN=$(pwd)/$PHUX_BIN ;; esac
H=$(mktemp -d /tmp/phux-tuiload-XXXX)
mkdir -p "$H/state" "$H/config/phux" "$H/run"
printf "PS1='BENCH> '\n" > "$H/shrc"
: > "$H/config/phux/config.toml"
ISO=(env -i PATH=/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin TERM=xterm-256color LANG=en_US.UTF-8 HOME="$H" XDG_STATE_HOME="$H/state" XDG_CONFIG_HOME="$H/config" XDG_RUNTIME_DIR="$H/run" SHELL=/bin/sh ENV="$H/shrc" PS1='BENCH> ' PHUX_PROFILE=tuiload PHUX_RENDER_PROF=1)
SOCK="$H/run/mux.sock"
"${ISO[@]}" "$PHUX_BIN" server --socket "$SOCK" --session bench --exit-after-idle 300 >"$H/server.out" 2>&1 &
SRV=$!
cleanup() {
  "${ISO[@]}" "$PHUX_BIN" kill --socket "$SOCK" --server >/dev/null 2>&1 || kill "$SRV" 2>/dev/null || true
}
trap cleanup EXIT
for _ in $(seq 1 100); do [[ -S $SOCK ]] && break; sleep 0.05; done
[[ -S $SOCK ]] || { echo "server never bound its socket" >&2; cat "$H/server.out" >&2; exit 1; }

if [[ $FLOOD != none ]]; then
  TARGET=$("${ISO[@]}" "$PHUX_BIN" ls --socket "$SOCK" --json | python3 -c 'import sys,json; print(json.load(sys.stdin)["terminals"][0])')
  MODE=spinner; FPS=10
  [[ $FLOOD == full ]] && { MODE=full; FPS=30; }
  "${ISO[@]}" "$PHUX_BIN" spawn --socket "$SOCK" --target "$TARGET" --split vertical --ratio 0.5 -- python3 "$HERE/flood.py" "$FPS" "$MODE" >"$H/spawn.out" 2>&1
  sleep 0.5
fi

cpu_seconds() { ps -o utime=,stime= -p "$1" 2>/dev/null | awk '{split($1,u,/[:.]/); split($2,s,/[:.]/); printf "%.3f\n", (u[1]*60+u[2]+u[3]/100)+(s[1]*60+s[2]+s[3]/100)}'; }
client_pid() { pgrep -f "^${PHUX_BIN} attach --socket ${SOCK}" | head -1; }
C0=$(cpu_seconds "$SRV"); T0=$(date +%s.%N)
( sleep 6; sample "$SRV" 4 1 -mayDie -file "$H/server-sample.txt" >/dev/null 2>&1 ) & J1=$!
( sleep 6; CP=$(client_pid); [[ -n $CP ]] && sample "$CP" 4 1 -mayDie -file "$H/client-sample.txt" >/dev/null 2>&1 ) & J2=$!
( sleep 14; CP=$(client_pid); [[ -n $CP ]] && ps -o utime=,stime=,rss= -p "$CP" > "$H/client-cpu.txt" ) & J3=$!
# Server telemetry (ADR-0095): two GET_PERF snapshots bracketing the steady
# state of the run; the interval between them is what `perf-interval.txt`
# renders, so the numbers exclude attach and settle.
( sleep 5; "${ISO[@]}" "$PHUX_BIN" perf --socket "$SOCK" --json > "$H/perf-0.json" 2>/dev/null; sleep 10; "${ISO[@]}" "$PHUX_BIN" perf --socket "$SOCK" --json > "$H/perf-1.json" 2>/dev/null ) & J4=$!
"${ISO[@]}" python3 "$HERE/pty-echo.py" --label "$LABEL" --iters "$ITERS" --cols "$COLS" --rows "$ROWS" --json "$H/echo.json" -- "$PHUX_BIN" attach --socket "$SOCK" bench >"$H/probe.out" 2>&1 || true
wait $J1 $J2 $J3 $J4 2>/dev/null || true
T1=$(date +%s.%N); C1=$(cpu_seconds "$SRV")
echo "server cpu ${C1}-${C0}s over $(echo "$T1 - $T0" | bc)s wall; client utime/stime/rss: $(cat "$H/client-cpu.txt" 2>/dev/null || echo n/a)"
cat "$H/echo.json" 2>/dev/null || { echo "probe failed:" >&2; tail -20 "$H/probe.out" >&2; exit 1; }
if [[ -s $H/perf-0.json && -s $H/perf-1.json ]]; then
  python3 - "$H/perf-0.json" "$H/perf-1.json" > "$H/perf-interval.txt" <<'PY' || true
import json, sys
a, b = (json.load(open(f)) for f in sys.argv[1:3])
by = {m["name"]: m for m in a["metrics"]}
span = (b["captured_unix_ms"] - a["captured_unix_ms"]) / 1000.0
print(f"server telemetry over {span:.1f}s of steady state (interval = snapshot 1 - snapshot 0)")
print(f"{'metric':24} {'count':>9} {'rate/s':>8} {'p50':>9} {'p99':>9} {'max':>9}")
def upper(idx):
    if idx < 32: return idx
    e = 5 + (idx - 32) // 8; sub = (idx - 32) % 8
    return ((1 << e) | (sub << (e - 3))) + (1 << (e - 3)) - 1
def pct(buckets, count, p):
    rank = max(1, -(-p * count // 100)); seen = 0
    for bkt in buckets:
        seen += bkt["count"]
        if seen >= rank: return upper(bkt["idx"])
    return 0
for m in b["metrics"]:
    prev = by.get(m["name"]); kind = m["kind"]
    if kind == "counter":
        n = m["value"] - (prev["value"] if prev else 0)
        print(f"{m['name']:24} {n:>9} {n/span:>8.1f}")
    elif kind == "gauge":
        print(f"{m['name']:24} {m['value']:>9}    gauge")
    else:
        h = m["value"]; ph = prev["value"] if prev else {"count": 0, "buckets": []}
        pb = {x["idx"]: x["count"] for x in ph["buckets"]}
        buckets = [{"idx": x["idx"], "count": x["count"] - pb.get(x["idx"], 0)} for x in h["buckets"] if x["count"] - pb.get(x["idx"], 0) > 0]
        n = h["count"] - ph["count"]
        if n <= 0: continue
        mx = upper(buckets[-1]["idx"]) if buckets else 0
        print(f"{m['name']:24} {n:>9} {n/span:>8.1f} {pct(buckets,n,50):>9} {pct(buckets,n,99):>9} {mx:>9}")
pa, pb_ = a.get("process") or {}, b.get("process") or {}
if pa and pb_:
    cpu = (pb_["cpu_user_us"] + pb_["cpu_system_us"] - pa["cpu_user_us"] - pa["cpu_system_us"]) / 1e6
    print(f"server cpu {cpu:.3f}s = {100*cpu/span:.1f}%  vol ctx switches {pb_['voluntary_ctx_switches']-pa['voluntary_ctx_switches']}  peak rss {pb_['max_rss_bytes']/1048576:.1f} MiB")
PY
  echo; cat "$H/perf-interval.txt"
fi
echo
echo "artifacts: $H"
