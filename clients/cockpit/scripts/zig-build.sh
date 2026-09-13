#!/usr/bin/env bash
# `zig build` with a per-worktree Zig global cache, an orphan reaper, and a
# refusal to build a tree you are not standing in. See phux-cockpit-2ml.11.
#
# WHY A WRAPPER EXISTS AT ALL
#
# Zig's global cache (`~/.cache/zig` by default) is shared by every checkout on
# the machine, and a cache entry is guarded by an EXCLUSIVE advisory lock on its
# manifest file in `<global cache>/h/<hash>.txt`. The lock is held for the whole
# time the entry is being produced. That is correct for one user and one
# checkout: the second build waits a moment and then gets the first build's
# work for free.
#
# With one worktree per agent it stops being correct. The manifests that land in
# the global cache are project-independent -- a hello-world and this repo were
# measured writing the SAME manifest, h/19941dd10bf085611a6d575c4f038f22.txt --
# so every worktree on this machine queues on the same file. A build runner that
# outlives its session holds that lock forever, and every other worktree stops.
# That is the 30-minute starvation recorded on the bead, and it was reproduced
# here deliberately:
#
#   held lock + shared global cache   -> blocked, killed at 45s (exit 124)
#   held lock + isolated global cache -> exit 0 in 3s
#   lock released + shared cache      -> exit 0 in 1s
#
# (scripts/zig-cache-isolation-check.sh runs that A/B on demand.)
#
# WHAT IS ISOLATED AND WHAT IS DELIBERATELY NOT
#
# Isolating the whole global cache per worktree would be the easy answer and the
# wrong one: `p/` is the package cache, and re-fetching the pinned SDK and
# ghostty per worktree costs a network round trip that an offline machine simply
# cannot make. Measured on this repo, `zig build --fetch=all` into a brand-new
# worktree:
#
#   fully private global cache   16s, 172 MB downloaded
#   private cache, shared `p/`    7s, 0 bytes downloaded
#
# So `p/` is shared by symlink and everything else (b/ h/ o/ z/ tmp/) is private.
# Sharing `p/` is safe in a way sharing the rest is not: its entries are
# immutable, content-addressed and hash-verified, written once by an atomic
# rename out of `tmp/`, and never rewritten. Nothing in it is produced under a
# long-held lock, so it cannot starve anybody.
#
# The private half is cheap because Zig 0.16 already materializes dependency
# SOURCE into a build-root-relative `zig-pkg/` (528-693 MB, per worktree,
# already). What is left in the global cache for a full `zig build test` here is
# 45 MB, of which the only substantial artifact is one `libcompiler_rt.a`:
#
#   du -sh <private global cache>/*  ->  b 28K   h 132K   o 3.1M   z 41M
#
# ORPHANS
#
# The other half of the damage: build runners outliving their session. A real
# one was found while writing this -- pid 65004, ppid 1, 73 minutes old, still
# spinning a test binary in a worktree whose session was long gone. Nothing was
# going to read its exit code. This script kills orphaned runners for ITS OWN
# build root before starting, never for anybody else's, and puts its own zig in
# a process group it tears down on exit so it does not become the next one.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
CACHE_DIR="$ROOT/.zig-global-cache"
SHARED_PKG_CACHE="${ZIG_SHARED_PKG_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/zig/p}"

# Isolation is for local worktrees that share a machine. CI has one checkout
# and a job-level timeout; isolated mode there restored a duplicate
# `.zig-global-cache` (its `p/` symlink doubled the package cache into a 4.4GB
# Actions restore) and the 600s watchdog killed legitimate shipping compiles.
if [ -n "${GITHUB_ACTIONS:-}" ]; then
    CACHE_MODE="${PHUX_ZIG_CACHE_MODE:-shared}"
    TIMEOUT_SECONDS="${PHUX_ZIG_BUILD_TIMEOUT:-0}"
