#!/usr/bin/env bash
# Capture the app's REAL composited pixels as PNG, with no Screen Recording
# permission, and measure them beside the reference screenshot of the same
# running instance.
#
#   ./scripts/capture-gpu-ink.sh                       # capture and report
#   ./scripts/capture-gpu-ink.sh --bundle <app>        # a bundle built elsewhere
#   ./scripts/capture-gpu-ink.sh --seconds 45          # hold it longer
#   ./scripts/capture-gpu-ink.sh --out <dir>           # keep the artifacts here
#
# WHAT THIS IS
# ------------
# `NATIVE_SDK_GPU_SHOT_DIR=<dir>` makes the AppKit host read the composited
# canvas texture back with `getBytes` and write it as PNG
# (`dumpCompositeShotWithPixelWidth:` in appkit_host.m). It needs no TCC, no
# focus and no visible window, which is the whole reason it matters: TCC is
# unavailable in CI, and every other real-pixel path on macOS is TCC-gated.
# See docs/RENDER_FIDELITY.md section 2 for the paths that were ruled out.
#
# WHY IT MAY REFUSE TO RUN
# ------------------------
# The dump only fires from the GPU composite pass, and at the pinned SDK that
# pass refuses any command kind outside its known-kind list -- a list without
# `cell_grid`. A Cockpit terminal frame is nothing but `cell_grid`, so every
# packet present is refused, the runtime falls back to its own CPU reference
# renderer, and the dump never runs: composite mode does not capture the real
# rasterizer, it REPLACES it. This script asserts `gpu_present_path=packet`
# and stops when it sees `pixels`, because a capture taken in that state is a
# picture of the reference renderer wearing the GPU path's name.
#
# docs/sdk-patches/composite-cell-grid.patch is the SDK-side fix. Apply it to a
# sandbox copy of the pin and build against that; docs/sdk-patches/README.md
# has the recipe.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
BUNDLE="${ROOT}/zig-out/package/phux-cockpit.app"
SECONDS_TO_HOLD=30
OUT=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --bundle) BUNDLE="$2"; shift 2 ;;
        --seconds) SECONDS_TO_HOLD="$2"; shift 2 ;;
        --out) OUT="$2"; shift 2 ;;
        -h|--help) sed -n '2,32p' "$0"; exit 0 ;;
        *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done
[[ -n "$OUT" ]] || OUT="$(mktemp -d)/capture"
# A shot directory that already holds PNGs would let a PREVIOUS run's frame be
# counted here and, worse, be selected as "the newest shot" — a capture of a
# binary that is not the one under test. Refuse rather than delete: this
# directory is whatever the caller named.
if [[ -d "${OUT}/shots" ]] && [[ -n "$(find "${OUT}/shots" -type f -name '*.png' -print -quit)" ]]; then
    printf 'error: %s already holds PNGs from an earlier run.\n' "${OUT}/shots" >&2
    printf '       Point --out somewhere fresh, or empty that directory yourself.\n' >&2
    exit 2
fi
mkdir -p "${OUT}/shots"

plist_value() {
    /usr/bin/plutil -extract "$2" raw -o - "$1/Contents/Info.plist" 2>/dev/null
}

