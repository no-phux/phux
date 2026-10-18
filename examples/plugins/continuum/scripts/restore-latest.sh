# shellcheck shell=sh
set -eu

phux_bin=${PHUX_BIN:-phux}
profile=${PHUX_WORKSPACE_PROFILE:-default}
root=${PHUX_PLUGIN_ROOT:-$(CDPATH=; cd -- "$(dirname -- "$0")/.." && pwd)}
state_dir=${PHUX_CONTINUUM_DIR:-"$root/state"}
archive=${PHUX_CONTINUUM_ARCHIVE:-"$state_dir/$profile.json"}

if [ ! -f "$archive" ]; then
    printf 'phux-continuum: no saved workspace archive at %s\n' "$archive" >&2
    exit 66
fi

if [ -n "${PHUX_SOCKET:-}" ]; then
    "$phux_bin" archive import "$archive" --socket "$PHUX_SOCKET"
else
    "$phux_bin" archive import "$archive"
fi
