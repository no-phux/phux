#!/usr/bin/env bash
#
# 06 — a phux client: read agents, see a question, answer it.
#
# The flow of docs/building.md against a throwaway server: a pane asks
# with the phux-ask title, the client finds it through `phux agent list` and
# `phux watch`, answers with `phux agent answer`, and checks the pane got it.
#
#   bash examples/agents/06-client.sh
#
# $PHUX, else target/debug/phux (see lib.sh).

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
phux_start_server

json() { python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"; }

pane="$(phux snapshot --json "$PHUX_SESSION" | json '"@" + str(d["pane"])')"

section "phux watch --timeout 0: the cursor to resume from"
phux watch --json --timeout 0 "$pane" >/dev/null 2>"$PHUX_TMPDIR/cursor" || true
cursor="$(tail -n 1 "$PHUX_TMPDIR/cursor" | json 'd["cursor"]')"
echo "$cursor"

section "the pane asks a question"
phux send-keys "$pane" \
    "printf '\\033]2;phux-ask[deploy-7]:Deploy to staging??s=yes|no\\033\\\\'; read reply; echo \"got: \$reply\"" \
    Enter

section "phux watch --after: the asked event"
asked="$(phux watch --json --after "$cursor" --until asked --timeout 20 "$pane" 2>/dev/null |
    grep '"event":"asked"')"
printf '%s\n' "$asked"
id="$(printf '%s' "$asked" | json 'd["id"]')"

section "phux agent list: the pane is blocked, its title is the question"
phux agent list --json --state blocked |
    json '"\n".join(a["terminal"] + "\t" + a["state"] + "\t" + str(a["title"]) for a in d["agents"])'

section "phux agent answer --id $id --choice 1"
phux agent answer --id "$id" --choice 1 --json "$pane"

phux wait --until "got: yes" --timeout 10 "$pane" >/dev/null
echo "the pane received the answer"
