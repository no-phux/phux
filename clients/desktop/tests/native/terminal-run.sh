#!/usr/bin/env bash
# The addon must be built in release with terminal-fixtures + GPUIX test-support.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
source "$root/scripts/lib/apple-toolchain-env.sh"
source "$root/scripts/lib/dev-toolchain.sh"
export RUSTUP_TOOLCHAIN="$RUST_CHANNEL"
: "${PHUX_DESKTOP_ADDON:?absolute path to the combined fixture addon is required}"
harness="$root/clients/desktop/.cache/terminal-harness"
mkdir -p "$harness"
cat > "$harness/Cargo.toml" <<'MANIFEST'
[package]
name = "terminal-painter-harness"
version = "0.0.0"
edition = "2024"
[workspace]
[[bin]]
name = "terminal-painter-harness"
path = "../../tests/native/terminal-server.rs"
[dependencies]
phux-server-testkit = { path = "../../../../crates/phux-server-testkit" }
portable-pty = "0.9"
tempfile = "3"
tokio = { version = "1", features = ["process", "time"] }
png = "=0.18.1"
serde_json = "1"
MANIFEST
export CARGO_TARGET_DIR="$root/clients/desktop/.cache/terminal-harness-target"
cargo run --manifest-path "$harness/Cargo.toml" -- \
    "$PHUX_DESKTOP_ADDON" "$root/clients/desktop/tests/native"
