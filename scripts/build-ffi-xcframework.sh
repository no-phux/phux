#!/usr/bin/env bash
# Build PhuxFFI.xcframework: crates/phux-client-ffi as a static library for
# iOS devices, the arm64 iOS simulator, and arm64 macOS, with the libghostty
# engine inside each slice, wrapped so Swift can `import PhuxFFI` (ADR-0133).
#
# This is the canonical native artifact a Swift-only phux-mobile consumes;
# Cockpit links the same crate from this checkout through clients/cockpit.
# ffi-xcframework.yml runs this on macOS and attaches the result to the root
# release; `just ffi-xcframework` is the local entry point.
#
# Output, under --out (default target/ffi-xcframework/Artifacts):
#   PhuxFFI.xcframework/      slices + Headers/phux/client.h + module.modulemap
#   provenance                key/value lines: phux rev, libghostty-vt rev,
#                             zig, rustc, profile, SDKs, per-slice SHA-256
#
# Usage: scripts/build-ffi-xcframework.sh [--full|--dev] [--profile NAME]
#                                         [--out DIR] [--skip-smoke]
#   --full          (default) aarch64-apple-ios + aarch64-apple-ios-sim
#                   + aarch64-apple-darwin
#   --dev           simulator + macOS only: no device slice, half the cold
#                   libghostty compile
#   --profile NAME  ffi-release (default) or ffi-dev. Both keep panic=unwind,
#                   which the C boundary requires (root Cargo.toml).
#   --out DIR       artifact directory; a relative DIR is taken from the
#                   directory the script was invoked in
#   --skip-smoke    do not build and run the SwiftPM smoke consumer
#
# Toolchain: full Xcode with the iOS SDK (xcodebuild, the iPhoneOS platform),
# rustup with the three Apple targets (added here), and the pinned Zig from
# .config/zig-toolchain.json on PATH (mise, or scripts/install-zig.sh).
#
# CPU floors: every Rust slice is built with an explicit per-target
# `-C target-cpu` (apple-a7 device, apple-a12 simulator, apple-m1 macOS, the
# rustc defaults for those targets and Cockpit's macOS floor). The macOS
# engine flat build is pinned with LIBGHOSTTY_VT_SYS_CPU=baseline; the iOS
# engine archives come from ghostty's xcframework emit, which selects its own
# platform targets and does not inherit that setting. Inherited RUSTFLAGS are
# dropped so a `-C target-cpu=native` in the caller's shell can never reach a
# shipped Rust slice; scripts/check-release-cpu-baselines.sh pins the strings.
#
# Environment: this script scrubs Nix devshell toolchain overrides before it
# builds. A cross-compile to iOS inside `nix develop` otherwise fails in ways
# that never mention Nix: the nixpkgs clang wrapper rejects
# -miphoneos-version-min next to -mmacos-version-min, NIX_LDFLAGS pulls a
# macOS libiconv dylib into an iOS link, nixpkgs' ld64 cannot read the TBDs
# Xcode 27 ships (`libSystem.tbd, malformed file`), the exported LD=ld
# makes xcodebuild hand clang driver flags to bare ld, and the devshell's
# DEVELOPER_DIR points xcode-select at the Nix apple-sdk instead of Xcode.
# Each of those was hit on a maintainer Mac; the scrub is what makes the
# build environment-neutral. The cargo target directory is whatever cargo
# resolves (CARGO_TARGET_DIR and build.target-dir are honoured, not
# guessed). The same lessons live in phux-mobile's
# scripts/apple-toolchain-env.sh.

set -euo pipefail

INVOKE_CWD="$PWD"
ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

