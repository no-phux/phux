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
# SCOPE is the `*_e2e.rs` naming convention, including subsystem harness
# modules under `tests/<harness>/`, not every test target, and that
# is deliberate. Outside that convention an `#[ignore]` can legitimately mean
# "this needs a host tool or a machine property CI does not have". Those are
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
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Recipes live in just/*.just; the root justfile is imports + default.
shopt -s nullglob
RECIPE_FILES=(justfile just/*.just)
if [ ! -f justfile ]; then
    phux_error "justfile not found"
    exit 1
fi

# The lane recipes: everything from `e2e:` to the end of `stress:`. Reading
# every recipe file would let a `binary_id(...)` mentioned in a comment
# elsewhere satisfy the gate; restricting to the recipe bodies keeps the
# evidence to text nextest actually receives. `just --dump` is not used on
# purpose -- it needs `just` on PATH, and this gate runs in jobs that have
# only bash.
lanes="$(cat "${RECIPE_FILES[@]}" | awk '
    /^(e2e|stress):/            { inrecipe = 1 }
    inrecipe && /^[^ \t#]/ && !/^(e2e|stress):/ { inrecipe = 0 }
    inrecipe                    { print }
')"

if [ -z "$lanes" ]; then
    phux_error "found no e2e/stress recipe bodies in just/test.just"
    phux_hint "if those recipes were renamed, update this gate to match."
    exit 1
fi

status=0
checked=0

# Keep scanning the original suite files after harness consolidation. Looking
# only at a subsystem's main.rs would miss every ignore in its child modules.
# Both top-level targets and tests/<harness>/<suite>_e2e.rs are supported;
# deeper or unwired modules fail closed rather than silently losing coverage.
while IFS= read -r path; do
    checked=$((checked + 1))

    crate="${path#crates/}"
    crate="${crate%%/*}"
    stem="$(basename "$path" .rs)"
    relative="${path#crates/"${crate}"/tests/}"
    harness="$stem"
    if [ "$relative" != "${stem}.rs" ]; then
        harness="${relative%%/*}"
        entry="crates/${crate}/tests/${harness}/main.rs"
        if [ "$relative" != "${harness}/${stem}.rs" ] ||
            [ ! -f "$entry" ] ||
            ! grep -Eq "^mod ${stem};$" "$entry"; then
            phux_error "${path} is not wired into a supported subsystem harness"
            phux_hint "expected ${entry} to declare: mod ${stem};"
            status=1
            continue
        fi
    fi
    binary_id="${crate}::${harness}"

    # No `#[ignore]` anywhere in the file means every test in it runs in the
    # default pool, which CI's unit step already executes. Nothing to check.
    if ! grep -q '#\[ignore' "$path"; then
        continue
    fi

    if printf '%s' "$lanes" | grep -Fq "binary_id(${binary_id})"; then
        continue
    fi
    # `just stress` selects by `--test <name>` rather than by binary id.
    if printf '%s' "$lanes" | grep -Eq -- "--test[[:space:]]+${harness}([[:space:]]|$)"; then
        continue
    fi

    status=1
    phux_error "${path} has #[ignore]d tests that no lane runs"
    phux_hint "binary id: ${binary_id}"
    phux_hint "#[ignore]d tests only run where a recipe passes --run-ignored AND"
    phux_hint "names the binary. This one is named nowhere, so it never executes"
    phux_hint "in CI and will stay green no matter what it asserts."
    echo "" >&2
    phux_hint "Fix by adding it to the fast PR lane in ${JUSTFILE} (recipe \`e2e\`):"
    phux_hint "  -E '... + binary_id(${binary_id})'"
    phux_hint "or, if it is too slow for the PR critical path, to the \`stress\`"
    phux_hint "recipe (which runs post-merge and nightly via stress.yml)."
    echo "" >&2
done < <(find crates -type f -path '*/tests/*_e2e.rs' | sort)

if [ "$checked" -eq 0 ]; then
    phux_error "no *_e2e.rs suites found under crates/*/tests/"
    phux_hint "the glob or the naming convention changed; update this gate."
    exit 1
fi

if [ "$status" -eq 0 ]; then
    phux_ok "e2e lane coverage: ${checked} e2e suites, all reachable"
fi

exit "$status"
