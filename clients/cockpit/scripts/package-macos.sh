#!/usr/bin/env bash
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$(CDPATH='' cd -- "${ROOT}/../.." && pwd)"
if [[ -z "${VERSION:-}" ]]; then
    IFS= read -r VERSION < "${ROOT}/version.txt"
fi
TARGET="${TARGET:-aarch64-macos}"
OPTIMIZE="${OPTIMIZE:-ReleaseSafe}"
OUT_DIR="${OUTPUT_DIR:-${ROOT}/zig-out/release}"
PACKAGE_APP="${ROOT}/zig-out/package/phux-cockpit.app"
APP="${OUT_DIR}/Phux Cockpit.app"
BASE="phux-cockpit-${VERSION}-macos-arm64"
ZIP="${OUT_DIR}/${BASE}.zip"
DMG="${OUT_DIR}/${BASE}.dmg"
CHECKSUMS="${OUT_DIR}/SHA256SUMS"
STAGING="${ROOT}/zig-out/package/dmg-staging"
NOTARY_ZIP="${ROOT}/zig-out/package/${BASE}-notarization.zip"
ZIP_VERIFY="${ROOT}/zig-out/package/zip-verify"
DMG_MOUNT="${ROOT}/zig-out/package/dmg-mount"
DMG_ATTACHED="false"

cleanup() {
    if [[ "${DMG_ATTACHED}" == "true" ]]; then
        /usr/bin/hdiutil detach -quiet "${DMG_MOUNT}" || \
            /usr/bin/hdiutil detach -quiet -force "${DMG_MOUNT}" || true
    fi
    rm -rf -- "${STAGING}" "${ZIP_VERIFY}" "${DMG_MOUNT}"
    rm -f -- "${NOTARY_ZIP}"
}
trap cleanup EXIT

if [[ "$(uname -s)" != "Darwin" ]]; then
    printf 'error: macOS packaging must run on macOS\n' >&2
    exit 1
fi
if [[ "${TARGET}" != "aarch64-macos" ]]; then
    printf 'error: release target must be aarch64-macos (got %s)\n' "${TARGET}" >&2
    exit 1
fi
if [[ "${OPTIMIZE}" != "ReleaseSafe" ]]; then
    printf 'error: release optimization must be ReleaseSafe (got %s)\n' "${OPTIMIZE}" >&2
    exit 1
