#!/usr/bin/env bash
# The required ci check: plan succeeded, and every other job passed or was
# not selected. A filtered skip passes. Main and the merge queue also
# require native setup and rust icount; the merge queue requires Apple.
# Pull requests gate on their selected lanes only: Apple, Linux packaging
# and the mandatory lanes qualify on the main push.
# A draft's skipped plan fails, so a stale draft ci never satisfies a ready PR.
set -euo pipefail

: "${RESULTS:?RESULTS is required}"
jq -r 'to_entries[] | "\(.value.result)\t\(.key)"' <<<"$RESULTS"
event="${EVENT_NAME:-}"
if [[ "$(jq -r .plan.result <<<"$RESULTS")" == skipped ]]; then
    echo "::error::plan was skipped (draft PR): ready the PR to run ci"
    exit 1
fi
jq -e --arg event "$event" '
  ($event == "push" or $event == "merge_group") as $mandatory |
  ($event == "merge_group") as $apple |
  .plan.result == "success" and
  ([.[].result] | all(. == "success" or . == "skipped"))
    and (($mandatory | not) or (.native.result == "success" and .icount.result == "success"))
    and (($apple | not) or .["queue-apple"].result == "success")
' <<<"$RESULTS" >/dev/null
