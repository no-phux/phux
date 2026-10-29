#!/usr/bin/env bash
# Build and package the Rune client for this host.
#
#   clients/rune/scripts/package.sh VERSION OUT_DIR [APP_DIR]
#
# macOS: `Phux Rune.app`, ad-hoc signed, zipped as
# phux-rune-VERSION-macos-arm64.zip. With APP_DIR the app is also left there
# for scripts/ci/sign-macos-desktop.sh --zip-only, which releases replace the
# ad-hoc zip with (Developer ID, notarized, stapled). Linux: phux-rune-VERSION-linux-ARCH.tar.gz
# with the binary and README. Each archive gets a `.sha256` beside it.
set -euo pipefail

version="${1:?usage: package.sh VERSION OUT_DIR}"
out="${2:?usage: package.sh VERSION OUT_DIR}"
keep="${3:-}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repo="$(cd "$here/../.." && pwd)"
mkdir -p "$out"
out="$(cd "$out" && pwd)"

cargo build --locked --release --manifest-path "$here/Cargo.toml"
binary="$here/target/release/phux-rune"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

checksum() {
    (cd "$out" && shasum -a 256 "$1" > "$1.sha256")
}

case "$(uname -s)" in
Darwin)
    [[ "$(uname -m)" == arm64 ]] || { echo "macOS packages are Apple silicon only" >&2; exit 1; }
    app="$stage/Phux Rune.app"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
    cp "$binary" "$app/Contents/MacOS/phux-rune"
    # CFBundleVersion takes only dot-separated integers: drop a prerelease.
    sed -e "s/@VERSION@/$version/g" -e "s/@BUILD@/${version%%-*}/g" \
        "$here/packaging/Info.plist" > "$app/Contents/Info.plist"
    iconset="$stage/AppIcon.iconset"
    mkdir -p "$iconset"
    for size in 16 32 128 256 512; do
        sips -z "$size" "$size" "$repo/clients/desktop/packaging/AppIcon.png" \
            --out "$iconset/icon_${size}x${size}.png" >/dev/null
        double=$((size * 2))
        sips -z "$double" "$double" "$repo/clients/desktop/packaging/AppIcon.png" \
            --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
    done
    iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"
    codesign --force --sign - --timestamp=none "$app"
    codesign --verify --strict "$app"
    archive="phux-rune-$version-macos-arm64.zip"
    rm -f "$out/$archive"
    ditto -c -k --keepParent "$app" "$out/$archive"
    if [[ -n "$keep" ]]; then
        mkdir -p "$keep"
        rm -rf "$keep/Phux Rune.app"
        ditto "$app" "$keep/Phux Rune.app"
    fi
    ;;
Linux)
    arch="$(uname -m)"
    dir="$stage/phux-rune-$version-linux-$arch"
    mkdir -p "$dir"
    cp "$binary" "$here/README.md" "$dir/"
    archive="phux-rune-$version-linux-$arch.tar.gz"
    tar -C "$stage" -czf "$out/$archive" "$(basename "$dir")"
    ;;
*)
    echo "unsupported host: $(uname -s)" >&2
    exit 1
    ;;
esac
checksum "$archive"
echo "$out/$archive"
