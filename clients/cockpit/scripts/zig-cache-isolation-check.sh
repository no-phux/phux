#!/usr/bin/env bash
# Regression check for scripts/zig-build.sh -- the per-worktree Zig global
# cache, the wrong-tree refusal, and the orphan reaper. See phux-cockpit-2ml.11.
#
# Every assertion here carries its own negative control, because this repo has
# already merged and then publicly retracted a finding that rested on an
# assertion nobody had watched fail. So each check either disables the fix and
# watches the check go RED, or asserts the absent state before acting. A check
# that has only ever been seen green is not evidence that anything works.
#
# The centrepiece is CONTENTION. It reproduces the starvation the bead records,
# with a lock deliberately held on a shared global cache manifest:
#
#   shared global cache, lock held    -> the build blocks until the watchdog
#   isolated global cache, lock held  -> the build finishes in seconds
#   shared global cache, lock released-> the build finishes in seconds
#
# The third case is what makes the first two mean something: it shows the lock,
# not the cache path, was the difference.
#
# Runtime is about a minute, most of it the deliberate block. Not wired into
# `zig build test`: it takes over a global cache manifest and spawns a fake
# orphan, neither of which belongs in a run somebody is waiting on.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
ZB="$ROOT/scripts/zig-build.sh"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/phux-zig-isolation.XXXXXX")"
HOLDER_PID=""
FAKE_ORPHAN_PID=""
FAILURES=0

cleanup() {
    [ -n "$HOLDER_PID" ] && kill "$HOLDER_PID" 2>/dev/null || true
    [ -n "$FAKE_ORPHAN_PID" ] && kill "$FAKE_ORPHAN_PID" 2>/dev/null || true
    rm -rf "$WORK"
}
trap cleanup EXIT

ok()   { printf '  ok: %s\n' "$1"; }
bad()  { printf 'FAILED: %s\n' "$1" >&2; FAILURES=$((FAILURES + 1)); }

