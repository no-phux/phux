#!/usr/bin/env bash
# Check only the selected work area's prerequisites. Never install or build.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib/dev-toolchain.sh"
scope="${1:-native}"
case "$scope" in
    docs|core|native|integrations|web|cockpit|ci) ;;
    *) echo "usage: bash scripts/doctor.sh [docs|core|native|integrations|web|cockpit|ci]" >&2; exit 2 ;;
esac
[[ $# -le 1 ]] || exit 2
cd "$DEV_ROOT"
# Rustup proxies must not turn a diagnostic into an implicit toolchain download.
export RUSTUP_AUTO_INSTALL=0
failures=0
ok() { printf 'ok: %s\n' "$1"; }
fail() { printf 'missing/incompatible: %s\n  remedy: %s\n' "$1" "$2" >&2; failures=$((failures + 1)); }
need() {
    if command -v "$1" >/dev/null 2>&1; then ok "$1"; else fail "$1" "$2"; fi
}
rust_tools() {
    local version
    version="$(rustc --version 2>/dev/null || true)"
    if [[ "$version" == "rustc $RUST_CHANNEL "* ]]; then ok "$version"; else
        fail "Rust $RUST_CHANNEL (found: ${version:-none})" "bash scripts/setup-rust.sh $1"
    fi
    if cargo --version >/dev/null 2>&1 && cargo fmt --version >/dev/null 2>&1 && cargo clippy --version >/dev/null 2>&1; then
        ok "Cargo, rustfmt, clippy"
    else fail "Cargo/rustfmt/clippy" "bash scripts/setup-rust.sh $1"; fi
    need cc 'Install platform compiler tools; see docs/SETUP.md#platform-packages'
    if [[ "$(uname -s)" == Linux ]]; then
        need mold 'sudo apt-get install -y mold (required by .cargo/config.toml on Linux GNU)'
    fi
}
zig_tool() {
    local version
    version="$(zig version 2>/dev/null || true)"
    if [[ "$version" == "$ZIG_VERSION" ]]; then ok "Zig $version"; else
        fail "Zig $ZIG_VERSION (found: ${version:-none})" 'export PATH="$(bash scripts/install-zig.sh "${XDG_DATA_HOME:-$HOME/.local/share}/phux/toolchains"):$PATH"'
    fi
}
mac_sdk() {
    if [[ "$(uname -s)" == Darwin ]]; then
        if xcrun --show-sdk-path >/dev/null 2>&1 && xcrun --find nmedit >/dev/null 2>&1; then
            ok "macOS SDK and nmedit"
        else fail "macOS SDK/nmedit" 'Install/select full Xcode: sudo xcode-select -s /Applications/Xcode.app/Contents/Developer'; fi
    fi
}
native_tools() {
    rust_tools native
    zig_tool
    need pkg-config 'brew install pkgconf; or sudo apt-get install -y pkg-config'
    mac_sdk
}
node_tools() {
    local version
    version="$(node -p 'process.versions.node.split(".")[0]' 2>/dev/null || true)"
    if [[ "$version" =~ ^[0-9]+$ ]] && [[ "$version" -ge 24 ]]; then ok "Node $version"; else
        fail 'Node 24+ (CI uses 24)' 'Install Node 24 LTS from https://nodejs.org or brew install node@24; put its bin directory on PATH'
    fi
    need npm 'Install npm with Node 24; see docs/SETUP.md'
}

need git 'Install Git with your platform package manager'
case "$scope" in
    docs)
        for tool in awk sed grep find sort; do need "$tool" 'Install standard Unix utilities'; done
        ;;
    core) rust_tools core ;;
    native) native_tools ;;
    integrations) node_tools ;;
    web)
        rust_tools web
        node_tools
        need wasm-pack 'cargo install --locked wasm-pack --version 0.15.0'
        need wasm-opt 'brew install binaryen; or sudo apt-get install -y binaryen'
        bindgen="$(sed -n 's/^wasm-bindgen = "=\([^"]*\)"/\1/p' clients/phux-web/Cargo.toml)"
        if [[ -n "$bindgen" && "$(wasm-bindgen --version 2>/dev/null || true)" == "wasm-bindgen $bindgen" ]]; then
            ok "wasm-bindgen $bindgen"
        else fail "wasm-bindgen CLI $bindgen" "cargo install --locked wasm-bindgen-cli --version $bindgen"; fi
        sysroot="$(rustc --print sysroot 2>/dev/null || true)"
        if [[ -n "$sysroot" && -d "$sysroot/lib/rustlib/wasm32-unknown-unknown/lib" ]]; then ok 'Rust WASM target'; else
            fail 'Rust WASM target' 'bash scripts/setup-rust.sh web'
        fi
        if [[ -f clients/phux-vt-web/vendor/ghostty-vt.wasm ]]; then ok 'Committed WASM engine present (run tests to verify ABI)'; else
            fail 'committed ghostty-vt.wasm' 'Restore clients/phux-vt-web/vendor/ghostty-vt.wasm from the checkout; see docs/SETUP.md#browser-client'
        fi
        ;;
    cockpit)
        native_tools
        node_tools
        need python3 'brew install python'
        if [[ "$(uname -s)/$(uname -m)" != Darwin/arm64 ]]; then
            fail 'Cockpit app requires Apple-silicon macOS' 'Use an Apple-silicon Mac for the app and host-rendering checks'
        fi
        ;;
    ci)
        native_tools
        node_tools
        for tool in just actionlint shellcheck jq python3 curl; do need "$tool" 'See docs/SETUP.md#full-root-validation'; done
        if [[ "${BASH_VERSINFO[0]}" -lt 4 ]]; then fail 'Bash 4+ for workflow routing checks' 'brew install bash; put Homebrew bin on PATH'; fi
        for tool in nextest deny; do
            if cargo "$tool" --version >/dev/null 2>&1; then ok "cargo-$tool"; else
                fail "cargo-$tool" "Install the prebuilt cargo-$tool binary; see docs/SETUP.md#full-root-validation"
            fi
        done
        ;;
esac
printf 'doctor %s: %d problem(s). See docs/SETUP.md.\n' "$scope" "$failures"
[[ "$failures" -eq 0 ]]
