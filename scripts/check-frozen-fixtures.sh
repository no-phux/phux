#!/usr/bin/env bash
# Keep frozen fixtures frozen.
#
# Each file under crates/*/tests/fixtures/frozen/ and
# crates/phux-server/tests/fixtures/upgrade-blob/ is something a released image
# wrote and a later one must read. They are the only proof that a rename is
# safe, and a sweeping rename would otherwise rewrite the proof along with the
# code. Add a fixture; never edit, move, or delete one (crates/phux-frozen).
set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"
cd "$(dirname "${BASH_SOURCE[0]}")/.."

base="${1:-origin/main}"
if ! merge_base="$(git merge-base "$base" HEAD 2>/dev/null)"; then
    phux_error "frozen-fixtures: no merge base with $base, so nothing was checked."
    exit 1
fi
changed="$(git diff --name-only --no-renames --diff-filter=MDT "$merge_base" -- \
    ':(glob)crates/*/tests/fixtures/frozen/**' \
    crates/phux-server/tests/fixtures/upgrade-blob)"
if [[ -n "$changed" ]]; then
    printf '%s\n' "$changed" >&2
    phux_error 'frozen-fixtures: frozen fixtures changed.'
    phux_hint 'Restore them and freeze the new shape in a new fixture; see crates/phux-frozen.'
    exit 1
fi
phux_ok 'frozen-fixtures: frozen fixtures are unchanged'
