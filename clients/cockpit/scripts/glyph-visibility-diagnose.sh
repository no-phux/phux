#!/usr/bin/env bash
# Diagnose a report that terminal glyphs look thin without changing visual
# tokens or pretending a reference screenshot sees CoreText.
#
#   ./scripts/glyph-visibility-diagnose.sh
#   ./scripts/glyph-visibility-diagnose.sh --installed /Applications/Phux Cockpit.app \
#       --dev zig-out/dev/'Phux Cockpit (dev).app' --snapshot /tmp/snapshot.txt --pid 1234
#
# The installed/dev comparison proves a stale binary when the installed version
# differs from this source. A same-version digest difference proves only that the
# builds differ (for example, automation or optimization); it cannot name which
# source or build configuration produced either executable.
# The snapshot comparison is deliberately narrower. `gpu_present_path=pixels`
# proves the observed app is using the reference renderer; `packet` proves only
# that the host path is active. Neither a packet snapshot nor an automate
# screenshot proves what CoreText put on glass. Use the printed GPU-capture and
# host-raster commands for those two separate questions.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALLED="/Applications/Phux Cockpit.app"
DEV="${ROOT}/zig-out/dev/Phux Cockpit (dev).app"
SNAPSHOT=""
EXPECTED_PID=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --installed) [[ $# -ge 2 ]] || { printf 'error: --installed needs a path\n' >&2; exit 2; }; INSTALLED="$2"; shift 2 ;;
        --dev) [[ $# -ge 2 ]] || { printf 'error: --dev needs a path\n' >&2; exit 2; }; DEV="$2"; shift 2 ;;
        --snapshot) [[ $# -ge 2 ]] || { printf 'error: --snapshot needs a path\n' >&2; exit 2; }; SNAPSHOT="$2"; shift 2 ;;
        --pid) [[ $# -ge 2 ]] || { printf 'error: --pid needs a value\n' >&2; exit 2; }; EXPECTED_PID="$2"; shift 2 ;;
        -h|--help) sed -n '2,17p' "$0"; exit 0 ;;
        *) printf 'error: unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done
if [[ -n "$SNAPSHOT" && ! "$EXPECTED_PID" =~ ^[1-9][0-9]*$ ]]; then
    printf 'error: --snapshot requires --pid with the PID launched for that snapshot\n' >&2
    exit 2
fi

[[ "$(uname -s)" == "Darwin" ]] || { printf 'error: macOS only\n' >&2; exit 1; }

plist_value() {
    local bundle="$1" key="$2"
    [[ -f "${bundle}/Contents/Info.plist" ]] || return 1
    /usr/bin/plutil -extract "$key" raw -o - "${bundle}/Contents/Info.plist" 2>/dev/null
}

file_digest() {
    local path="$1"
    [[ -f "$path" ]] || { printf 'absent'; return 0; }
    /usr/bin/shasum -a 256 "$path" | /usr/bin/cut -d' ' -f1
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
            printf 'error: %s has an unsafe CFBundleExecutable=%s\n' "$bundle" "$executable" >&2
            return 1
            ;;
    esac
    bundle_real="$(CDPATH='' cd -- "$bundle" && pwd -P)" || return 1
    macos_real="$(CDPATH='' cd -- "${bundle_real}/Contents/MacOS" && pwd -P)" || return 1
    resolved="$(resolve_realpath "${macos_real}/${executable}" 0)" || return 1
    case "$resolved" in
        "${macos_real}"/*) ;;
        *)
            printf 'error: %s executable resolves outside Contents/MacOS: %s\n' \
                "$bundle" "$resolved" >&2
            return 1
            ;;
    esac
    [[ -f "$resolved" && -x "$resolved" ]] || return 1
    printf '%s' "$resolved"
}

bundle_report() {
    local label="$1" bundle="$2" version build identifier executable resolved digest
    printf '\n%s: %s\n' "$label" "$bundle"
    if [[ ! -d "$bundle" ]]; then
        printf '  status: absent (stale-binary comparison is incomplete)\n'
        return 0
    fi
    version="$(plist_value "$bundle" CFBundleShortVersionString || printf '<missing>')"
    build="$(plist_value "$bundle" CFBundleVersion || printf '<missing>')"
    identifier="$(plist_value "$bundle" CFBundleIdentifier || printf '<missing>')"
    executable="$(plist_value "$bundle" CFBundleExecutable || printf '<missing>')"
    resolved="$(bundle_executable_path "$bundle" || true)"
    if [[ -z "$resolved" ]]; then
        printf '  status: invalid executable path (refused)\n'
        return 0
    fi
    digest="$(file_digest "$resolved")"
    printf '  version:    %s (build %s)\n' "$version" "$build"
    printf '  bundle id:  %s\n' "$identifier"
    printf '  executable: %s\n' "$executable"
    printf '  resolved:   %s\n' "$resolved"
    printf '  sha256:     %s\n' "$digest"
}

for bundle in "$INSTALLED" "$DEV"; do
    [[ -d "$bundle" ]] || continue
    bundle_executable_path "$bundle" >/dev/null || exit 2
done

bundle_report installed "$INSTALLED"
bundle_report dev "$DEV"

SOURCE_VERSION="$(sed -n '1p' "${ROOT}/version.txt")"
SOURCE_COMMIT="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || printf '<unknown>')"
INSTALLED_VERSION="$(plist_value "$INSTALLED" CFBundleShortVersionString || printf '')"
DEV_VERSION="$(plist_value "$DEV" CFBundleShortVersionString || printf '')"
INSTALLED_ID="$(plist_value "$INSTALLED" CFBundleIdentifier || printf '')"
DEV_ID="$(plist_value "$DEV" CFBundleIdentifier || printf '')"
INSTALLED_EXECUTABLE="$(plist_value "$INSTALLED" CFBundleExecutable || printf '')"
DEV_EXECUTABLE="$(plist_value "$DEV" CFBundleExecutable || printf '')"
INSTALLED_PATH="$(bundle_executable_path "$INSTALLED" || true)"
DEV_PATH="$(bundle_executable_path "$DEV" || true)"
INSTALLED_DIGEST="$(file_digest "$INSTALLED_PATH")"
DEV_DIGEST="$(file_digest "$DEV_PATH")"

printf '\nBINARY IDENTITY\n'
if [[ -z "$INSTALLED_ID" || -z "$DEV_ID" ]]; then
    printf '  installed vs dev: UNKNOWN (bundle metadata is missing)\n'
elif [[ "$INSTALLED_ID" == "$DEV_ID" && "$INSTALLED_EXECUTABLE" == "$DEV_EXECUTABLE" ]]; then
    printf '  installed vs dev: CLASH (same bundle id and executable name)\n'
else
    printf '  installed vs dev: DISTINCT (bundle id and/or executable name differs)\n'
fi

printf '\nBINARY VERDICT\n'
printf '  source:     version=%s commit=%s\n' "$SOURCE_VERSION" "$SOURCE_COMMIT"
printf '  build provenance: not embedded in bundle; same-version digest differences cannot identify automation, target, or optimization flags\n'
if [[ -z "$INSTALLED_VERSION" || "$INSTALLED_DIGEST" == absent ]]; then
    printf '  stale binary: UNKNOWN (installed bundle metadata or executable is missing)\n'
elif [[ "$INSTALLED_VERSION" != "$SOURCE_VERSION" ]]; then
    printf '  stale binary: CONFIRMED (installed version differs from this source)\n'
elif [[ "$DEV_DIGEST" == absent || -z "$DEV_VERSION" ]]; then
    printf '  stale binary: UNKNOWN (dev executable metadata is missing; comparison is incomplete)\n'
elif [[ "$DEV_VERSION" != "$SOURCE_VERSION" ]]; then
    printf '  stale binary: UNKNOWN (dev bundle version differs from this source)\n'
elif [[ "$INSTALLED_DIGEST" != "$DEV_DIGEST" ]]; then
    printf '  stale binary: DIFFERENT BUILD (same version, executable digest differs; stale source/config unresolved)\n'
else
    printf '  stale binary: NOT SHOWN (installed version and executable digest match this dev bundle)\n'
fi

printf '\nDISPLAY / RASTER VERDICT\n'
if [[ -z "$SNAPSHOT" ]]; then
    printf '  snapshot:    absent\n'
    printf '  display raster: UNMEASURED (a snapshot or capture is required)\n'
elif [[ ! -f "$SNAPSHOT" ]]; then
    printf '  snapshot:    missing at %s\n' "$SNAPSHOT"
    printf '  display raster: UNMEASURED (the supplied snapshot does not exist)\n'
else
    header="$(sed -n '1p' "$SNAPSHOT")"
    canvas_line="$(grep -m1 '^[[:space:]]*view @w[0-9][^ ]*/phux-cockpit-canvas kind=gpu_surface ' "$SNAPSHOT" || true)"
    publisher="$(printf '%s\n' "$header" | sed -n 's/.*publisher_pid=\([^ ]*\).*/\1/p')"
    if [[ "$header" != ready=true* || -z "$canvas_line" ]]; then
        printf '  snapshot:    REFUSED (missing ready header or Cockpit GPU canvas)\n' >&2
        printf '  display raster: UNMEASURED (input is not a live Cockpit automation snapshot)\n' >&2
        exit 1
    fi
    if [[ "$publisher" != "$EXPECTED_PID" ]]; then
        printf '  snapshot:    REFUSED (publisher_pid=%s, expected=%s)\n' \
            "${publisher:-<missing>}" "$EXPECTED_PID" >&2
        printf '  display raster: UNMEASURED (snapshot belongs to another or unknown process)\n' >&2
        exit 1
    fi
    path="$(printf '%s\n' "$canvas_line" | sed -n 's/.*gpu_present_path=\([^ ]*\).*/\1/p')"
    fallback="$(printf '%s\n' "$canvas_line" | sed -n 's/.*present_fallback=\([^ ]*\).*/\1/p')"
    nonblank="$(printf '%s\n' "$canvas_line" | sed -n 's/.*gpu_nonblank=\([^ ]*\).*/\1/p')"
    if [[ "$path" == pixels && "$nonblank" == true ]]; then
        printf '  display raster: REFERENCE FALLBACK (CoreText did not rasterize this frame)\n'
    elif [[ "$path" == packet && "$fallback" == none && "$nonblank" == true ]]; then
        printf '  display raster: HOST PATH ACTIVE; CoreText output still needs a real-pixel capture\n'
    else
        printf '  display raster: UNKNOWN (snapshot is not a healthy nonblank present state)\n'
    fi
fi

sdk_url=""
sdk_status=0
# shellcheck source=scripts/lib/zon.sh
. "${ROOT}/scripts/lib/zon.sh"
sdk_url="$(zon_dependency_url "${ROOT}/build.zig.zon" native_sdk)" || sdk_status=$?
sdk_sha=""
if [[ "$sdk_status" == 0 ]]; then
    sdk_sha="$(printf '%s' "$sdk_url" | sed -n 's#.*/archive/\([0-9a-f][0-9a-f]*\)\.tar\.gz$#\1#p')"
fi

printf '\nPROPOSED HOST COMPARISON\n'
printf '  SDK pin: %s\n' "${sdk_sha:-<unpublished/local override>}"
printf '  headless (real CoreText, two shipped SDK states):\n'
if [[ -n "$sdk_sha" ]]; then
    printf '    %s/scripts/host-raster-compare.sh e8bd84886 %s\n' "$ROOT" "$sdk_sha"
else
    printf '    unavailable: build.zig.zon has no published native_sdk pin\n'
fi
printf '  live (real app frame, patched SDK composite capture):\n'
printf '    %s/scripts/capture-gpu-ink.sh --bundle "%s"\n' "$ROOT" "$DEV"
printf '  Refuse any capture whose snapshot says gpu_present_path=pixels; that is the\n'
printf '  CPU reference renderer, not the host rasterizer.\n'
