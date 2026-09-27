#!/usr/bin/env bash
# Does the minimum-contrast floor change what reaches the glass, and by how
# much?
#
#   ./scripts/contrast-floor-check.sh
#
# Half one greps the `CONTRAST-FLOOR` lines of the measured test in
# src/tests/minimum_contrast_tests.zig (the colour this build projects per
# floor); half two inks those colours through the pinned SDK's real CoreText
# host path (scripts/measure-cell-contrast.m). Both come from shipped code.
# Read `distinct` and `peak_delta`: absolute `solid`/`lit` thresholds cannot
# tell dark ink on a dark ground from blank space. See docs/RENDER_FIDELITY.md.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
PIN_CACHE="${PHUX_COCKPIT_SDK_CACHE:-${ROOT}/.zig-cache/pinned-sdk}"

# The SDK source must be the one the app is PINNED to, or the numbers describe
# a rasterizer the app does not ship. Same resolution rule as
# scripts/host-raster-check.sh.
SDK_SRC="${PHUX_COCKPIT_SDK_SRC:-}"
if [[ -z "$SDK_SRC" ]]; then
    if [[ ! -d "${PIN_CACHE}/.git" ]]; then
        printf 'error: no pinned SDK checkout at %s\n' "$PIN_CACHE" >&2
        printf '       run ./scripts/build-automation-cli.sh first, or set PHUX_COCKPIT_SDK_SRC\n' >&2
        exit 1
    fi
    url="$(awk '/\.native_sdk = \.\{/ { found = 1 } found && /\.url = / { print; exit }' "${ROOT}/build.zig.zon" \
        | sed -E 's/.*"(.*)".*/\1/')"
    sha="$(printf '%s' "${url}" | sed -E 's#.*/archive/([0-9a-f]+)\.tar\.gz$#\1#')"
    have="$(git -C "$PIN_CACHE" rev-parse HEAD)"
    if [[ "$have" != "$sha" ]]; then
        printf 'error: %s is at %s, but build.zig.zon pins %s\n' "$PIN_CACHE" "${have:0:9}" "${sha:0:9}" >&2
        printf '       run ./scripts/build-automation-cli.sh to re-checkout the pin\n' >&2
        exit 1
    fi
    SDK_SRC="$PIN_CACHE"
    printf 'sdk: %s at %s (pinned)\n' "$PIN_CACHE" "${have:0:9}"
else
    printf 'sdk: %s (PHUX_COCKPIT_SDK_SRC override - NOT the pin)\n' "$SDK_SRC"
fi

HOST_M="${SDK_SRC}/src/platform/macos/appkit_host.m"
if [[ ! -f "$HOST_M" ]]; then
    printf 'error: no appkit_host.m at %s\n' "$HOST_M" >&2
    exit 1
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

printf 'asking the build what it projects...\n'
( cd "$ROOT" && zig build test -Dmeasure=true ) >"${WORK}/measure.log" 2>&1 || {
    printf 'error: zig build test -Dmeasure=true failed; see below\n' >&2
    tail -40 "${WORK}/measure.log" >&2
    exit 1
}
grep -o 'CONTRAST-FLOOR .*' "${WORK}/measure.log" | sort -u >"${WORK}/projected" || true
if [[ ! -s "${WORK}/projected" ]]; then
    printf 'error: the MEASURED test printed no CONTRAST-FLOOR lines.\n' >&2
    printf '       Without them this script would have nothing but colours somebody typed.\n' >&2
    exit 1
fi
cat "${WORK}/projected"

BIN="${WORK}/measure-cell-contrast"
# The same flags build/app.zig compiles this translation unit with, so the
# harness measures the code as the app builds it.
clang -w -fobjc-arc -fno-sanitize=builtin -ObjC -mmacosx-version-min=11.0 \
    -DNATIVE_SDK_APPKIT_HOST="\"${HOST_M}\"" \
    -o "$BIN" "${ROOT}/scripts/measure-cell-contrast.m" \
    -framework Foundation -framework AppKit -framework Metal \
    -framework QuartzCore -framework CoreText -framework CoreGraphics \
    -framework ImageIO -framework AVFoundation \
    -framework UniformTypeIdentifiers -framework WebKit -framework Security \
    -framework ScreenCaptureKit -framework CoreMedia -framework CoreVideo \
    -framework IOKit -framework Carbon -framework Accelerate \
    -framework MediaToolbox

specs=()
while read -r line; do
    floor="$(printf '%s' "$line" | sed -E 's/.*floor=([^ ]+).*/\1/')"
    label="$(printf '%s' "$line" | sed -E 's/.*label=([^ ]+).*/\1/')"
    fg="$(printf '%s' "$line" | sed -E 's/.*fg=([^ ]+).*/\1/')"
    bg="$(printf '%s' "$line" | sed -E 's/.*bg=([^ ]+).*/\1/')"
    specs+=("floor${floor}-${label}:${fg}:${bg}")
done <"${WORK}/projected"

printf '\ninking each of those through the host rasterizer...\n'
"$BIN" "${ROOT}/src/fonts/JetBrainsMonoNLNerdFontMono-Regular.ttf" "${specs[@]}"
