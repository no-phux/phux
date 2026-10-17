#!/usr/bin/env bash
# Fail unless every workspace crate resolves to the version in a vX.Y.Z tag.
# The crate list comes from `cargo metadata`, so a new crate is covered the
# moment it exists. Usage: check-release-version.sh vX.Y.Z
set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"

tag="${1:?usage: check-release-version.sh vX.Y.Z}"
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  phux_error "release tag must look like vX.Y.Z, got: ${tag}"
  exit 1
}
cd "$(dirname "$0")/.."

metadata="$(cargo metadata --locked --format-version 1 --no-deps)"
count="$(jq '.packages | length' <<<"$metadata")"
mismatches="$(jq -r --arg want "${tag#v}" '
  .packages | sort_by(.name)[] | select(.version != $want)
  | "error: \(.name) resolves to \(.version), expected \($want)"' <<<"$metadata")"

if [ "$count" -lt 1 ] || [ -n "$mismatches" ]; then
  printf '%s\n' "$mismatches" >&2
  phux_error "workspace versions disagree with release tag ${tag} (${count} crates)"
  exit 1
fi
phux_ok "release version: ${tag} (${count} workspace crates)"
