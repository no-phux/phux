#!/usr/bin/env bash
# dev-live.sh — the full local live demo against your NATIVE phux binary.
#
# Runs real `phux server` with its native WebSocket transport (PHUX_WS_ADDR) and
# a dev seed shell, then `astro dev` with the demo island pointed at it. No
# Docker, no bridge — the browser's phux-web wasm client speaks the phux wire
# straight to your local phux. Ctrl-C tears down both.
#
# This runs the REAL native phux server. The deployed demo instead runs phux-edge
# (a curated shell) as WASM in a Durable Object — see worker/ + DEPLOY.md.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo"

PHUX_BIN="${PHUX_BIN:-$repo/../phux/target/release/phux}"
[ -x "$PHUX_BIN" ] || PHUX_BIN="$repo/../phux/target/debug/phux"
if [ ! -x "$PHUX_BIN" ]; then
  echo "phux binary not found. Build it first:" >&2
  echo "  (cd $repo/../phux && cargo build --release --bin phux)" >&2
  exit 1
fi

# Bind + dial 127.0.0.1 explicitly. `localhost` resolves to ::1 first on macOS,
# so a dual-stack listener on the same port (or a stale one) can shadow phux.
port="${PHUX_WS_PORT:-8080}"
ws="ws://127.0.0.1:${port}/"

# Real phux server: native WebSocket + a seed pane running the dev demo shell.
# A fresh runtime dir + socket per run so repeat invocations don't collide.
runtime_dir="$(mktemp -d)"
SHELL="$repo/sandbox/dev/seed.sh" \
PHUX_WS_ADDR="127.0.0.1:${port}" \
XDG_RUNTIME_DIR="$runtime_dir" \
TERM="xterm-256color" \
  "$PHUX_BIN" server --session default &
phux_pid=$!
trap 'kill "$phux_pid" 2>/dev/null || true; rm -rf "$runtime_dir"' EXIT INT TERM

# Wait for the WebSocket port to accept connections before starting the site.
for _ in $(seq 1 40); do
  nc -z 127.0.0.1 "$port" >/dev/null 2>&1 && break
  sleep 0.25
done

echo "phux live demo (native) → $ws"
echo "  phux : $PHUX_BIN"
exec env PUBLIC_PHUX_DEMO_WS="$ws" bun run dev
