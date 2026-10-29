#!/usr/bin/env bash
#
# 04 — the read+act+wait loop, end to end.
#
# This is the loop an agent actually runs against a pane it cannot predict:
#
#     loop:
#       READ  the screen        (snapshot --json)
#       DECIDE what to do        (parse lines / cursor)
#       ACT   send input         (send-keys, or run for discrete commands)
#       WAIT  for the effect     (wait --until / --idle, bounded by --timeout)
#
# The scenario: drive an interactive program that prompts for a number,
# answer it, and confirm the program reacted -- without ever attaching a
# TTY.
#
# Two subtleties this example bakes in deliberately:
#
#   * lib.sh selects a clean POSIX shell; the program also uses `sh -c`
#     so its syntax is explicit when copied into an agent.
#   * `wait --until` matches ANY visible line, INCLUDING the echo of the
#     command you just typed. So we wait on `result=49` -- a value the
#     program COMPUTES at runtime, which never appears in the command's
#     own source text. Match on output, never on your own keystrokes.
#
# Run it:   bash examples/agents/04-read-act-wait-loop.sh

# shellcheck source=examples/agents/lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
phux_start_server

# A tiny interactive program standing in for an installer / wizard: it
# prompts, reads a number, and prints its square. Wrapped in `sh -c` for
# portability across login shells.
# shellcheck disable=SC2016 # Arithmetic expands in the pane, not this script.
PROMPT_PROG='sh -c '\''printf "Pick a %s: " number; read -r n; echo "result=$((n * n))"'\'''

section "ACT: start the interactive prompt"
phux send-keys "$PHUX_SESSION" "$PROMPT_PROG" Enter

# READ + WAIT: the formatted prompt is absent from the echoed source too.
section "READ+WAIT: block until the prompt is on screen"
phux wait --until "Pick a number:" --timeout 10 "$PHUX_SESSION"

# READ + DECIDE: snapshot the waiting prompt. An agent feeds `lines` to its
# decision step here; we just show what it would see.
section "READ: snapshot the waiting prompt"
phux snapshot "$PHUX_SESSION" --json | python3 -c '
import json, sys
scr = json.load(sys.stdin)
prompt = next((l for l in scr["lines"] if "Pick a number:" in l), "(prompt not found)")
print("  decision input: " + repr(prompt.strip()))
cur = scr["cursor"]
print("  cursor waiting at x={}, y={}".format(cur["x"], cur["y"]))
'

# ACT: answer the prompt, then WAIT on the program's COMPUTED RESULT
# (`result=49`), which exists only in output -- not in the echoed "7" or
# the command source. This is the honest way to confirm an effect.
section "ACT: answer 7, then WAIT for the computed result"
phux send-keys "$PHUX_SESSION" "7" Enter
phux wait --until "result=49" --timeout 10 "$PHUX_SESSION"
echo "  program computed 7*7=49; the loop's effect is observed in OUTPUT"

# Back to a settled prompt. From here the agent would loop again: read,
# decide, act, wait.
phux wait --idle 500 --timeout 10 "$PHUX_SESSION"

section "final screen"
phux snapshot "$PHUX_SESSION"
