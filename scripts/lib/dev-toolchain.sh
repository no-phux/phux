#!/usr/bin/env bash
# Shared reads of existing pins: no second Rust/Zig version registry.
# Sourced by the setup helpers and doctor (Bash 3.2 compatible).
DEV_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RUST_CHANNEL="$(sed -n 's/^channel = "\([^"]*\)"/\1/p' "$DEV_ROOT/rust-toolchain.toml")"
ZIG_PINS="$DEV_ROOT/.config/zig-toolchain.json"
ZIG_VERSION="$(sed -n 's/^[[:space:]]*"version": "\([^"]*\)",*/\1/p' "$ZIG_PINS")"
if [[ -z "$RUST_CHANNEL" || -z "$ZIG_VERSION" ]]; then
    echo "cannot read Rust/Zig pins; check rust-toolchain.toml and .config/zig-toolchain.json" >&2
    return 1
fi

# Check before the desktop's Apple normalization changes PATH: otherwise its
# doctor can pass with rustup while desktop-cli still runs a mixed environment.
check_desktop_toolchain() {
    local cargo_path rustc_path version
    cargo_path="$(command -v cargo || true)"
    rustc_path="$(command -v rustc || true)"
    if [[ -n "${IN_NIX_SHELL:-}" ]] &&
        { [[ "${PHUX_ENV:-}" == mise ]] || [[ "$cargo_path" == */mise/* || "$cargo_path" == */command-wrappers/bin/cargo ]] || [[ "$rustc_path" == */mise/* ]]; }; then
        printf '%s\n' 'phux: mixed Nix/Mise desktop toolchain; refusing to build.' \
            'Select PHUX_ENV=mise in .envrc.local, then direnv reload (or leave nix develop and open a clean Mise shell).' \
            'mise exec changes PATH but does not unload Nix SDK/compiler variables.' >&2
        return 1
    fi
    version="$(cd "$DEV_ROOT" && RUSTUP_AUTO_INSTALL=0 rustc --version 2>/dev/null || true)"
    if [[ "$version" != "rustc $RUST_CHANNEL "* ]]; then
        printf 'phux: desktop requires Rust %s (found: %s); run mise install or reload nix develop.\n' \
            "$RUST_CHANNEL" "${version:-none}" >&2
        return 1
    fi
}

zig_digest() {
    # The toolchain manifest owns the audited archive hashes. Never fetch the
    # expected checksum from the same endpoint as the compiler at install time.
    awk -v target="$1" '
        index($0, "\"" target "\": {") { found=1; next }
        found && /"sha256":/ { split($0, parts, "\""); print parts[4]; exit }
        found && /}/ { exit }
    ' "$ZIG_PINS"
}
