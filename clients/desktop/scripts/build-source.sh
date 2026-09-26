#!/usr/bin/env bash
# Build the source-locked framework with phux's Rust pin and Apple's Metal tools.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
# shellcheck source=scripts/lib/apple-toolchain-env.sh
source "$root/scripts/lib/apple-toolchain-env.sh"
# shellcheck source=scripts/lib/dev-toolchain.sh
source "$root/scripts/lib/dev-toolchain.sh"
export RUSTUP_TOOLCHAIN="$RUST_CHANNEL"
source_dir="$root/clients/desktop/toolchain/gpuix"
cd "$source_dir/packages/native"
bun x --no-install napi build --platform --esm --js index.js --release --features test-support -- --locked
bun x --no-install napi build --platform --js index.cjs --release --features test-support -- --locked
bun run build:js
cd "$source_dir/packages/solid"
bun run build
