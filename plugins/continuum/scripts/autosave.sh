# shellcheck shell=sh
set -eu

phux_bin=${PHUX_BIN:-phux}
profile=${PHUX_WORKSPACE_PROFILE:-default}
root=${PHUX_PLUGIN_ROOT:-$(CDPATH=; cd -- "$(dirname -- "$0")/.." && pwd)}
state_dir=${PHUX_CONTINUUM_DIR:-"$root/state"}
archive=${PHUX_CONTINUUM_ARCHIVE:-"$state_dir/$profile.json"}
tmp=$archive.tmp

mkdir -p "$(dirname -- "$archive")"
if [ -n "${PHUX_SOCKET:-}" ]; then
    "$phux_bin" archive export --socket "$PHUX_SOCKET" --output "$tmp"
else
    "$phux_bin" archive export --output "$tmp"
fi
mv -f "$tmp" "$archive"
printf '{"schema_version":1,"profile":"%s","archive":"%s"}\n' "$profile" "$archive"
