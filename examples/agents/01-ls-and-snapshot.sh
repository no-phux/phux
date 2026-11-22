#!/usr/bin/env bash
#
# 01 — discover and read: `ls` and `snapshot`.
#
# The two side-effect-free reads at the bottom of the agent surface. An
# agent starts a turn by asking "what sessions exist?" (`ls`) and "what's
# on screen?" (`snapshot`). Neither attaches, neither resizes, both are
# safe to poll against a pane a human is using.
#
# Run it:   bash examples/agents/01-ls-and-snapshot.sh
#
# This script stands up its own throwaway server (see lib.sh). A real
# agent skips all that and runs `phux ls` / `phux snapshot` against the
# user's existing one-per-user server, with no --socket.

# shellcheck source=examples/agents/lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
phux_start_server

# Put something on screen so the snapshot has content to show.
phux run "$PHUX_SESSION" "echo 'hello from the demo pane'" >/dev/null

# --- ls: list sessions ------------------------------------------------------
# Human form: one line per session, tmux-`ls`-ish. Good for a log; bad for
# a program to parse.
section "phux ls (human)"
phux ls

# JSON form: the stable, versioned contract (schema_version + sessions[]).
# This is what an agent parses to enumerate work.
section "phux ls --json"
phux ls --json

# Example of consuming it: pull session names out with python (no jq dep).
section "session names, extracted"
phux ls --json | python3 -c '
import json, sys
doc = json.load(sys.stdin)
for s in doc["sessions"]:
    flag = " (attached)" if s["attached"] else ""
    print("{}: {} window(s){}".format(s["name"], s["windows"], flag))
'

# --- snapshot: read the focused pane ---------------------------------------
# Human form: a boxed view of the viewport plus a cursor/dims footer. Use
# it to eyeball what a pane is showing.
section "phux snapshot (human, boxed)"
phux snapshot "$PHUX_SESSION"

# JSON form: schema_version, pane id, cols/rows, cursor {x,y,visible}, and
# `lines` (the viewport, top to bottom, right-trimmed). This is the read
# half of the read+act+wait loop.
section "phux snapshot --json"
phux snapshot "$PHUX_SESSION" --json

# Consuming it: the cursor tells you whether a prompt is waiting; `lines`
# is the screen text. Here we print just the non-empty lines.
section "non-empty screen lines, extracted"
phux snapshot "$PHUX_SESSION" --json | python3 -c '
import json, sys
scr = json.load(sys.stdin)
print("pane {} is {}x{}".format(scr["pane"], scr["cols"], scr["rows"]))
for line in scr["lines"]:
    if line.strip():
        print("  | " + line)
'