MODE="full"
PROFILE="ffi-release"
OUT="$ROOT/target/ffi-xcframework/Artifacts"
SMOKE=1
while [[ $# -gt 0 ]]; do
    case "$1" in
    --full) MODE="full" ;;
    --dev) MODE="dev" ;;
    --profile)
        [[ $# -ge 2 ]] || { echo "--profile needs a value" >&2; exit 2; }
        PROFILE="$2"
        shift
        ;;
    --out)
        [[ $# -ge 2 ]] || { echo "--out needs a value" >&2; exit 2; }
        OUT="$2"
        shift
        ;;
    --skip-smoke) SMOKE=0 ;;
    -h | --help)
        # The whole leading comment block, up to the first blank line.
        sed -n '2,/^$/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
        exit 0
        ;;
    *)
        echo "unknown argument: $1 (see --help)" >&2
        exit 2
        ;;
    esac
    shift
done

case "$PROFILE" in
ffi-release | ffi-dev) ;;
*)
    echo "error: profile must be ffi-release or ffi-dev (panic=unwind at the C boundary); got $PROFILE" >&2
    exit 2
    ;;
esac
case "$OUT" in
/*) ;;
*) OUT="$INVOKE_CWD/$OUT" ;;
esac
mkdir -p "$OUT"
OUT="$(CDPATH='' cd -- "$OUT" && pwd)"

CRATE="phux-client-ffi"
LIB="libphux_client_ffi.a"
DEVICE_TARGET="aarch64-apple-ios"
SIM_TARGET="aarch64-apple-ios-sim"
MAC_TARGET="aarch64-apple-darwin"
# x86_64-apple-ios is absent on purpose: libghostty-vt-sys builds its iOS
# slices through ghostty's emit-xcframework path, which emits an arm64-only
# simulator library, and the crate refuses the Intel simulator target.
if [[ "$MODE" == "full" ]]; then
    TARGETS=("$DEVICE_TARGET" "$SIM_TARGET" "$MAC_TARGET")
else
    TARGETS=("$SIM_TARGET" "$MAC_TARGET")
fi

# The floor phux-mobile's PhuxKit declares for both platforms. C dependencies
# otherwise inherit the host SDK's point release and produce objects a lower
# deployment floor cannot load. Override per build with the same names.
IOS_FLOOR="${PHUX_FFI_IOS_DEPLOYMENT_TARGET:-26.0}"
MACOS_FLOOR="${PHUX_FFI_MACOS_DEPLOYMENT_TARGET:-26.0}"

die() {
    echo "build-ffi-xcframework: error: $*" >&2
    exit 1
}

step() {
    echo "==> $*"
}

# Every scratch directory the script creates, removed on any exit so a
# failed `swift build` or an aborted run leaves nothing under $TMPDIR.
SHIMS_DIR=""
HEADERS_DIR=""
SMOKE_DIR=""
cleanup() {
    local dir
    for dir in "$SHIMS_DIR" "$HEADERS_DIR" "$SMOKE_DIR"; do
        [[ -n "$dir" ]] && rm -rf "$dir"
    done
    return 0
}
trap cleanup EXIT

# The explicit Rust CPU floor per target: rustc's own default for the two iOS
# targets, and Cockpit's floor for macOS (clients/cockpit/scripts/
# build-phux-artifacts.sh builds the same archive with apple-m1). The literal
# strings are what scripts/check-release-cpu-baselines.sh pins.
target_rustflags() {
    case "$1" in
    "$DEVICE_TARGET") echo "-C target-cpu=apple-a7" ;;
    "$SIM_TARGET") echo "-C target-cpu=apple-a12" ;;
    "$MAC_TARGET") echo "-C target-cpu=apple-m1" ;;
    *) die "no CPU floor for $1" ;;
    esac
}

# Drop every devshell override the header describes, then put Xcode's tools
# first with cc/clang routed through xcrun (raw xctoolchain clang resolves no
# default SDK, and `ld: library 'System' not found` follows for every host
# build script). A stock Mac already resolves /usr/bin/cc this way, so the
# prepend is a no-op there.
apple_toolchain_env() {
    unset LD CC CXX AR RANLIB NM STRIP OBJCOPY OBJDUMP READELF SIZE STRINGS
    unset CC_FOR_BUILD CXX_FOR_BUILD LD_FOR_BUILD
    unset NIX_LDFLAGS NIX_CFLAGS_COMPILE NIX_CFLAGS_LINK NIX_LDFLAGS_FOR_BUILD
    unset NIX_CFLAGS_COMPILE_FOR_BUILD NIX_CC NIX_CC_FOR_BUILD NIX_BINTOOLS
    unset NIX_BINTOOLS_FOR_BUILD NIX_IGNORE_LD_THROUGH_GCC
    unset LIBRARY_PATH LD_LIBRARY_PATH DYLD_LIBRARY_PATH DYLD_FALLBACK_LIBRARY_PATH
    unset SDKROOT MACOSX_DEPLOYMENT_TARGET IPHONEOS_DEPLOYMENT_TARGET
    unset CFLAGS CXXFLAGS LDFLAGS
    # The devshell's DEVELOPER_DIR is the Nix apple-sdk store path, which
    # xcode-select -p reports in place of Xcode; the real one is re-exported
    # below from the probe.
    unset DEVELOPER_DIR
    # Inherited Rust flags (a `-C target-cpu=native`, a nix `-L/nix/store`
    # link arg) would reach every object and the iOS link; the per-target
    # floors below are the only flags a shipped slice carries.
    unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_RUSTFLAGS

    local developer_dir
    developer_dir="$(/usr/bin/xcode-select -p 2>/dev/null || true)"
    [[ -n "$developer_dir" && -d "$developer_dir/Platforms/iPhoneOS.platform" ]] ||
        die "a full Xcode with the iOS platform is required (xcode-select -p gave '${developer_dir:-nothing}'); select it with: sudo xcode-select -s /Applications/Xcode.app"
    export DEVELOPER_DIR="$developer_dir"

    SHIMS_DIR="$(mktemp -d "${TMPDIR:-/tmp}/phux-ffi-shims.XXXXXX")"
    local tool
    for tool in cc c++ clang clang++; do
        printf '#!/bin/sh\nexec /usr/bin/xcrun %s "$@"\n' "$tool" > "$SHIMS_DIR/$tool"
        chmod +x "$SHIMS_DIR/$tool"
    done
    PATH="$SHIMS_DIR:$developer_dir/Toolchains/XcodeDefault.xctoolchain/usr/bin:$developer_dir/usr/bin:$PATH"
    # rustup's cargo, not a devshell's: only rustup carries the Apple
    # cross-compilation std libraries that `rustup target add` installs.
    if [[ -x "${CARGO_HOME:-$HOME/.cargo}/bin/cargo" ]]; then
        PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
    fi
    export PATH

    # cc-rs and cargo honour per-target overrides, so the host build stays on
    # the default driver while the iOS objects and links use Xcode's clang.
    local xc_clang
    xc_clang="$(xcrun --sdk iphoneos -f clang)"
    export CC_aarch64_apple_ios="$xc_clang"
    export CC_aarch64_apple_ios_sim="$xc_clang"
    export CARGO_TARGET_AARCH64_APPLE_IOS_LINKER="$xc_clang"
    export CARGO_TARGET_AARCH64_APPLE_IOS_SIM_LINKER="$xc_clang"

    # Per-target flags apply only to `--target` builds, so host build
    # scripts keep the default CPU while every slice gets its floor.
    export CARGO_TARGET_AARCH64_APPLE_IOS_RUSTFLAGS
    CARGO_TARGET_AARCH64_APPLE_IOS_RUSTFLAGS="$(target_rustflags "$DEVICE_TARGET")"
    export CARGO_TARGET_AARCH64_APPLE_IOS_SIM_RUSTFLAGS
    CARGO_TARGET_AARCH64_APPLE_IOS_SIM_RUSTFLAGS="$(target_rustflags "$SIM_TARGET")"
    export CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS
    CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS="$(target_rustflags "$MAC_TARGET")"

    export IPHONEOS_DEPLOYMENT_TARGET="$IOS_FLOOR"
    export MACOSX_DEPLOYMENT_TARGET="$MACOS_FLOOR"
    # Pin the macOS engine's flat build. For iOS libghostty-vt-sys extracts
    # ghostty's own platform-selected xcframework archive; -Dcpu does not
    # propagate into those slices.
    export LIBGHOSTTY_VT_SYS_CPU=baseline
}

check_toolchain() {
    # shellcheck source=scripts/lib/dev-toolchain.sh
    source "$ROOT/scripts/lib/dev-toolchain.sh"
    command -v zig >/dev/null 2>&1 ||
        die "zig $ZIG_VERSION is not on PATH; run 'mise install' or 'bash scripts/install-zig.sh <dir>'"
    local zig_version
    zig_version="$(zig version)"
    [[ "$zig_version" == "$ZIG_VERSION" ]] ||
        die "zig on PATH is $zig_version; the pinned engine build needs $ZIG_VERSION (.config/zig-toolchain.json)"
    command -v rustup >/dev/null 2>&1 || die "rustup is required to add the Apple targets"
    command -v xcodebuild >/dev/null 2>&1 || die "xcodebuild is required to assemble the xcframework"
    command -v plutil >/dev/null 2>&1 || die "plutil is required to verify the xcframework"
    command -v jq >/dev/null 2>&1 || die "jq is required to read the xcframework manifest"
    xcrun --sdk iphoneos --show-sdk-path >/dev/null 2>&1 ||
        die "the iOS SDK is missing; install it from Xcode > Settings > Components"
}

# One resolved workspace graph for the archive paths and the provenance
# revisions, read after the toolchain is set so it is rustup's cargo that
# answers and `--locked` proves Cargo.lock is current.
METADATA=""
TARGET_DIR=""
resolve_workspace() {
    METADATA="$(cargo metadata --locked --format-version 1)" ||
        die "cargo metadata --locked failed; is Cargo.lock current?"
    TARGET_DIR="$(jq -r '.target_directory' <<<"$METADATA")"
    [[ -n "$TARGET_DIR" && "$TARGET_DIR" != "null" ]] || die "cargo metadata reported no target_directory"
}

slice_archive() {
    echo "$TARGET_DIR/$1/$PROFILE/$LIB"
}

build_slices() {
    step "ensuring rust targets: ${TARGETS[*]}"
    local target
    for target in "${TARGETS[@]}"; do
        rustup target add "$target" >/dev/null
    done
    step "building $CRATE ($PROFILE) per target"
    for target in "${TARGETS[@]}"; do
        echo "    - $target"
        # staticlib only: the crate also declares cdylib and rlib, and the
        # xcframework carries archives. This is the same invocation Cockpit's
        # release uses, plus --target.
        # The path comes from cargo's target_directory. Removing the final
        # archive keeps an unrelated stale file from satisfying the check
        # below; Cargo's fingerprint decides whether dependencies rebuild.
        rm -f "$(slice_archive "$target")"
        cargo rustc --locked --profile "$PROFILE" --target "$target" \
            -p "$CRATE" --lib --crate-type staticlib
        [[ -s "$(slice_archive "$target")" ]] ||
            die "expected $(slice_archive "$target") after the build"
    done
}

assemble_xcframework() {
    HEADERS_DIR="$(mktemp -d "${TMPDIR:-/tmp}/phux-ffi-headers.XXXXXX")"
    mkdir -p "$HEADERS_DIR/phux"
    cp "$ROOT/crates/$CRATE/include/phux/client.h" "$HEADERS_DIR/phux/client.h"
    # One clang module so Swift can `import PhuxFFI` straight from the
    # archive; the header is self-contained apart from the C standard headers.
    printf 'module PhuxFFI {\n    header "phux/client.h"\n    export *\n}\n' > "$HEADERS_DIR/module.modulemap"

    step "assembling PhuxFFI.xcframework"
    rm -rf "$OUT/PhuxFFI.xcframework"
    mkdir -p "$OUT"
    local args=() target
    for target in "${TARGETS[@]}"; do
        args+=(-library "$(slice_archive "$target")" -headers "$HEADERS_DIR")
    done
    xcodebuild -create-xcframework "${args[@]}" -output "$OUT/PhuxFFI.xcframework" >/dev/null
}

slice_identifier() {
    case "$1" in
    "$DEVICE_TARGET") echo ios-arm64 ;;
    "$SIM_TARGET") echo ios-arm64-simulator ;;
    "$MAC_TARGET") echo macos-arm64 ;;
    *) die "no xcframework slice identifier for $1" ;;
    esac
}

# LC_BUILD_VERSION platform numbers from <mach-o/loader.h>.
slice_macho_platform() {
    case "$1" in
    ios-arm64) echo 2 ;;
    ios-arm64-simulator) echo 7 ;;
    macos-arm64) echo 1 ;;
    esac
}

verify_slice() {
    local identifier="$1" framework="$OUT/PhuxFFI.xcframework"
    local archive="$framework/$identifier/$LIB"
    [[ -s "$archive" ]] || die "missing slice archive $archive"
    [[ -f "$framework/$identifier/Headers/phux/client.h" ]] || die "$identifier lost phux/client.h"
    [[ -f "$framework/$identifier/Headers/module.modulemap" ]] || die "$identifier lost module.modulemap"
    [[ "$(lipo -archs "$archive")" == "arm64" ]] || die "$identifier is not arm64-only"
    local load_commands platforms expected
    load_commands="$(otool -l "$archive")"
    platforms="$(awk '$1 == "cmd" { build = ($2 == "LC_BUILD_VERSION") }
        build && $1 == "platform" { print $2 }' <<<"$load_commands" | sort -u)"
    expected="$(slice_macho_platform "$identifier")"
    [[ "$platforms" == "$expected" ]] ||
        die "$identifier Mach-O platform is '${platforms//$'\n'/,}', expected $expected"
    # Rust's precompiled standard-library members can carry legacy
    # LC_VERSION_MIN_* markers instead of LC_BUILD_VERSION. Reject markers
    # that cannot belong to this slice; simulator objects always identify as
    # LC_BUILD_VERSION platform 7, so either legacy marker is foreign there.
    local foreign_min_pattern floor
    case "$identifier" in
    ios-arm64) foreign_min_pattern=LC_VERSION_MIN_MACOSX floor="$IOS_FLOOR" ;;
    ios-arm64-simulator) foreign_min_pattern='LC_VERSION_MIN_(MACOSX|IPHONEOS)' floor="$IOS_FLOOR" ;;
    macos-arm64) foreign_min_pattern=LC_VERSION_MIN_IPHONEOS floor="$MACOS_FLOOR" ;;
    esac
    ! grep -Eq "cmd $foreign_min_pattern\$" <<<"$load_commands" ||
        die "$identifier contains a foreign LC_VERSION_MIN_* member"
    # No member may sit above the slice's deployment floor, and the crate's
    # own objects must sit exactly on it. Precompiled Rust std objects carry
    # older floors, which a consumer's link tolerates.
    awk -v floor="$floor" '
        function num(v,  p) { split(v, p, "."); return (p[1] + 0) * 10000 + (p[2] + 0) * 100 + (p[3] + 0) }
        $1 == "cmd" { build = ($2 == "LC_BUILD_VERSION"); legacy = ($2 ~ /^LC_VERSION_MIN_/) }
        (build && $1 == "minos") || (legacy && $1 == "version") {
            count += 1
            if (num($2) > num(floor)) bad = $2
            if (num($2) == num(floor)) has_floor = 1
        }
        END { if (bad != "" || count == 0 || !has_floor) exit 1 }' <<<"$load_commands" ||
        die "$identifier does not preserve the $floor deployment floor"
}

verify_xcframework() {
    step "verifying slices"
    local framework="$OUT/PhuxFFI.xcframework" expected actual target
    expected="$(for target in "${TARGETS[@]}"; do slice_identifier "$target"; done | LC_ALL=C sort)"
    actual="$(plutil -extract AvailableLibraries json -o - "$framework/Info.plist" |
        jq -r '.[].LibraryIdentifier' | LC_ALL=C sort)"
    [[ "$actual" == "$expected" ]] ||
        die "unexpected slice set; expected [${expected//$'\n'/ }], got [${actual//$'\n'/ }]"
    local identifier
    while IFS= read -r identifier; do
        verify_slice "$identifier"
    done <<<"$expected"
}

sha256_of() {
    shasum -a 256 "$1" | awk '{print $1}'
}

# The libghostty-rs revision Cargo resolved (the `git+URL#rev` source in the
# locked graph), cross-checked against the root Cargo.toml pin when that pin
# is on its usual one-line form.
libghostty_vt_rev() {
    local resolved pinned
    resolved="$(jq -r '.packages[] | select(.name == "libghostty-vt") | .source // empty' <<<"$METADATA" |
        sed -n 's/.*#\([0-9a-f]*\)$/\1/p' | head -n1)"
    pinned="$(sed -n 's/^libghostty-vt = .*rev = "\([0-9a-f]*\)".*/\1/p' "$ROOT/Cargo.toml" | head -n1)"
    if [[ -n "$resolved" && -n "$pinned" && "$resolved" != "$pinned" ]]; then
        die "libghostty-vt resolves to $resolved but Cargo.toml pins $pinned"
    fi
    echo "${resolved:-${pinned:-unknown}}"
}

