#!/usr/bin/env bash
# Build the canonical UniFFI mobile projection from this phux revision.
#
# Output under --out (default target/mobile-ffi-xcframework/Artifacts):
#   PhuxFFI.xcframework/  engine-bearing device, simulator, and macOS slices
#   Generated/            matching UniFFI Swift source
#   provenance            source/toolchain/digest facts for atomic consumers
#
# Usage: scripts/build-mobile-ffi-xcframework.sh [--full|--dev]
#          [--profile ffi-release|ffi-dev] [--out DIR] [--skip-smoke]

set -euo pipefail

INVOKE_CWD="$PWD"
ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

MODE=full
PROFILE=ffi-release
OUT="$ROOT/target/mobile-ffi-xcframework/Artifacts"
SMOKE=1
while [[ $# -gt 0 ]]; do
    case "$1" in
    --full) MODE=full ;;
    --dev) MODE=dev ;;
    --profile)
        [[ $# -ge 2 ]] || { echo "--profile needs a value" >&2; exit 2; }
        PROFILE="$2"; shift ;;
    --out)
        [[ $# -ge 2 ]] || { echo "--out needs a value" >&2; exit 2; }
        OUT="$2"; shift ;;
    --skip-smoke) SMOKE=0 ;;
    -h | --help)
        sed -n '2,/^$/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
        exit 0 ;;
    *) echo "unknown argument: $1 (see --help)" >&2; exit 2 ;;
    esac
    shift
done

case "$PROFILE" in ffi-release | ffi-dev) ;; *)
    echo "profile must be ffi-release or ffi-dev" >&2; exit 2 ;;
