#!/usr/bin/env bash
# Hermetic coverage of the persisted-field guard in check-upgrade-fixtures.sh.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/check-upgrade-fixtures-test.XXXXXX")"
trap 'rm -rf -- "$WORK"' EXIT
FAILURES=0
blob=crates/phux-server/src/upgrade/blob.rs

g() { git -C "$WORK" -c user.name=t -c user.email=t@t -c commit.gpgsign=false "$@"; }

mkdir -p "$WORK/scripts/lib" "$WORK/$(dirname "$blob")"
cp "$ROOT/scripts/check-upgrade-fixtures.sh" "$WORK/scripts/"
cp "$ROOT/scripts/lib/output.sh" "$WORK/scripts/lib/"
cat >"$WORK/$blob" <<'EOF'
pub struct Pane {
    pub terminal_id: u64,
    pub(crate) grid_rows: u16,
    #[serde(default)]
    title: String,
}
EOF
g init -q -b main
g add .
g commit -q -m base

expect() {
    local want="$1" label="$2" got=0
    bash "$WORK/scripts/check-upgrade-fixtures.sh" main 2>"$WORK/err" >/dev/null || got=$?
    if [[ "$got" == "$want" ]]; then
        printf '  ok: %s\n' "$label"
    else
        printf '  FAILED: %s (exit %s): %s\n' "$label" "$got" "$(<"$WORK/err")" >&2
        FAILURES=$((FAILURES + 1))
    fi
}

g checkout -q -b change
expect 0 'an unchanged blob passes'

cat >"$WORK/$blob" <<'EOF'
pub struct Pane {
    pub pane_id: u64,
    pub(crate) grid_rows: u16,
    #[serde(default)]
    title: String,
}
EOF
g add .
g commit -q -m rename
expect 1 'a bare field rename fails'

cat >"$WORK/$blob" <<'EOF'
pub struct Pane {
    #[serde(rename = "terminal_id")]
    pub pane_id: u64,
    pub(crate) grid_rows: u16,
    #[serde(default)]
    title: String,
}
EOF
g add .
g commit -q -m keep-name
expect 0 'a rename that keeps the persisted name passes'

cat >"$WORK/$blob" <<'EOF'
pub struct Pane {
    #[serde(rename = "terminal_id")]
    pub pane_id: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    pub(crate) grid_rows: u32,
}
EOF
g add .
g commit -q -m reorder
expect 0 'moving or retyping a field passes'

if ((FAILURES > 0)); then
    printf 'check-upgrade-fixtures_test: %d failure(s)\n' "$FAILURES" >&2
    exit 1
fi
printf 'check-upgrade-fixtures_test: ok\n'
