#!/usr/bin/env bash
set -euo pipefail
desktop="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export PHUX_DESKTOP_ADDON="${PHUX_DESKTOP_ADDON:-$desktop/.cache/multiwindow-host/phux-desktop-native.darwin-arm64.node}"
fixture_home="$(mktemp -d "$desktop/.cache/multiwindow-home.XXXXXX")"
source_package="$desktop/toolchain/gpuix/packages/solid"
fixture="$(mktemp "$source_package/multiwindow.XXXXXX.tsx")"
trap 'rm -rf "$fixture_home"; rm -f "$fixture"' EXIT
cp "$desktop/tests/native/multiwindow.tsx" "$fixture"
export MULTIWINDOW_ARTIFACTS="$desktop/.cache"
export HOME="$fixture_home"
export XDG_CONFIG_HOME="$fixture_home/config"
export XDG_STATE_HOME="$fixture_home/state"
export XDG_CACHE_HOME="$fixture_home/cache"
export XDG_DATA_HOME="$fixture_home/data"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME" "$XDG_DATA_HOME"
log="$desktop/.cache/multiwindow-native-receipt.log"
bun --no-install --preload "$desktop/tests/native/multiwindow-preload.ts" --preload "$source_package/dist/preload.js" "$fixture" | tee "$log"
# AppKit must return control to the host: an early native exit(0) is not a pass.
grep -q '^MULTIWINDOW_PASS' "$log"
