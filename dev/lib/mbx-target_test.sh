#!/usr/bin/env bash
# Hermetic: a dangling mbx target symlink gets its directory back, and a
# dangling symlink that is not mbx's cache is not created.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=mbx-target.sh
. "$ROOT/dev/lib/mbx-target.sh"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/mbx-target-test.XXXXXX")"
trap 'rm -rf -- "$WORK"' EXIT
FAILURES=0

ok() { printf '  ok: %s\n' "$*"; }
bad() { printf '  FAILED: %s\n' "$*" >&2; FAILURES=$((FAILURES + 1)); }

# ------------------------------------------------------------- dangling mbx
MBX="$WORK/Caches/mbx/targets/v1/abc"
WT="$WORK/wt"
mkdir -p "$WT"
ln -s "$MBX" "$WT/target"
mbx_heal_target "$WT"
if [[ -d "$MBX" && -L "$WT/target" ]]; then
    ok "recreates a pruned mbx target directory"
else
    bad "mbx destination was not recreated"
fi

# A second call is a no-op once the directory exists.
mbx_heal_target "$WT"
if [[ -d "$MBX" ]]; then
    ok "leaves an existing mbx target alone"
else
    bad "second heal removed the directory"
fi

# ------------------------------------------------------------- not our link
OTHER="$WORK/not-mbx/missing"
ln -sfn "$OTHER" "$WT/target"
mbx_heal_target "$WT"
if [[ ! -e "$OTHER" ]]; then
    ok "does not create a non-mbx destination"
else
    bad "created a destination outside the mbx cache"
fi

# ------------------------------------------------------------- real directory
rm "$WT/target"
mkdir -p "$WT/target"
mbx_heal_target "$WT"
if [[ -d "$WT/target" && ! -L "$WT/target" ]]; then
    ok "leaves a real target directory alone"
else
    bad "rewrote a real target directory"
fi

if [[ "$FAILURES" -ne 0 ]]; then
    printf '%s failure(s)\n' "$FAILURES" >&2
    exit 1
fi
printf 'mbx-target: ok\n'
