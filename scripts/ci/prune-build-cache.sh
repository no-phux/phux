#!/usr/bin/env bash
# Keep one current build cache per lane/release prefix.
#
# A key from the current UTC day is never deleted. When one exists, older
# keys for that prefix go. When it does not, the newest older key stays so
# a restore still has a source. --replace-prefix drops those older keys even
# when they are the only copy: cache-warm.yml does that after the lane has
# restored and before mbx saves today's objects. Measured on 2026-11-13 the
# repo held 10.55 GB across 14 caches (list sum 11.18 GB, 17 entries), so a
# second day's mbx test entry (2.35 GiB) would LRU-evict the Rune archives.
#
# --dry-run prints the same decisions and does not call DELETE.
set -euo pipefail

today="$(date -u +%Y%m%d)"
today_iso="$(date -u +%Y-%m-%d)"
dry=0
replace=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) dry=1; shift ;;
    --replace-prefix)
      [[ $# -ge 2 ]] || { echo "prune-build-cache: --replace-prefix needs a prefix" >&2; exit 2; }
      replace+=("$2")
      shift 2
      ;;
    *)
      echo "prune-build-cache: unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

# One series each. ios-native is the Rune simulator leg's rust-cache key.
sweep=(
  linux-arm64-mbx-phux-check-
  linux-arm64-mbx-phux-test-
  v0-rust-phux-uniffi-Linux-
  linux-x64-mbx-phux-icount-
  v0-rust-ffi-android-Linux-x64-
  v0-rust-release-deps-Linux-x64-
  v0-rust-release-deps-Linux-arm64-
  v0-rust-release-deps-Darwin-arm64-
  v0-rust-mini-v1-desktop-release-
  mini-v1-ffi-xcframework-zig-
  v0-rust-windows-desktop-wsl-server-Linux-x64-
  v0-rust-windows-desktop-msvc-Windows_NT-x64-
  v0-rust-desktop-macOS-Darwin-
  v0-rust-desktop-macOS-26-Darwin-
  v0-rust-desktop-Linux-Linux-
  v0-rust-rune-macos-arm64-Darwin-
  v0-rust-rune-linux-x86_64-Linux-
  v0-rust-rune-linux-aarch64-Linux-
  v0-rust-rune-ios-native-
)

is_current_day() {
  local key=$1 created=$2
  [[ $key == *-"$today" || $created == "$today_iso"* ]]
}

prune_prefix() {
  local prefix=$1 mode=$2
  local entries id key created have_today=0 kept_restore=0
  entries="$(
    gh api --paginate \
      "repos/${GITHUB_REPOSITORY:?}/actions/caches?key=${prefix}&sort=created_at&direction=desc&per_page=100" \
      --jq '.actions_caches[] | [.id, .key, .created_at] | @tsv'
  )"
  if [[ -z $entries ]]; then
    echo "no caches for ${prefix}"
    return 0
  fi
  while IFS=$'\t' read -r id key created; do
    [[ -n $id ]] || continue
    if is_current_day "$key" "$created"; then
      have_today=1
    fi
  done <<< "$entries"
  while IFS=$'\t' read -r id key created; do
    [[ -n $id ]] || continue
    if is_current_day "$key" "$created"; then
      echo "keeping current-day ${key}"
      continue
    fi
    if [[ $mode == keep && $have_today -eq 0 && $kept_restore -eq 0 ]]; then
      echo "keeping newest restore ${key}"
      kept_restore=1
      continue
    fi
    if [[ $dry -eq 1 ]]; then
      echo "would delete ${key} (${id})"
      continue
    fi
    echo "deleting ${key} (${id})"
    # A 404 means another main run already pruned it.
    gh api -X DELETE "repos/${GITHUB_REPOSITORY}/actions/caches/${id}" --silent || true
    sleep 0.25
  done <<< "$entries"
}

if [[ ${#replace[@]} -gt 0 ]]; then
  for prefix in "${replace[@]}"; do
    prune_prefix "$prefix" replace
  done
else
  for prefix in "${sweep[@]}"; do
    prune_prefix "$prefix" keep
  done
fi
