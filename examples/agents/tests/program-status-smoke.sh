#!/usr/bin/env bash
# The OSC 7501 emitter writes the bytes the program protocol specifies.
# No server and no phux binary: this only checks the sequence.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
emitter="$ROOT/examples/agents/report-status.sh"

blocked="$(printf '\033]7501;state=blocked:kind=permission:app=mytool\033\\')"
got="$(sh "$emitter" blocked permission mytool)"
if [[ "$got" != "$blocked" ]]; then
    printf 'report-status.sh blocked sequence mismatch\n' >&2
    printf 'got: %q\n' "$got" >&2
    exit 1
fi

working="$(printf '\033]7501;state=working:app=mytool\033\\')"
got="$(sh "$emitter" working "" mytool)"
if [[ "$got" != "$working" ]]; then
    printf 'report-status.sh working sequence mismatch\n' >&2
    printf 'got: %q\n' "$got" >&2
    exit 1
fi

# The no-argument sequence is what the demo runs. title is base64 of "deploy".
titled_working="$(printf '\033]7501;state=working:app=mytool:title=ZGVwbG95\033\\')"
titled_blocked="$(printf '\033]7501;state=blocked:kind=permission:app=mytool:title=ZGVwbG95\033\\')"
titled_done="$(printf '\033]7501;state=done:app=mytool:title=ZGVwbG95\033\\')"
sequence="$(PHUX_STATUS_HOLD=0 sh "$emitter")"
want="${titled_working}${titled_blocked}${titled_done}"
if [[ "$sequence" != "$want" ]]; then
    printf 'report-status.sh default sequence mismatch\n' >&2
    printf 'got: %q\n' "$sequence" >&2
    exit 1
fi
