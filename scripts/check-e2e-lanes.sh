#!/usr/bin/env bash
# check-e2e-lanes.sh
#
# Coverage gate for the `just e2e` nextest filterset.
#
# THE HOLE THIS CLOSES. `#[ignore]`d tests do not run in the default pool
# (`cargo nextest run --workspace`, which is CI's unit step). They run only
# where a lane passes `--run-ignored` and names their binary. `just e2e`
# names binaries explicitly, one `binary_id(...)` per line, and that list is
# hand-maintained. A new `*_e2e.rs` full of `#[ignore]`d tests therefore
# compiles, passes review, and then never executes anywhere -- green forever,
# testing nothing. That is not hypothetical: phux/tests/upgrade_e2e.rs
# shipped with a module doc claiming "run via `just e2e`" while absent from
# the filterset, and plugin_agent_bench_e2e and workspace_archive_e2e were in
# the same state.
#
# WHY NOT DERIVE THE LIST. Two reasons the filterset cannot simply be
# generated from `crates/*/tests/*_e2e.rs`:
#   1. It is a curated PR-critical-path subset by design. The heavy
#      `stress_*` storms are deliberately excluded (CPU-starved on a 2-core
#      runner; they live in stress.yml), and a future expensive e2e binary
#      must be able to make the same choice.
#   2. The filterset is split across two nextest invocations with different
#      `--run-ignored` modes (all vs ignored-only), so "the list" is not one
#      list to begin with.
# So the contract enforced here is the weaker, checkable one: every e2e
# binary must run SOMEWHERE. A binary with no `#[ignore]` already runs in the
# default pool and needs no entry; a binary that has at least one `#[ignore]`
# must be named by a lane recipe, or those tests are dead weight.
#
# SCOPE is the `*_e2e.rs` naming convention, not every test target, and that
# is deliberate. Outside that convention an `#[ignore]` can legitimately mean
# "this needs a host tool or a machine property CI does not have" -- e.g.
# phux-server's kip_roundtrip probe needs a host-provided htop, and one
# replay_equivalence case is ignored with a recorded finding. Those are
# correct as unrun; forcing them into a lane would mean weakening them or
# maintaining an exemption list, which is the very failure mode this gate
# exists to remove. `*_e2e.rs` carries the opposite intent by convention:
# spawn a real server, run in a lane.
#
# Scanning the justfile text (rather than asking nextest) keeps this
# compile-free: it is a seconds-long gate that can run on the docs-only fast
# path and in a job that never builds the workspace.
#
# Usage:
#   bash scripts/check-e2e-lanes.sh     # or: just e2e-lane-check
#
# Exit codes:
#   0   every e2e binary runs in some lane
#   1   an e2e binary carries #[ignore]d tests that no lane selects

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

JUSTFILE="justfile"

if [ ! -f "$JUSTFILE" ]; then
    echo "error: ${JUSTFILE} not found" >&2
    exit 1
fi

# The lane recipes: everything from `e2e:` to the end of `stress:`. Reading
# the whole justfile would let a `binary_id(...)` mentioned in a comment
# elsewhere satisfy the gate; restricting to the recipe bodies keeps the
# evidence to text nextest actually receives. `just --dump` is not used on
# purpose -- it needs `just` on PATH, and this gate runs in jobs that have
# only bash.
lanes="$(awk '
    /^(e2e|stress):/            { inrecipe = 1 }
    inrecipe && /^[^ \t#]/ && !/^(e2e|stress):/ { inrecipe = 0 }
    inrecipe                    { print }
' "$JUSTFILE")"

if [ -z "$lanes" ]; then
    echo "error: found no e2e/stress recipe bodies in ${JUSTFILE}" >&2
    echo "  if those recipes were renamed, update this gate to match." >&2
    exit 1
fi

status=0
checked=0

# `crates/*/tests/*_e2e.rs` is the naming convention; a file there is one
# cargo test target, and its binary id is `<crate>::<file stem>`.
for path in crates/*/tests/*_e2e.rs; do
    [ -e "$path" ] || continue
    checked=$((checked + 1))

    crate="${path#crates/}"
    crate="${crate%%/*}"
    stem="$(basename "$path" .rs)"
    binary_id="${crate}::${stem}"

    # No `#[ignore]` anywhere in the file means every test in it runs in the
    # default pool, which CI's unit step already executes. Nothing to check.
    if ! grep -q '#\[ignore' "$path"; then
        continue
    fi

    if printf '%s' "$lanes" | grep -Fq "binary_id(${binary_id})"; then
        continue
    fi
    # `just stress` selects by `--test <name>` rather than by binary id.
    if printf '%s' "$lanes" | grep -Eq -- "--test[[:space:]]+${stem}([[:space:]]|$)"; then
        continue
    fi

    status=1
    echo "error: ${path} has #[ignore]d tests that no lane runs" >&2
    echo "  binary id: ${binary_id}" >&2
    echo "  #[ignore]d tests only run where a recipe passes --run-ignored AND" >&2
    echo "  names the binary. This one is named nowhere, so it never executes" >&2
    echo "  in CI and will stay green no matter what it asserts." >&2
    echo "" >&2
    echo "  Fix by adding it to the fast PR lane in ${JUSTFILE} (recipe \`e2e\`):" >&2
    echo "    -E '... + binary_id(${binary_id})'" >&2
    echo "  or, if it is too slow for the PR critical path, to the \`stress\`" >&2
    echo "  recipe (which runs post-merge and nightly via stress.yml)." >&2
    echo "" >&2
done

if [ "$checked" -eq 0 ]; then
    echo "error: no crates/*/tests/*_e2e.rs files found" >&2
    echo "  the glob or the naming convention changed; update this gate." >&2
    exit 1
fi

if [ "$status" -eq 0 ]; then
    echo "e2e lane coverage ok: ${checked} e2e binaries, all reachable"
fi

exit "$status"
