#!/usr/bin/env bash
# Make the Apple toolchain usable from inside a Nix devshell. `source` this.
#
# Why this file exists. Since bb568f1 the TestFlight build is cut FROM A MAC
# rather than from CI, and phux-mobile sits next to ../phux, whose flake-pinned
# devshell many maintainers have active (direnv, `nix develop`). That devshell
# replaces the host toolchain, and an iOS cross-compile then fails five distinct
# ways, NONE of whose error messages mention Nix. Every one of these was hit on
# a real maintainer machine cutting v0.3.34:
#
#   1. `clang` is a Nix clang-wrapper that is not built for cross-compilation:
#        clang: error: invalid argument '-mmacos-version-min=26.0' not allowed
#               with '-miphoneos-version-min=26.0'
#
#   2. cargo still LINKS with `cc` (the same wrapper), so fixing the compiler
#      alone only moves the failure from compile to link.
#
#   3. NIX_LDFLAGS carries -L/nix/store/... into the wrapper, which is how a
#      macOS dylib reaches an iOS link:
#        ld: building for iOS Simulator, but linking in dylib built for macOS,
#            file '/nix/store/.../libiconv-113/lib/libiconv.dylib'
#
#   4. `ld` in PATH is the wrapper's linker rather than Apple's.
#
#   5. THE one that actually blocks `xcodebuild archive`, and the one that
#      wastes the most time because it survives fixing 1-4: the devshell
#      exports a whole toolchain override set --
#        LD=ld  CC=clang  CXX=clang++  AR=ar  RANLIB=ranlib  NM=nm  STRIP=strip
#      -- and Xcode honours LD as its LINK DRIVER, a job clang normally does.
#      Bare `ld` is then handed clang driver flags and rejects them:
#        ld: unknown options: -Xlinker -isysroot -iframework -nostdlib
#            -fobjc-link-runtime ...
#      This is NOT a PATH problem. It persists with Apple's ld first in PATH,
#      because the right binary is being asked to do the wrong job. Anyone who
#      reads that message as "wrong ld" will fix PATH, watch the error not
#      move, and lose an afternoon.
#
# Sourcing this is a no-op outside a Nix devshell, so it is safe on CI and on a
# stock Mac.

# (5) Toolchain overrides. Unset before anything else: this is the blocker.
unset LD CC CXX AR RANLIB NM STRIP OBJCOPY OBJDUMP READELF SIZE STRINGS

# (3) Devshell compile/link flags must not reach a cross-compile.
unset NIX_LDFLAGS NIX_CFLAGS_COMPILE NIX_CFLAGS_LINK
unset LIBRARY_PATH LD_LIBRARY_PATH DYLD_LIBRARY_PATH DYLD_FALLBACK_LIBRARY_PATH

# (6) The devshell also pins an SDK:
#   SDKROOT=/nix/store/...-apple-sdk-14.4/.../MacOSX.sdk
#   MACOSX_DEPLOYMENT_TARGET=14.0
# A Nix-provided macOS 14.4 SDK was built with Swift 5.10, so Xcode 26's
# Swift 6.3.3 refuses it outright:
#   error: failed to build module 'Swift'; this SDK is not supported by the
#   compiler ... Please select a toolchain which matches the SDK.
# This breaks `swift test` (just test-pkg) and therefore `just land`, which
# runs the full local CI before it will attest and merge a PR. Drop both and
# let Xcode choose its own matching SDK. DEVELOPER_DIR gets the same
# treatment: a devshell that points it at a Nix SDK makes every `xcrun -f cc`
# (including the shims above) resolve to the Nix clang wrapper, so host
# build-script links die with `unable to execute tool 'cc'`.
unset SDKROOT MACOSX_DEPLOYMENT_TARGET DEVELOPER_DIR

