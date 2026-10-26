#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
# shellcheck source=scripts/lib/dev-toolchain.sh
source "$root/scripts/lib/dev-toolchain.sh"
cd "$root"
check_desktop_toolchain
case "${1:---debug}" in
    --debug) cargo build --locked -p phux ;;
    --release) cargo build --locked --release -p phux ;;
    *) echo "Usage: build-cli.sh [--debug|--release]" >&2; exit 2 ;;
esac
