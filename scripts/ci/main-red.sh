#!/usr/bin/env bash
# Report main's health after the merge (.github/workflows/main-red.yml).
#
# Pull requests merge on a fast Linux gate; the slow lanes (native setup, rust
# icount, Rune Linux qualification, Apple) first run on main's push. When one
# of those push runs ends red, this opens the one `ci/main-red` issue (later
# reds comment on it) and lists the commits since that workflow's last green
# main push as suspects. When every tracked workflow's latest main push run is
# green again, it closes the issue.
#
# Reads only run metadata and commit subjects; never checks out or runs the
# reported run's code. Inputs arrive through the environment, never through
# workflow expression interpolation.
set -euo pipefail

: "${GH_REPO:?}" "${WORKFLOW_NAME:?}" "${RUN_URL:?}" "${RUN_CREATED_AT:?}"
: "${CONCLUSION:?}" "${HEAD_SHA:?}"
TRACKED="${TRACKED_WORKFLOWS:-ci rune-macos}"
LABEL=ci/main-red

open_issue() {
  gh issue list --label "$LABEL" --state open --limit 1 --json number --jq '.[0].number // empty'
}

# The newest green main push run of WORKFLOW_NAME created before this run.
last_green_sha() {
  gh api "repos/$GH_REPO/actions/runs?branch=main&event=push&status=success&per_page=50" \
    --jq "[.workflow_runs[] | select(.name == \"$WORKFLOW_NAME\" and .created_at < \"$RUN_CREATED_AT\")][0].head_sha // empty"
}

suspects() {
  local good
  good="$(last_green_sha)"
  if [[ -z "$good" ]]; then
    gh api "repos/$GH_REPO/commits/$HEAD_SHA" \
      --jq '"- \(.sha[0:10]) \(.commit.message | split("\n")[0])"'
    return
  fi
  gh api "repos/$GH_REPO/compare/$good...$HEAD_SHA" \
    --jq '.commits | reverse | .[] | "- \(.sha[0:10]) \(.commit.message | split("\n")[0])"'
  echo
  echo "Last green \`$WORKFLOW_NAME\` push: \`${good:0:10}\`."
}

# Whether the latest completed main push run of every tracked workflow passed.
all_green() {
  local name conclusion
  for name in $TRACKED; do
    conclusion="$(gh api "repos/$GH_REPO/actions/runs?branch=main&event=push&status=completed&per_page=50" \
      --jq "[.workflow_runs[] | select(.name == \"$name\" and .conclusion != \"cancelled\")][0].conclusion // \"success\"")"
    [[ "$conclusion" == success ]] || return 1
  done
}

report_red() {
  local issue body
  issue="$(open_issue)"
  body="$(printf '`%s` failed on main at `%s`: %s\n\nSuspect commits, oldest first:\n\n%s\n' \
    "$WORKFLOW_NAME" "${HEAD_SHA:0:10}" "$RUN_URL" "$(suspects)")"
  if [[ -n "$issue" ]]; then
    gh issue comment "$issue" --body "$body"
    return
  fi
  gh label create "$LABEL" --color B60205 --description "main's post-merge CI is red" 2>/dev/null || true
  gh issue create --label "$LABEL" --title "main is red: $WORKFLOW_NAME at ${HEAD_SHA:0:10}" --body "$body"
}

report_green() {
  local issue
  issue="$(open_issue)"
  [[ -n "$issue" ]] || return 0
  all_green || return 0
  gh issue close "$issue" --comment "Green again at \`${HEAD_SHA:0:10}\`: every tracked workflow ($TRACKED) passed its latest main push."
}

case "$CONCLUSION" in
  failure | timed_out) report_red ;;
  success) report_green ;;
  *) echo "main-red: ignoring conclusion $CONCLUSION" ;;
esac
