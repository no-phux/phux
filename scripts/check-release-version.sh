#!/usr/bin/env bash
# check-release-version.sh
#
# Assert that every workspace crate resolves to the version encoded in a
# release tag. Runs in release.yml (twice: the prepare gate and each build
# matrix leg) and publish-crate.yml, before anything irreversible happens.
#
# The package list is DERIVED from `cargo metadata`, not hand-maintained.
# It used to be a literal `packages=()` array, and that array had already
# rotted: it was missing phux-dial, phux-plugin, and phux-relay, so the gate
# verified 10 of 13 crates while its name and output claimed it verified the
# release. A hand list fails silently in exactly the wrong direction -- the
# gate goes green having checked less -- and a brand-new crate is precisely
# the thing most likely to carry a stale version. Deriving means a crate is
# covered the moment its `crates/*/Cargo.toml` exists.
#
# `cargo metadata --no-deps` reports exactly the workspace members. The
# `clients/` wasm crates are a separate workspace (excluded in the root
# manifest) and are correctly out of scope: they ship with the web client,
# not in a phux release tarball.
#
# Usage:
#   bash scripts/check-release-version.sh v0.3.1   # or: just release-check v0.3.1
#
# Exit codes:
#   0   every workspace crate is at the tag's version
#   1   a version mismatch, a malformed tag, or a missing tool

set -euo pipefail

tag="${1:?usage: check-release-version.sh <tag>  e.g. v0.0.2}"

case "$tag" in
  v[0-9]*.[0-9]*.[0-9]*) ;;
  *)
    echo "error: release tag must look like vX.Y.Z, got: ${tag}" >&2
    exit 1
    ;;
esac

version="${tag#v}"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

if ! command -v jq >/dev/null 2>&1; then
  # jq parses the cargo metadata JSON. It is in the nix devshell (flake.nix)
  # and preinstalled on every GitHub hosted runner image, which covers both
  # callers; a missing jq is a real environment problem, never a reason to
  # let a release through unchecked.
  echo "error: jq not found on PATH; cannot read cargo metadata" >&2
  exit 1
fi

metadata="$(cargo metadata --locked --format-version 1 --no-deps)"

# Sanity floor: an empty member list would make every comparison below
# vacuously pass, turning the gate into a silent no-op -- the same failure
# mode as the stale hand list. Refuse it explicitly.
count="$(printf '%s' "$metadata" | jq '.packages | length')"
if [ "$count" -lt 1 ]; then
  echo "error: cargo metadata reported no workspace members" >&2
  exit 1
fi

# Report EVERY mismatch, not just the first. During a release you want the
# whole list in one shot; bisecting a version bump one failed run at a time
# is a needless round trip through a workflow dispatch.
mismatches="$(printf '%s' "$metadata" | jq -r --arg want "$version" '
  .packages
  | sort_by(.name)[]
  | select(.version != $want)
  | "error: \(.name) resolves to \(.version), expected \($want)"
')"

if [ -n "$mismatches" ]; then
  printf '%s\n' "$mismatches" >&2
  echo "error: workspace versions disagree with release tag ${tag}" >&2
  exit 1
fi

echo "release version ok: ${tag} (${count} workspace crates)"