# The ghostty commit libghostty-vt-sys compiles, from its build script.
ghostty_rev() {
    local manifest
    manifest="$(jq -r '.packages[] | select(.name == "libghostty-vt-sys") | .manifest_path' <<<"$METADATA" | head -n1)"
    if [[ -n "$manifest" && -f "$(dirname "$manifest")/build.rs" ]]; then
        sed -n 's/^const GHOSTTY_COMMIT: &str = "\([0-9a-f]*\)";/\1/p' "$(dirname "$manifest")/build.rs" | head -n1
    fi
}

write_provenance() {
    step "recording provenance"
    local tree="clean" ghostty abi target
    # Untracked files count: a slice built with a new source file on disk
    # does not correspond to phux-rev either.
    [[ -z "$(git -C "$ROOT" status --porcelain --untracked-files=normal 2>/dev/null)" ]] || tree="dirty"
    ghostty="$(ghostty_rev)"
    abi="$(sed -n 's/^#define PHUX_CLIENT_ABI_VERSION \([0-9]*\)u/\1/p' "$ROOT/crates/$CRATE/include/phux/client.h")"
    {
        printf 'phux-rev %s\n' "$(git -C "$ROOT" rev-parse HEAD)"
        printf 'phux-tree %s\n' "$tree"
        printf 'phux-client-abi-version %s\n' "${abi:-unknown}"
        printf 'libghostty-vt-rev %s\n' "$(libghostty_vt_rev)"
        printf 'ghostty-rev %s\n' "${ghostty:-unknown}"
        printf 'zig %s\n' "$(zig version)"
        printf 'rustc %s\n' "$(rustc --version | awk '{print $2}')"
        printf 'cargo-profile %s\n' "$PROFILE"
        printf 'mode %s\n' "$MODE"
        printf 'targets %s\n' "${TARGETS[*]}"
        for target in "${TARGETS[@]}"; do
            printf 'rustflags %s %s\n' "$target" "$(target_rustflags "$target")"
        done
        printf 'libghostty-cpu-macos %s\n' "$LIBGHOSTTY_VT_SYS_CPU"
        printf 'libghostty-cpu-ios %s\n' ghostty-xcframework-targets
        printf 'xcode %s\n' "$(xcodebuild -version | awk 'NR == 1 { print $2 }')"
        printf 'macosx-sdk %s\n' "$(xcrun --sdk macosx --show-sdk-version 2>/dev/null || echo unknown)"
        printf 'iphoneos-sdk %s\n' "$(xcrun --sdk iphoneos --show-sdk-version 2>/dev/null || echo unknown)"
        printf 'ios-deployment-target %s\n' "$IOS_FLOOR"
        printf 'macos-deployment-target %s\n' "$MACOS_FLOOR"
        for target in "${TARGETS[@]}"; do
            printf 'slice-sha256 %s %s\n' "$(slice_identifier "$target")" \
                "$(sha256_of "$OUT/PhuxFFI.xcframework/$(slice_identifier "$target")/$LIB")"
        done
    } > "$OUT/provenance"
}

