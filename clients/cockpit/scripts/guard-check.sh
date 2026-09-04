#!/usr/bin/env bash
#
# The CHEAP half of the guard ritual, wired into `zig build test`.
#
# A guard is a regression test that has been WATCHED FAILING with its fix
# removed. `scripts/guard-red-run.sh` is what watches; this script is what
# keeps the resulting bookkeeping from rotting, and it is deliberately the
# only half that runs on every gate, because it costs milliseconds and the
# other half costs minutes.
#
# It answers three questions, all of them statically:
#
#   1. Does every `// GUARD: <name>` marker in src/ have a guard file, and
#      does every guard file name a test that still exists with that marker?
#      A marker whose file was deleted is a claim with nothing behind it.
#
#   2. Has every guard actually been demonstrated red? A guard file with no
#      `red:` line is a test-shaped comment — exactly the thing phux-cockpit-2ml.2
#      exists to stop. guard-red-run.sh is the only writer of that line, and it
#      writes it only after watching the named test fail.
#
#   3. Does each guard's break still APPLY to the tree? This is the staleness
#      test. `red:` records a run against a particular shape of the fix; if the
#      fix has since moved, the recorded proof is about code that is gone. A
#      patch that no longer applies is not a failure of the fix — it is a
#      demand to re-derive the guard and watch it go red again.
#
# What it deliberately does NOT check is whether the test would still go red.
# Nothing static can know that. That is what guard-red-run.sh is for, and the
# gap between "the break still applies" and "the test still fails without the
# fix" is real. See docs/GUARDS.md.
#
# Silent on success: this runs as a build step with inherited stdio, and Zig's
# build runner reports any step that wrote to stderr as a failure.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
GIT_PREFIX="$(git -C "$ROOT" rev-parse --show-prefix)"
GIT_PREFIX="${GIT_PREFIX%/}"
GIT_APPLY=(git -C "$ROOT" apply)
[[ -z "$GIT_PREFIX" ]] || GIT_APPLY+=(--directory="$GIT_PREFIX")
GUARD_DIR="$ROOT/scripts/guards"
# Every source root that may carry a `// GUARD:` marker. The TypeScript-core
# spike's extension keeps its own guards beside the module they prove.
SCAN_ROOTS=("$ROOT/src" "$ROOT/typescript-spike/src")

# The guard currently being re-derived, if any, named by guard-red-run.sh.
#
# Checks 2 and 3 are exactly what a re-derivation is on its way to satisfying,
# and this script runs inside the `zig build test` that re-derivation uses as
# its baseline — so without this the mechanism deadlocks the moment a guard
# goes stale: guard-check refuses the tree because the guard is unproved, and
# guard-red-run refuses to prove it because the baseline is not green. It was
# reachable the first time a fix's CONTEXT moved, which is the ordinary life of
# any line of code.
#
# Deliberately narrow: only the guard being re-derived, only the two checks it
# is in the middle of answering. Its marker and its test must still exist, and
# every OTHER guard is held to the full standard.
GUARD_CHECK_REDERIVING="${GUARD_CHECK_REDERIVING:-}"
rederiving() {
    local name="$1" candidate
    for candidate in $GUARD_CHECK_REDERIVING; do
        [[ "$candidate" == "$name" ]] && return 0
    done
    return 1
}

status=0
complain() {
    printf 'guard-check: %s\n' "$1" >&2
    status=1
}

# ------------------------------------------------- guard file -> source tree

shopt -s nullglob
guard_files=("$GUARD_DIR"/*.guard)
shopt -u nullglob

# A space-delimited set rather than `declare -A`. Associative arrays are bash
# 4, and macOS ships bash 3.2 as /bin/bash — which is what a GitHub macOS
# runner resolves `bash scripts/guard-check.sh` to, so this script died on
# `declare: -A: invalid option` in CI while passing on every developer machine
# with a Homebrew bash on PATH. A build step that only runs on the machine that
# wrote it is not a gate.
known_guards=" "

for guard in "${guard_files[@]}"; do
    name="$(basename "$guard" .guard)"
    known_guards="${known_guards}${name} "

    test_name="$(sed -n 's/^test: //p' "$guard" | head -1)"
    if [[ -z "$test_name" ]]; then
        complain "$name: no 'test:' line. A guard must name the test it proves."
        continue
    fi

    # -F: Zig test names are prose and contain regex metacharacters.
    hit="$(grep -rFn "test \"$test_name\" {" "${SCAN_ROOTS[@]}" || true)"
    if [[ -z "$hit" ]]; then
        complain "$name: names test \"$test_name\", which no longer exists under src/ or typescript-spike/src/."
        continue
    fi
    if [[ "$(printf '%s\n' "$hit" | wc -l)" -ne 1 ]]; then
        complain "$name: test \"$test_name\" is declared more than once; the guard is ambiguous."
        continue
    fi

    src_file="${hit%%:*}"
    rest="${hit#*:}"
    line="${rest%%:*}"

    # The marker sits on the line directly above the test, so a reader who
    # opens the test sees the claim without going looking for it.
    marker="$(sed -n "$((line - 1))p" "$src_file")"
    if [[ "$marker" != *"// GUARD: $name"* ]]; then
        complain "$name: expected '// GUARD: $name' directly above test \"$test_name\" in $src_file:$line."
    fi

    if rederiving "$name"; then
        continue
    fi

    if ! grep -q '^red: ' "$guard"; then
        complain "$name: never demonstrated red. Run: scripts/guard-red-run.sh $name"
    fi

    # Staleness. The break is stored as a patch against the FIXED tree, so it
    # must still apply forward. When it does not, the fix it removes has moved
    # and the recorded proof is about code that no longer exists.
    if ! sed -n '/^diff --git /,$p' "$guard" | "${GIT_APPLY[@]}" --unidiff-zero --check - 2>/dev/null; then
        complain "$name: its break no longer applies to the tree. The fix moved; re-derive with scripts/guard-red-run.sh --record."
    fi
done

# ------------------------------------------------- source tree -> guard file

while IFS= read -r marker_line; do
    [[ -z "$marker_line" ]] && continue
    name="$(printf '%s' "$marker_line" | sed -n 's/.*\/\/ GUARD: \([A-Za-z0-9_-]*\).*/\1/p')"
    if [[ -z "$name" ]]; then
        complain "malformed GUARD marker (expected '// GUARD: <name>'): $marker_line"
        continue
    fi
    if [[ "$known_guards" != *" $name "* ]]; then
        complain "$name: marked in ${marker_line%%:*} but scripts/guards/$name.guard does not exist."
    fi
done < <(grep -rn '// GUARD: ' "${SCAN_ROOTS[@]}" || true)

exit "$status"
