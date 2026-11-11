# shellcheck shell=sh
# Record the hook event in the environment ($PHUX_EVENT and its keys) and
# write phux.agent-stats/v1 on the server that fired the hook ($PHUX_SOCKET).
set -eu

phux_bin=${PHUX_BIN:-}
# A server started from the GUI (launchd) or systemd may not have phux on PATH; try the usual
# install dirs before giving up.
if [ -z "$phux_bin" ]; then
    for candidate in phux "$HOME/.local/bin/phux" "$HOME/bin/phux" /opt/homebrew/bin/phux /usr/local/bin/phux /opt/phux/bin/phux; do
        if command -v "$candidate" >/dev/null 2>&1; then phux_bin=$candidate; break; fi
    done
fi
phux_bin=${phux_bin:-phux}
if [ -n "${PHUX_SOCKET:-}" ]; then
    exec "$phux_bin" --socket "$PHUX_SOCKET" agent stats --publish
fi
exec "$phux_bin" agent stats --publish
