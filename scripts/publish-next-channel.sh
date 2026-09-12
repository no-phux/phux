#!/usr/bin/env bash
# Publish SHA-qualified next-channel assets onto the moving `next` prerelease.
#
# Usage: bash scripts/publish-next-channel.sh SHA DIST_DIR
#
# DIST_DIR contains phux-next.<sha>-<target>.tar.gz and matching .sha256
# sidecars. channel.json is written last so an in-flight client cannot
# observe a pointer whose archive is not yet attached. The previous SHA's
# assets are kept; older next assets are pruned.
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: bash scripts/publish-next-channel.sh SHA DIST_DIR" >&2
  exit 2
fi

sha="$1"
dist="$2"

if [[ ! "$sha" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: not a git SHA: ${sha}" >&2
  exit 1
fi

version="$(sed -n 's/^version = "\([0-9][0-9.]*\)"/\1/p' Cargo.toml | head -n 1)"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "error: could not read workspace version from Cargo.toml" >&2
  exit 1
}

shopt -s nullglob
assets=("$dist"/phux-next."${sha}"-*.tar.gz "$dist"/phux-next."${sha}"-*.tar.gz.sha256)
if (( ${#assets[@]} == 0 )); then
  echo "error: no next-channel artifacts for ${sha} in ${dist}" >&2
  exit 1
fi

required_targets=(
  aarch64-apple-darwin
  x86_64-unknown-linux-gnu
  aarch64-unknown-linux-gnu
)
for target in "${required_targets[@]}"; do
  base="phux-next.${sha}-${target}.tar.gz"
  [[ -f "${dist}/${base}" && -f "${dist}/${base}.sha256" ]] || {
    echo "error: missing ${base} (or its sidecar)" >&2
    exit 1
  }
done

prev=""
if gh release view next >/dev/null 2>&1; then
  tmp="$(mktemp)"
  if gh release download next --pattern channel.json --output "$tmp" 2>/dev/null; then
    prev="$(sed -n 's/.*"sha"[[:space:]]*:[[:space:]]*"\([0-9a-f]\{40\}\)".*/\1/p' "$tmp" | head -n 1)"
  fi
  rm -f "$tmp"
  gh release upload next "${assets[@]}" --clobber
  gh api -X PATCH "repos/${GH_REPO}/git/refs/tags/next" -f sha="$sha" -F force=true >/dev/null
  gh release edit next --prerelease --latest=false --target "$sha" --title "next"
else
  gh release create next "${assets[@]}" \
    --prerelease --latest=false --target "$sha" --title "next" \
    --notes "Moving next channel (green main). Not a stable release."
fi

published_at="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
# The asset name is the basename. `gh release upload file#label` sets a
# label, not the filename, so a mktemp path uploaded as `tmp.XXXX`.
pointer_dir="$(mktemp -d)"
channel_json="$pointer_dir/channel.json"
cat > "$channel_json" <<EOF
{
  "schema_version": 1,
  "channel": "next",
  "sha": "${sha}",
  "version": "${version}",
  "published_at": "${published_at}"
}
EOF
gh release upload next "$channel_json" --clobber
rm -rf "$pointer_dir"

names="$(gh release view next --json assets --jq '.assets[].name')"
keep_prefix="phux-next.${sha}-"
prev_prefix=""
if [[ -n "$prev" && "$prev" != "$sha" ]]; then
  prev_prefix="phux-next.${prev}-"
fi
while IFS= read -r name; do
  [ -n "$name" ] || continue
  case "$name" in
    channel.json) continue ;;
    "${keep_prefix}"*) continue ;;
    tmp.*)
      gh release delete-asset next "$name" --yes
      continue
      ;;
  esac
  if [[ -n "$prev_prefix" && "$name" == "${prev_prefix}"* ]]; then
    continue
  fi
  case "$name" in
    phux-next.*)
      gh release delete-asset next "$name" --yes
      ;;
  esac
done <<<"$names"
