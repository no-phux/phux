#!/usr/bin/env bash
# Use an existing rustup installation without changing an existing default.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib/dev-toolchain.sh"
scope="${1:-core}"
case "$scope" in
    core|native|cockpit|web|profiling|editor) ;;
    *) echo "usage: bash scripts/setup-rust.sh [core|native|cockpit|web|profiling|editor]" >&2; exit 2 ;;
esac
[[ $# -le 1 ]] || exit 2
command -v rustup >/dev/null 2>&1 || {
    echo "Install rustup from https://rustup.rs (minimal profile); then rerun this command." >&2
    exit 1
}
# The manifest owns the default components; this helper only adds opt-in ones.
components="$(sed -n 's/^components = \[\(.*\)\]/\1/p' "$DEV_ROOT/rust-toolchain.toml" | tr -d '" ')"
[[ -n "$components" ]] || { echo 'cannot read rust-toolchain.toml components' >&2; exit 1; }
rustup toolchain install "$RUST_CHANNEL" --profile minimal --component "$components"
case "$scope" in
    web) rustup target add --toolchain "$RUST_CHANNEL" wasm32-unknown-unknown ;;
    profiling) rustup component add --toolchain "$RUST_CHANNEL" llvm-tools-preview ;;
    editor) rustup component add --toolchain "$RUST_CHANNEL" rust-src rust-analyzer ;;
esac
