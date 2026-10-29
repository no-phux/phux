# shellcheck shell=sh
set -eu

exec "${PHUX_BIN:-phux}" agent usage "$@"
