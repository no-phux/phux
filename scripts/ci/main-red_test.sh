#!/usr/bin/env bash
# Fixture tests for scripts/ci/main-red.sh against a stubbed `gh`.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# The stub logs every call and answers from fixture state in $STATE.
mkdir -p "$tmp/bin"
cat >"$tmp/bin/gh" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$STATE/calls"
case "$1 $2" in
  "issue list") cat "$STATE/open" 2>/dev/null || true ;;
  "api repos/o/r/actions/runs?branch=main&event=push&status=success&per_page=50") cat "$STATE/green" ;;
  "api repos/o/r/actions/runs?branch=main&event=push&status=completed&per_page=50") cat "$STATE/latest" ;;
  "api repos/o/r/compare/"*) echo "- bbbbbbbbbb feat: suspect (#2)" ;;
  "api repos/o/r/commits/"*) echo "- cccccccccc fix: only commit (#3)" ;;
esac
STUB
chmod +x "$tmp/bin/gh"

run() {
  PATH="$tmp/bin:$PATH" STATE="$tmp/state" GH_REPO=o/r WORKFLOW_NAME=ci \
    RUN_URL=https://example.test/run RUN_CREATED_AT=2026-11-14T12:00:00Z \
    HEAD_SHA=cccccccccccccccc CONCLUSION="$1" bash "$here/main-red.sh" >/dev/null
}

fresh() {
  rm -rf "$tmp/state"
  mkdir -p "$tmp/state"
  : >"$tmp/state/calls"
}

fail() {
  echo "main-red_test: $1" >&2
  cat "$tmp/state/calls" >&2
  exit 1
}

# A red run with no open issue opens one naming the suspects since green.
fresh
echo '{"workflow_runs":[{"name":"ci","created_at":"2026-11-14T11:00:00Z","head_sha":"aaaaaaaaaaaa"}]}' |
  jq -r '[.workflow_runs[] | select(.name == "ci")][0].head_sha' >"$tmp/state/green"
run failure
grep -q '^issue create --label ci/main-red --title main is red: ci at cccccccccc' "$tmp/state/calls" ||
  fail "red run did not open the issue"
grep -q 'bbbbbbbbbb feat: suspect (#2)' "$tmp/state/calls" || fail "issue lacks the suspect range"

# A red run with no earlier green run names the head commit alone.
fresh
: >"$tmp/state/green"
run failure
grep -q 'cccccccccc fix: only commit (#3)' "$tmp/state/calls" || fail "no green run: head commit missing"

# A red run with an open issue comments instead of opening another.
fresh
echo 7 >"$tmp/state/open"
echo aaaaaaaaaaaa >"$tmp/state/green"
run failure
grep -q '^issue comment 7 ' "$tmp/state/calls" || fail "second red did not comment"
if grep -q '^issue create' "$tmp/state/calls"; then fail "second red opened a duplicate"; fi

# Green closes the issue only when every tracked workflow is green.
fresh
echo 7 >"$tmp/state/open"
echo success >"$tmp/state/latest"
run success
grep -q '^issue close 7 ' "$tmp/state/calls" || fail "all green did not close"

fresh
echo 7 >"$tmp/state/open"
echo failure >"$tmp/state/latest"
run success
if grep -q '^issue close' "$tmp/state/calls"; then fail "closed while a workflow is still red"; fi

# Green with no open issue, and cancelled runs, do nothing.
for conclusion in success cancelled; do
  fresh
  echo success >"$tmp/state/latest"
  run "$conclusion"
  if grep -qE '^issue (create|comment|close)' "$tmp/state/calls"; then
    fail "$conclusion wrote to an issue"
  fi
done

echo "main-red tests passed"
