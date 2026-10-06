#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
# shellcheck source=scripts/lib/dev-toolchain.sh
source "$root/scripts/lib/dev-toolchain.sh"
cd "$root"
check_desktop_toolchain
cargo build -p phux
