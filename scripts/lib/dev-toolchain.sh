#!/usr/bin/env bash
# Shared reads of existing pins: no second Rust/Zig version registry.
# Sourced by the setup helpers and doctor (Bash 3.2 compatible).
DEV_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RUST_CHANNEL="$(sed -n 's/^channel = "\([^"]*\)"/\1/p' "$DEV_ROOT/rust-toolchain.toml")"
ZIG_VERSION="$(sed -n 's/^[[:space:]]*ZIG_VERSION: \([^[:space:]]*\).*/\1/p' "$DEV_ROOT/.github/workflows/release.yml" | head -n 1)"
if [[ -z "$RUST_CHANNEL" || -z "$ZIG_VERSION" ]]; then
    echo "cannot read Rust/Zig pins; check rust-toolchain.toml and release.yml" >&2
    return 1
fi

zig_digest() {
    # The release matrix owns the audited archive hashes. Never fetch the
    # expected checksum from the same endpoint as the compiler at install time.
    awk -v target="$1" '
        index($0, "archive=\"zig-" target "-${ZIG_VERSION}.tar.xz\"") { found=1; next }
        found && /sha="/ { split($0, parts, "\""); print parts[2]; exit }
    ' "$DEV_ROOT/.github/workflows/release.yml"
}
