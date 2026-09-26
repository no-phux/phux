#!/usr/bin/env bash
# Publish SHA-qualified next-channel assets onto the moving `next` prerelease.
#
# Usage: bash scripts/publish-next-channel.sh [--cockpit] SHA DIST_DIR
#
# Default (phux): DIST_DIR contains phux-next.<sha>-<target>.tar.gz and
# matching .sha256 sidecars; the pointer is channel.json.
# --cockpit: DIST_DIR contains phux-cockpit-next.<sha>-macos-arm64.zip and its
# .sha256 sidecar; the pointer is cockpit-channel.json. The two products move
# independently, so a Cockpit-only change never rebuilds the CLI and a failed
# Cockpit build never holds the CLI back.
#
# The pointer is written last so an in-flight client cannot observe a pointer
# whose archive is not yet attached. The previous SHA's assets are kept; older
# assets of the same product are pruned.
set -euo pipefail

product="phux"
if [[ "${1:-}" == "--cockpit" ]]; then
  product="cockpit"
  shift
fi

if [[ $# -ne 2 ]]; then
  echo "usage: bash scripts/publish-next-channel.sh [--cockpit] SHA DIST_DIR" >&2
  exit 2
fi

sha="$1"
dist="$2"

if [[ ! "$sha" =~ ^[0-9a-f]{40}$ ]]; then
  echo "error: not a git SHA: ${sha}" >&2
  exit 1
fi

if [[ "$product" == "cockpit" ]]; then
  asset_family="phux-cockpit-next."
  pointer_name="cockpit-channel.json"
  IFS= read -r version < clients/cockpit/version.txt
  required=("phux-cockpit-next.${sha}-macos-arm64.zip")
else
  asset_family="phux-next."
  pointer_name="channel.json"
  version="$(sed -n 's/^version = "\([0-9][0-9.]*\)"/\1/p' Cargo.toml | head -n 1)"
  required=(
    "phux-next.${sha}-aarch64-apple-darwin.tar.gz"
    "phux-next.${sha}-x86_64-unknown-linux-gnu.tar.gz"
    "phux-next.${sha}-aarch64-unknown-linux-gnu.tar.gz"
  )
fi
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "error: could not read the ${product} version" >&2
  exit 1
}

shopt -s nullglob
keep_prefix="${asset_family}${sha}-"
assets=("$dist/${keep_prefix}"*)
if (( ${#assets[@]} == 0 )); then
  echo "error: no next-channel ${product} artifacts for ${sha} in ${dist}" >&2
  exit 1
fi
for base in "${required[@]}"; do
  [[ -f "${dist}/${base}" && -f "${dist}/${base}.sha256" ]] || {
    echo "error: missing ${base} (or its sidecar)" >&2
    exit 1
  }
done

prev=""
if gh release view next >/dev/null 2>&1; then
  tmp="$(mktemp)"
  if gh release download next --pattern "$pointer_name" --output "$tmp" 2>/dev/null; then
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
channel_json="$pointer_dir/$pointer_name"
cat > "$channel_json" <<EOF
{
  "schema_version": 1,
  "channel": "next",
  "product": "${product}",
  "sha": "${sha}",
  "version": "${version}",
  "published_at": "${published_at}"
}
EOF
gh release upload next "$channel_json" --clobber
rm -rf "$pointer_dir"

names="$(gh release view next --json assets --jq '.assets[].name')"
prev_prefix=""
if [[ -n "$prev" && "$prev" != "$sha" ]]; then
  prev_prefix="${asset_family}${prev}-"
fi
while IFS= read -r name; do
  [ -n "$name" ] || continue
  case "$name" in
    channel.json|cockpit-channel.json) continue ;;
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
    "${asset_family}"*)
      gh release delete-asset next "$name" --yes
      ;;
  esac
done <<<"$names"
