#!/usr/bin/env bash
#
# The EXPENSIVE half of the guard ritual: remove a fix, watch its test go red,
# put the fix back.
#
# phux-cockpit-2ml.2. On 2026-08-10 a regression test PASSED against the very
# bug it was written to catch. It allowed "a few more frames" to settle, and
# the old one-pane-per-frame behaviour needed exactly four frames for four
# panes - inside the allowance. It was caught only because someone remembered
# to disable the fix by hand and re-run, at which point it was tightened to
# assert EXACTLY ONE viewport dispatch.
#
# That step is the only thing separating a regression test from a test-shaped
# comment, and doing it by memory is doing it sometimes. This script does it on
# demand, records the evidence in the guard file, and refuses to record
# anything it did not watch happen.
#
#   scripts/guard-red-run.sh                        prove every guard
#   scripts/guard-red-run.sh NAME [NAME...]         prove the named guards
#   scripts/guard-red-run.sh --record NAME \
#       --test "the zig test name" [PATH...]        capture the break sitting
#                                                   in the working tree, then
#                                                   prove it
#   ... --build "test -Dtypescript-spike=true -Dplatform=null"
#                                                   for a test that only exists
#                                                   in another build graph
#
# PATH narrows what is captured, and giving it is the safe habit: without it
# the capture is the whole `git diff`, which on a shared checkout has already
# swept up an unrelated build.zig.zon edit that belonged to someone else.
#
# Cost: one full `zig build test` per guard, plus a green baseline. About 40
# seconds each on this machine. This is NOT a gate - it is what you run when
# you add or move a guard. The cheap half that DOES run on every gate is
# scripts/guard-check.sh; see docs/GUARDS.md for the division of labour.
#
# WHAT COUNTS AS PROOF, and why the checks are the shape they are:
#
#   * The exit code is necessary and nowhere near sufficient. A break that
#     stops the tree COMPILING also exits non-zero, and proves nothing about
#     the test. So the red run must name the test: Zig prints
#     `error: '<module>.test.<name>' failed:` for a genuine assertion failure
#     and nothing of the sort for a compile error.
#
#   * The green baseline runs FIRST, before anything is touched. A guard
#     "proved" against an already-red tree is proving the wrong thing, and the
#     baseline is also the only run that prints the verdict block - which is
#     where the `source root:` line lives. A build that silently ran in a
#     sibling worktree has happened here; the exit code was real and the tree
#     was not.
#
#   * The recorded evidence names the HEAD it was proved at, and the run
#     re-reads that HEAD at the end. A checkout that moved underneath the run
#     invalidates the whole thing, and on a shared working directory that is
#     not hypothetical - it happened while this script was being written.
#
#   * The break is a git patch, applied and reverse-applied. Never a hand-edit
#     performed here and compared against a state no build was ever in. See the
#     retraction recorded in scripts/host-raster-compare.sh for what that costs.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
GUARD_DIR="$ROOT/scripts/guards"
LOG_DIR="${TMPDIR:-/tmp}/phux-cockpit-guards"
mkdir -p "$LOG_DIR" "$GUARD_DIR"
GIT_PREFIX="$(git -C "$ROOT" rev-parse --show-prefix)"
GIT_PREFIX="${GIT_PREFIX%/}"

RECORD=""
RECORD_TEST=""
RECORD_BUILD=""
positional=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --record) RECORD="${2:?--record needs a guard name}"; shift 2 ;;
        --test) RECORD_TEST="${2:?--test needs a zig test name}"; shift 2 ;;
        --build) RECORD_BUILD="${2:?--build needs zig build arguments}"; shift 2 ;;
        -h|--help) sed -n '2,34p' "$0"; exit 0 ;;
        -*) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
        *) positional+=("$1"); shift ;;
    esac
done

die() { printf 'guard-red-run: %s\n' "$1" >&2; exit 1; }

# Tracked modifications only. An untracked file cannot be part of a guard
# patch, and --record writes a new untracked guard file before proving it.
tracked_dirty() { [[ -n "$(git -C "$ROOT" status --porcelain --untracked-files=no)" ]]; }

require_clean() {
    if tracked_dirty; then
        git -C "$ROOT" status --porcelain --untracked-files=no >&2
        die "the working tree has tracked changes. A guard can only be proved against the fix as committed."
    fi
}

patch_of() { sed -n '/^diff --git /,$p' "$1"; }

git_apply() {
    if [[ -n "$GIT_PREFIX" ]]; then
        git -C "$ROOT" apply --directory="$GIT_PREFIX" "$@"
    else
        git -C "$ROOT" apply "$@"
    fi
}

# The `zig build` arguments a guard's test runs under. The default gate is
# `test`; a guard whose test only exists in another graph (the TypeScript-core
# spike, say) names that graph on a `build:` line, because a red run in the
# wrong graph cannot even find the test and would be reported as WRONG RED.
build_args_of() {
    local value
    value="$(sed -n 's/^build: //p' "$1" | head -1)"
    printf '%s' "${value:-test}"
}

