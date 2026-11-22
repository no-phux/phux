#!/bin/sh
# Emit one OSC 7501 program-status report, or a short working/blocked/done
# sequence. No phux library, no socket, no hook. The bytes go to this
# process's terminal. See docs/any-program-status.md.
#
# One report. The second argument is kind (empty except with blocked):
#   sh examples/agents/report-status.sh blocked permission mytool
#   sh examples/agents/report-status.sh working "" mytool
#
# No arguments: working, then blocked, hold, then done. Each report also
# sends title, base64 of "deploy". phux uses title as the agent name and
# app as the kind; they differ so the pane is addressable as %deploy.
# PHUX_STATUS_APP overrides the app name (default mytool).
# PHUX_STATUS_TITLE overrides the title (default ZGVwbG95, "deploy").
# PHUX_STATUS_HOLD is how long to stay blocked, in seconds (default 15).
set -u

app=${PHUX_STATUS_APP:-mytool}
title=${PHUX_STATUS_TITLE:-ZGVwbG95}

emit() {
  state=$1
  kind=$2
  app_name=$3
  label=${4:-}
  if [ -n "$kind" ]; then
    body=$(printf 'state=%s:kind=%s:app=%s' "$state" "$kind" "$app_name")
  else
    body=$(printf 'state=%s:app=%s' "$state" "$app_name")
  fi
  if [ -n "$label" ]; then
    body=$(printf '%s:title=%s' "$body" "$label")
  fi
  printf '\033]7501;%s\033\\' "$body"
}

if [ "$#" -gt 0 ]; then
  emit "$1" "${2:-}" "${3:-$app}"
  exit 0
fi

emit working "" "$app" "$title"
emit blocked permission "$app" "$title"
sleep "${PHUX_STATUS_HOLD:-15}"
emit "done" "" "$app" "$title"
