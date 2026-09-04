#!/usr/bin/env bash
# Package the staged TypeScript composition root without patching the Native
# SDK. The pinned SDK's generated package step currently resolves app.zon from
# the build root even when AppOptions.app_root is "typescript-spike"; invoking
# the same pinned CLI with the candidate manifest is the narrow local bridge
# until the shipping files move to the root.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
AUTOMATION=0
PHUX=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --automation) AUTOMATION=1 ;;
        --phux) PHUX=1 ;;
        -h|--help) sed -n '2,8p' "$0"; exit 0 ;;
        *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
    shift
done

build_args=(-Dtypescript-spike=true)
[[ "$AUTOMATION" == 1 ]] && build_args+=(-Dautomation=true)
[[ "$PHUX" == 1 ]] && build_args+=(-Dphux-enabled=true)

( cd "$ROOT" && ./scripts/zig-build.sh "${build_args[@]}" )

if [[ -z "${NATIVE:-}" ]]; then
    NATIVE="$("${ROOT}/scripts/build-automation-cli.sh")"
fi
SDK_ROOT="$(CDPATH='' cd -- "$(dirname -- "$NATIVE")/../.." && pwd)"
OUTPUT="${ROOT}/zig-out/package/phux-cockpit-typescript-spike.app"
BINARY="${ROOT}/zig-out/bin/phux-cockpit-typescript-spike"

[[ -x "$NATIVE" ]] || { printf 'native CLI is not executable: %s\n' "$NATIVE" >&2; exit 1; }
[[ -x "$BINARY" ]] || { printf 'TypeScript candidate binary is missing: %s\n' "$BINARY" >&2; exit 1; }

env NATIVE_SDK_PATH="$SDK_ROOT" "$NATIVE" package \
    --target macos \
    --manifest "${ROOT}/typescript-spike/app.zon" \
    --output "$OUTPUT" \
    --binary "$BINARY" \
    --optimize ReleaseFast \
    --web-layer exclude

printf '%s\n' "$OUTPUT"
