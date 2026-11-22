# shellcheck shell=sh
# Collect this host's plan usage and write phux.usage/v1 on the server that
# fired the hook ($PHUX_SOCKET). `phux agent usage` spaces vendor requests and
# backs off on rate limits itself, so firing on every event is cheap.
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
    exec "$phux_bin" --socket "$PHUX_SOCKET" agent usage --publish "$@"
fi
exec "$phux_bin" agent usage --publish "$@"
