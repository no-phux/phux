#!/usr/bin/env bash
# Warn when a pull request adds or renames a flat top-level docs/*.md that
# no-phux/website main does not mention in scripts/docs-catalog.ts.
#
# Warning-only pending a maintainer decision after one week (around
# Nov 5, 2026). A missing entry, a failed catalog fetch, or a base commit
# that is not in this checkout prints a warning and exits 0. This must not
# fail the guards job or the ci aggregate, and it is not a required check.
#
# Candidates come from `git diff --name-status --diff-filter=AR BASE HEAD`.
# An add's path is the second field; a rename's destination is the third.
# Only docs/<name>.md counts, not docs/<dir>/<name>.md.
set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CATALOG_URL="https://raw.githubusercontent.com/no-phux/website/main/scripts/docs-catalog.ts"

warn_skip() {
  phux_warn "website docs catalog: $1"
  printf '%s\n' "::warning::website docs catalog: $1"
}

if [[ -z "${BASE_SHA:-}" ]]; then
  printf '%s\n' "website docs catalog: no pull request base; nothing to check"
  exit 0
fi

if ! git -C "$ROOT" rev-parse --verify --quiet "${BASE_SHA}^{commit}" >/dev/null; then
  warn_skip "base ${BASE_SHA} is not in this checkout; skipping"
  exit 0
fi

diff_out="$(git -C "$ROOT" diff --name-status --diff-filter=AR "$BASE_SHA" HEAD)"
candidates=()
while IFS=$'\t' read -r status from to || [[ -n "${status:-}" ]]; do
  [[ -n "${status:-}" ]] || continue
  path=""
  case "$status" in
    A) path="$from" ;;
    R*) path="${to:-}" ;;
    *) continue ;;
  esac
  # `case` globs match '/', so this is a regex: one path segment, then .md.
  [[ "$path" =~ ^docs/[^/]+\.md$ ]] || continue
  candidates+=("$path")
done <<<"$diff_out"

if [[ ${#candidates[@]} -eq 0 ]]; then
  printf '%s\n' "website docs catalog: no added or renamed docs/*.md; nothing to check"
  exit 0
fi

if ! catalog="$(curl -fsSL --proto '=https' --max-time 30 --retry 2 "$CATALOG_URL")"; then
  warn_skip "could not fetch ${CATALOG_URL}; skipping"
  exit 0
fi
if [[ -z "$catalog" ]]; then
  warn_skip "fetched an empty catalog from ${CATALOG_URL}; skipping"
  exit 0
fi

# Slug (`foo`) as its own token. The boundary keeps `install` from matching
# `installation`. Fixed-string search, so a slug is never a regex.
slug_listed() {
  local slug="$1"
  [[ -n "$slug" ]] || return 1
  awk -v slug="$slug" '
    BEGIN { found = 0; n = length(slug) }
    {
      rest = $0
      while ((i = index(rest, slug)) > 0) {
        pre = (i == 1) ? "" : substr(rest, i - 1, 1)
        post = substr(rest, i + n, 1)
        okpre = (pre == "" || pre !~ /[A-Za-z0-9_-]/)
        okpost = (post == "" || post !~ /[A-Za-z0-9_-]/)
        if (okpre && okpost) { found = 1; exit }
        rest = substr(rest, i + 1)
      }
    }
    END { exit(found ? 0 : 1) }
  ' <<<"$catalog"
}

for path in "${candidates[@]}"; do
  base="${path##*/}"
  slug="${base%.md}"
  if grep -q -F -- "$base" <<<"$catalog" || slug_listed "$slug"; then
    printf '%s\n' "website docs catalog: ${path} is listed"
    continue
  fi
  msg="open a PR to no-phux/website adding ${slug} to scripts/docs-catalog.ts before merging this one"
  phux_warn "${path} is missing from the website docs catalog"
  phux_hint "$msg"
  printf '%s\n' "::warning file=${path}::${path} is missing from the website docs catalog; ${msg}"
done

exit 0