else
    CACHE_MODE="${PHUX_ZIG_CACHE_MODE:-isolated}"
    TIMEOUT_SECONDS="${PHUX_ZIG_BUILD_TIMEOUT:-600}"
fi

ALLOW_FOREIGN_CWD=0
ACTION=build
ZIG_ARGS=()

usage() {
    sed -n '2,58p' "$0"
    exit 0
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --print-config) ACTION=print-config ;;
        --list-orphans) ACTION=list-orphans ;;
        --reap) ACTION=reap ;;
        --allow-foreign-cwd) ALLOW_FOREIGN_CWD=1 ;;
        --timeout) TIMEOUT_SECONDS="$2"; shift ;;
        -h|--help) usage ;;
        --) shift; ZIG_ARGS+=("$@"); break ;;
        *) ZIG_ARGS+=("$1") ;;
    esac
    shift
done

# ---------------------------------------------------------------- orphans
#
# A Zig build runner's argv is
#   <local cache>/o/<hash>/build <zig exe> <lib dir> <BUILD ROOT> <local cache> \
#     <global cache> --seed ... <steps>
# so the build root it is compiling is in the command line, and `ps` can tell
# you which tree a runner belongs to without guessing. `pgrep -f` is not used
# anywhere here: it matches the shell running it (phux-cockpit-2ml.11 rule 4).
runners_matching() {
    # $1: a build root to match, or the empty string for every runner.
    local want="$1"
    ps -eo pid=,ppid=,etime=,command= 2>/dev/null | awk -v want="$want" '
        index($0, "/build ") == 0 { next }
        index($0, "/o/") == 0 { next }
        want != "" && index($0, want) == 0 { next }
        { print }
    '
}

orphaned_runners() {
    # ppid 1 means the session that launched it is gone, so nobody is left to
    # read its exit code -- the definition of an orphan worth killing.
    runners_matching "$1" | awk '$2 == 1'
}

reap_own_orphans() {
    local line pid
    while IFS= read -r line; do
        [ -n "$line" ] || continue
        pid="$(printf '%s' "$line" | awk '{print $1}')"
        printf 'zig-build.sh: reaping orphaned build runner for this root: %s\n' "$line" >&2
        # The runner's own children (the test binary) die with the group.
        kill -TERM "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
    done < <(orphaned_runners "$ROOT")
}

# ---------------------------------------------------------------- cache setup
prepare_cache() {
    mkdir -p "$CACHE_DIR"
    mkdir -p "$SHARED_PKG_CACHE"
    # `p` as a symlink rather than a copy: the package cache is the one part of
    # the global cache that is safe to share, and the one part that is
    # expensive to duplicate.
    if [ -L "$CACHE_DIR/p" ] || [ ! -e "$CACHE_DIR/p" ]; then
        ln -sfn "$SHARED_PKG_CACHE" "$CACHE_DIR/p"
    fi
}

resolved_cache_dir() {
    if [ "$CACHE_MODE" = "shared" ]; then
        printf '%s' "${XDG_CACHE_HOME:-$HOME/.cache}/zig"
    else
        printf '%s' "$CACHE_DIR"
    fi
}

print_config() {
    printf 'build root:    %s\n' "$ROOT"
    printf 'global cache:  %s\n' "$(resolved_cache_dir)"
    printf 'package cache: %s\n' "$SHARED_PKG_CACHE"
    printf 'isolation:     %s\n' "$CACHE_MODE"
    if [ "$TIMEOUT_SECONDS" = "0" ]; then
        printf 'timeout:       none\n'
    else
        printf 'timeout:       %ss\n' "$TIMEOUT_SECONDS"
    fi
}

case "$ACTION" in
    print-config)
        print_config
        exit 0
        ;;
    reap)
        reap_own_orphans
        exit 0
        ;;
    list-orphans)
        # Repo-wide, and REPORT ONLY. Killing another worktree's runner is not
        # this script's business even when it is obviously stuck; the operator
        # decides that.
        found=0
        while IFS= read -r line; do
            [ -n "$line" ] || continue
            found=1
            printf '%s\n' "$line"
        done < <(orphaned_runners "")
        [ "$found" = 1 ] || printf 'no orphaned zig build runners\n'
        exit 0
        ;;
