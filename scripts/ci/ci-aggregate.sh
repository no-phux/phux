#!/usr/bin/env bash
# The required ci check: plan succeeded, and every other job passed or was
# not selected. A filtered skip passes. Main, the merge queue and a release
# bot's PR also require native setup and rust icount. The queue and a
# Rune-affecting release PR require Apple; a release PR also requires its
# selected Rune Linux lane. Other pull requests gate on their selected lanes
# only: Apple, Linux packaging and the mandatory lanes qualify on main.
# A draft's skipped plan passes, but only under the `ci (draft)` job name:
# a draft never reports the required `ci`, so it stays pending, not red.
set -euo pipefail

: "${RESULTS:?RESULTS is required}"
jq -r 'to_entries[] | "\(.value.result)\t\(.key)"' <<<"$RESULTS"
event="${EVENT_NAME:-}"
if [[ "$(jq -r .plan.result <<<"$RESULTS")" == skipped ]]; then
    if [[ "${DRAFT:-}" == true ]]; then
        echo "::notice::draft PR: lanes skipped; ready the PR to run ci"
        exit 0
    fi
    echo "::error::plan was skipped: ready the PR to run ci"
    exit 1
fi
jq -e --arg event "$event" '
  ($event == "pull_request" and .plan.outputs.release_qualify == "true") as $release |
  ($event == "push" or $event == "merge_group" or $release) as $mandatory |
  ($release and .plan.outputs.rune == "true") as $linux |
  ($event == "merge_group" or $linux) as $apple |
  .plan.result == "success" and
  ([.[].result] | all(. == "success" or . == "skipped"))
    and (($mandatory | not) or (.native.result == "success" and .icount.result == "success"))
    and (($apple | not) or .["queue-apple"].result == "success")
    and (($linux | not) or .rune.result == "success")
' <<<"$RESULTS" >/dev/null

# The validation receipt the ci job uploads for a passing PR or queue run.
if [[ -n "${RECEIPT:-}" ]]; then
    mkdir -p "$RUNNER_TEMP/receipt"
    printf '%s\n' "$RECEIPT" >"$RUNNER_TEMP/receipt/receipt.json"
fi
