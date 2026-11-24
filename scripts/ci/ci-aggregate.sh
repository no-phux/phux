#!/usr/bin/env bash
# The required ci check: plan succeeded, and every other job passed or was
# not selected. A filtered skip passes. Main, the merge queue and a release
# bot's PR also require native setup and rust icount. The queue and a
# Rune-affecting release PR require Apple; a release PR also requires its
# selected Rune Linux lane. Other pull requests gate on their selected lanes
# only: Apple, Linux packaging and the mandatory lanes qualify on main.
# Draft notices never qualify or write receipts. The renamed draft job cannot
# invalidate an older same-SHA check; automated arming also needs pr-ready-ci.
set -euo pipefail

: "${RESULTS:?RESULTS is required}"
jq -r 'to_entries[] | "\(.value.result)\t\(.key)"' <<<"$RESULTS"
event="${EVENT_NAME:-}"
if [[ "${DRAFT:-}" == true ]]; then
    if jq -e '[.[].result] | all(. == "skipped")' <<<"$RESULTS" >/dev/null; then
        echo "::notice::draft PR: lanes skipped; ready the PR to run ci"
        exit 0
    fi
    echo "::error::a draft must not run qualification"
    exit 1
fi
jq -e --arg event "$event" '
  def flags: ["guards", "lint", "tests", "e2e", "ffi", "icount",
    "integrations", "web", "native", "rune", "rune_qualify", "release_qualify"];
  .plan.outputs as $p |
  ($event == "pull_request" and .plan.outputs.release_qualify == "true") as $release |
  ($event == "push" or $event == "merge_group" or $release) as $mandatory |
  ($release and .plan.outputs.rune == "true") as $linux |
  ($event == "merge_group" or $linux) as $apple |
  {
    guards: ($p.guards == "true"), lint: ($p.lint == "true"),
    "test-build": ($p.tests == "true" or $p.e2e == "true"),
    tests: ($p.tests == "true"), e2e: ($p.e2e == "true"),
    ffi: ($p.ffi == "true"), integrations: ($p.integrations == "true"),
    "web-vt": ($p.web == "true"), "web-client": ($p.web == "true"),
    native: ($mandatory or $p.native == "true"),
    icount: ($mandatory or $p.icount == "true"),
    rune: ($p.rune == "true"), "queue-apple": $apple
  } as $required |
  .plan.result == "success" and
  (flags | all(. as $key | $p[$key] == "true" or $p[$key] == "false")) and
  (($linux | not) or $p.rune_qualify == "true") and
  ([.[].result] | all(. == "success" or . == "skipped"))
    and (. as $results | $required | to_entries | all(
      .key as $id | if .value then $results[$id].result == "success"
      else ($results[$id].result == "success" or $results[$id].result == "skipped") end))
' <<<"$RESULTS" >/dev/null

# The validation receipt the ci job uploads for a passing PR or queue run.
if [[ -n "${RECEIPT:-}" ]]; then
    mkdir -p "$RUNNER_TEMP/receipt"
    printf '%s\n' "$RECEIPT" >"$RUNNER_TEMP/receipt/receipt.json"
fi
