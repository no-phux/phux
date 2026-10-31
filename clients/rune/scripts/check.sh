#!/usr/bin/env bash
# The Rune client's gate, from any directory: `just rune-check` and the build
# job in .github/workflows/rune-build.yml. Its workspace is its own, so the
# root recipes never reach it.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$here"
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
python3 -B -m unittest discover -s scripts -p 'test_*.py'
