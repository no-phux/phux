#!/usr/bin/env bash
# Desktop-only Apple setup. Source after dev-toolchain.sh (Bash 3.2 compatible).
# Never change the machine's xcode-select setting or install/replace Xcode.

macos_desktop_xcode_version() {
    DEVELOPER_DIR="$1" xcodebuild -version 2>/dev/null | sed -n 's/^Xcode \([0-9][0-9.]*\)$/\1/p'
}

macos_desktop_compatible() {
    local version
    version="$(macos_desktop_xcode_version "$1")" || return 1
    [[ "$version" =~ ^[0-9]+(\.[0-9]+)*$ ]] || return 1
    [[ "${version%%.*}" -ge 26 && "${version%%.*}" -le "$2" ]]
}

macos_desktop_select() {
    local requested="$1" host="$2" candidate
    if [[ -n "$requested" ]]; then
        macos_desktop_compatible "$requested" "$host" || {
            phux_error "DEVELOPER_DIR is not a compatible Xcode 26+ for macOS $host: $requested"
            return 1
        }
        printf '%s\n' "$requested"
        return
    fi
    # Prefer the user's selected Xcode, then the standard and versioned apps.
    for candidate in "$(xcode-select -p 2>/dev/null || true)" /Applications/Xcode.app/Contents/Developer /Applications/Xcode*.app/Contents/Developer; do
        [[ -d "$candidate/Toolchains/XcodeDefault.xctoolchain" ]] || continue
        if macos_desktop_compatible "$candidate" "$host"; then
            printf '%s\n' "$candidate"
            return
        fi
    done
    phux_error "Install full Xcode 26+ compatible with macOS $host, open it once to accept its license, then rerun."
    return 1
}

macos_desktop_metal() {
    if xcrun --sdk macosx metal --version >/dev/null 2>&1; then return; fi
    if [[ "$1" == --check ]]; then
        phux_error 'Metal compiler unavailable; run bash scripts/setup-macos-desktop.sh'
        return 1
    fi
    # Xcode resolves the component compatible with THIS selected toolchain,
    # rather than a hard-coded asset version or whatever Xcode is newest.
    phux_info "Installing MetalToolchain for Xcode $(macos_desktop_xcode_version "$DEVELOPER_DIR")"
    xcodebuild -downloadComponent MetalToolchain >&2 || return 1
    xcrun --sdk macosx metal --version >&2 || {
        phux_error 'MetalToolchain was downloaded but its compiler still cannot execute'
        return 1
    }
}

prepare_macos_desktop() {
    local requested="${DEVELOPER_DIR:-}" host selected
    host="$(sw_vers -productVersion)" || return 1
    [[ "$host" =~ ^[0-9]+(\.[0-9]+)*$ ]] || return 1
    if [[ "${host%%.*}" -lt 26 ]]; then
        phux_error 'Phux Desktop requires macOS 26 or later'
        return 1
    fi
    # A Nix SDK is not an explicit request for an Xcode installation.
    case "$requested" in /nix/store/*) requested= ;; esac
    # shellcheck source=scripts/lib/apple-toolchain-env.sh
    source "$DEV_ROOT/scripts/lib/apple-toolchain-env.sh" || return 1
    if [[ -n "${IN_NIX_SHELL:-}" ]]; then
        # System dispatchers honor DEVELOPER_DIR below. Raw compiler binaries
        # from a different Xcode (or Nix's xcbuild) must not win on PATH.
        export PATH="/usr/bin:$PATH"
    fi
    selected="$(macos_desktop_select "$requested" "${host%%.*}")" || return 1
    export DEVELOPER_DIR="$selected"
    export SDKROOT
    SDKROOT="$(xcrun --sdk macosx --show-sdk-path)" || return 1
    # Set AFTER Apple normalization: never accidentally inherit the build
    # host's newer floor or let the normalizer erase the release workflow's.
    export MACOSX_DEPLOYMENT_TARGET=26.0
    macos_desktop_metal "${1:-install}"
}
