#!/usr/bin/env bash
# Live dogfood gate for placement/layout/watch/ask. Uses a private real phux
# server and ordinary shells, so no paid agent CLI or user socket is touched.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
PHUX="${PHUX:-$ROOT/target/debug/phux}"
if [[ ! -x "$PHUX" ]]; then
    echo "build phux first or set PHUX=/path/to/phux" >&2
    exit 2
fi

tmp="$(mktemp -d "${TMPDIR:-/tmp}/phux-fleet-live.XXXXXX")"
socket="$tmp/phux.sock"
server_pid=""
watch_pid=""
cleanup() {
    [[ -n "$watch_pid" ]] && kill "$watch_pid" 2>/dev/null || true
    [[ -n "$server_pid" ]] && kill "$server_pid" 2>/dev/null || true
    [[ -n "$server_pid" ]] && wait "$server_pid" 2>/dev/null || true
    rm -rf "$tmp"
}
trap cleanup EXIT INT TERM

session="fleet-live-$$"
"$PHUX" server --session "$session" --socket "$socket" >"$tmp/server.log" 2>&1 &
server_pid=$!
for _ in $(seq 1 800); do
    [[ -S "$socket" ]] && break
    sleep 0.025
done
if [[ ! -S "$socket" ]]; then
    cat "$tmp/server.log" >&2 || true
    echo "live dogfood server did not bind" >&2
    exit 1
fi

json_field() {
    local field="$1"
    python3 -c 'import json,sys; print(json.load(sys.stdin)[sys.argv[1]])' "$field"
}

seed_json="$($PHUX snapshot --json --socket "$socket" "$session")"
seed="@$(printf '%s' "$seed_json" | json_field pane)"

# Real explicit placement: vertical divider means side-by-side. The placement
# path seeds layout metadata for the session when it is absent.
first_json="$($PHUX spawn --json --socket "$socket" --target "$seed" --split vertical --ratio 0.55 -c "$tmp")"
first="@$(printf '%s' "$first_json" | json_field terminal_id)"

# Real unplaced spawn followed by existing-pane insertion.
second_json="$($PHUX spawn --json --socket "$socket" -c "$tmp")"
second="@$(printf '%s' "$second_json" | json_field terminal_id)"
insert_json="$($PHUX insert-pane --socket "$socket" "$first" "$second" --split horizontal --ratio 0.5 --json)"
[[ "$(printf '%s' "$insert_json" | json_field direction)" == "horizontal" ]]

# Exercise both remaining existing-pane topology edits against the live server.
move_json="$($PHUX move-pane --socket "$socket" "$seed" "$second" --split vertical --ratio 0.5 --json)"
[[ "$(printf '%s' "$move_json" | json_field direction)" == "vertical" ]]
"$PHUX" swap-pane --socket "$socket" "$first" "$second" --json >/dev/null

# The watch is a real subscription. Raise a real Asked event and prove it lands
# in JSONL, then bound and reap the stream ourselves.
"$PHUX" watch --json --socket "$socket" "$first" >"$tmp/events.jsonl" &
watch_pid=$!
sleep 0.15
"$PHUX" ask --json --socket "$socket" "$first" --id live-review \
    --suggest yes --suggest no "Approve live dogfood?" >"$tmp/ask.json"

deadline=$((SECONDS + 10))
while ! grep -q '"event":"asked"' "$tmp/events.jsonl"; do
    if (( SECONDS >= deadline )); then
        echo "asked event did not reach live watch" >&2
        cat "$tmp/events.jsonl" >&2 || true
        exit 1
    fi
    sleep 0.05
done
kill "$watch_pid" 2>/dev/null || true
wait "$watch_pid" 2>/dev/null || true
watch_pid=""

grep -q '"question":"Approve live dogfood?"' "$tmp/events.jsonl"
[[ "$(json_field event <"$tmp/ask.json")" == "asked" ]]
[[ "$(json_field terminal <"$tmp/ask.json")" == "$first" ]]
# Optional, explicit paid-agent dogfood. Disabled by default. This uses direct
# spawn so availability is exactly `command -v`; the private server is killed at
# the end of the gate. Opt in only when API/account side effects are acceptable.
if [[ "${PHUX_DOGFOOD_REAL_AGENTS:-0}" == "1" ]]; then
    owner="$second"
    for agent in claude codex; do
        if command -v "$agent" >/dev/null 2>&1; then
            echo "live dogfood: opt-in spawning $agent" >&2
            launched="$($PHUX spawn --json --socket "$socket" --target "$owner" \
                --split vertical --ratio 0.5 -c "$tmp" -- "$agent")"
            owner="@$(printf '%s' "$launched" | json_field terminal_id)"
        else
            echo "live dogfood: $agent not found; skipping" >&2
        fi
    done
fi

printf 'placed-fleet live: placement, insert/move/swap, watch, and ask verified\n'
