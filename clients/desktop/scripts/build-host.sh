#!/usr/bin/env bash
# Build the standalone GPUI desktop at
# clients/desktop/native/target/{debug,release}/phux-desktop.
# The pinned GPUI snapshot is parent-managed (`just desktop-source`).
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
# shellcheck source=scripts/lib/dev-toolchain.sh
source "$root/scripts/lib/dev-toolchain.sh"
check_desktop_toolchain
if [[ "$(uname -s)" == Darwin ]]; then
    # shellcheck source=scripts/lib/macos-desktop.sh
    source "$root/scripts/lib/macos-desktop.sh"
    prepare_macos_desktop
fi
export RUSTUP_TOOLCHAIN="$RUST_CHANNEL"
# Ignore an inherited CARGO_TARGET_DIR. The shipping path is this directory.
export CARGO_TARGET_DIR="$root/clients/desktop/native/target"
profile=debug
case "${1:-}" in
    ""|--debug) profile=debug ;;
    --release) profile=release ;;
    *) echo "usage: $0 [--debug|--release]" >&2; exit 2 ;;
esac
cd "$root"
args=(--locked --manifest-path clients/desktop/native/Cargo.toml --bin phux-desktop)
if [[ "$profile" == release ]]; then
    args+=(--release)
fi
cargo build "${args[@]}"
binary="$CARGO_TARGET_DIR/$profile/phux-desktop"
test -x "$binary"
if [[ "$(uname -s)" == Darwin ]]; then
    PHUX_MACOS_MIN_MAX=26.0 bash "$root/scripts/check-binary-portability.sh" "$binary"
fi
printf '%s\n' "$binary"
