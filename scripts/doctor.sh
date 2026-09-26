#!/usr/bin/env bash
# Check only the selected work area's prerequisites. Never install, and never
# build anything but a throwaway hello-world link probe in a temp directory.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib/dev-toolchain.sh"
scope="${1:-native}"
case "$scope" in
    docs|core|native|integrations|web|cockpit|desktop|ci) ;;
    *) echo "usage: bash scripts/doctor.sh [docs|core|native|integrations|web|cockpit|desktop|ci]" >&2; exit 2 ;;
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
# Prefer the environment the caller is in. Native helpers are CI fallbacks,
# not the human path (docs/SETUP.md).
env_remedy() {
    local fallback="$1"
    if [[ -n "${IN_NIX_SHELL:-}" ]]; then
        printf 'nix develop should provide this; check flake.nix\n  native fallback: %s' "$fallback"
    else
        printf 'mise install\n  or: nix develop\n  native fallback: %s' "$fallback"
    fi
}
rust_tools() {
    local version
    version="$(rustc --version 2>/dev/null || true)"
    if [[ "$version" == "rustc $RUST_CHANNEL "* ]]; then ok "$version"; else
        fail "Rust $RUST_CHANNEL (found: ${version:-none})" "$(env_remedy "bash scripts/setup-rust.sh $1")"
    fi
    if cargo --version >/dev/null 2>&1 && cargo fmt --version >/dev/null 2>&1 && cargo clippy --version >/dev/null 2>&1; then
        ok "Cargo, rustfmt, clippy"
    else fail "Cargo/rustfmt/clippy" "$(env_remedy "bash scripts/setup-rust.sh $1")"; fi
    need cc 'Install platform compiler tools; see docs/SETUP.md#platform-packages'
    rust_link_probe
    if [[ "$(uname -s)" == Linux ]]; then
        need mold 'sudo apt-get install -y mold (required by .cargo/config.toml on Linux GNU)'
    fi
}
# Every tool can be present and still not link: a linker older than the
# selected macOS SDK rejects its .tbd stubs ("unknown architecture"). Only a
# real link finds that, so link the smallest program there is.
# Cargo's per-target linker override is honored so the probe links the way
# `cargo build` will.
rust_link_probe() {
    local dir detail host linker_var linker
    dir="$(mktemp -d "${TMPDIR:-/tmp}/phux-doctor.XXXXXX")"
    printf 'fn main() {}\n' >"$dir/probe.rs"
    host="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')"
    linker_var="CARGO_TARGET_$(printf '%s' "$host" | tr 'a-z-' 'A-Z_')_LINKER"
    linker="${host:+${!linker_var:-}}"
    if rustc ${linker:+-C "linker=$linker"} -o "$dir/probe" "$dir/probe.rs" >"$dir/log" 2>&1 &&
        "$dir/probe"; then
        ok 'Rust links and runs a binary'
    else
        detail="$(grep -m1 -E 'unknown architecture|symbol\(s\) not found' "$dir/log" ||
            grep -m1 -E 'error' "$dir/log" || true)"
        fail "Rust link probe (${detail:-no output})" \
            'Make cc and the SDK agree: open a clean shell so no SDKROOT/DEVELOPER_DIR is inherited, and see docs/SETUP.md#platform-packages'
    fi
    rm -rf "$dir"
}
zig_tool() {
    local version
    version="$(zig version 2>/dev/null || true)"
    if [[ "$version" == "$ZIG_VERSION" ]]; then ok "Zig $version"; else
        fail "Zig $ZIG_VERSION (found: ${version:-none})" "$(env_remedy 'export PATH="$(bash scripts/install-zig.sh "${XDG_DATA_HOME:-$HOME/.local/share}/phux/toolchains"):$PATH"')"
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
        fail 'Node 24+ (CI uses 24)' "$(env_remedy 'Install Node 24 LTS from https://nodejs.org or brew install node@24; put its bin directory on PATH')"
    fi
    need npm 'Install npm with Node 24; see docs/SETUP.md'
}

