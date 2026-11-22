# shellcheck shell=bash
#
# Sourced by the examples/agents/ scripts: locates `phux`, starts a throwaway
# server on a private socket, and cleans it up on exit. A real agent needs none
# of this; it runs `phux <verb>` against the user's one-per-user server.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PHUX="${PHUX:-$repo_root/target/debug/phux}"
if [[ ! -x "$PHUX" ]]; then
    echo "examples/agents: build this checkout first (just build), or set PHUX=/absolute/path/to/phux" >&2
    exit 2
fi
export PHUX

# A private socket alone does not isolate credentials, config, logs, or state.
# Drop pane-inherited phux settings, including all listener and TLS overrides.
for _name in ${!PHUX_@}; do
    unset "$_name"
done
PHUX_TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/phux-example.XXXXXX")"
export PHUX_SOCKET="$PHUX_TMPDIR/phux.sock"
PHUX_SESSION=demo
export HOME="$PHUX_TMPDIR/home"
export XDG_CONFIG_HOME="$HOME/.config" XDG_STATE_HOME="$HOME/.local/state"
export XDG_DATA_HOME="$HOME/.local/share" XDG_CACHE_HOME="$HOME/.cache"
export XDG_RUNTIME_DIR="$PHUX_TMPDIR/run"
export SHELL=/bin/sh PHUX_NO_UPDATE_CHECK=1
unset ENV BASH_ENV ZDOTDIR PYTHONSTARTUP
mkdir -p "$HOME" "$XDG_RUNTIME_DIR"

_phux_server_pid=""

# Start the throwaway server and block until its socket is bound.
phux_start_server() {
    local attempt
    "$PHUX" server --session "$PHUX_SESSION" --socket "$PHUX_SOCKET" --exit-after-idle 60 \
        >"$PHUX_TMPDIR/server.log" 2>&1 &
    _phux_server_pid=$!
    # Poll for the bind for up to ~5s.
    for (( attempt=0; attempt<200; attempt++ )); do
        [[ -S "$PHUX_SOCKET" ]] && return 0
        kill -0 "$_phux_server_pid" 2>/dev/null || break
        sleep 0.025
    done
    echo "examples/agents: server did not bind $PHUX_SOCKET" >&2
    cat "$PHUX_TMPDIR/server.log" >&2 || true
    return 1
}

phux_cleanup() {
    if [[ -n "$_phux_server_pid" ]]; then
        kill "$_phux_server_pid" 2>/dev/null || true
        wait "$_phux_server_pid" 2>/dev/null || true
    fi
    rm -rf "$PHUX_TMPDIR" 2>/dev/null || true
}
trap phux_cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Insert socket before trailing positional args (not into run's command).
phux() {
    local verb="$1"
    shift
    "$PHUX" "$verb" --socket "$PHUX_SOCKET" "$@"
}

section() {
    printf '\n=== %s ===\n' "$*"
}

# A deliberate miss must be timeout (124), never a missing server/target.
phux_expect_timeout() {
    local rc
    if phux wait "$@"; then
        echo "examples/agents: expected a wait timeout, got success" >&2
        return 1
    else
        rc=$?
    fi
    if [[ "$rc" -ne 124 ]]; then
        echo "examples/agents: wait failed unexpectedly (exit $rc)" >&2
        return "$rc"
    fi
    echo "  timed out as expected (exit 124)"
}