fi
if [[ ! "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    printf 'error: VERSION must be a semantic version (got %s)\n' "${VERSION}" >&2
    exit 1
fi

notary_values=(
    "${APPLE_NOTARY_KEY_PATH:-}"
    "${APPLE_NOTARY_KEY_ID:-}"
    "${APPLE_NOTARY_ISSUER_ID:-}"
)
notary_count=0
for value in "${notary_values[@]}"; do
    [[ -n "${value}" ]] && notary_count=$((notary_count + 1))
done
if [[ "${notary_count}" -ne 0 && "${notary_count}" -ne 3 ]]; then
    printf 'error: notarization requires all three APPLE_NOTARY_* values or none\n' >&2
    exit 1
elif [[ "${notary_count}" -eq 3 && -z "${MACOS_SIGNING_IDENTITY:-}" ]]; then
    printf 'error: notarization credentials require MACOS_SIGNING_IDENTITY\n' >&2
    exit 1
fi

for tool in zig codesign ditto hdiutil lipo plutil shasum unzip; do
    if ! command -v "${tool}" >/dev/null 2>&1; then
        printf 'error: required tool not found: %s\n' "${tool}" >&2
        exit 1
    fi
done

PHUX_SOURCE_DIR="${PHUX_SOURCE_DIR:-${REPO_ROOT}}"
PHUX_CLIENT_FFI_INCLUDE_DIR="${PHUX_CLIENT_FFI_INCLUDE_DIR:-${REPO_ROOT}/crates/phux-client-ffi/include}"
PHUX_CLIENT_FFI_LIB_DIR="${PHUX_CLIENT_FFI_LIB_DIR:-${REPO_ROOT}/target/ffi-release}"
[[ -f "${PHUX_CLIENT_FFI_INCLUDE_DIR}/phux/client.h" ]] || {
    printf 'error: phux/client.h is missing from %s\n' "${PHUX_CLIENT_FFI_INCLUDE_DIR}" >&2
    exit 1
}
[[ -f "${PHUX_CLIENT_FFI_LIB_DIR}/libphux_client_ffi.a" ]] || {
    printf 'error: libphux_client_ffi.a is missing from %s\n' "${PHUX_CLIENT_FFI_LIB_DIR}" >&2
    exit 1
}
"${ROOT}/scripts/verify-phux-ffi.py" \
    --phux-tree "${PHUX_SOURCE_DIR}" \
    --ffi-include-dir "${PHUX_CLIENT_FFI_INCLUDE_DIR}" \
    --ffi-lib-dir "${PHUX_CLIENT_FFI_LIB_DIR}"

rm -rf -- "${PACKAGE_APP}" "${APP}" "${STAGING}" "${ZIP_VERIFY}" "${DMG_MOUNT}"
rm -f -- "${ZIP}" "${DMG}" "${CHECKSUMS}" "${NOTARY_ZIP}"
mkdir -p -- "${OUT_DIR}"

(
    cd "${ROOT}"
    # Source verification above requires the enclosing checkout's default FFI
    # paths. Let the shared build resolve them identically to the PR app build.
    bash ./scripts/build-shipping-app.sh package --summary all
)

if [[ ! -d "${PACKAGE_APP}" ]]; then
    printf 'error: native-sdk did not produce %s\n' "${PACKAGE_APP}" >&2
    exit 1
fi
mv -- "${PACKAGE_APP}" "${APP}"

RESOURCES="${APP}/Contents/Resources"
/bin/cp "${ROOT}/LICENSE" "${RESOURCES}/LICENSE.txt"
/bin/cp "${ROOT}/THIRD_PARTY_NOTICES.md" "${RESOURCES}/THIRD_PARTY_NOTICES.md"
/bin/cp "${ROOT}/assets/fonts/JetBrainsMono-OFL.txt" "${RESOURCES}/JetBrainsMono-OFL.txt"
/bin/cp "${ROOT}/assets/licenses/Phux-FFI-THIRD-PARTY.html" "${RESOURCES}/Phux-FFI-THIRD-PARTY.html"
"${ROOT}/scripts/verify-phux-ffi.py" \
    --write-provenance "${RESOURCES}/Phux-FFI-Provenance.json"
rm -f -- "${RESOURCES}/package-manifest.zon"
cat > "${RESOURCES}/README.txt" <<'EOF'
Phux Cockpit

This bundle is assembled by the project's release packaging script after
native-sdk creates the application skeleton. See signing-plan.txt for the
signature mode and THIRD_PARTY_NOTICES.md for bundled software licenses.
EOF

PLIST="${APP}/Contents/Info.plist"
/usr/bin/plutil -lint "${PLIST}" >/dev/null

SIGNING_MODE="adhoc"
VERIFIER_SIGNATURE_MODE="adhoc"
ENTITLEMENTS_POLICY="absent"
SIGNING_PLAN="${RESOURCES}/signing-plan.txt"
if [[ -n "${MACOS_SIGNING_IDENTITY:-}" ]]; then
    SIGNING_MODE="identity"
    VERIFIER_SIGNATURE_MODE="developer-id-unnotarized"
    sign_args=(--force --deep --options runtime --timestamp --sign "${MACOS_SIGNING_IDENTITY}")
    if [[ -n "${MACOS_ENTITLEMENTS:-}" ]]; then
        [[ -f "${MACOS_ENTITLEMENTS}" ]] || {
            printf 'error: MACOS_ENTITLEMENTS does not name a file\n' >&2
            exit 1
        }
        sign_args+=(--entitlements "${MACOS_ENTITLEMENTS}")
        ENTITLEMENTS_POLICY="allow"
    fi
    printf 'signing=identity\nsigned with %s\n' "${MACOS_SIGNING_IDENTITY}" > "${SIGNING_PLAN}"
    /usr/bin/codesign "${sign_args[@]}" "${APP}"
else
    printf 'signing=adhoc\nad-hoc signed local build\n' > "${SIGNING_PLAN}"
    /usr/bin/codesign --force --deep --timestamp=none --sign - "${APP}"
fi

/usr/bin/codesign --verify --deep --strict --verbose=2 "${APP}"

verify_coordinator() {
    local coordinator="$1/Contents/MacOS/phux"
    [[ -x "${coordinator}" && "$(/usr/bin/lipo -archs "${coordinator}")" == "arm64" ]] || {
        printf 'error: matching arm64 coordinator CLI is missing from %s\n' "$1" >&2
        return 1
    }
}

verify_bundle() {
    local bundle="$1"
    local plist="${bundle}/Contents/Info.plist"
    local resources="${bundle}/Contents/Resources"
    local executable="${bundle}/Contents/MacOS/phux-cockpit"
    local archs signing_marker

    /usr/bin/plutil -lint "${plist}" >/dev/null
    while IFS='=' read -r key expected; do
        [[ "$(/usr/bin/plutil -extract "${key}" raw -o - "${plist}")" == "${expected}" ]] || {
            printf 'error: unexpected %s in %s\n' "${key}" "${bundle}" >&2
            return 1
        }
    done <<EOF
CFBundleIdentifier=dev.phux.cockpit
CFBundleName=Phux Cockpit
CFBundleDisplayName=Phux Cockpit
CFBundleExecutable=phux-cockpit
CFBundleIconFile=AppIcon.icns
CFBundlePackageType=APPL
CFBundleShortVersionString=${VERSION}
CFBundleVersion=${VERSION}
LSMinimumSystemVersion=11.0
EOF

    [[ -x "${executable}" && -s "${resources}/AppIcon.icns" ]] || {
        printf 'error: executable or application icon is missing from %s\n' "${bundle}" >&2
        return 1
    }
    verify_coordinator "${bundle}" || return 1
    for resource in LICENSE.txt README.txt THIRD_PARTY_NOTICES.md JetBrainsMono-OFL.txt Phux-FFI-THIRD-PARTY.html Phux-FFI-Provenance.json signing-plan.txt; do
        [[ -s "${resources}/${resource}" ]] || {
            printf 'error: required resource %s is missing from %s\n' "${resource}" "${bundle}" >&2
            return 1
        }
    done
    IFS= read -r signing_marker < "${resources}/signing-plan.txt"
    [[ "${signing_marker}" == "signing=${SIGNING_MODE}" ]] || {
        printf 'error: signing metadata does not match package signature mode\n' >&2
        return 1
    }
    archs="$(/usr/bin/lipo -archs "${executable}")"
    [[ "${archs}" == "arm64" ]] || {
        printf 'error: expected an arm64-only executable, found: %s\n' "${archs}" >&2
        return 1
    }
    /usr/bin/codesign --verify --deep --strict --verbose=2 "${bundle}"
}

verify_signature() {
    "${ROOT}/scripts/verify-macos-app.sh" \
        --app "$1" \
        --version "${VERSION}" \
        --signature-mode "${VERIFIER_SIGNATURE_MODE}" \
        --entitlements "${ENTITLEMENTS_POLICY}" \
        --quarantine ignore
}

verify_bundle "${APP}"
verify_signature "${APP}"

NOTARIZED="false"
if [[ "${notary_count}" -eq 3 ]]; then
    [[ -f "${APPLE_NOTARY_KEY_PATH}" ]] || {
        printf 'error: APPLE_NOTARY_KEY_PATH does not name a file\n' >&2
        exit 1
    }
    /usr/bin/ditto -c -k --keepParent "${APP}" "${NOTARY_ZIP}"
    /usr/bin/xcrun notarytool submit "${NOTARY_ZIP}" \
        --key "${APPLE_NOTARY_KEY_PATH}" \
        --key-id "${APPLE_NOTARY_KEY_ID}" \
        --issuer "${APPLE_NOTARY_ISSUER_ID}" \
        --wait
    /usr/bin/xcrun stapler staple "${APP}"
    /usr/bin/xcrun stapler validate "${APP}"
    /usr/sbin/spctl --assess --type execute --verbose=2 "${APP}"
    NOTARIZED="true"
    VERIFIER_SIGNATURE_MODE="developer-id-notarized"
fi
rm -f -- "${NOTARY_ZIP}"

# ditto preserves the signed bundle's resource forks, metadata, and symlinks.
/usr/bin/ditto -c -k --sequesterRsrc --keepParent "${APP}" "${ZIP}"
/usr/bin/unzip -tq "${ZIP}"
mkdir -p -- "${ZIP_VERIFY}"
/usr/bin/ditto -x -k "${ZIP}" "${ZIP_VERIFY}"
verify_bundle "${ZIP_VERIFY}/Phux Cockpit.app"
verify_signature "${ZIP_VERIFY}/Phux Cockpit.app"
rm -rf -- "${ZIP_VERIFY}"

mkdir -p -- "${STAGING}"
/usr/bin/ditto "${APP}" "${STAGING}/Phux Cockpit.app"
ln -s /Applications "${STAGING}/Applications"
/usr/bin/hdiutil create \
    -quiet \
    -ov \
    -fs HFS+ \
    -format UDZO \
    -imagekey zlib-level=9 \
    -volname "Phux Cockpit" \
    -srcfolder "${STAGING}" \
    "${DMG}"
/usr/bin/hdiutil verify -quiet "${DMG}"
rm -rf -- "${STAGING}"
mkdir -p -- "${DMG_MOUNT}"
/usr/bin/hdiutil attach -quiet -readonly -nobrowse -mountpoint "${DMG_MOUNT}" "${DMG}"
DMG_ATTACHED="true"
verify_bundle "${DMG_MOUNT}/Phux Cockpit.app"
verify_signature "${DMG_MOUNT}/Phux Cockpit.app"
[[ -L "${DMG_MOUNT}/Applications" && "$(/usr/bin/readlink "${DMG_MOUNT}/Applications")" == "/Applications" ]] || {
    printf 'error: disk image does not contain the expected Applications link\n' >&2
    exit 1
}
/usr/bin/hdiutil detach -quiet "${DMG_MOUNT}"
DMG_ATTACHED="false"
rm -rf -- "${DMG_MOUNT}"

(
    cd "${OUT_DIR}"
    /usr/bin/shasum -a 256 "$(basename "${ZIP}")" "$(basename "${DMG}")" > "$(basename "${CHECKSUMS}")"
    /usr/bin/shasum -a 256 -c "$(basename "${CHECKSUMS}")"
)

if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    printf 'signing=%s\nnotarized=%s\n' "${SIGNING_MODE}" "${NOTARIZED}" >> "${GITHUB_OUTPUT}"
fi

printf 'Created and verified:\n'
printf '  %s\n  %s\n  %s\n  %s\n' "${APP}" "${ZIP}" "${DMG}" "${CHECKSUMS}"
printf 'Signing: %s; notarized: %s\n' "${SIGNING_MODE}" "${NOTARIZED}"