resolve_realpath() {
    local path="$1" depth="$2" dir target
    (( depth < 32 )) || return 1
    if [[ -L "$path" ]]; then
        target="$(readlink "$path")" || return 1
        dir="$(CDPATH='' cd -- "$(dirname "$path")" && pwd -P)" || return 1
        [[ "$target" == /* ]] || target="${dir}/${target}"
        resolve_realpath "$target" "$((depth + 1))"
        return
    fi
    [[ -e "$path" ]] || return 1
    dir="$(CDPATH='' cd -- "$(dirname "$path")" && pwd -P)" || return 1
    printf '%s/%s' "$dir" "$(basename "$path")"
}

bundle_executable_path() {
    local bundle="$1" executable bundle_real macos_real resolved
    executable="$(plist_value "$bundle" CFBundleExecutable)" || return 1
    case "$executable" in
        ""|.|..|*/*)
            printf 'error: unsafe CFBundleExecutable=%s\n' "$executable" >&2
            return 1
            ;;
    esac
    bundle_real="$(CDPATH='' cd -- "$bundle" && pwd -P)" || return 1
    macos_real="$(CDPATH='' cd -- "${bundle_real}/Contents/MacOS" && pwd -P)" || return 1
    resolved="$(resolve_realpath "${macos_real}/${executable}" 0)" || return 1
    case "$resolved" in
        "${macos_real}"/*) ;;
        *)
            printf 'error: executable resolves outside Contents/MacOS: %s\n' "$resolved" >&2
            return 1
            ;;
    esac
    [[ -f "$resolved" && -x "$resolved" ]] || return 1
    printf '%s' "$resolved"
}

APP_EXECUTABLE="$(plist_value "$BUNDLE" CFBundleExecutable || true)"
APP_PATH="$(bundle_executable_path "$BUNDLE" || true)"
if [[ -z "$APP_PATH" ]]; then
    printf 'error: no safe executable for bundle %s (CFBundleExecutable=%s)\n' \
        "$BUNDLE" "${APP_EXECUTABLE:-<missing>}" >&2
    printf '       run: zig build package -Dautomation=true\n' >&2
    exit 2
fi
NATIVE="$("${ROOT}/scripts/build-automation-cli.sh")"

# The captured pane: a fixed block of bold and regular runs, and ONE ticking
# line at the bottom.
#
# The tick is load-bearing. A composite present happens only when the packet
# content changes, so a settled pane presents essentially never and the dump
# has nothing to fire on; the tick keeps presents flowing. It is at the bottom
# so the measured crop can exclude it and still compare two runs pixel for
# pixel -- everything above it is byte-identical from one run to the next,
# which is the property that makes a difference mean something.
cat >"${OUT}/pane.sh" <<'PANE'
#!/bin/sh
printf '\033[?25l\033[2J\033[H'
n=0
while [ "$n" -lt 12 ]; do
    printf '\033[1mBOLD bold BOLD bold BOLD bold\033[0m  regular regular regular\n'
    n=$((n + 1))
done
i=0
while :; do
    printf '\033[20;1Htick %08d' "$i"
    i=$((i + 1))
done
PANE
chmod +x "${OUT}/pane.sh"
printf 'command = /bin/sh %s\nfont-size = 13\n' "${OUT}/pane.sh" >"${OUT}/config"

APP_PID=""
cleanup() { [[ -n "$APP_PID" ]] && kill "$APP_PID" 2>/dev/null || true; }
trap cleanup EXIT

cd "$ROOT"
NATIVE_SDK_GPU_COMPOSITE=1 \
NATIVE_SDK_GPU_SHOT_DIR="${OUT}/shots" \
NATIVE_SDK_GPU_SHOT_EVERY=1 \
PHUX_COCKPIT_CONFIG="${OUT}/config" \
PHUX_COCKPIT_STATE="${OUT}/workspace.state" \
    "$APP_PATH" >"${OUT}/app.log" 2>&1 &
APP_PID=$!
printf 'launched pid=%s out=%s\n' "$APP_PID" "$OUT"

"$NATIVE" automate wait >/dev/null
sleep "$SECONDS_TO_HOLD"
"$NATIVE" automate snapshot >"${OUT}/snapshot.txt"

# phux-cockpit-2ml.10: app activation is global and name-based, so a snapshot
# published by SOMEBODY ELSE'S instance is indistinguishable from ours except
# by this. Live-app automation is serial-only; this is the check that says so.
pub="$(grep -o 'publisher_pid=[0-9]*' "${OUT}/snapshot.txt" | head -1 | cut -d= -f2 || true)"
if [[ "$pub" != "$APP_PID" ]]; then
    printf 'REFUSING TO REPORT: publisher_pid=%s but we launched %s.\n' "${pub:-none}" "$APP_PID" >&2
    printf 'Another instance is publishing; these numbers would describe it.\n' >&2
    exit 1
fi

path="$(grep -o 'gpu_present_path=[a-z_]*' "${OUT}/snapshot.txt" | head -1 | cut -d= -f2 || true)"
fallback="$(grep -o 'present_fallback=[a-z_]*' "${OUT}/snapshot.txt" | head -1 | cut -d= -f2 || true)"
printf 'gpu_present_path=%s present_fallback=%s\n' "${path:-none}" "${fallback:-none}"
if [[ "$path" != "packet" ]]; then
    printf '\nREFUSING TO CAPTURE: the host is not rasterizing this frame.\n' >&2
    printf 'gpu_present_path=%s means every packet present was refused and the\n' "${path:-none}" >&2
    printf 'engine fell back to its own CPU reference renderer. Whatever the\n' >&2
    printf 'composite pass would dump is that renderer, not CoreText.\n' >&2
    printf 'Fix: docs/sdk-patches/composite-cell-grid.patch (see that README).\n' >&2
    exit 1
fi

shots="$(find "${OUT}/shots" -type f -name '*.png' | wc -l | tr -d ' ')"
printf 'shots=%s\n' "$shots"
if [[ "$shots" -lt 2 ]]; then
    # Shot 1 is the first composite present, which lands before any pty output
    # has been drawn. A capture that only has it captured an empty terminal.
    printf 'REFUSING TO REPORT: %s shot(s). Shot 1 is the empty first frame.\n' "$shots" >&2
    exit 1
fi
# Newest by MTIME, not by name: the files are numbered by present count, and
# sorted as text `-p10` lands before `-p3`. `/usr/bin/stat` by absolute path
# because GNU coreutils on PATH shadows it and spells this `-c`, not `-f`;
# this script is macOS-only anyway (it launches a .app and links Cocoa).
last="$(find "${OUT}/shots" -type f -name '*.png' -exec /usr/bin/stat -f '%m %N' {} + | sort -rn | head -1 | cut -d' ' -f2-)"
cp "$last" "${OUT}/gpu.png"

rm -f "${ROOT}/.zig-cache/native-sdk-automation/screenshot-phux-cockpit-canvas.png"
if "$NATIVE" automate screenshot phux-cockpit-canvas >/dev/null 2>&1; then
    mv "${ROOT}/.zig-cache/native-sdk-automation/screenshot-phux-cockpit-canvas.png" "${OUT}/reference.png"
fi

# The crop: the fixed block, with the ticking line and the chrome excluded.
# Derived by eye from a capture at this font size and window size and then
# checked by measurement, not guessed: two runs of the same build diff to
# ZERO pixels over this rect (scripts/diff-png-region.m), which is the only
# thing that makes it the right rect. Re-derive it if the pane, the font size
# or the window size changes.
CROP=(0 60 470 240)

BIN="$(mktemp -d)"
trap 'rm -rf "$BIN"; cleanup' EXIT
clang -w -o "${BIN}/measure-png-ink" "${ROOT}/scripts/measure-png-ink.m" \
    -framework Foundation -framework CoreGraphics -framework ImageIO

printf '\n--- ink over rect %s ---\n' "${CROP[*]}"
"${BIN}/measure-png-ink" "${OUT}/gpu.png" "${CROP[@]}"
[[ -f "${OUT}/reference.png" ]] && "${BIN}/measure-png-ink" "${OUT}/reference.png" "${CROP[@]}"
printf '\nNOTE: do not read across those two rows. They are different renderers\n'
printf 'with different outlines and different hinting; the gap between them is\n'
printf 'legitimate. Compare a renderer against ITSELF across two builds.\n'
printf '\ngpu=%s\nreference=%s\n' "${OUT}/gpu.png" "${OUT}/reference.png"
