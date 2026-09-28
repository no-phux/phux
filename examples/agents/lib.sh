# shellcheck shell=bash
#
# Sourced by the examples/agents/ scripts: locates `phux`, starts a throwaway
# server on a private socket, and cleans it up on exit. A real agent needs none
# of this; it runs `phux <verb>` against the user's one-per-user server.

set -euo pipefail

# $PHUX, else `phux` on PATH, else a debug build of this checkout.
if [[ -n "${PHUX:-}" ]]; then
    :
elif command -v phux >/dev/null 2>&1; then
    PHUX="$(command -v phux)"
else
    repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
    candidate="$repo_root/target/debug/phux"
    if [[ ! -x "$candidate" ]]; then
        echo "examples/agents: building phux (one-time, may be slow)..." >&2
        # The dev shell provides zig for libghostty-vt's build.
        ( cd "$repo_root" && nix develop -c cargo build -p phux ) >&2
    fi
    PHUX="$candidate"
fi
export PHUX

PHUX_TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/phux-example.XXXXXX")"
export PHUX_SOCKET="$PHUX_TMPDIR/phux.sock"
PHUX_SESSION="${PHUX_SESSION:-demo}"
export PHUX_SESSION

_phux_server_pid=""

# Start the throwaway server and block until its socket is bound.
phux_start_server() {
    "$PHUX" server --session "$PHUX_SESSION" --socket "$PHUX_SOCKET" \
        >"$PHUX_TMPDIR/server.log" 2>&1 &
    _phux_server_pid=$!
    # Poll for the bind for up to ~5s.
    for _ in $(seq 1 200); do
        [[ -S "$PHUX_SOCKET" ]] && return 0
        sleep 0.025
    done
    echo "examples/agents: server did not bind $PHUX_SOCKET" >&2
    cat "$PHUX_TMPDIR/server.log" >&2 || true
    return 1
}

phux_cleanup() {
    [[ -n "$_phux_server_pid" ]] && kill "$_phux_server_pid" 2>/dev/null || true
    rm -rf "$PHUX_TMPDIR" 2>/dev/null || true
}
trap phux_cleanup EXIT

# `--socket` is a per-subcommand flag that must follow the verb and precede
# trailing positional args, so insert it right after the verb.
phux() {
    local verb="$1"
    shift
    "$PHUX" "$verb" --socket "$PHUX_SOCKET" "$@"
}

section() {
    printf '\n=== %s ===\n' "$*"
}
