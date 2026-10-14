#!/usr/bin/env bash
# Same entry point locally and on a clean native CI runner. This is a setup
# smoke, not a replacement for workspace tests and real-server e2e coverage.
set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"
cd "$(dirname "${BASH_SOURCE[0]}")/.."
phux_info "native smoke: checking prerequisites"
bash scripts/doctor.sh native
phux_info "native smoke: core and protocol tests"
cargo test --locked -p phux-core
cargo test --locked -p phux-protocol --features server
phux_info "native smoke: building CLI and MCP"
cargo build --locked -p phux -p phux-mcp
"${CARGO_TARGET_DIR:-target}/debug/phux" --version
"${CARGO_TARGET_DIR:-target}/debug/phux-mcp" --version
phux_ok "native smoke passed"
