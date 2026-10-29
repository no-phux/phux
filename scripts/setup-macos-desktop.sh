#!/usr/bin/env bash
# Install only a missing Metal component, using a compatible installed Xcode.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib/dev-toolchain.sh"
# shellcheck source=scripts/lib/macos-desktop.sh
source "$DEV_ROOT/scripts/lib/macos-desktop.sh"
[[ $# -eq 0 && "$(uname -s)" == Darwin ]] || {
    phux_error 'usage (macOS only): bash scripts/setup-macos-desktop.sh'
    exit 2
}
prepare_macos_desktop
phux_ok "Xcode $(macos_desktop_xcode_version "$DEVELOPER_DIR"), Metal ready; desktop deployment target $MACOSX_DEPLOYMENT_TARGET"
