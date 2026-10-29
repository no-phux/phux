#!/usr/bin/env bash
# Hermetic Apple-version and component-selection tests; no Xcode/network needed.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib/dev-toolchain.sh"
# shellcheck source=scripts/lib/macos-desktop.sh
source "$DEV_ROOT/scripts/lib/macos-desktop.sh"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/phux-macos-setup.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/Xcode 26.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain"
fixture_selected="$scratch/Xcode 26.app/Contents/Developer"
fixture_host=26.0
fixture_xcode=26.2
sw_vers() { printf '%s\n' "$fixture_host"; }
xcode-select() { printf '%s\n' "$fixture_selected"; }
xcodebuild() {
    case "$1" in
        -version) printf 'Xcode %s\nBuild version fixture\n' "$fixture_xcode" ;;
        -downloadComponent)
            [[ "$DEVELOPER_DIR" == "$fixture_selected" && "$2" == MetalToolchain ]]
            printf '%s\n' "$fixture_xcode" >> "$scratch/downloads"
            [[ ! -e "$scratch/download-fails" ]] || return 1
            [[ -e "$scratch/broken-metal" ]] || touch "$scratch/metal"
            ;;
        *) return 1 ;;
    esac
}
xcrun() {
    case "$*" in
        '--sdk iphoneos -f clang') return 0 ;;
        '--sdk macosx --show-sdk-path') printf '%s/SDKs/MacOSX.sdk\n' "$DEVELOPER_DIR" ;;
        '--sdk macosx metal --version') [[ -e "$scratch/metal" ]] ;;
        *) return 1 ;;
    esac
}
expect_fail() {
    if "$@" >"$scratch/output" 2>&1; then
        phux_error "unexpected success: $*"
        exit 1
    fi
}

# Read-only checks cannot install; explicit Xcode and paths with spaces survive
# normalization. A successful install is not repeated on the second invocation.
unset IN_NIX_SHELL
export DEVELOPER_DIR="$fixture_selected" SDKROOT=/wrong/sdk MACOSX_DEPLOYMENT_TARGET=27.0
expect_fail prepare_macos_desktop --check
test ! -e "$scratch/downloads"
prepare_macos_desktop
test "$SDKROOT" = "$fixture_selected/SDKs/MacOSX.sdk"
test "$MACOSX_DEPLOYMENT_TARGET" = 26.0
prepare_macos_desktop
test "$(wc -l < "$scratch/downloads" | tr -d ' ')" = 1
test "$(macos_desktop_select '' 26)" = "$fixture_selected"

# Same setup on a newer host keeps the older supported deployment floor.
fixture_host=27.0
fixture_xcode=27.1
prepare_macos_desktop
test "$MACOSX_DEPLOYMENT_TARGET" = 26.0
fixture_host=26.0
expect_fail prepare_macos_desktop
fixture_host=25.6
fixture_xcode=26.2
expect_fail prepare_macos_desktop
fixture_host=26.0
fixture_xcode=16.4
expect_fail prepare_macos_desktop
fixture_xcode=invalid
expect_fail prepare_macos_desktop
fixture_xcode=26.2

# Missing/failed components remain failures, never a successful stamp that
# suppresses repair. Nix's SDK is not mistaken for a selected full Xcode.
export DEVELOPER_DIR=/nix/store/fixture-apple-sdk
rm "$scratch/metal"
touch "$scratch/download-fails"
expect_fail prepare_macos_desktop
rm "$scratch/download-fails"
touch "$scratch/broken-metal"
expect_fail prepare_macos_desktop
rm "$scratch/broken-metal"
prepare_macos_desktop
test "$DEVELOPER_DIR" = "$fixture_selected"
phux_ok 'macOS desktop setup: 26/27 detection, selected Xcode, read-only checks, matching Metal, failure recovery and idempotence passed'
