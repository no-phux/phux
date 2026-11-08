# shellcheck shell=sh
set -eu

if [ -n "${PHUX_SOCKET:-}" ]; then
    exec "${PHUX_BIN:-phux}" --socket "$PHUX_SOCKET" agent stats
fi
exec "${PHUX_BIN:-phux}" agent stats
