#!/usr/bin/env bash
# Build only the canonical wire codec, using a private fixture target directory.
set -euo pipefail

# Cargo leaves the previous metadata-hash rlib beside the new one after a
# crate-version bump. Pick the newest mtime instead of requiring exactly one
# (phux-pfyn).
newest_rlib() {
    local cand="" f
    for f in "$@"; do
        [[ -e "$f" ]] || continue
        if [[ -z "$cand" || "$f" -nt "$cand" ]]; then
            cand="$f"
        fi
    done
    [[ -n "$cand" ]] || return 1
    printf '%s\n' "$cand"
}

if [[ "${BASH_SOURCE[0]}" != "$0" ]]; then
    return 0
fi

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
TARGET="$ROOT/target/operation-fixtures"
cargo build --locked --manifest-path "$ROOT/Cargo.toml" -p phux-protocol --target-dir "$TARGET"
protocol="$(newest_rlib "$TARGET"/debug/deps/libphux_protocol-*.rlib)"
bytes="$(newest_rlib "$TARGET"/debug/deps/libbytes-*.rlib)"
rustc --edition=2024 "$ROOT/clients/cockpit/src/tests/operation_fixtures.rs" \
    -L "dependency=$TARGET/debug/deps" --extern "phux_protocol=${protocol}" \
    --extern "bytes=${bytes}" -o "$TARGET/generate-operation-fixtures"
"$TARGET/generate-operation-fixtures" "$ROOT/clients/cockpit/src/tests/fixtures"
