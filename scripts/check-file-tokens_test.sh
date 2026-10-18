#!/usr/bin/env bash
# Hermetic coverage for the staged-file token gate (check-file-tokens.sh): a
# scratch repo with an oversized file on both sides of a merge, exercised as
# an ordinary commit (the ratchet holds), as a merge that only carries the
# file (passes), and as a merge that introduces a new oversized file (fails).
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
GATE="${ROOT}/scripts/check-file-tokens.sh"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/check-file-tokens-test.XXXXXX")"
trap 'rm -rf -- "$WORK"' EXIT
FAILURES=0

ok() { printf '  ok: %s\n' "$*"; }
bad() { printf '  FAILED: %s\n' "$*" >&2; FAILURES=$((FAILURES + 1)); }

REPO="${WORK}/repo"
mkdir -p "$REPO"
g() { git -C "$REPO" -c user.name=t -c user.email=t@t "$@"; }
gate() { (cd "$REPO" && bash "$GATE" 2>"${WORK}/gate.err"); }
oversized() { head -c 25000 /dev/zero | tr '\0' 'x' >"$1"; echo >>"$1"; }
small() { printf '%s\n' "$2" >"$1"; }

g init -q -b main
small "${REPO}/README.md" seed
g add -A && g commit -q -m seed

# ------------------------------------------- an ordinary commit ratchets
oversized "${REPO}/big.md"
g add -A
if gate; then bad "an oversized file in an ordinary commit must be rejected"; else ok "ordinary commit: oversized file rejected"; fi
if grep -q 'big.md' "${WORK}/gate.err"; then ok "the rejection names the file"; else bad "the rejection names the file"; fi
g -c core.hooksPath=/dev/null commit -q -m "big on main"

# A branch that grows the same oversized file; main moves on independently.
g checkout -q -b feature
echo "feature line" >>"${REPO}/big.md"
small "${REPO}/feature.md" feature
g add -A && g -c core.hooksPath=/dev/null commit -q -m "feature touches big"
g checkout -q main
echo "main line" >>"${REPO}/big.md"
g add -A && g -c core.hooksPath=/dev/null commit -q -m "main touches big"

# ------------------------------------- a merge carrying the file passes
g checkout -q feature
if g merge -q --no-commit main >/dev/null 2>&1; then bad "fixture: big.md was expected to conflict"; fi
{ head -c 25000 /dev/zero | tr '\0' 'x'; echo; echo "feature line"; echo "main line"; } >"${REPO}/big.md"
g add big.md
g rev-parse -q --verify MERGE_HEAD >/dev/null || bad "fixture: not in a merge"
if gate; then ok "merge: a file oversized in both parents is not the merge's to split"; else bad "merge: inherited oversized file rejected: $(cat "${WORK}/gate.err")"; fi

# A file oversized in only one parent is still inherited, not introduced.
small "${REPO}/big.md" shrunk
g add big.md
oversized "${REPO}/only-main.md"
g add only-main.md
g -c core.hooksPath=/dev/null commit -q --no-verify -m "merge main" >/dev/null 2>&1 || true
g checkout -q main
g merge -q --no-ff --no-commit feature >/dev/null 2>&1 || true
g rev-parse -q --verify MERGE_HEAD >/dev/null || bad "fixture: not in a merge"
if gate; then ok "merge: a file oversized in one parent passes"; else bad "merge: file oversized in one parent rejected: $(cat "${WORK}/gate.err")"; fi

# ------------------------------- a merge introducing a new oversized file fails
oversized "${REPO}/born-in-merge.md"
g add born-in-merge.md
if gate; then bad "merge: a newly introduced oversized file must be rejected"; else ok "merge: newly introduced oversized file rejected"; fi
if grep -q 'born-in-merge.md' "${WORK}/gate.err"; then ok "the rejection names the new file"; else bad "the rejection names the new file"; fi
if grep -q 'only-main.md' "${WORK}/gate.err"; then bad "the inherited file must not be listed beside the new one"; else ok "the inherited file is not listed"; fi

# ------------------------------------------ exemptions still apply in a merge
g rm -q --cached born-in-merge.md && rm "${REPO}/born-in-merge.md"
oversized "${REPO}/vendored.md"
printf 'vendored.md linguist-vendored=true\n' >"${REPO}/.gitattributes"
g add .gitattributes vendored.md
if gate; then ok "merge: .gitattributes exemption still honoured"; else bad "merge: vendored exemption lost: $(cat "${WORK}/gate.err")"; fi

if ((FAILURES > 0)); then
    printf 'check-file-tokens_test: %d failure(s)\n' "$FAILURES" >&2
    exit 1
fi
printf 'check-file-tokens_test: ok\n'
