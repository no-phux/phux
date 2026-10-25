#!/usr/bin/env bash
# Run from the immutable source checkout, with tooling from this harness.
set -euo pipefail
: "${ARCHIVE:?}" "${PHUX_BUILD_CHANNEL:?}" "${RUNNER_TEMP:?}"
[[ "$PHUX_BUILD_CHANNEL" == stable || "$PHUX_BUILD_CHANNEL" == next ]]
harness="$(cd "$(dirname "$0")/../.." && pwd)"
source_root="$PWD"
app="$source_root/clients/desktop/dist/app/Phux.app"
artifacts="$RUNNER_TEMP/desktop-release"
phux="$source_root/target/release/phux"

verify_bundle() {
    codesign --verify --deep --strict --verbose=2 "$app"
    if [[ "$PHUX_BUILD_CHANNEL" == next ]]; then
        codesign -dv --verbose=4 "$app" 2>&1 | grep -F 'Signature=adhoc'
        test "$(/usr/libexec/PlistBuddy -c 'Print :PhuxDesktopChannel' "$app/Contents/Info.plist")" = next
    else
        codesign -dv --verbose=4 "$app" 2>&1 | grep -F 'Authority=Developer ID Application:'
        xcrun stapler validate "$app"
    fi
    PHUX_MACOS_MIN_MAX=26.0 bash "$harness/scripts/check-binary-portability.sh" "$app/Contents/MacOS/phux-desktop"
    test "$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$app/Contents/Info.plist")" = 26.0
    test ! -e "$app/Contents/Resources/phux-desktop-native.darwin-arm64.node"
    test "$(/usr/libexec/PlistBuddy -c 'Print :PhuxDesktopHost' "$app/Contents/Info.plist")" = gpui
    test "$(/usr/libexec/PlistBuddy -c 'Print :PhuxBuildSHA' "$app/Contents/Info.plist")" = "$(git rev-parse HEAD)"
}

checksum() {
    # The sidecar records the basename, never a runner-local path.
    (cd "$artifacts" && shasum -a 256 "$1" > "$1.sha256" && shasum -a 256 -c "$1.sha256")
}

qualify_dmg() (
    # Subshell scopes the mount's cleanup, including a failed smoke test.
    local dmg="$artifacts/${ARCHIVE%.zip}.dmg"
    local mount="$RUNNER_TEMP/desktop-mounted"
    hdiutil verify "$dmg"
    xcrun stapler validate "$dmg"
    spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
    hdiutil attach -readonly -nobrowse -mountpoint "$mount" "$dmg"
    trap 'hdiutil detach "$mount"' EXIT
    test "$(readlink "$mount/Applications")" = /Applications
    test -f "$mount/Install Phux.txt"
    ditto "$mount/Phux.app" "$RUNNER_TEMP/desktop-dmg-installed/Phux.app"
    local installed="$RUNNER_TEMP/desktop-dmg-installed/Phux.app"
    codesign --verify --deep --strict --verbose=2 "$installed"
    xcrun stapler validate "$installed"
    spctl --assess --type execute --verbose=2 "$installed"
    bun "$harness/clients/desktop/scripts/smoke-app.ts" --app "$installed" --phux "$phux" \
        --output "$RUNNER_TEMP/desktop-dmg-evidence" --allow-unconfirmed-present
)

verify_bundle
mkdir -p "$artifacts" "$RUNNER_TEMP/desktop-qualified"
if [[ "$PHUX_BUILD_CHANNEL" == next ]]; then
    ditto -c -k --norsrc --keepParent "$app" "$artifacts/$ARCHIVE"
fi
checksum "$ARCHIVE"
if [[ "$PHUX_BUILD_CHANNEL" == stable ]]; then checksum "${ARCHIVE%.zip}.dmg"; fi
cat "$artifacts"/*.sha256 > "$artifacts/SHA256SUMS"
ditto -x -k "$artifacts/$ARCHIVE" "$RUNNER_TEMP/desktop-qualified"
codesign --verify --deep --strict --verbose=2 "$RUNNER_TEMP/desktop-qualified/Phux.app"
# Virtual displays confirm submission, not presentation; the evidence says so.
bun "$harness/clients/desktop/scripts/smoke-app.ts" --app "$RUNNER_TEMP/desktop-qualified/Phux.app" \
    --phux "$phux" --output "$RUNNER_TEMP/desktop-evidence" --allow-unconfirmed-present
if [[ "$PHUX_BUILD_CHANNEL" == stable ]]; then qualify_dmg; fi