esac
case "$OUT" in /*) ;; *) OUT="$INVOKE_CWD/$OUT" ;; esac
OUT_PARENT="$(dirname -- "$OUT")"
OUT_NAME="$(basename -- "$OUT")"
mkdir -p "$OUT_PARENT"
OUT_PARENT="$(CDPATH='' cd -- "$OUT_PARENT" && pwd)"
DEST="$OUT_PARENT/$OUT_NAME"
STAGE="$(mktemp -d "$OUT_PARENT/.${OUT_NAME}.staging.XXXXXX")"
PREVIOUS=""
OUT="$STAGE"

DEVICE_TARGET=aarch64-apple-ios
SIM_TARGET=aarch64-apple-ios-sim
MAC_TARGET=aarch64-apple-darwin
TARGETS=("$SIM_TARGET" "$MAC_TARGET")
[[ "$MODE" == full ]] && TARGETS=("$DEVICE_TARGET" "${TARGETS[@]}")
CRATE=phux-mobile-ffi
LIB=libphux_mobile_ffi.a
FEATURES=engine,wire
IOS_FLOOR="${PHUX_FFI_IOS_DEPLOYMENT_TARGET:-26.0}"
MACOS_FLOOR="${PHUX_FFI_MACOS_DEPLOYMENT_TARGET:-26.0}"
GENERATED="$OUT/Generated"
HEADERS=""
SMOKE_DIR=""

cleanup() {
    [[ -z "$HEADERS" ]] || rm -rf "$HEADERS"
    [[ -z "$SMOKE_DIR" ]] || rm -rf "$SMOKE_DIR"
    [[ -z "$STAGE" ]] || rm -rf "$STAGE"
    if [[ -n "$PREVIOUS" && ! -e "$DEST" ]]; then
        mv "$PREVIOUS" "$DEST"
    fi
}
trap cleanup EXIT

die() { echo "build-mobile-ffi-xcframework: error: $*" >&2; exit 1; }
step() { echo "==> $*"; }

# shellcheck source=scripts/lib/apple-toolchain-env.sh
source "$ROOT/scripts/lib/apple-toolchain-env.sh"
export IPHONEOS_DEPLOYMENT_TARGET="$IOS_FLOOR"
export MACOSX_DEPLOYMENT_TARGET="$MACOS_FLOOR"
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_RUSTFLAGS
export CARGO_TARGET_AARCH64_APPLE_IOS_RUSTFLAGS="-C target-cpu=apple-a7"
export CARGO_TARGET_AARCH64_APPLE_IOS_SIM_RUSTFLAGS="-C target-cpu=apple-a12"
export CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS="-C target-cpu=apple-m1"
export LIBGHOSTTY_VT_SYS_CPU=baseline

command -v zig >/dev/null || die "zig is required"
command -v rustup >/dev/null || die "rustup is required"
command -v xcodebuild >/dev/null || die "full Xcode is required"
command -v jq >/dev/null || die "jq is required"

METADATA="$(cargo metadata --locked --format-version 1)"
TARGET_DIR="$(jq -r .target_directory <<<"$METADATA")"
[[ -n "$TARGET_DIR" && "$TARGET_DIR" != null ]] || die "cargo metadata reported no target directory"

slice_archive() { printf '%s/%s/%s/%s\n' "$TARGET_DIR" "$1" "$PROFILE" "$LIB"; }
slice_dylib() { printf '%s/%s/%s/libphux_mobile_ffi.dylib\n' "$TARGET_DIR" "$1" "$PROFILE"; }

step "ensuring Apple Rust targets"
for target in "${TARGETS[@]}"; do rustup target add "$target" >/dev/null; done

step "building $CRATE ($PROFILE, $FEATURES)"
for target in "${TARGETS[@]}"; do
    echo "    - $target"
    rm -f "$(slice_archive "$target")"
    cargo build --locked --profile "$PROFILE" --target "$target" \
        -p "$CRATE" --features "$FEATURES"
    [[ -s "$(slice_archive "$target")" ]] || die "missing archive for $target"
done

step "generating matching Swift bindings"
rm -rf "$GENERATED"
mkdir -p "$GENERATED"
cargo run --locked --profile "$PROFILE" -p "$CRATE" --features "$FEATURES" \
    --bin uniffi-bindgen -- generate --library "$(slice_dylib "$MAC_TARGET")" \
    --language swift --out-dir "$GENERATED"

HEADERS="$(mktemp -d "${TMPDIR:-/tmp}/phux-mobile-ffi-headers.XXXXXX")"
ffi_header="$(find "$GENERATED" -maxdepth 1 -type f -name '*FFI.h' -print)"
ffi_module="$(find "$GENERATED" -maxdepth 1 -type f -name '*FFI.modulemap' -print)"
[[ -n "$ffi_header" && "$(printf '%s\n' "$ffi_header" | wc -l | tr -d ' ')" == 1 ]] || die "UniFFI did not emit one header"
[[ -n "$ffi_module" && "$(printf '%s\n' "$ffi_module" | wc -l | tr -d ' ')" == 1 ]] || die "UniFFI did not emit one module map"
mv "$ffi_header" "$HEADERS/"
mv "$ffi_module" "$HEADERS/module.modulemap"
[[ -s "$GENERATED/PhuxFFI.swift" ]] || die "UniFFI did not emit PhuxFFI.swift"

step "assembling PhuxFFI.xcframework"
rm -rf "$OUT/PhuxFFI.xcframework"
xcargs=()
for target in "${TARGETS[@]}"; do
    xcargs+=(-library "$(slice_archive "$target")" -headers "$HEADERS")
done
xcodebuild -create-xcframework "${xcargs[@]}" -output "$OUT/PhuxFFI.xcframework" >/dev/null

expected=$'ios-arm64-simulator\nmacos-arm64'
[[ "$MODE" == full ]] && expected=$'ios-arm64\nios-arm64-simulator\nmacos-arm64'
actual="$(plutil -extract AvailableLibraries json -o - "$OUT/PhuxFFI.xcframework/Info.plist" \
    | jq -r '.[].LibraryIdentifier' | sort)"
[[ "$actual" == "$expected" ]] || die "unexpected xcframework slices: $actual"
for target in "${TARGETS[@]}"; do
    case "$target" in
    "$DEVICE_TARGET") identifier=ios-arm64 ;;
    "$SIM_TARGET") identifier=ios-arm64-simulator ;;
    "$MAC_TARGET") identifier=macos-arm64 ;;
    esac
    archive="$OUT/PhuxFFI.xcframework/$identifier/$LIB"
    [[ -s "$archive" ]] || die "missing $identifier archive"
    [[ "$(lipo -archs "$archive")" == arm64 ]] || die "$identifier is not arm64-only"
done

step "writing provenance"
phux_rev="$(git rev-parse HEAD)"
engine_rev="$(sed -nE 's/^libghostty-vt[[:space:]]*=.*rev = "([0-9a-f]+)".*/\1/p' Cargo.toml)"
dirty=false
[[ -z "$(git status --porcelain --untracked-files=no)" ]] || dirty=true
{
    echo "format=phux-mobile-ffi-xcframework-v1"
    echo "phux_rev=$phux_rev"
    echo "phux_tree=$(git rev-parse 'HEAD^{tree}')"
    echo "phux_dirty=$dirty"
    echo "engine_rev=$engine_rev"
    echo "profile=$PROFILE"
    echo "mode=$MODE"
    echo "rustc=$(rustc --version)"
    echo "zig=$(zig version)"
    echo "generated_sha256=$(shasum -a 256 "$GENERATED/PhuxFFI.swift" | awk '{print $1}')"
    for target in "${TARGETS[@]}"; do
        case "$target" in
        "$DEVICE_TARGET") identifier=ios-arm64 ;;
        "$SIM_TARGET") identifier=ios-arm64-simulator ;;
        "$MAC_TARGET") identifier=macos-arm64 ;;
        esac
        archive="$OUT/PhuxFFI.xcframework/$identifier/$LIB"
        echo "archive_${identifier//-/_}_sha256=$(shasum -a 256 "$archive" | awk '{print $1}')"
    done
} > "$OUT/provenance"