# (4) Apple's toolchain first, so clang/ld/swiftc resolve to Xcode's. Nix
# entries stay later in PATH so `just`, `cargo` and `mise` still resolve.
# Devshell-only: on a stock Mac this prepend is what BREAKS the build rather
# than fixing it, because /usr/bin/cc there is already Xcode's clang with SDK
# autodetection.
#
# The compiler DRIVERS must not resolve to the raw xctoolchain binaries. The
# xctoolchain bin carries a raw `cc -> clang` symlink, and raw toolchain
# clang — unlike the /usr/bin/cc xcrun shim — does not resolve a default SDK.
# With SDKROOT unset (see 6, and engine-local.sh's `env -u SDKROOT`), every
# host-arch cargo build-script link then dies with
# `ld: library 'System' not found` (phux-jfx). So a shim directory routing
# cc/c++/clang/clang++ through `xcrun` — which resolves the default macOS SDK
# per invocation — goes first, and the raw toolchain sits right behind it for
# everything else (ld, swiftc, dsymutil, ...).
if [ -n "${IN_NIX_SHELL:-}" ] && [ -d /Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin ]; then
    _apple_shim_dir="${TMPDIR:-/tmp}/phux-apple-xcrun-shims"
    mkdir -p "$_apple_shim_dir"
    for _apple_tool in cc c++ clang clang++; do
        printf '#!/bin/sh\nexec /usr/bin/xcrun %s "$@"\n' "$_apple_tool" \
            > "$_apple_shim_dir/$_apple_tool"
        chmod +x "$_apple_shim_dir/$_apple_tool"
    done
    # cc-rs and other build scripts invoke bare `xcrun` (SDK discovery), which
    # Xcode's Developer/usr/bin does NOT provide — xcrun lives at /usr/bin.
    # Inside a devshell that PATH slot is taken by Nix's xcbuild xcrun stand-in,
    # which fails `xcrun --show-sdk-path --sdk iphonesimulator` outright.
    printf '#!/bin/sh\nexec /usr/bin/xcrun "$@"\n' > "$_apple_shim_dir/xcrun"
    chmod +x "$_apple_shim_dir/xcrun"
    PATH="$_apple_shim_dir:/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin:/Applications/Xcode.app/Contents/Developer/usr/bin:$PATH"
    export PATH
    unset _apple_shim_dir _apple_tool
fi

# (7) `cargo`/`rustc` must be rustup's, not the devshell's. The Nix rust has
# no Apple cross-compilation std, and `rustup target add` in
# build-xcframework.sh adds the triple to rustup's toolchain — a different one
# — so the failure reads as a missing target that was just installed:
#   error[E0463]: can't find crate for `core`
#   note: the `aarch64-apple-ios-sim` target may not be installed
#   help: consider downloading the target with `rustup target add ...`
# mise pins the same toolchain through rustup, so this only changes WHICH copy runs.
if [ -n "${IN_NIX_SHELL:-}" ] && [ -x "${CARGO_HOME:-$HOME/.cargo}/bin/cargo" ]; then
    PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
    export PATH
fi

# (1) + (2) Compiler and linker for the Rust cross-build. cc-rs and cargo both
# honour per-target overrides, which leaves the host build alone.
_apple_xc_clang="$(xcrun --sdk iphoneos -f clang 2>/dev/null || true)"
if [ -n "$_apple_xc_clang" ]; then
    export CC_aarch64_apple_ios="$_apple_xc_clang"
    export CC_aarch64_apple_ios_sim="$_apple_xc_clang"
    export CC_x86_64_apple_ios="$_apple_xc_clang"
    export CARGO_TARGET_AARCH64_APPLE_IOS_LINKER="$_apple_xc_clang"
    export CARGO_TARGET_AARCH64_APPLE_IOS_SIM_LINKER="$_apple_xc_clang"
    export CARGO_TARGET_X86_64_APPLE_IOS_LINKER="$_apple_xc_clang"
fi
unset _apple_xc_clang
