#!/usr/bin/env bash
# Does the minimum-contrast floor change what reaches the GLASS, and by how
# much?
#
#   ./scripts/contrast-floor-check.sh
#
# TWO HALVES, NEITHER OF THEM AUTHORED HERE
# -----------------------------------------
# The first half asks THIS BUILD what colour it projects for each low-contrast
# SGR case, at each floor. That is a MEASURED test in
# src/tests/minimum_contrast_tests.zig which paints a real session through
# `grid.paint` and reads the resolved foreground off the display list; this
# script only greps its `CONTRAST-FLOOR` lines.
#
# The second half feeds those colours to scripts/measure-cell-contrast.m, which
# `#include`s the PINNED SDK's own appkit_host.m and inks a terminal row
# through the host's real CoreText rasterizer.
#
# So both columns come out of code that shipped. Flip `minimum-contrast` and
# the whole table moves on its own. This is the rule phux-cockpit-aht was
# merged and retracted for breaking: a "fix" was validated against a
# hand-edited SDK state no build was ever in, and against the commit that
# actually shipped it moved zero pixels.
#
# WHAT IT CAN AND CANNOT SEE
# --------------------------
# It proves the rasterizer's response to the colour the projection chose. It is
# NOT a screenshot of the app: it cannot see a layout mistake or a command that
# was never emitted. `zig build test` covers the projection-to-display-list
# wire (including the one through terminal_painter.zig); this covers
# display-list-to-pixels. See docs/RENDER_FIDELITY.md for why no single
# instrument covers both without the Screen Recording permission.
#
# READING THE NUMBERS
# -------------------
# `solid` and `lit` are absolute luminance thresholds and are BLIND to the
# defect: dark ink on a dark ground is ink that no absolute threshold counts,
# so unreadable text and blank space both score zero. `distinct` (pixels more
# than one 8-bit step from the row's own background) and `peak_delta` are the
# statistics that separate them, and they are what to read here.
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
