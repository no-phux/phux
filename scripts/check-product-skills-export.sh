#!/usr/bin/env bash
# Export the product skills package to a temp dir and assert the publish
# contract: allowlisted SKILL.md files match, openai.yaml is present, beads
# is absent, and README/LICENSE exist.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/phux-skills-export.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

fail() {
  echo "error: $*" >&2
  exit 1
}

bash "${ROOT}/scripts/export-product-skills.sh" "$TMP"

[[ -f "${TMP}/README.md" ]] || fail "exported README.md missing"
[[ -f "${TMP}/LICENSE" ]] || fail "exported LICENSE missing"
[[ -f "${TMP}/NOTICE" ]] || fail "exported NOTICE missing"
cmp -s "${ROOT}/LICENSE" "${TMP}/LICENSE" || fail "exported LICENSE does not match repo LICENSE"
if cmp -s "${ROOT}/NOTICE" "${TMP}/NOTICE"; then
  fail "exported NOTICE must not be the product NOTICE"
fi
grep -Fq 'npx skills add no-phux/skills' "${TMP}/README.md" \
  || fail "package README missing npx skills add no-phux/skills"
if grep -Fq 'npx skills add no-phux/phux' "${TMP}/README.md"; then
  fail "package README must not advertise npx skills add no-phux/phux"
fi
[[ ! -e "${TMP}/skills/beads" ]] || fail "beads must not be exported"

expected=()
while IFS= read -r line || [[ -n "$line" ]]; do
  name="${line%%#*}"
  name="${name#"${name%%[![:space:]]*}"}"
  name="${name%"${name##*[![:space:]]}"}"
  [[ -z "$name" ]] && continue
  [[ "$name" != beads ]] || fail "allowlist contains beads"
  [[ -f "${TMP}/skills/${name}/SKILL.md" ]] || fail "missing exported SKILL.md for ${name}"
  cmp -s "${ROOT}/.agents/skills/${name}/SKILL.md" "${TMP}/skills/${name}/SKILL.md" \
    || fail "SKILL.md mismatch for ${name}"
  [[ -f "${TMP}/skills/${name}/agents/openai.yaml" ]] \
    || fail "missing openai.yaml for ${name}"
  expected+=("$name")
done < "${ROOT}/scripts/product-skills"

[[ ${#expected[@]} -gt 0 ]] || fail "allowlist is empty"

shopt -s nullglob
for dir in "${TMP}/skills"/*; do
  [[ -d "$dir" ]] || continue
  base="$(basename "$dir")"
  found=0
  for name in "${expected[@]}"; do
    if [[ "$base" == "$name" ]]; then
      found=1
      break
    fi
  done
  [[ "$found" -eq 1 ]] || fail "unexpected exported skill: ${base}"
done

echo "product skills export check passed"
