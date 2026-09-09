#!/usr/bin/env bash
# Opt-in, bounded Rust mutation testing. See --help for installation and reports.
set -euo pipefail
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$script_dir/rust_runner.py" "$@"
