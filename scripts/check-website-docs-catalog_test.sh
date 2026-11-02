#!/usr/bin/env bash
# Fixture for the website docs catalog warning. A disposable repo, no network.
# The check stays exit 0; this test fails when that contract breaks.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/phux-docs-catalog-test.XXXXXX")"
scratch="$(cd "$scratch" && pwd -P)"
trap 'rm -rf "$scratch"' EXIT

repo="$scratch/repo"
mkdir -p "$repo/scripts/lib" "$repo/docs"
cp "$root/scripts/check-website-docs-catalog.sh" "$repo/scripts/"
cp "$root/scripts/lib/output.sh" "$repo/scripts/lib/"

git -C "$repo" init -q
git -C "$repo" config user.email "catalog-test@example.invalid"
git -C "$repo" config user.name "Catalog Test"
git -C "$repo" config commit.gpgsign false
printf '%s\n' 'base' >"$repo/README.md"
git -C "$repo" add README.md
git -C "$repo" commit -q -m "base"
base="$(git -C "$repo" rev-parse HEAD)"

printf '%s\n' '# zz' >"$repo/docs/zz-uncatalogued.md"
git -C "$repo" add docs/zz-uncatalogued.md
git -C "$repo" commit -q -m "add zz-uncatalogued"
head="$(git -C "$repo" rev-parse HEAD)"

missing="$scratch/missing-catalog.ts"
listed="$scratch/listed-catalog.ts"
printf '%s\n' '{ file: "concepts.md", slug: "concepts" }' >"$missing"
printf '%s\n' '{ file: "zz-uncatalogued.md", slug: "zz-uncatalogued" }' >"$listed"

status=0
run() {
  local url="$1"
  status=0
  NO_COLOR=1 BASE_SHA="$base" HEAD_SHA="$head" CATALOG_URL="$url" \
    bash "$repo/scripts/check-website-docs-catalog.sh" >"$scratch/out" 2>"$scratch/err" || status=$?
  cat "$scratch/out" "$scratch/err" >"$scratch/combined"
}

fail() {
  printf '%s\n' "$1" >&2
  cat "$scratch/combined" >&2
  exit 1
}

run "$missing"
[[ "$status" -eq 0 ]] || fail "missing catalog: expected exit 0, got ${status}"
grep -F '::warning file=docs/zz-uncatalogued.md' "$scratch/combined" >/dev/null ||
  fail "missing catalog: expected a warning for docs/zz-uncatalogued.md"

run "file://${listed}"
[[ "$status" -eq 0 ]] || fail "listed catalog: expected exit 0, got ${status}"
if grep -F '::warning' "$scratch/combined" >/dev/null; then
  fail "listed catalog: unexpected warning"
fi

printf '%s\n' "website docs catalog fixture: missing entry warns and a listed file does not"
