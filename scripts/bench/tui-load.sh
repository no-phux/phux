#!/usr/bin/env bash
# Keystroke echo latency under a TUI-style redraw load.
#
# Starts an isolated phux server (its own HOME, XDG dirs, socket, and a
# scrubbed environment; never the user's), optionally spawns a sibling pane
# running scripts/bench/flood.py beside the probe pane, then runs
# scripts/bench/pty-echo.py against `phux attach` at the requested size. The
# probe measures one typed byte from write(2) on the master until it comes
# back, so its floor is the pty and the process under test, not a screen
# scrape. Telemetry snapshots bracket the measured phase after shell
# readiness, before detach. PHUX_BENCH_SAMPLE=1 additionally records macOS
# CPU profiles; keep it off for timing comparisons (sampling adds overhead).
#
# Usage: tui-load.sh PHUX_BIN LABEL FLOOD [COLS] [ROWS] [ITERS]
#   FLOOD  none     quiet baseline
#          spinner  one line redrawn at 10 Hz in the sibling pane
#          full     the whole sibling pane repainted at 30 fps
# Prints the pty-echo JSON and artifacts directory holding echo.json,
# perf-{0,1}.json, perf-interval.txt and the client log.
set -euo pipefail
PHUX_BIN=$1; LABEL=$2; FLOOD=$3; COLS=${4:-188}; ROWS=${5:-48}; ITERS=${6:-60}
case $FLOOD in none|spinner|full) ;; *) echo "FLOOD must be none, spinner or full" >&2; exit 2 ;; esac
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
trap '
  "${ISO[@]}" "$PHUX_BIN" kill --socket "$SOCK" --server >/dev/null 2>&1 || kill "$SRV" 2>/dev/null || true
' EXIT
for _ in $(seq 1 100); do [[ -S $SOCK ]] && break; sleep 0.05; done
[[ -S $SOCK ]] || { echo "server never bound its socket" >&2; cat "$H/server.out" >&2; exit 1; }

if [[ $FLOOD != none ]]; then
  TARGET=$("${ISO[@]}" "$PHUX_BIN" ls --socket "$SOCK" --json | python3 -c 'import sys,json; print(json.load(sys.stdin)["terminals"][0])')
  MODE=spinner; FPS=10
  [[ $FLOOD == full ]] && { MODE=full; FPS=30; }
  "${ISO[@]}" "$PHUX_BIN" spawn --socket "$SOCK" --target "$TARGET" --split vertical --ratio 0.5 -- python3 "$HERE/flood.py" "$FPS" "$MODE" >"$H/spawn.out" 2>&1
  sleep 0.5
fi

JOBS=()
if [[ ${PHUX_BENCH_SAMPLE:-0} == 1 ]] && command -v sample >/dev/null; then
  sample "$SRV" 4 1 -mayDie -file "$H/server-sample.txt" >/dev/null 2>&1 & JOBS+=("$!")
  (
    for _ in $(seq 1 100); do
      CP=$(pgrep -f "^${PHUX_BIN} attach --socket ${SOCK}" | head -1) || true
      if [[ -n $CP ]]; then
        sample "$CP" 4 1 -mayDie -file "$H/client-sample.txt" >/dev/null 2>&1
        break
      fi
      sleep 0.05
    done
  ) & JOBS+=("$!")
fi
# Both snapshots are collected by the probe at the measurement boundary,
# not on sleeps which may miss a short run or include detach.
PERF_CMD=$(python3 -c 'import shlex,sys; print(shlex.join(sys.argv[1:]))' "$PHUX_BIN" perf --socket "$SOCK" --json)
PROBE_STATUS=0
"${ISO[@]}" PHUX_LOG="$H/client.log" python3 "$HERE/pty-echo.py" --label "$LABEL" --iters "$ITERS" --cols "$COLS" --rows "$ROWS" --telemetry-command "$PERF_CMD" --json "$H/echo.json" -- "$PHUX_BIN" attach --socket "$SOCK" bench >"$H/probe.out" 2>&1 || PROBE_STATUS=$?
[[ -s $H/echo.json ]] || { echo "probe failed; artifacts: $H" >&2; cat "$H/probe.out" >&2; exit 1; }
for job in "${JOBS[@]}"; do wait "$job" || true; done
python3 - "$H" <<'PY' || PROBE_STATUS=1
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
result = json.loads((root / "echo.json").read_text())
failed = False
for key, filename in (("telemetry_before", "perf-0.json"), ("telemetry_after", "perf-1.json")):
    report = result.get(key)
    if report is not None:
        (root / filename).write_text(json.dumps(report) + "\n")
    if not report or "metrics" not in report:
        print(f"{key}: {report or 'not collected (probe did not become ready)'}", file=sys.stderr)
        failed = True
sys.exit(1 if failed else 0)
PY
cat "$H/echo.json" 2>/dev/null || { echo "probe failed:" >&2; cat "$H/probe.out" >&2; exit 1; }
if [[ -s $H/perf-0.json && -s $H/perf-1.json ]]; then
  python3 - "$H/perf-0.json" "$H/perf-1.json" > "$H/perf-interval.txt" <<'PY' || true
import json, sys
a, b = (json.load(open(f)) for f in sys.argv[1:3])
if "metrics" not in a or "metrics" not in b:
    print("telemetry unavailable; collection failure retained in echo.json")
    sys.exit(0)
by = {m["name"]: m for m in a["metrics"]}
span = (b["captured_unix_ms"] - a["captured_unix_ms"]) / 1000.0
if span <= 0:
    print("telemetry interval is below wall-clock resolution")
    sys.exit(0)
print(f"server telemetry over {span:.3f}s of the probe (interval = snapshot 1 - snapshot 0)")
print(f"{'metric':24} {'count':>9} {'rate/s':>8} {'total':>12} {'p50':>9} {'p99':>9} {'max~':>9}")
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
        h = m["value"]; ph = prev["value"] if prev else {"count": 0, "sum": 0, "buckets": []}
        pb = {x["idx"]: x["count"] for x in ph["buckets"]}
        buckets = [{"idx": x["idx"], "count": x["count"] - pb.get(x["idx"], 0)} for x in h["buckets"] if x["count"] - pb.get(x["idx"], 0) > 0]
        n = h["count"] - ph["count"]
        if n <= 0: continue
        mx = upper(buckets[-1]["idx"]) if buckets else 0
        total = h["sum"] - ph["sum"]
        print(f"{m['name']:24} {n:>9} {n/span:>8.1f} {total:>12} {pct(buckets,n,50):>9} {pct(buckets,n,99):>9} {mx:>9}")
pa, pb_ = a.get("process") or {}, b.get("process") or {}
if pa and pb_:
    cpu = (pb_["cpu_user_us"] + pb_["cpu_system_us"] - pa["cpu_user_us"] - pa["cpu_system_us"]) / 1e6
    print(f"server cpu {cpu:.3f}s = {100*cpu/span:.1f}%  vol ctx switches {pb_['voluntary_ctx_switches']-pa['voluntary_ctx_switches']}  peak rss {pb_['max_rss_bytes']/1048576:.1f} MiB")
PY
  echo; cat "$H/perf-interval.txt"
fi
echo
echo "artifacts: $H"
exit "$PROBE_STATUS"