# The paths a guard's patch touches, so every restore is narrowed to them.
# `git checkout -- .` is not an acceptable fallback: this working directory has
# turned out to be shared, and a blanket revert discards a neighbour's work.
patch_paths() {
    patch_of "$1" | sed -n 's|^diff --git a/\(.*\) b/.*|\1|p' | sort -u
}

restore() {
    local guard="$1"
    patch_of "$guard" | git_apply --unidiff-zero -R - 2>/dev/null || true
    if tracked_dirty; then
        # shellcheck disable=SC2046
        git -C "$ROOT" checkout -- $(patch_paths "$guard") 2>/dev/null || true
    fi
    if tracked_dirty; then
        git -C "$ROOT" status --porcelain --untracked-files=no >&2
        die "FAILED TO RESTORE the tree after breaking it. Fix this by hand before doing anything else."
    fi
}

start_head="$(git -C "$ROOT" rev-parse HEAD)"
start_branch="$(git -C "$ROOT" rev-parse --abbrev-ref HEAD)"

# ------------------------------------------------------------ recording mode

if [[ -n "$RECORD" ]]; then
    [[ -n "$RECORD_TEST" ]] || die "--record also needs --test \"the zig test name\""
    [[ "$RECORD" =~ ^[A-Za-z0-9_-]+$ ]] || die "guard names are [A-Za-z0-9_-]: $RECORD"
    tracked_dirty || die "--record captures the break sitting in the working tree, and the tree is clean. Disable the fix first."

    guard="$GUARD_DIR/$RECORD.guard"
    [[ -e "$guard" ]] && die "$guard already exists. Delete it to re-derive."

    if [[ ${#positional[@]} -gt 0 ]]; then
        diff_text="$(git -C "$ROOT" diff --relative -- "${positional[@]}")"
    else
        diff_text="$(git -C "$ROOT" diff --relative)"
    fi
    [[ -n "$diff_text" ]] || die "the capture is empty. Staged changes are not captured, by design."

    {
        printf '# GUARD %s\n#\n' "$RECORD"
        printf '# Describe the defect this test exists to catch, and what removing the\n'
        printf '# fix below puts back. Prose here is preserved across re-runs.\n'
        printf 'test: %s\n' "$RECORD_TEST"
        [[ -n "$RECORD_BUILD" ]] && printf 'build: %s\n' "$RECORD_BUILD"
        printf '%s\n' "$diff_text"
    } > "$guard"

    printf 'captured into %s:\n' "$guard"
    patch_of "$guard" | git_apply --unidiff-zero --stat - | sed 's/^/  /'

    # shellcheck disable=SC2046
    git -C "$ROOT" checkout -- $(patch_paths "$guard")
    positional=("$RECORD")
fi

# --------------------------------------------------------------- select work

names=("${positional[@]}")
if [[ ${#names[@]} -eq 0 ]]; then
    shopt -s nullglob
    for g in "$GUARD_DIR"/*.guard; do names+=("$(basename "$g" .guard)"); done
    shopt -u nullglob
fi
[[ ${#names[@]} -gt 0 ]] || die "no guards to prove."

require_clean

# --------------------------------------------------------- the green baseline

printf '=== baseline: the tree as committed must be GREEN ===\n'
printf 'HEAD %s (%s)\n' "$(git -C "$ROOT" rev-parse --short HEAD)" "$start_branch"
baseline_log="$LOG_DIR/baseline.log"
set +e
# The guards being proved right now are exempted from guard-check's "has it
# been demonstrated red" and "does its break still apply" complaints, for the
# duration of this run only. Those two are precisely what this run answers, and
# the gate that asks them lives inside the baseline it would otherwise refuse.
export GUARD_CHECK_REDERIVING="${names[*]}"
(cd "$ROOT" && zig build test) > "$baseline_log" 2>&1
baseline_exit=$?
set -e
printf 'zig build test exit=%d  (%s)\n' "$baseline_exit" "$baseline_log"
[[ $baseline_exit -eq 0 ]] || die "the baseline is not green. Nothing proved here would mean anything."

# The verdict block prints only on a green run, and it is the only place the
# build states which tree it compiled. Confirm it is THIS one.
if ! grep -qF "source root:   $ROOT" "$baseline_log"; then
    printf -- '--- the verdict block said ---\n' >&2
    grep -F 'source root:' "$baseline_log" >&2 || printf '(no source root line at all)\n' >&2
    die "the baseline build did not name this worktree as its source root."
fi
printf 'source root: confirmed %s\n\n' "$ROOT"

# ------------------------------------------------------------- the red runs

failed=0
# A plain array, not `declare -A`: associative arrays are bash 4 and macOS
# ships 3.2. This script is developer-run rather than CI-run, so it never blew
# up the way guard-check.sh did — but the same trap is the same trap, and a
# guard tool that only works under a Homebrew bash is a guard tool somebody
# will one day run under /bin/bash.
proved=()
baselined_graphs=()

for name in "${names[@]}"; do
    guard="$GUARD_DIR/$name.guard"
    [[ -f "$guard" ]] || die "no such guard: $guard"
    test_name="$(sed -n 's/^test: //p' "$guard" | head -1)"
    [[ -n "$test_name" ]] || die "$name: no 'test:' line."

    printf '=== %s ===\n' "$name"
    printf 'guards: %s\n' "$test_name"
    build_args="$(build_args_of "$guard")"

    # A guard in another graph needs that graph's own green baseline: the
    # default gate above never compiled its test, so it has proved nothing
    # about it yet. Once per graph per run.
    if [[ "$build_args" != "test" && " ${baselined_graphs[*]:-} " != *" $build_args "* ]]; then
        graph_log="$LOG_DIR/baseline-$(printf '%s' "$build_args" | tr -c 'A-Za-z0-9' '_').log"
        set +e
        (cd "$ROOT" && zig build $build_args) > "$graph_log" 2>&1
        graph_exit=$?
        set -e
        printf 'zig build %s exit=%d  (%s)\n' "$build_args" "$graph_exit" "$graph_log"
        [[ $graph_exit -eq 0 ]] || die "the '$build_args' baseline is not green. Nothing proved here would mean anything."
        baselined_graphs+=("$build_args")
    fi

    if ! patch_of "$guard" | git_apply --unidiff-zero --check - 2>/dev/null; then
        printf 'BREAK NO LONGER APPLIES. The fix moved; re-derive this guard.\n\n' >&2
        failed=1
        continue
    fi
    patch_of "$guard" | git_apply --unidiff-zero -
    printf 'removed the fix:\n'
    git -C "$ROOT" diff --stat | sed 's/^/  /'

    log="$LOG_DIR/$name.red.log"
    set +e
    (cd "$ROOT" && zig build $build_args) > "$log" 2>&1
    red_exit=$?
    set -e

    restore "$guard"

    printf 'zig build %s exit=%d  (%s)\n' "$build_args" "$red_exit" "$log"

    if [[ $red_exit -eq 0 ]]; then
        printf 'NOT A GUARD: the suite stayed GREEN with the fix removed. This test\n' >&2
        printf 'does not catch the thing it claims to catch.\n\n' >&2
        failed=1
        continue
    fi

    # An exit code alone would also be satisfied by a break that stopped the
    # tree compiling. Zig names the test that failed; require THIS one.
    needle=".test.$test_name' failed:"
    if ! grep -qF "$needle" "$log"; then
        printf 'WRONG RED. The suite failed, but not at the test this guard names.\n' >&2
        printf 'A break that stops the tree compiling looks exactly like this and\n' >&2
        printf 'proves nothing. What the run actually said:\n' >&2
        grep -E '^error|[0-9]+ fail' "$log" | head -5 | sed 's/^/  /' >&2
        printf '\n' >&2
        failed=1
        continue
    fi

    printf 'RED, at the named test:\n'
    grep -F "$needle" "$log" | head -1 | sed 's/^/  /'
    grep -E '^\+- run test .*fail' "$log" | head -1 | sed 's/^/  /' || true
    grep -A 8 -F "$needle" "$log" | grep -F "$ROOT/src" | head -1 | sed 's/^/  /' || true
    proved+=("$name")
    printf '\n'
done

require_clean

# ------------------------------------------------------------- provenance
#
# Everything above is worthless if the checkout moved underneath it. This
# working directory has been shared before, and a run that began at one commit
# and ended at another proved something about a tree nobody can name.

end_head="$(git -C "$ROOT" rev-parse HEAD)"
if [[ "$end_head" != "$start_head" ]]; then
    printf 'THE CHECKOUT MOVED DURING THIS RUN: %s -> %s\n' \
        "${start_head:0:7}" "${end_head:0:7}" >&2
    die "nothing is recorded. Re-run in a working directory nothing else is using."
fi

# --------------------------------------------------------- record the evidence
#
# Written only for guards this run actually watched fail, and written last, so
# a run that aborted midway leaves no claim behind.

today="$(date +%Y-%m-%d)"
for name in ${proved[@]+"${proved[@]}"}; do
    guard="$GUARD_DIR/$name.guard"
    line="red: $today at ${start_head:0:7}"
    tmp="$(mktemp)"
    if grep -q '^red: ' "$guard"; then
        sed "s|^red: .*|$line|" "$guard" > "$tmp"
    else
        awk -v line="$line" '{ print } /^test: / && !seen { print line; seen = 1 }' "$guard" > "$tmp"
    fi
    mv "$tmp" "$guard"
done

if [[ $failed -ne 0 ]]; then
    printf 'guard-red-run: at least one guard could not be proved. See above.\n' >&2
    exit 1
fi
printf 'guard-red-run: %d guard(s) proved red and restored green.\n' "${#proved[@]}"
