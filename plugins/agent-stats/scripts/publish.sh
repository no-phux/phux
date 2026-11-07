# shellcheck shell=sh
# Record the hook event in the environment ($PHUX_EVENT and its keys) and
# write phux.agent-stats/v1 on the server that fired the hook ($PHUX_SOCKET).
set -eu

phux_bin=${PHUX_BIN:-phux}
if [ -n "${PHUX_SOCKET:-}" ]; then
    exec "$phux_bin" --socket "$PHUX_SOCKET" agent stats --publish
fi
exec "$phux_bin" agent stats --publish