# A throwaway SwiftPM executable that imports the module and drives the
# handle lifecycle proves the module map and the archive link the way a real
# Swift consumer will use them. It is built for macOS, the one slice `swift
# build` can run on the build host.
smoke_consumer() {
    step "smoke: SwiftPM consumer imports PhuxFFI and creates a client"
    command -v swift >/dev/null 2>&1 || die "swift is required for the smoke consumer (or pass --skip-smoke)"
    SMOKE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/phux-ffi-smoke.XXXXXX")"
    local pkg="$SMOKE_DIR"
    mkdir -p "$pkg/Sources/Smoke"
    cp -R "$OUT/PhuxFFI.xcframework" "$pkg/PhuxFFI.xcframework"
    cat > "$pkg/Package.swift" <<EOF
// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "PhuxFFISmoke",
    platforms: [.macOS("$MACOS_FLOOR")],
    targets: [
        .binaryTarget(name: "PhuxFFI", path: "PhuxFFI.xcframework"),
        .executableTarget(name: "Smoke", dependencies: ["PhuxFFI"], path: "Sources/Smoke"),
    ]
)
EOF
    cat > "$pkg/Sources/Smoke/main.swift" <<'EOF'
import Foundation
import PhuxFFI

var options = PhuxClientOptions()
options.size = MemoryLayout<PhuxClientOptions>.size
options.version = PHUX_CLIENT_ABI_VERSION
options.max_bootstrap_chunk_bytes = 1024
options.max_history_page_bytes = 1024
options.max_history_page_rows = 128
options.max_history_cache_bytes = 4096
options.max_history_materialized_rows = 1024
options.history_prefetch_rows = 64

