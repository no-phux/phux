#!/bin/sh
# Emit one OSC 7501 program-status report, or a short working/blocked/done
# sequence. No phux library, no socket, no hook. The bytes go to this
# process's terminal. See docs/any-program-status.md.
#
# One report. The second argument is kind (empty except with blocked):
#   sh examples/agents/report-status.sh blocked permission mytool
#   sh examples/agents/report-status.sh working "" mytool
#
# No arguments: working, then blocked, hold, then done.
# PHUX_STATUS_APP overrides the app name (default mytool).
# PHUX_STATUS_HOLD is how long to stay blocked, in seconds (default 15).
set -u

app=${PHUX_STATUS_APP:-mytool}

emit() {
  state=$1
  kind=$2
  app_name=$3
  if [ -n "$kind" ]; then
    printf '\033]7501;state=%s:kind=%s:app=%s\033\\' "$state" "$kind" "$app_name"
  else
    printf '\033]7501;state=%s:app=%s\033\\' "$state" "$app_name"
  fi
}

if [ "$#" -gt 0 ]; then
  emit "$1" "${2:-}" "${3:-$app}"
  exit 0
fi

emit working "" "$app"
emit blocked permission "$app"
sleep "${PHUX_STATUS_HOLD:-15}"
emit "done" "" "$app"
