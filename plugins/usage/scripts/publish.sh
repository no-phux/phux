# shellcheck shell=sh
# Collect this host's plan usage and write phux.usage/v1 on the server that
# fired the hook ($PHUX_SOCKET). `phux agent usage` spaces vendor requests and
# backs off on rate limits itself, so firing on every event is cheap.
set -eu

phux_bin=${PHUX_BIN:-phux}
if [ -n "${PHUX_SOCKET:-}" ]; then
    exec "$phux_bin" --socket "$PHUX_SOCKET" agent usage --publish "$@"
fi
exec "$phux_bin" agent usage --publish "$@"