var client: OpaquePointer? = nil
let result = phux_client_new(&options, &client)
guard result == PHUX_CLIENT_OK, let handle = client else {
    FileHandle.standardError.write("phux_client_new failed: \(result)\n".data(using: .utf8)!)
    exit(1)
}
phux_client_free(handle)
print("phux-ffi-smoke: ok (abi \(PHUX_CLIENT_ABI_VERSION))")
EOF
    (
        cd "$pkg"
        swift build -c release 2>&1 | tail -n 20
        "$(swift build -c release --show-bin-path)/Smoke"
    ) || die "the SwiftPM smoke consumer failed to build or run"
}

main() {
    step "mode: $MODE ($PROFILE); targets: ${TARGETS[*]}; out: ${OUT#"$ROOT"/}"
    apple_toolchain_env
    check_toolchain
    resolve_workspace
    build_slices
    assemble_xcframework
    verify_xcframework
    write_provenance
    if [[ "$SMOKE" == "1" ]]; then
        smoke_consumer
    fi
    step "done"
    echo "    xcframework: ${OUT#"$ROOT"/}/PhuxFFI.xcframework"
    echo "    provenance:  ${OUT#"$ROOT"/}/provenance"
    [[ "$MODE" == "full" ]] || echo "    note: simulator + macOS only; --full adds the device slice"
}

main
