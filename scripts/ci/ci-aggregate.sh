#!/usr/bin/env bash
# The required ci check: plan succeeded, and every other job passed or was
# not selected. A filtered skip passes. Main, the merge queue, and PRs whose
# base branch does not verifiably enforce a merge queue (fallback PRs) still
# require native setup and rust icount; the queue and Rune-affecting fallback
# PRs require Apple, and fallback PRs require their selected Rune Linux lane.
set -euo pipefail

: "${RESULTS:?RESULTS is required}"
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