# Bounded run without depending on coreutils `timeout` being installed.
# Prints the exit status; 124 means the bound was hit, matching timeout(1).
run_bounded() {
    local limit="$1"; shift
    set -m
    "$@" >"$WORK/bounded.log" 2>&1 &
    local pid=$!
    ( sleep "$limit"; kill -TERM "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null ) &
    local killer=$!
    local status=0
    wait "$pid" || status=$?
    if kill -0 "$killer" 2>/dev/null; then
        kill "$killer" 2>/dev/null || true
    else
        status=124
    fi
    set +m
    printf '%s' "$status"
}

printf 'zig cache isolation check\n'
printf '  root: %s\n' "$ROOT"

# --------------------------------------------------------------- 1. the path
#
# The fix disabled (PHUX_ZIG_CACHE_MODE=shared) must produce the machine-wide
# cache, and enabled must produce a path under THIS build root. Without the
# disabled half, "the path is under the root" would pass for a script that
# ignored the setting entirely.
shared_path="$(PHUX_ZIG_CACHE_MODE=shared "$ZB" --print-config | awk '/^global cache:/ {print $3}')"
iso_path="$(PHUX_ZIG_CACHE_MODE=isolated "$ZB" --print-config | awk '/^global cache:/ {print $3}')"

case "$shared_path" in
    "$ROOT"/*) bad "fix-disabled mode still produced a per-root cache ($shared_path); this check cannot fail" ;;
    *) ok "fix disabled -> shared cache $shared_path" ;;
esac
case "$iso_path" in
    "$ROOT"/*) ok "fix enabled  -> per-worktree cache $iso_path" ;;
    *) bad "isolated mode produced $iso_path, which is not under $ROOT" ;;
esac
[ "$shared_path" != "$iso_path" ] || bad "isolated and shared cache paths are identical"

# ------------------------------------------------------ 2. the shared p/ link
#
# Absent, act, present. The package cache must come back as a SYMLINK: a copy
# would mean re-downloading the pinned SDK per worktree, which is the tradeoff
# this design deliberately refuses.
#
# The guard around the `rm` is not paranoia. Run with the fix disabled, this
# section resolved `$iso_path` to the MACHINE-WIDE cache and pointed `rm` at
# its 793 MB package directory; only `rm` refusing to unlink a directory
# without -r stood between this check and deleting every worktree's packages.
# A destructive step that can be aimed outside the build root by an
# environment variable is a bug even when it happens to miss.
case "$iso_path" in
    "$ROOT"/*)
        [ -L "$iso_path/p" ] && rm -f "$iso_path/p"
        if [ -e "$iso_path/p" ]; then
            bad "could not clear $iso_path/p for the negative control"
        else
            ok "negative control: $iso_path/p absent before the run"
        fi
        if [ "$(cd "$ROOT" && run_bounded 300 "$ZB" --fetch)" = 0 ]; then
            ok "zig build --fetch through the wrapper"
        else
            bad "zig build --fetch through the wrapper: $(tail -3 "$WORK/bounded.log")"
        fi
        if [ -L "$iso_path/p" ]; then
            ok "package cache shared by symlink -> $(readlink "$iso_path/p")"
        else
            bad "$iso_path/p is not a symlink; the package cache was copied, not shared"
        fi
        ;;
    *)
        bad "cache $iso_path is outside $ROOT; skipping the package-cache checks rather than writing there"
        ;;
esac

# ------------------------------------------------------ 3. wrong-tree refusal
mkdir -p "$WORK/other"
git -C "$WORK/other" init -q
git -C "$WORK/other" commit -q --allow-empty -m init

refused=0
( cd "$WORK/other" && "$ZB" --fetch ) >"$WORK/foreign.log" 2>&1 || refused=$?
if [ "$refused" = 2 ]; then
    ok "refuses to build $ROOT from a cwd inside another repo"
else
    bad "foreign-cwd build was not refused (exit $refused)"
fi

allowed=0
( cd "$WORK/other" && "$ZB" --allow-foreign-cwd --fetch ) >"$WORK/foreign-allowed.log" 2>&1 || allowed=$?
if [ "$allowed" = 0 ]; then
    ok "negative control: --allow-foreign-cwd lets the same build through (exit 0)"
else
    bad "negative control broken: --allow-foreign-cwd also failed ($allowed), so the refusal above proves nothing"
fi

# ----------------------------------------------------------- 4. contention
#
# The whole point of the bead. A hello-world is enough: the manifest a trivial
# build takes in the global cache is the SAME one this repo takes
# (h/19941dd10bf085611a6d575c4f038f22.txt on Zig 0.16.0/aarch64-macos), which is
# exactly why one stuck worktree could stop all the others.
if ! command -v python3 >/dev/null 2>&1; then
    printf '  SKIPPED contention check: python3 is needed to hold a lock\n' >&2
else
    printf 'pub fn main() void {}\n' > "$WORK/hello.zig"
    gc_shared="$WORK/gc-shared"
    gc_private="$WORK/gc-private"
    mkdir -p "$gc_shared" "$gc_private"

    zig build-exe "$WORK/hello.zig" --global-cache-dir "$gc_shared" \
        --cache-dir "$WORK/lc-seed" -femit-bin="$WORK/hello-seed" >"$WORK/seed.log" 2>&1

    cat > "$WORK/hold.py" <<'PY'
import fcntl, sys, time
handles = []
for path in sys.argv[1:]:
    handle = open(path, "r+")
    fcntl.flock(handle, fcntl.LOCK_EX)
    handles.append(handle)
sys.stdout.write("locked\n")
sys.stdout.flush()
time.sleep(3600)
PY
    # shellcheck disable=SC2046
    python3 "$WORK/hold.py" $(ls "$gc_shared"/h/*.txt) >"$WORK/holder.log" 2>&1 &
    HOLDER_PID=$!
    until grep -q locked "$WORK/holder.log" 2>/dev/null; do
        kill -0 "$HOLDER_PID" 2>/dev/null || break
    done

    if [ "$(run_bounded 25 zig build-exe "$WORK/hello.zig" --global-cache-dir "$gc_shared" --cache-dir "$WORK/lc-shared" -femit-bin="$WORK/hello-shared")" = 124 ]; then
        ok "RED half: a held lock on the SHARED global cache starves the build"
    else
        bad "a held lock on the shared global cache did not block; this check cannot show the bug"
    fi

    if [ "$(run_bounded 120 zig build-exe "$WORK/hello.zig" --global-cache-dir "$gc_private" --cache-dir "$WORK/lc-private" -femit-bin="$WORK/hello-private")" = 0 ]; then
        ok "GREEN half: the same held lock does not touch an isolated cache"
    else
        bad "isolated global cache also blocked: $(tail -3 "$WORK/bounded.log")"
    fi

    kill "$HOLDER_PID" 2>/dev/null || true
    HOLDER_PID=""
    if [ "$(run_bounded 120 zig build-exe "$WORK/hello.zig" --global-cache-dir "$gc_shared" --cache-dir "$WORK/lc-released" -femit-bin="$WORK/hello-released")" = 0 ]; then
        ok "control: the shared cache builds fine once the lock is released"
    else
        bad "the shared cache still fails with no lock held, so the RED half above had another cause"
    fi
fi

# ------------------------------------------------------------- 5. orphans
#
# The fake wears a real runner's argv shape -- <local cache>/o/<hash>/build
# followed by zig exe, lib dir and BUILD ROOT -- because that shape is what the
# reaper matches on.
# Captured, never piped straight into `grep -q`: grep exits at the first match
# and SIGPIPEs the producer, and `set -o pipefail` then reports the whole
# pipeline as failed. That reads as "the reaper saw nothing" -- an assertion
# that fails hardest exactly when the thing it is testing WORKS.
if printf '%s' "$("$ZB" --list-orphans)" | grep -q isolationcheck; then
    bad "an 'isolationcheck' runner was already reported before one was spawned"
else
    ok "negative control: no isolationcheck runner reported before spawning one"
fi

mkdir -p "$WORK/o/isolationcheck"
cat > "$WORK/o/isolationcheck/build" <<'SH'
#!/bin/sh
# A stand-in for a Zig build runner: it does nothing but hold the argv shape.
sleep 300
SH
chmod +x "$WORK/o/isolationcheck/build"

# The subshell exits immediately, which is what reparents the child to pid 1 --
# the state a build runner ends up in when its session dies.
( nohup "$WORK/o/isolationcheck/build" \
    /usr/bin/false /usr/lib "$ROOT" .zig-cache /tmp/gc --seed 0x0 test \
    >/dev/null 2>&1 & )

# Bounded, never a spin: an unbounded wait on a process that may have died on
# launch is how a check hangs forever instead of failing (phux-cockpit-2ml.11
# rule 4 is the same mistake in a different costume).
for _ in $(seq 1 100); do
    # `|| true`: awk's early `exit` closes the pipe under ps, and with
    # `set -o pipefail` that SIGPIPE would otherwise end the whole script.
    FAKE_ORPHAN_PID="$(ps -eo pid=,ppid=,command= | awk 'index($0, "isolationcheck") && $2 == 1 {print $1; exit}')" || true
    [ -n "$FAKE_ORPHAN_PID" ] && break
    sleep 0.05
done
if [ -z "$FAKE_ORPHAN_PID" ]; then
    bad "could not spawn a fake orphaned runner; the reaper checks below prove nothing"
fi

if printf '%s' "$("$ZB" --list-orphans)" | grep -q isolationcheck; then
    ok "an orphaned runner for this root is reported (pid $FAKE_ORPHAN_PID)"
else
    bad "the orphan reaper did not see a runner it should have matched"
fi

"$ZB" --reap >/dev/null 2>&1 || true
if [ -n "$FAKE_ORPHAN_PID" ] && kill -0 "$FAKE_ORPHAN_PID" 2>/dev/null; then
    bad "the orphaned runner survived --reap"
else
    ok "--reap killed the orphaned runner for this root"
    FAKE_ORPHAN_PID=""
fi

printf '\n'
if [ "$FAILURES" = 0 ]; then
    printf 'zig cache isolation check: PASS\n'
    exit 0
fi
printf 'zig cache isolation check: FAIL (%s)\n' "$FAILURES"
exit 1
