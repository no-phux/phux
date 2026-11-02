#!/usr/bin/env bash
# Drives the built plugin through a throwaway phux server on its own socket:
# windows become panes, buffers are shared, and closing works both ways.
# Usage: tests/e2e.sh [path to phux]. Needs nvim and a release build.
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
phux_bin=${1:-$(command -v phux)}
bin=$here/target/release/phux-nvim
dir=$(mktemp -d "${TMPDIR:-/tmp}/pn.XXXXXX")
sock=$dir/s
fails=0

mkdir "$dir/shim"
ln -s "$phux_bin" "$dir/shim/phux"
export PATH=$dir/shim:$PATH
export XDG_RUNTIME_DIR=$dir PHUX_SOCKET=$sock
unset PHUX_PROFILE PHUX_WS_TOKENS PHUX_WS_TLS_CERT PHUX_WS_TLS_KEY PHUX_TERMINAL_ID

phux server --socket "$sock" --session e2e >"$dir/server.log" 2>&1 &
server=$!
cleanup() {
  if [ "$fails" != 0 ] || [ -z "${finished:-}" ]; then
    tail -n 60 "$dir"/phux-nvim-*.log >&2 || true
    phux snapshot "${first:-@1}" >&2 || true
  fi
  kill "$server" 2>/dev/null || true
  wait "$server" 2>/dev/null || true
  rm -rf "$dir"
}
trap cleanup EXIT

for _ in $(seq 100); do [ -S "$sock" ] && break; sleep 0.05; done

check() { # name, command...
  local name=$1; shift
  if "$@"; then echo "ok   $name"; else echo "FAIL $name"; fails=$((fails + 1)); fi
}
live() { phux ls --json | python3 -c '
import json, sys
for r in json.load(sys.stdin)["resources"]:
    if r["lifecycle"] == "running": print(r["id"])'; }
panes() { live | wc -l | tr -d ' '; }
await() { # text, pane
  for _ in $(seq 100); do
    phux snapshot "$2" 2>/dev/null | grep -q -- "$1" && return 0
    sleep 0.05
  done
  phux snapshot "$2" >&2 || true
  return 1
}
await_panes() { # count
  for _ in $(seq 100); do [ "$(panes)" = "$1" ] && return 0; sleep 0.05; done
  echo "panes: $(panes), wanted $1" >&2
  return 1
}
# shellcheck disable=SC2329 # called through check
await_status() { # pane, row (1-based) the statusline should sit on
  for _ in $(seq 100); do
    phux snapshot "$1" 2>/dev/null | sed -n "$(($2 + 1))p" | grep -q 'b.txt' && return 0
    sleep 0.05
  done
  phux snapshot "$1" >&2 || true
  return 1
}
keys() { local t=$1; shift; phux send-keys "$t" "$@"; }

printf 'first line\n' >"$dir/a.txt"
first=$(live | head -1)
keys "$first" "cd '$dir' && '$bin' --clean a.txt" Enter
check "the file opens in the launcher's pane" await 'first line' "$first"

keys "$first" $':vsplit\r'
check ":vsplit opens a real pane" await_panes 2
new=$(live | grep -vx "$first")
check "the new pane shows the same buffer" await 'first line' "$new"

keys "$first" gg O 'shared edit' Escape
check "an edit shows in both panes" await 'shared edit' "$new"
check "and in the first" await 'shared edit' "$first"

keys "$new" $':split\r'
check ":split from the other pane opens a third" await_panes 3
third=$(live | grep -vx "$first" | grep -vx "$new")
check "the third shows the buffer" await 'shared edit' "$third"
keys "$third" $':close\r'
check ":close closes its pane" await_panes 2

phux kill --yes "$new" >/dev/null
check "closing a pane leaves the other" await_panes 1
check "the editor keeps running" await 'shared edit' "$first"

printf 'other file\n' >"$dir/b.txt"
keys "$first" $':vsplit b.txt\r'
check ":vsplit FILE opens a pane" await_panes 2
new=$(live | grep -vx "$first")
check "showing that file" await 'other file' "$new"
keys "$first" $'Oafter split\033'
check "keys from the old pane follow Neovim to the new window" await 'after split' "$new"
keys "$first" $'\027w'
keys "$first" $'Owindow motion\033'
check "<C-w>w moves the keys with it" await 'window motion' "$first"
keys "$new" $'Ofrom new pane\033'
check "typing in another pane selects its window" await 'from new pane' "$new"

phux resize "$new" 50x15 >/dev/null
check "a resized pane refits its window" await_status "$new" 14
phux kill --yes "$first" >/dev/null 2>&1 || true
check "closing the launcher's pane keeps the editor" await_panes 1
keys "$new" $'Ostill alive\033'
check "and it still edits" await 'still alive' "$new"
first=$new

keys "$first" $':qa!\r'
check "quitting closes the last pane's editor" await_panes 0
for _ in $(seq 60); do pgrep -f "phux-nvim daemon $dir" >/dev/null || break; sleep 0.05; done
check "the daemon exits with Neovim" bash -c "! pgrep -f 'phux-nvim daemon $dir' >/dev/null"

finished=1
[ "$fails" = 0 ] && echo "all passed"
exit "$fails"
