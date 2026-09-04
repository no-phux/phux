#!/usr/bin/env bash
# Did an SDK change actually move the glyph pixels? Two numbers, one basis.
#
#   ./scripts/host-raster-compare.sh <ref-before> [ref-after]
# measures: host raster pixels at two SDK commits that both shipped
#
# `ref-before` and `ref-after` are commits of the pinned SDK fork. `ref-after`
# defaults to the commit build.zig.zon pins, so the common call is
#
#   ./scripts/host-raster-compare.sh e8bd84886      # shipped-then vs pinned-now
#
# To separate an old installed app from a current dev app before comparing
# raster output, use `scripts/glyph-visibility-diagnose.sh`.
#
# WHY THIS EXISTS
# ---------------
# scripts/host-raster-check.sh measures ONE rasterizer. That is enough to catch
# a regression against a pinned floor, and not enough to answer the question
# that actually matters when a render fix is proposed: does the shipping build
# change?
#
# phux-cockpit-aht is the worked example, and it cost four rounds. The
# historical diagnosis said the SDK was missing `CGContextSetShouldSmoothFonts`.
# Someone added it, measured the fixed SDK against a copy with the calls set
# explicitly to `false`, saw solid 4179 vs 3081 - a real 26.3% - and shipped it
# as the faint-text fix.
#
# But no build was ever in the `false` state. The state before the fix was the
# calls being ABSENT, and absent is not false: on this CGBitmapContext the
# default is smoothing ENABLED. Measured against the commit the shipping app
# was actually built from, the fix moves nothing:
#
#   e8bd84886 (no calls, shipped)   cell_grid solid=4179  mean_luma=45.7978
#   f3678832f (calls = true, pin)   cell_grid solid=4179  mean_luma=45.7978
#   f3678832f with calls = false    cell_grid solid=3081  mean_luma=37.1124
#
# The control was synthetic. A synthetic control can only tell you the knob is
# wired up; it cannot tell you the knob was ever in the other position. This
# script compares two REAL commits, so the before side is a state that shipped.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
PIN_CACHE="${PHUX_COCKPIT_SDK_CACHE:-${ROOT}/.zig-cache/pinned-sdk}"

if [[ $# -lt 1 || $# -gt 2 ]]; then
    sed -n '2,10p' "$0"
    exit 2
fi

if [[ ! -d "${PIN_CACHE}/.git" ]]; then
    printf 'error: no pinned SDK checkout at %s\n' "$PIN_CACHE" >&2
    printf '       run ./scripts/build-automation-cli.sh first\n' >&2
    exit 1
fi

# shellcheck source=scripts/lib/measure.sh
source "${ROOT}/scripts/lib/measure.sh"
# Block-scoped read: the old one-liner answered a local `.path` override with
# GHOSTTY's url, which would have made this compare two ghostty commits while
# reporting them as SDK pins. See phux-cockpit-yo5.
read_status=0
pinned_url="$(zon_dependency_url "${ROOT}/build.zig.zon" native_sdk)" || read_status=$?
if [[ "${read_status}" -ne 0 ]]; then
    printf 'error: .native_sdk has no published pin to compare against (local override?).\n' >&2
    printf 'Pass both commits explicitly, or restore a published pin.\n' >&2
    exit 1
fi
pinned_sha="$(printf '%s' "${pinned_url}" | sed -E 's#.*/archive/([0-9a-f]+)\.tar\.gz.*#\1#')"

BEFORE="$1"
AFTER="${2:-$pinned_sha}"

WORK="$(mktemp -d)"
resolve_ref() {
    local ref="$1"
    git -C "$PIN_CACHE" cat-file -e "${ref}^{commit}" 2>/dev/null || \
        git -C "$PIN_CACHE" fetch --quiet origin "$ref"
    git -C "$PIN_CACHE" rev-parse "${ref}^{commit}"
}
BEFORE_SHA="$(resolve_ref "$BEFORE")"
AFTER_SHA="$(resolve_ref "$AFTER")"
BEFORE_SOURCE="$(measure_raster_worktree_source "$WORK" before "$BEFORE_SHA")"
AFTER_SOURCE="$(measure_raster_worktree_source "$WORK" after "$AFTER_SHA")"
cleanup() {
    for source in "$BEFORE_SOURCE" "$AFTER_SOURCE"; do
        git -C "$PIN_CACHE" worktree remove --force "$source" 2>/dev/null || true
    done
    rm -rf "$WORK"
}
trap cleanup EXIT

measure_raster_comparison_basis "$BEFORE_SHA" "$AFTER_SHA" \
    "$BEFORE_SOURCE" "$AFTER_SOURCE" \
    "./scripts/host-raster-compare.sh ${BEFORE} ${AFTER}"

measure() {
    local ref="$1" source="$2" label="$3"
    git -C "$PIN_CACHE" worktree add --quiet --detach "$source" "$ref"
    printf '\n--- %s (%s) ---\n' "$label" "${ref:0:9}"
    PHUX_COCKPIT_SDK_SRC="$source" "${ROOT}/scripts/host-raster-check.sh" | grep -v '^sdk'
}

measure "$BEFORE_SHA" "$BEFORE_SOURCE" before
measure "$AFTER_SHA" "$AFTER_SOURCE" after

printf '\nRead the two cell_grid lines against each other. Equal numbers mean the\n'
printf 'change did not move a single glyph pixel, whatever its diff says.\n'
