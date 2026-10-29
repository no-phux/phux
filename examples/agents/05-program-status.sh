#!/usr/bin/env bash
#
# 05 — any program can report status. OSC 7501, no hooks.
#
# A throwaway server, one pane, and examples/agents/report-status.sh.
# While that program is blocked, this prints `phux agent list` and
# `phux agent explain` for the pane. A reader with `phux` already
# installed can run it in well under a minute:
#
#   bash examples/agents/05-program-status.sh
#
# $PHUX, else `phux` on PATH, else target/debug/phux (see lib.sh).

source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
phux_start_server

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
emitter="$root/examples/agents/report-status.sh"
hold="${PHUX_STATUS_HOLD:-20}"

section "report-status.sh (holds on blocked for ${hold}s)"
# `phux run` waits until the command finishes. Background it so list and
# explain can read the pane during the hold, before a shell prompt (OSC
# 133 A) or process exit can drop a working or blocked record.
phux run "$PHUX_SESSION" "PHUX_STATUS_HOLD=$hold sh $(printf '%q' "$emitter")" \
    >"$PHUX_TMPDIR/run.out" 2>&1 &
run_pid=$!

list=""
deadline=$((SECONDS + hold))
while (( SECONDS < deadline )); do
    list="$(phux agent list 2>/dev/null || true)"
    if [[ "$list" == *$'\tblocked\t'* ]]; then
        break
    fi
    sleep 0.2
done

if [[ "$list" != *$'\tblocked\t'* ]]; then
    echo "examples/agents: program status did not show as blocked" >&2
    printf '%s\n' "$list" >&2
    cat "$PHUX_TMPDIR/run.out" >&2 || true
    wait "$run_pid" || true
    exit 1
fi

section "phux agent list"
printf '%s\n' "$list"

pane="$(phux snapshot --json "$PHUX_SESSION" | python3 -c 'import json,sys; print("@" + str(json.load(sys.stdin)["pane"]))')"

section "phux agent explain $pane"
phux agent explain "$pane"

section "phux resource show $pane"
phux resource show "$pane"

wait "$run_pid"