esac

# ------------------------------------------------------- wrong-tree refusal
#
# The failure this whole bead is named for: an agent believed it was in its own
# worktree, ran `zig build test`, and got a real exit code for somebody else's
# code. A wrapper cannot read the agent's belief, but it can refuse the one
# shape the mistake takes -- the tree you are STANDING IN is not the tree this
# script would build.
if [ "$ALLOW_FOREIGN_CWD" = 0 ]; then
    cwd_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
    expected_root="$(git -C "$ROOT" rev-parse --show-toplevel 2>/dev/null || true)"
    if [ -n "$cwd_root" ] && [ "$cwd_root" != "$expected_root" ]; then
        printf 'zig-build.sh: REFUSING to build a tree you are not standing in.\n' >&2
        printf '  your cwd is in:  %s\n' "$cwd_root" >&2
        printf '  this script builds from: %s\n' "$expected_root" >&2
        printf '  An exit code from the wrong tree is indistinguishable from a\n' >&2
        printf '  correct one. cd to the tree you mean, or pass --allow-foreign-cwd.\n' >&2
        exit 2
    fi
fi

[ "$CACHE_MODE" = "shared" ] || prepare_cache
print_config >&2
cd "$ROOT"

run_zig() {
    zig build --global-cache-dir "$(resolved_cache_dir)" "${ZIG_ARGS[@]}"
}

# `zig build --fetch` exits after the dependency tree is on disk. Nested
# tarball fetches against github.com sometimes get HttpConnectionClosing
# on Actions; retry those, never a compile or test.
is_fetch_only=0
for arg in "${ZIG_ARGS[@]}"; do
    case "$arg" in
        --fetch|--fetch=all|--fetch=needed) is_fetch_only=1 ;;
    esac
done
if [ "$is_fetch_only" = 1 ]; then
    n=0
    until run_zig; do
        n=$((n + 1))
        if [ "$n" -ge 4 ]; then
            printf 'zig-build.sh: package fetch failed after %s attempts\n' "$n" >&2
            exit 1
        fi
        printf 'zig-build.sh: package fetch failed (attempt %s); retrying\n' "$n" >&2
        sleep $((n * 8))
    done
    exit 0
fi

# Timeout 0: the caller (Actions job, an operator) owns the limit. Exec zig
# directly so a shipping compile is not killed at 10 minutes and CI does not
# pay for process-group / watchdog / orphan-reaper machinery it does not need.
if [ "$TIMEOUT_SECONDS" = "0" ]; then
    exec zig build --global-cache-dir "$(resolved_cache_dir)" "${ZIG_ARGS[@]}"
fi

reap_own_orphans

# Own-lifetime guard. `set -m` puts zig in its own process group, and the trap
# tears that group down on any exit path -- so cancelling this script does not
# leave the runner (or the test binary under it) behind as the next orphan.
set -m
zig build --global-cache-dir "$(resolved_cache_dir)" "${ZIG_ARGS[@]}" &
zig_pid=$!
cleanup() { kill -TERM "-$zig_pid" 2>/dev/null || true; }
trap cleanup EXIT INT TERM HUP

( sleep "$TIMEOUT_SECONDS"; kill -TERM "-$zig_pid" 2>/dev/null ) &
watchdog_pid=$!

status=0
wait "$zig_pid" || status=$?
kill "$watchdog_pid" 2>/dev/null || true
trap - EXIT INT TERM HUP

if [ "$status" -ne 0 ]; then
    printf 'zig-build.sh: zig build exited %s (timeout was %ss)\n' "$status" "$TIMEOUT_SECONDS" >&2
fi
exit "$status"
