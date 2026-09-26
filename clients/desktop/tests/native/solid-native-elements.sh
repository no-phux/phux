#!/usr/bin/env bash
set -euo pipefail
desktop="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export PHUX_DESKTOP_ADDON="${PHUX_DESKTOP_ADDON:-$desktop/.cache/solid-elements-host/phux-desktop-native.darwin-arm64.node}"
source_package="$desktop/toolchain/gpuix/packages/solid"
fixture_home="$(mktemp -d "$desktop/.cache/solid-elements-home.XXXXXX")"
fixture="$(mktemp "$source_package/solid-elements.XXXXXX.tsx")"
trap 'rm -rf "$fixture_home"; rm -f "$fixture"' EXIT
cp "$desktop/tests/native/solid-native-elements.tsx" "$fixture"
export SOLID_ELEMENTS_ARTIFACTS="$desktop/.cache"
# Check the application's JSX module augmentation against the actual built types.
"$source_package/node_modules/.bin/tsc" --noEmit --skipLibCheck --target esnext --module esnext --moduleResolution bundler --jsx preserve --jsxImportSource @gpuix/solid --typeRoots "$desktop/toolchain/gpuix/packages/native/node_modules/@types" --types node "$fixture"
export HOME="$fixture_home"
export XDG_CONFIG_HOME="$fixture_home/config"
export XDG_STATE_HOME="$fixture_home/state"
export XDG_CACHE_HOME="$fixture_home/cache"
export XDG_DATA_HOME="$fixture_home/data"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME" "$XDG_DATA_HOME"
log="$desktop/.cache/solid-native-elements-receipt.log"
bun --no-install --preload "$desktop/tests/native/solid-native-elements.mjs" --preload "$source_package/dist/preload.js" "$fixture" | tee "$log"
grep -q '^SOLID_NATIVE_ELEMENTS_PASS' "$log"
