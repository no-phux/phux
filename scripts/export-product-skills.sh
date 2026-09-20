#!/usr/bin/env bash
# Write the product skills package to DEST for the no-phux/skills publish
# mirror. Source of truth remains .agents/skills/; this tree is generated.
#
# Usage: scripts/export-product-skills.sh DEST
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
ALLOWLIST="${ROOT}/scripts/product-skills"
SOURCE="${ROOT}/.agents/skills"
PACKAGE="${ROOT}/scripts/skills-package"

usage() {
  echo "usage: export-product-skills.sh DEST" >&2
  exit 2
}

[[ $# -eq 1 && -n "$1" ]] || usage

dest=$1
mkdir -p "$dest"
dest="$(CDPATH='' cd -- "$dest" && pwd)"

case "$dest" in
  "$ROOT"|"$SOURCE"|"$PACKAGE"|"${ROOT}/.agents"|"${ROOT}/scripts")
    echo "error: DEST must not be the phux source tree" >&2
    exit 1
    ;;
esac

if [[ ! -f "$ALLOWLIST" ]]; then
  echo "error: missing allowlist: ${ALLOWLIST}" >&2
  exit 1
fi
if [[ ! -f "${PACKAGE}/README.md" ]]; then
  echo "error: missing package README: ${PACKAGE}/README.md" >&2
  exit 1
fi
if [[ ! -f "${PACKAGE}/NOTICE" ]]; then
  echo "error: missing package NOTICE: ${PACKAGE}/NOTICE" >&2
  exit 1
fi
if [[ ! -f "${ROOT}/LICENSE" ]]; then
  echo "error: missing LICENSE: ${ROOT}/LICENSE" >&2
  exit 1
fi

names=()
while IFS= read -r line || [[ -n "$line" ]]; do
  name="${line%%#*}"
  name="${name#"${name%%[![:space:]]*}"}"
  name="${name%"${name##*[![:space:]]}"}"
  [[ -z "$name" ]] && continue
  case "$name" in
    beads)
      echo "error: beads is maintainer tooling and must not be published" >&2
      exit 1
      ;;
    *[!a-zA-Z0-9_-]*)
      echo "error: invalid skill name in allowlist: ${name}" >&2
      exit 1
      ;;
  esac
  if [[ ! -d "${SOURCE}/${name}" ]]; then
    echo "error: missing skill directory: ${SOURCE}/${name}" >&2
    exit 1
  fi
  if [[ ! -f "${SOURCE}/${name}/SKILL.md" ]]; then
    echo "error: missing SKILL.md: ${SOURCE}/${name}/SKILL.md" >&2
    exit 1
  fi
  names+=("$name")
done < "$ALLOWLIST"

if [[ ${#names[@]} -eq 0 ]]; then
  echo "error: allowlist is empty: ${ALLOWLIST}" >&2
  exit 1
fi

cp "${PACKAGE}/README.md" "${dest}/README.md"
cp "${ROOT}/LICENSE" "${dest}/LICENSE"
cp "${PACKAGE}/NOTICE" "${dest}/NOTICE"

rm -rf "${dest}/skills"
mkdir -p "${dest}/skills"
for name in "${names[@]}"; do
  mkdir -p "${dest}/skills/${name}"
  cp -R "${SOURCE}/${name}/." "${dest}/skills/${name}/"
done

# Defense: never leave beads in the published tree even if DEST was dirty.
rm -rf "${dest}/skills/beads"

echo "exported ${#names[@]} product skill(s) -> ${dest}"
