#!/usr/bin/env bash
# The required ci aggregate: selected qualification cannot pass as skipped.
set -euo pipefail

jq -r 'to_entries[] | "\(.value.result)\t\(.key)"' <<<"$RESULTS"
event="${EVENT_NAME:-}"
jq -e --arg event "$event" '
  ($event == "pull_request" and .plan.outputs.queue_verified != "true") as $fallback |
  ($event == "push" or $event == "merge_group" or $fallback) as $mandatory |
  ($fallback and .plan.outputs.rune == "true") as $linux |
  ($event == "merge_group" or $linux) as $apple |
  .plan.result == "success" and
  ([.[].result] | all(. == "success" or . == "skipped"))
    and (($mandatory | not) or (.native.result == "success" and .icount.result == "success"))
    and (($apple | not) or .["queue-apple"].result == "success")
    and (($linux | not) or .rune.result == "success")
' <<<"$RESULTS" >/dev/null
