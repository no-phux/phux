#!/usr/bin/env bash
# Build only the canonical wire codec, using a private fixture target directory.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)"
TARGET="$ROOT/target/operation-fixtures"
cargo build --locked --manifest-path "$ROOT/Cargo.toml" -p phux-protocol --target-dir "$TARGET"
protocol=("$TARGET"/debug/deps/libphux_protocol-*.rlib)
bytes=("$TARGET"/debug/deps/libbytes-*.rlib)
[[ ${#protocol[@]} == 1 && ${#bytes[@]} == 1 ]]
rustc --edition=2024 "$ROOT/clients/cockpit/src/tests/operation_fixtures.rs" \
    -L "dependency=$TARGET/debug/deps" --extern "phux_protocol=${protocol[0]}" \
    --extern "bytes=${bytes[0]}" -o "$TARGET/generate-operation-fixtures"
"$TARGET/generate-operation-fixtures" "$ROOT/clients/cockpit/src/tests/fixtures"
