#!/usr/bin/env bash
# Same entry point locally and on a clean native CI runner. This is a setup
# smoke, not a replacement for workspace tests and real-server e2e coverage.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
bash scripts/doctor.sh native
cargo test --locked -p phux-core
cargo test --locked -p phux-protocol --features server
cargo build --locked -p phux -p phux-mcp
"${CARGO_TARGET_DIR:-target}/debug/phux" --version
"${CARGO_TARGET_DIR:-target}/debug/phux-mcp" --version
