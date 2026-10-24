#!/usr/bin/env bash
# Stable macOS distribution only. Never uses the runner's login keychain.
set -euo pipefail

for name in APPLE_DEVELOPER_ID_P12 APPLE_DEVELOPER_ID_PASSWORD APPLE_NOTARY_KEY_P8 APPLE_NOTARY_KEY_ID APPLE_NOTARY_ISSUER_ID; do
    if [[ -z "${!name:-}" ]]; then
        echo "Missing $name; see docs/releasing.md#macos-desktop-signing. Stable desktop publication requires Developer ID and notarization." >&2
        exit 1
    fi
done
[[ "$APPLE_NOTARY_KEY_ID" =~ ^[A-Z0-9]{10}$ ]] || { echo 'Invalid Apple notary key ID' >&2; exit 1; }
[[ "$APPLE_NOTARY_ISSUER_ID" =~ ^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$ ]] || { echo 'Invalid Apple notary issuer ID' >&2; exit 1; }
if [[ "${1:-}" == --check-config ]]; then exit 0; fi
if [[ $# != 2 || "$2" != *.zip ]]; then
    echo 'Usage: sign-macos-desktop.sh APP OUTPUT.zip (also writes OUTPUT.dmg)' >&2
    exit 2
fi

app="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
mkdir -p "$(dirname "$2")"
archive="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"
dmg="${archive%.zip}.dmg"
test -f "$app/Contents/Info.plist"
test ! -e "$archive"
test ! -e "$dmg"
harness="$(cd "$(dirname "$0")/../.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/phux-apple-sign.XXXXXX")"
keychain="$work/release.keychain-db"
cleanup() {
    security delete-keychain "$keychain" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT
umask 077
printf '%s' "$APPLE_DEVELOPER_ID_P12" | base64 --decode > "$work/identity.p12"
printf '%s' "$APPLE_NOTARY_KEY_P8" | base64 --decode > "$work/notary.p8"
password="$(uuidgen)"
security create-keychain -p "$password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$password" "$keychain"
security import "$work/identity.p12" -k "$keychain" -P "$APPLE_DEVELOPER_ID_PASSWORD" -T /usr/bin/codesign >/dev/null
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" "$keychain" >/dev/null
identities="$(security find-identity -v -p codesigning "$keychain" | awk '/"Developer ID Application:/ {print $2}')"
if [[ -z "$identities" || "$identities" == *$'\n'* ]]; then
    echo 'The P12 must contain exactly one valid Developer ID Application identity (not Apple Distribution).' >&2
    exit 1
fi

# The GPUI app has one executable and no nested code or JIT entitlements.
codesign --force --keychain "$keychain" --sign "$identities" --options runtime --timestamp "$app"
codesign --verify --deep --strict --verbose=2 "$app"

notarize() {
    local file="$1" receipt="$2"
    if ! xcrun notarytool submit "$file" --key "$work/notary.p8" \
        --key-id "$APPLE_NOTARY_KEY_ID" --issuer "$APPLE_NOTARY_ISSUER_ID" \
        --wait --timeout 30m --output-format json > "$receipt"; then
        cat "$receipt" >&2
        echo 'Apple notarization failed or timed out; no artifacts may publish.' >&2
        return 1
    fi
    if ! jq -e '.status == "Accepted"' "$receipt" >/dev/null; then
        cat "$receipt" >&2
        echo 'Apple did not accept the notarization; no artifacts may publish.' >&2
        return 1
    fi
}

# Staple the app BEFORE making either final container so ZIP updaters receive
# the same ticket as Finder installs (including on an offline first launch).
ditto -c -k --norsrc --keepParent "$app" "$work/submission.zip"
notarize "$work/submission.zip" "${archive%.zip}-app-notary.json"
xcrun stapler staple "$app"
xcrun stapler validate "$app"
spctl --assess --type execute --verbose=2 "$app"
ditto -c -k --norsrc --keepParent "$app" "$archive"
bun "$harness/clients/desktop/scripts/package-dmg.ts" --app "$app" --output "$dmg"
codesign --keychain "$keychain" --sign "$identities" --timestamp "$dmg"
notarize "$dmg" "${archive%.zip}-dmg-notary.json"
xcrun stapler staple "$dmg"
xcrun stapler validate "$dmg"
spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
