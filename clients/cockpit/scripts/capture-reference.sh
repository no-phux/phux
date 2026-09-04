#!/usr/bin/env bash
# Capture a public video post as local, gitignored product-reference evidence.
#
#   ./scripts/capture-reference.sh <url> [label]
#
# Set REFERENCE_COOKIES_FROM_BROWSER to a yt-dlp browser name (for example
# "safari" or "chrome") when a post requires an authenticated X session.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
REFERENCE_ROOT="${REFERENCE_ROOT:-${ROOT}/.references}"

usage() {
    sed -n '2,7p' "$0"
}

if [[ $# -eq 1 && ( "$1" == "-h" || "$1" == "--help" ) ]]; then
    usage
    exit 0
fi

if [[ $# -lt 1 || $# -gt 2 ]]; then
    usage
    exit 2
fi

URL="$1"
LABEL="${2:-}"

if command -v yt-dlp >/dev/null 2>&1; then
    YTDLP=(yt-dlp)
elif command -v uvx >/dev/null 2>&1; then
    # uvx keeps this workflow usable without adding a project dependency.
    YTDLP=(uvx --from yt-dlp yt-dlp)
else
    printf 'error: yt-dlp is required (install it with: brew install yt-dlp)\n' >&2
    exit 1
fi

command -v jq >/dev/null 2>&1 || {
    printf 'error: jq is required (install it with: brew install jq)\n' >&2
    exit 1
}

COOKIE_ARGS=()
if [[ -n "${REFERENCE_COOKIES_FROM_BROWSER:-}" ]]; then
    COOKIE_ARGS=(--cookies-from-browser "${REFERENCE_COOKIES_FROM_BROWSER}")
fi

mkdir -p "$REFERENCE_ROOT"
metadata_tmp="$(mktemp "${REFERENCE_ROOT}/metadata.XXXXXX")"
cleanup() {
    rm -f "$metadata_tmp"
}
trap cleanup EXIT

printf 'Inspecting %s\n' "$URL"
"${YTDLP[@]}" "${COOKIE_ARGS[@]}" --no-playlist --dump-single-json -- "$URL" \
    > "$metadata_tmp"

id="$(jq -r '.id // "unknown"' "$metadata_tmp")"
extractor="$(jq -r '.extractor_key // .extractor // "reference"' "$metadata_tmp")"
if [[ -z "$LABEL" ]]; then
    LABEL="${extractor}-${id}"
fi
LABEL="$(printf '%s' "$LABEL" | tr '[:upper:]' '[:lower:]' | \
    sed -E 's/[^a-z0-9._-]+/-/g; s/^-+//; s/-+$//')"
LABEL="${LABEL:0:80}"
case "$LABEL" in
    ""|"."|"..") LABEL="reference-unknown" ;;
esac

DEST="${REFERENCE_ROOT}/${LABEL}"
if [[ -e "$DEST" ]]; then
    existing_url="$(cat "${DEST}/source.url" 2>/dev/null || true)"
    if [[ -n "$existing_url" && "$existing_url" != "$URL" ]]; then
        printf 'error: label %s already belongs to %s\n' "$LABEL" "$existing_url" >&2
        exit 1
    fi
    printf 'Resuming existing capture at %s\n' "$DEST"
fi

mkdir -p "${DEST}/media" "${DEST}/frames"
mv "$metadata_tmp" "${DEST}/metadata.json"
printf '%s\n' "$URL" > "${DEST}/source.url"
if [[ ! -e "${DEST}/notes.md" ]]; then
    cat > "${DEST}/notes.md" <<EOF
# ${LABEL}

Source: ${URL}

## Observed Behavior

## Interaction Details

## Cockpit Adaptation
EOF
fi

printf 'Downloading media to %s\n' "$DEST"
"${YTDLP[@]}" "${COOKIE_ARGS[@]}" \
    --no-playlist \
    --write-info-json \
    --write-description \
    --write-thumbnail \
    --write-subs \
    --write-auto-subs \
    --sub-langs 'en.*' \
    --merge-output-format mp4 \
    --output "${DEST}/media/%(title).160B [%(id)s].%(ext)s" \
    -- "$URL"

video="$(find "${DEST}/media" -type f \( \
    -iname '*.mp4' -o -iname '*.mov' -o -iname '*.webm' -o -iname '*.mkv' \
    \) -print -quit)"

if [[ -n "$video" ]] && command -v ffmpeg >/dev/null 2>&1 && \
    command -v ffprobe >/dev/null 2>&1; then
    duration="$(ffprobe -v error -show_entries format=duration \
        -of default=noprint_wrappers=1:nokey=1 "$video" 2>/dev/null || true)"
    interval="$(awk -v duration="${duration:-0}" \
        'BEGIN { interval = duration / 60; if (interval < 1) interval = 1; printf "%.3f", interval }')"
    printf 'Extracting review frames every %ss\n' "$interval"
    find "${DEST}/frames" -type f -name '*.jpg' -delete
    ffmpeg -hide_banner -loglevel error -y -i "$video" \
        -vf "fps=1/${interval},scale='min(1600,iw)':-2" -q:v 2 \
        "${DEST}/frames/%04d.jpg"
fi

printf '\nCaptured reference: %s\n' "$DEST"
printf 'Review metadata.json, media/, frames/, and notes.md.\n'
