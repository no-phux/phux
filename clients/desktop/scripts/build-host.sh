#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
# shellcheck source=scripts/lib/apple-toolchain-env.sh
source "$root/scripts/lib/apple-toolchain-env.sh"
# shellcheck source=scripts/lib/dev-toolchain.sh
source "$root/scripts/lib/dev-toolchain.sh"
export RUSTUP_TOOLCHAIN="$RUST_CHANNEL"
export CARGO_TARGET_DIR="$root/clients/desktop/.cache/host-target"
output=clients/desktop/.cache/host
features=()
case "${1:-}" in
    "") ;;
    --terminal-fixtures)
        output=clients/desktop/.cache/host-fixtures
        features=(--features "terminal-fixtures,gpuix-native/test-support")
        ;;
    *) echo "usage: $0 [--terminal-fixtures]" >&2; exit 2 ;;
esac
cd "$root"
clients/desktop/toolchain/gpuix/packages/native/node_modules/.bin/napi build \
    --manifest-path clients/desktop/native/Cargo.toml \
    --config-path clients/desktop/native/napi.json \
    --package-json-path clients/desktop/toolchain/gpuix/packages/native/package.json \
    --output-dir "$output" --platform --esm --js index.mjs \
    --release "${features[@]}" -- --locked
if [[ -z "${1:-}" ]]; then
    bun clients/desktop/native/check-generated.mjs
fi
