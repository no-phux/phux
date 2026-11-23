#!/usr/bin/env bash
# Same entry point locally and on a clean native CI runner. This is a setup
# smoke, not a replacement for workspace tests and real-server e2e coverage.
set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"
cd "$(dirname "${BASH_SOURCE[0]}")/.."
phux_info "native smoke: checking prerequisites"
bash scripts/doctor.sh native
phux_info "native smoke: core, protocol, and VT tests"
# One feature-unified schedule avoids compiling the shared graph once per crate.
cargo test --locked -p phux-core -p phux-protocol -p phux-vt --features phux-vt/kitty
phux_info "native smoke: building CLI and MCP"
cargo build --locked -p phux -p phux-mcp
"${CARGO_TARGET_DIR:-target}/debug/phux" --version
"${CARGO_TARGET_DIR:-target}/debug/phux-mcp" --version
phux_ok "native smoke passed"
