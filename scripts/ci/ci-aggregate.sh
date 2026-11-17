#!/usr/bin/env bash
# The required ci check: plan succeeded, and every other job passed or was
# not selected. A filtered skip passes. Main and the merge queue still
# require native setup and rust icount, and the queue requires Apple.
set -euo pipefail

: "${RESULTS:?RESULTS is required}"
jq -r 'to_entries[] | "\(.value.result)\t\(.key)"' <<<"$RESULTS"
event="${EVENT_NAME:-}"
jq -e --arg event "$event" '
  .plan.result == "success" and
  (
    [to_entries[] | select(.value.result != "success")]
    | all(
        .value.result == "skipped" and
        (
          ($event != "push" and $event != "merge_group") or
          (.key != "native" and .key != "icount")
        )
      )
  ) and ($event != "merge_group" or .["queue-apple"].result == "success")
' <<<"$RESULTS" >/dev/null