desktop_tools() {
    # Match the desktop build's existing Apple-toolchain normalization.
    # shellcheck source=scripts/lib/apple-toolchain-env.sh
    source "$DEV_ROOT/scripts/lib/apple-toolchain-env.sh"
    native_tools
    node_tools
    need python3 'mise install; or: nix develop'
    local expected actual
    expected="$(sed -n 's/^bun = "\([^"]*\)"/\1/p' mise.toml)"
    actual="$(bun --version 2>/dev/null || true)"
    if [[ "$actual" == "$expected" ]]; then ok "Bun $actual"; else
        fail "Bun $expected (found: ${actual:-none})" 'mise install; use the repository Bun pin'
    fi
    if [[ "$(uname -s)/$(uname -m)" != Darwin/arm64 ]]; then
        fail 'Desktop first platform requires Apple-silicon macOS' 'Use an Apple-silicon Mac; Linux follows in phux-d4x9.19'
        return
    fi
    if xcrun metal --version >/dev/null 2>&1; then ok 'Metal compiler executes'; else
        fail 'Metal compiler unavailable in this environment' 'xcodebuild -downloadComponent MetalToolchain; verify xcrun metal --version; see clients/desktop/toolchain/README.md for inherited Nix SDK environments'
    fi
}

need git 'Install Git with your platform package manager'
case "$scope" in
    docs)
        for tool in awk sed grep find sort; do need "$tool" 'Install standard Unix utilities'; done
        ;;
    core) rust_tools core ;;
    native) native_tools ;;
    desktop) desktop_tools ;;
    integrations) node_tools ;;
    web)
        rust_tools web
        node_tools
        need wasm-pack "$(env_remedy 'cargo install --locked wasm-pack --version 0.15.0')"
        need wasm-opt 'brew install binaryen; or sudo apt-get install -y binaryen; Nix: nix develop'
        bindgen="$(sed -n 's/^wasm-bindgen = "=\([^"]*\)"/\1/p' clients/phux-web/Cargo.toml)"
        if [[ -n "$bindgen" && "$(wasm-bindgen --version 2>/dev/null || true)" == "wasm-bindgen $bindgen" ]]; then
            ok "wasm-bindgen $bindgen"
        else fail "wasm-bindgen CLI $bindgen" "$(env_remedy "cargo install --locked wasm-bindgen-cli --version $bindgen")"; fi
        sysroot="$(rustc --print sysroot 2>/dev/null || true)"
        if [[ -n "$sysroot" && -d "$sysroot/lib/rustlib/wasm32-unknown-unknown/lib" ]]; then ok 'Rust WASM target'; else
            fail 'Rust WASM target' "$(env_remedy 'bash scripts/setup-rust.sh web')"
        fi
        if [[ -f clients/phux-vt-web/vendor/ghostty-vt.wasm ]]; then ok 'Committed WASM engine present (run tests to verify ABI)'; else
            fail 'committed ghostty-vt.wasm' 'Restore clients/phux-vt-web/vendor/ghostty-vt.wasm from the checkout; see docs/SETUP.md#browser-client'
        fi
        ;;
    cockpit)
        native_tools
        node_tools
        need python3 'mise install; or: nix develop; or: brew install python'
        if [[ "$(uname -s)/$(uname -m)" != Darwin/arm64 ]]; then
            fail 'Cockpit app requires Apple-silicon macOS' 'Use an Apple-silicon Mac for the app and host-rendering checks'
        fi
        ;;
    ci)
        native_tools
        node_tools
        for tool in just actionlint shellcheck jq python3 curl; do need "$tool" "$(env_remedy 'See docs/SETUP.md#full-root-validation')"; done
        if [[ "${BASH_VERSINFO[0]}" -lt 4 ]]; then fail 'Bash 4+ for workflow routing checks' 'brew install bash; put Homebrew bin on PATH'; fi
        for tool in nextest deny; do
            if cargo "$tool" --version >/dev/null 2>&1; then ok "cargo-$tool"; else
                if [[ "$tool" == nextest ]]; then
                    fail "cargo-nextest" 'Install the official prebuilt binary: https://nexte.st/docs/installation/pre-built-binaries/ (Mise has no registry entry; Nix provides it)'
                else
                    fail "cargo-$tool" "$(env_remedy "Install the prebuilt cargo-$tool binary; see docs/SETUP.md#full-root-validation")"
                fi
            fi
        done
        ;;
esac
printf 'doctor %s: %d problem(s). See docs/SETUP.md.\n' "$scope" "$failures"
[[ "$failures" -eq 0 ]]