if [[ "$SMOKE" == 1 ]]; then
    step "smoke-testing generated Swift against the macOS slice"
    SMOKE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/phux-mobile-ffi-smoke.XXXXXX")"
    mkdir -p "$SMOKE_DIR/Sources/Bindings" "$SMOKE_DIR/Sources/Smoke"
    cp "$GENERATED/PhuxFFI.swift" "$SMOKE_DIR/Sources/Bindings/PhuxFFI.swift"
    ln -s "$OUT/PhuxFFI.xcframework" "$SMOKE_DIR/PhuxFFI.xcframework"
    cat > "$SMOKE_DIR/Package.swift" <<EOF
// swift-tools-version: 6.0
import PackageDescription
let package = Package(
    name: "PhuxMobileFFISmoke",
    platforms: [.macOS("26.0")],
    targets: [
        .binaryTarget(name: "PhuxFFIBinary", path: "PhuxFFI.xcframework"),
        .target(name: "Bindings", dependencies: ["PhuxFFIBinary"], swiftSettings: [.swiftLanguageMode(.v5)]),
        .executableTarget(name: "Smoke", dependencies: ["Bindings"]),
    ]
)
EOF
    cat > "$SMOKE_DIR/Sources/Smoke/main.swift" <<'EOF'
import Bindings
precondition(bridgeReady())
print("phux-mobile-ffi-smoke: ok")
EOF
    swift run --package-path "$SMOKE_DIR" Smoke
fi

step "publishing the completed artifact set"
if [[ -e "$DEST" ]]; then
    PREVIOUS="${DEST}.previous.$$"
    rm -rf "$PREVIOUS"
    mv "$DEST" "$PREVIOUS"
fi
mv "$STAGE" "$DEST"
STAGE=""
rm -rf "$PREVIOUS"
PREVIOUS=""
OUT="$DEST"
GENERATED="$OUT/Generated"

step "mobile xcframework complete"
echo "    framework: $OUT/PhuxFFI.xcframework"
echo "    bindings:  $GENERATED/PhuxFFI.swift"
echo "    provenance: $OUT/provenance"
