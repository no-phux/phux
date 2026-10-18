#!/usr/bin/env bash
# check-generated-font.sh
#
# Drift gate for the only generated Rust source in the workspace:
# crates/phux-record/src/font/spleen_8x16.rs, produced from the vendored
# Spleen BDF face by `bun tools/main.ts gen-bitmap-font` (tools/src/font/).
#
# WHY a CI gate and not a build.rs. Committing the generated table is a
# deliberate call (ADR-0060): a plain `cargo build` then compiles ordinary
# Rust with no tooling runtime on PATH, no BDF parser in the dependency graph, no
# build-script node added to a 13-crate workspace, and nothing that breaks
# the hermetic nix devshell story. The price of committing a generated
# artifact is that nothing links it back to its source. Before this script
# existed, `grep` for the generator across every workflow, script, and
# the justfile returned zero hits: you could hand-edit a glyph row, or bump
# the .bdf and forget to regenerate, and every other gate stayed green
# because the result is still valid Rust that compiles and renders.
#
# This script pays that price down without paying the build.rs cost: it
# re-runs the generator into a scratch file and byte-compares.
#
# WHY a byte comparison rather than something structural. The generator is
# deterministic by construction -- a pure BDF parse, sorted range output, no
# timestamps, no environment input -- so exact equality is the honest
# equality here. Anything weaker (parse both, compare glyph tables) would
# wave through a whitespace-only or comment-only hand edit, which is exactly
# the "someone tweaked the generated file directly" case this exists to catch.
#
# Usage:
#   bash scripts/check-generated-font.sh     # or: just font-check
#
# Exit codes:
#   0   the committed table matches a fresh regeneration
#   1   drift, or the generator could not run

set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

GENERATOR="tools/src/font/bitmap.ts"
SOURCE="crates/phux-record/assets/spleen-8x16.bdf"
GENERATED="crates/phux-record/src/font/spleen_8x16.rs"

# The fix command, spelled once and printed verbatim on every failure path.
# A drift gate that only says "these differ" makes the reader reconstruct the
# invocation from the generator's docstring; this one hands it over.
FIX="bun tools/main.ts gen-bitmap-font ${SOURCE} ${GENERATED}"

for path in "$GENERATOR" "$SOURCE" "$GENERATED"; do
    if [ ! -f "$path" ]; then
        phux_error "missing ${path}"
        phux_hint "the generated-font gate expects generator, source, and artifact to travel together."
        exit 1
    fi
done

if ! command -v bun >/dev/null 2>&1; then
    # Hard failure, never a skip. A gate that quietly opts out when its
    # runtime is absent is indistinguishable from the hole it replaced.
    # The nix devshell and mise both pin bun precisely so this runs.
    phux_error "bun not found on PATH; cannot verify ${GENERATED}"
    phux_hint "run inside the dev shell:  nix develop -c bash scripts/check-generated-font.sh"
    exit 1
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/phux-font-check.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
regenerated="${TMP}/spleen_8x16.rs"

if ! bun tools/main.ts gen-bitmap-font "$SOURCE" "$regenerated" >/dev/null; then
    phux_error "gen-bitmap-font failed on ${SOURCE}"
    phux_hint "reproduce with:  ${FIX}"
    exit 1
fi

if cmp -s "$GENERATED" "$regenerated"; then
    phux_ok "generated font: ${GENERATED} matches ${SOURCE}"
    exit 0
fi

phux_error "${GENERATED} does not match a fresh regeneration from ${SOURCE}"
echo "" >&2
phux_hint "Either the vendored face was bumped without regenerating, or the"
phux_hint "generated table was edited by hand. It is generated -- do not edit it."
echo "" >&2
phux_hint "Fix with:"
phux_hint "  ${FIX}"
echo "" >&2
phux_hint "then commit the regenerated file alongside the .bdf."
echo "" >&2
# The table is ~112 KB, so an unbounded diff would bury the instructions
# above under thousands of glyph rows. Show enough to identify the change.
phux_hint "First 40 differing lines (committed '<' vs regenerated '>'):"
diff "$GENERATED" "$regenerated" | head -40 >&2 || true
exit 1
