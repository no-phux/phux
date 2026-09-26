#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
source "$root/scripts/lib/apple-toolchain-env.sh"
source "$root/scripts/lib/dev-toolchain.sh"
export RUSTUP_TOOLCHAIN="$RUST_CHANNEL"
: "${PHUX_DESKTOP_ADDON:?absolute path to the combined production addon is required}"
harness="$root/clients/desktop/.cache/feasibility-harness"
mkdir -p "$harness"
cat > "$harness/Cargo.toml" <<'MANIFEST'
[package]
name = "solid-terminal-feasibility"
version = "0.0.0"
edition = "2024"
[workspace]
[[bin]]
name = "solid-terminal-feasibility"
path = "../../tests/feasibility/server.rs"
[dependencies]
phux-server-testkit = { path = "../../../../crates/phux-server-testkit" }
portable-pty = "0.9"
nix = { version = "0.31", features = ["signal", "process"] }
tempfile = "3"
tokio = { version = "1", features = ["process", "time"] }
MANIFEST
export CARGO_TARGET_DIR="$root/clients/desktop/.cache/feasibility-target"
cargo build --manifest-path "$harness/Cargo.toml"
fixture_home="$(mktemp -d "${TMPDIR:-/tmp}/phux-solid-feasibility.XXXXXX")"
trap 'rm -rf "$fixture_home"' EXIT
HOME="$fixture_home" XDG_CONFIG_HOME="$fixture_home/config" \
  XDG_DATA_HOME="$fixture_home/data" XDG_CACHE_HOME="$fixture_home/cache" \
  "$CARGO_TARGET_DIR/debug/solid-terminal-feasibility" \
  "$root/clients/desktop/scripts/check-terminal.ts"
