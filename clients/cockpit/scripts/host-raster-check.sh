#!/usr/bin/env bash
# Rasterizer-fidelity check: does the SDK host still ink glyphs as thickly as
# it did when the baseline was pinned?
#
#   ./scripts/host-raster-check.sh                 # measure and report
#   ./scripts/host-raster-check.sh --min-solid N   # ...and fail below N
#   ./scripts/host-raster-check.sh --png-prefix P  # keep the rasters
#   PHUX_COCKPIT_SDK_SRC=<dir> ./scripts/host-raster-check.sh   # a different SDK
# measures: pinned SDK host/CoreText glyph rasterization
#
# WHAT THIS IS FOR
# ----------------
# `native automate screenshot` is rendered by the SDK's deterministic CPU
# reference renderer, which fills glyph outlines with a bundled std-only
# TrueType rasterizer and never calls CoreText. A defect in the REAL macOS
# rasterizer therefore cannot appear in a reference screenshot.
#
# phux-cockpit-aht (the report that all terminal text looked ~35% too thin) is
# the worked example. The original smoothing diagnosis was wrong: the calls
# were absent in the shipped "before" commit, and that CGBitmapContext already
# defaulted to smoothing enabled. This check measures the host path directly;
# it does not credit smoothing for a change that moved zero pixels.
#
# scripts/measure-host-raster.m closes that gap by #including the pinned SDK's
# own appkit_host.m and rasterizing a terminal row through the host's real
# raster builder. No window, no focus, no Screen Recording permission, so it
# runs in CI where screen capture cannot.
#
# WHAT IT IS NOT
# --------------
# It is not a screenshot of the app. It proves the RASTERIZER, not the frame:
# it cannot see a layout mistake, a wrong colour chosen upstream, or a command
# that was never emitted. Reference screenshots remain the instrument for
# those. See docs/RENDER_FIDELITY.md for what each instrument can and cannot
# see, for the evidence behind that split, and for how the CI floor was derived.
#
# .github/workflows/ci.yml runs this on every push and pull request, and
# .github/workflows/sdk-head.yml runs it after repointing at the fork's branch
# head. Both use `--min-solid 4000` after
# `scripts/build-automation-cli.sh --checkout-only` materializes the pin this
# script insists on below.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/measure.sh
source "${ROOT}/scripts/lib/measure.sh"
PIN_CACHE="${PHUX_COCKPIT_SDK_CACHE:-${ROOT}/.zig-cache/pinned-sdk}"
ARGS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --min-solid|--png-prefix) ARGS+=("$1" "$2"); shift 2 ;;
        -h|--help) sed -n '2,32p' "$0"; exit 0 ;;
        *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done

# The SDK source must be the one the app is PINNED to, or the numbers describe
# a rasterizer the app does not ship. Resolve it the way
# scripts/build-automation-cli.sh does, and refuse a checkout at any other
# commit or with local source changes rather than measuring it quietly.
SDK_SRC="${PHUX_COCKPIT_SDK_SRC:-}"
SDK_MODE=pinned
if [[ -z "$SDK_SRC" ]]; then
    if [[ ! -d "${PIN_CACHE}/.git" ]]; then
        printf 'error: no pinned SDK checkout at %s\n' "$PIN_CACHE" >&2
        printf '       run ./scripts/build-automation-cli.sh first, or set PHUX_COCKPIT_SDK_SRC\n' >&2
        exit 1
    fi
    # Block-scoped read: the old one-liner answered a local `.path` override
    # with GHOSTTY's url, so this pin check compared against ghostty's sha.
    # See scripts/lib/zon.sh and phux-cockpit-yo5.
    read_status=0
    url="$(zon_dependency_url "${ROOT}/build.zig.zon" native_sdk)" || read_status=$?
    if [[ "${read_status}" -ne 0 ]]; then
        printf 'error: .native_sdk has no published pin to check against (local override?).\n' >&2
        exit 1
    fi
    sha="$(printf '%s' "${url}" | sed -E 's#.*/archive/([0-9a-f]+)\.tar\.gz$#\1#')"
    have="$(git -C "$PIN_CACHE" rev-parse HEAD)"
    if [[ "$have" != "$sha" ]]; then
        printf 'error: %s is at %s, but build.zig.zon pins %s\n' "$PIN_CACHE" "${have:0:9}" "${sha:0:9}" >&2
        printf '       run ./scripts/build-automation-cli.sh to re-checkout the pin\n' >&2
        exit 1
    fi
    dirty="$(git -C "$PIN_CACHE" status --porcelain --untracked-files=all)"
    if [[ -n "$dirty" ]]; then
        printf 'error: %s is at the pinned commit but has local source changes:\n' "$PIN_CACHE" >&2
        printf '%s\n' "$dirty" >&2
        printf '       restore a clean checkout before measuring it\n' >&2
        exit 1
    fi
    SDK_SRC="$PIN_CACHE"
    printf 'sdk: %s at %s (pinned)\n' "$PIN_CACHE" "${have:0:9}"
else
    SDK_MODE=override
    printf 'sdk: %s (PHUX_COCKPIT_SDK_SRC override - NOT the pin)\n' "$SDK_SRC"
fi

HOST_M="${SDK_SRC}/src/platform/macos/appkit_host.m"
if [[ ! -f "$HOST_M" ]]; then
    printf 'error: no appkit_host.m at %s\n' "$HOST_M" >&2
    exit 1
fi

BIN="$(mktemp -d)/measure-host-raster"
trap 'rm -rf "$(dirname "$BIN")"' EXIT

# The same flags build/app.zig compiles this translation unit with, so the
# harness measures the code as the app builds it.
clang -w -fobjc-arc -fno-sanitize=builtin -ObjC -mmacosx-version-min=11.0 \
    -DNATIVE_SDK_APPKIT_HOST="\"${HOST_M}\"" \
    -o "$BIN" "${ROOT}/scripts/measure-host-raster.m" \
    -framework Foundation -framework AppKit -framework Metal \
    -framework QuartzCore -framework CoreText -framework CoreGraphics \
    -framework ImageIO -framework AVFoundation \
    -framework UniformTypeIdentifiers -framework WebKit -framework Security \
    -framework ScreenCaptureKit -framework CoreMedia -framework CoreVideo \
    -framework IOKit -framework Carbon -framework Accelerate \
    -framework MediaToolbox

SDK_REF="$(git -C "$SDK_SRC" rev-parse HEAD 2>/dev/null || printf non-git)"
if [[ "$SDK_MODE" == override ]]; then
    DERIVE="PHUX_COCKPIT_SDK_SRC=${SDK_SRC} ./scripts/host-raster-check.sh"
else
    DERIVE='./scripts/host-raster-check.sh'
fi
measure_basis host_raster \
    "JetBrains Mono NL regular, fixed terminal row, host CoreText rasterizer; sdk_mode=${SDK_MODE}; sdk_source=${SDK_SRC}; sdk_ref=${SDK_REF}" \
    "$DERIVE"
"$BIN" "${ROOT}/src/fonts/JetBrainsMonoNLNerdFontMono-Regular.ttf" "${ARGS[@]+"${ARGS[@]}"}"
