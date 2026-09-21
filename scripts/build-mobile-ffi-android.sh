#!/usr/bin/env bash
# Build the canonical UniFFI Android artifact from this phux revision.
#
# Output under --out (default target/mobile-ffi-android/Artifacts):
#   kotlin/               generated + patched UniFFI Kotlin
#   jniLibs/arm64-v8a/    libphux_mobile_ffi.so
#   jniLibs/x86_64/       libphux_mobile_ffi.so
#   provenance            source/toolchain/digest facts for atomic consumers
#
# The producer is crates/phux-client-ffi with --no-default-features
# --features uniffi (ADR-0135); `c-abi` stays off so this artifact never
# carries the extern "C" surface. The crate's `uniffi::setup_scaffolding!`
# namespace and uniffi-android.toml's `cdylib_name` are both pinned to
# `phux_mobile_ffi` (not this crate's own name), and the built .so is
# renamed to match, so the zip layout, the dev.phux.mobile.ffi Kotlin
# package, the generated file name and the `loadLibrary` call are all
# byte-for-byte unchanged from the phux-mobile-ffi era.
#
# Usage: scripts/build-mobile-ffi-android.sh [--out DIR]
set -euo pipefail

INVOKE_CWD="$PWD"
ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

OUT="$ROOT/target/mobile-ffi-android/Artifacts"
while [[ $# -gt 0 ]]; do
    case "$1" in
    --out)
        [[ $# -ge 2 ]] || { echo "--out needs a value" >&2; exit 2; }
        OUT="$2"; shift ;;
    -h | --help)
        sed -n '2,/^$/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
        exit 0 ;;
    *) echo "unknown argument: $1 (see --help)" >&2; exit 2 ;;
    esac
    shift
done

case "$OUT" in /*) ;; *) OUT="$INVOKE_CWD/$OUT" ;; esac
OUT_PARENT="$(dirname -- "$OUT")"
OUT_NAME="$(basename -- "$OUT")"
mkdir -p "$OUT_PARENT"
OUT_PARENT="$(CDPATH='' cd -- "$OUT_PARENT" && pwd)"
DEST="$OUT_PARENT/$OUT_NAME"
STAGE="$(mktemp -d "$OUT_PARENT/.${OUT_NAME}.staging.XXXXXX")"
PREVIOUS=""
OUT="$STAGE"

CRATE=phux-client-ffi
FEATURES=uniffi
# What cargo actually names the cdylib for this crate; renamed below.
BUILT_SO=libphux_client_ffi.so
# What the jniLibs zip and provenance publish it as (unchanged from the
# phux-mobile-ffi era; see uniffi-android.toml's cdylib_name).
SO=libphux_mobile_ffi.so
KT_REL="dev/phux/mobile/ffi/phux_mobile_ffi.kt"

cleanup() {
    [[ -z "$STAGE" ]] || rm -rf "$STAGE"
    if [[ -n "$PREVIOUS" && ! -e "$DEST" ]]; then
        mv "$PREVIOUS" "$DEST"
    fi
}
trap cleanup EXIT

die() { echo "build-mobile-ffi-android: error: $*" >&2; exit 1; }
step() { echo "==> $*"; }

command -v zig >/dev/null || die "zig is required"
command -v rustup >/dev/null || die "rustup is required"
command -v cargo-ndk >/dev/null || die "cargo-ndk is required"
command -v jq >/dev/null || die "jq is required"
: "${ANDROID_NDK_HOME:?ANDROID_NDK_HOME is required}"

METADATA="$(cargo metadata --locked --format-version 1)"
TARGET_DIR="$(jq -r .target_directory <<<"$METADATA")"
[[ -n "$TARGET_DIR" && "$TARGET_DIR" != null ]] || die "cargo metadata reported no target directory"

case "$(uname -s)" in
Darwin) HOST_LIB="$TARGET_DIR/debug/libphux_client_ffi.dylib" ;;
Linux) HOST_LIB="$TARGET_DIR/debug/libphux_client_ffi.so" ;;
*) die "unsupported metadata host $(uname -s)" ;;
esac

step "building host metadata library (debug; strip hides UNIFFI_META_*)"
cargo build --locked -p "$CRATE" --no-default-features --features "$FEATURES"
[[ -s "$HOST_LIB" ]] || die "missing host library $HOST_LIB"

step "generating Kotlin bindings"
rm -rf "$OUT/kotlin"
mkdir -p "$OUT/kotlin"
cargo run --locked -p "$CRATE" --no-default-features --features "$FEATURES" --bin uniffi-bindgen -- \
    generate --library "$HOST_LIB" \
    --language kotlin --out-dir "$OUT/kotlin" \
    --config "$ROOT/crates/phux-client-ffi/uniffi-android.toml"
KT="$OUT/kotlin/$KT_REL"
[[ -s "$KT" ]] || die "UniFFI did not emit $KT_REL"
grep -q 'fun `stopConnection`()' "$KT" || die "generated Kotlin does not expose stopConnection"
grep -q 'class TerminalEngine' "$KT" || die "generated Kotlin does not expose TerminalEngine"

step "cross-compiling Android cdylibs"
rustup target add aarch64-linux-android x86_64-linux-android >/dev/null
rm -rf "$OUT/jniLibs"
mkdir -p "$OUT/jniLibs"
(
    cd "$ROOT/crates/phux-client-ffi"
    cargo ndk --platform 24 \
        -t arm64-v8a -t x86_64 \
        -o "$OUT/jniLibs" \
        build --locked --release --no-default-features --features "$FEATURES"
)
for abi in arm64-v8a x86_64; do
    built="$OUT/jniLibs/$abi/$BUILT_SO"
    [[ -s "$built" ]] || die "missing $abi cdylib"
    mv "$built" "$OUT/jniLibs/$abi/$SO"
done

step "writing provenance"
phux_rev="$(git rev-parse HEAD)"
engine_rev="$(sed -nE 's/^libghostty-vt[[:space:]]*=.*rev = "([0-9a-f]+)".*/\1/p' Cargo.toml)"
[[ -n "$engine_rev" ]] || die "could not read libghostty-vt rev from Cargo.toml"
dirty=false
[[ -z "$(git status --porcelain --untracked-files=no)" ]] || dirty=true
ndk_ver="$(basename "$ANDROID_NDK_HOME")"
{
    echo "format=phux-mobile-ffi-android-v1"
    echo "phux_rev=$phux_rev"
    echo "phux_tree=$(git rev-parse 'HEAD^{tree}')"
    echo "phux_dirty=$dirty"
    echo "engine_rev=$engine_rev"
    echo "profile=ffi-release"
    echo "rustc=$(rustc --version)"
    echo "zig=$(zig version)"
    echo "ndk=$ndk_ver"
    echo "abis=arm64-v8a,x86_64"
    echo "generated_sha256=$(shasum -a 256 "$KT" | awk '{print $1}')"
    echo "archive_arm64_v8a_sha256=$(shasum -a 256 "$OUT/jniLibs/arm64-v8a/$SO" | awk '{print $1}')"
    echo "archive_x86_64_sha256=$(shasum -a 256 "$OUT/jniLibs/x86_64/$SO" | awk '{print $1}')"
} > "$OUT/provenance"

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

echo "build-mobile-ffi-android: wrote $OUT"
echo "    kotlin:     $OUT/kotlin/$KT_REL"
echo "    jniLibs:    $OUT/jniLibs/{arm64-v8a,x86_64}/$SO"
echo "    provenance: $OUT/provenance"
