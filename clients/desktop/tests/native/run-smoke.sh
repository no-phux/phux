#!/usr/bin/env bash
set -euo pipefail

desktop="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export PHUX_DESKTOP_ADDON="${PHUX_DESKTOP_ADDON:-$desktop/.cache/host/phux-desktop-native.darwin-arm64.node}"
fixture_home="$(mktemp -d "$desktop/.cache/host-smoke.XXXXXX")"
trap 'rm -rf "$fixture_home"' EXIT
export HOME="$fixture_home"
export XDG_CONFIG_HOME="$fixture_home/config"
export XDG_STATE_HOME="$fixture_home/state"
export XDG_CACHE_HOME="$fixture_home/cache"
export XDG_DATA_HOME="$fixture_home/data"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME" "$XDG_DATA_HOME"

# Each script needs fresh process-wide native extension startup state.
bun "$desktop/tests/native/extension-smoke.ts"
bun "$desktop/tests/native/late-install-smoke.ts"
