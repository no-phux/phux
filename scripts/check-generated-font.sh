#!/usr/bin/env bash
# check-generated-font.sh
#
# Drift gate for the only generated Rust source in the workspace:
# crates/phux-record/src/font/spleen_8x16.rs, produced from the vendored
# Spleen BDF face by scripts/gen-bitmap-font.py.
#
# WHY a CI gate and not a build.rs. Committing the generated table is a
# deliberate call (ADR-0060): a plain `cargo build` then compiles ordinary
# Rust with no Python on PATH, no BDF parser in the dependency graph, no
# build-script node added to a 13-crate workspace, and nothing that breaks
# the hermetic nix devshell story. The price of committing a generated
# artifact is that nothing links it back to its source. Before this script
# existed, `grep` for gen-bitmap-font.py across every workflow, script, and
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

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

GENERATOR="scripts/gen-bitmap-font.py"
SOURCE="crates/phux-record/assets/spleen-8x16.bdf"
GENERATED="crates/phux-record/src/font/spleen_8x16.rs"

# The fix command, spelled once and printed verbatim on every failure path.
# A drift gate that only says "these differ" makes the reader reconstruct the
# invocation from the generator's docstring; this one hands it over.
FIX="python3 ${GENERATOR} ${SOURCE} ${GENERATED}"

for path in "$GENERATOR" "$SOURCE" "$GENERATED"; do
    if [ ! -f "$path" ]; then
        echo "error: missing ${path}" >&2
        echo "  the generated-font gate expects generator, source, and artifact to travel together." >&2
        exit 1
    fi
done

if ! command -v python3 >/dev/null 2>&1; then
    # Hard failure, never a skip. A gate that quietly opts out when its
    # interpreter is absent is indistinguishable from the hole it replaced.
    # python3 is in the nix devshell (flake.nix) precisely so this runs.
    echo "error: python3 not found on PATH; cannot verify ${GENERATED}" >&2
    echo "  run inside the dev shell:  nix develop -c bash scripts/check-generated-font.sh" >&2
    exit 1
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/phux-font-check.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
regenerated="${TMP}/spleen_8x16.rs"

if ! python3 "$GENERATOR" "$SOURCE" "$regenerated" >/dev/null; then
    echo "error: ${GENERATOR} failed on ${SOURCE}" >&2
    echo "  reproduce with:  ${FIX}" >&2
    exit 1
fi

if cmp -s "$GENERATED" "$regenerated"; then
    echo "generated font ok: ${GENERATED} matches ${SOURCE}"
    exit 0
fi

echo "error: ${GENERATED} does not match a fresh regeneration from ${SOURCE}" >&2
echo "" >&2
echo "  Either the vendored face was bumped without regenerating, or the" >&2
echo "  generated table was edited by hand. It is generated -- do not edit it." >&2
echo "" >&2
echo "  Fix with:" >&2
echo "    ${FIX}" >&2
echo "" >&2
echo "  then commit the regenerated file alongside the .bdf." >&2
echo "" >&2
# The table is ~112 KB, so an unbounded diff would bury the instructions
# above under thousands of glyph rows. Show enough to identify the change.
echo "  First 40 differing lines (committed '<' vs regenerated '>'):" >&2
diff "$GENERATED" "$regenerated" | head -40 >&2 || true
exit 1
