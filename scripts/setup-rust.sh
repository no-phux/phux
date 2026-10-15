#!/usr/bin/env bash
# Use an existing rustup installation without changing an existing default.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib/dev-toolchain.sh"
scope="${1:-core}"
case "$scope" in
    core|native|web|profiling|editor) ;;
    *) phux_error "usage: bash scripts/setup-rust.sh [core|native|web|profiling|editor]"; exit 2 ;;
esac
[[ $# -le 1 ]] || exit 2
command -v rustup >/dev/null 2>&1 || {
    phux_error "rustup is not installed"
    phux_hint "Install rustup from https://rustup.rs (minimal profile); then rerun this command."
    exit 1
}
# The manifest owns the default components; this helper only adds opt-in ones.
components="$(sed -n 's/^components = \[\(.*\)\]/\1/p' "$DEV_ROOT/rust-toolchain.toml" | tr -d '" ')"
[[ -n "$components" ]] || { phux_error 'cannot read rust-toolchain.toml components'; exit 1; }
phux_info "setting up Rust $RUST_CHANNEL ($scope)"
rustup toolchain install "$RUST_CHANNEL" --profile minimal --component "$components"
case "$scope" in
    web) rustup target add --toolchain "$RUST_CHANNEL" wasm32-unknown-unknown ;;
    profiling) rustup component add --toolchain "$RUST_CHANNEL" llvm-tools-preview ;;
    editor) rustup component add --toolchain "$RUST_CHANNEL" rust-src rust-analyzer ;;
esac
phux_ok "Rust $RUST_CHANNEL ($scope) is ready"
