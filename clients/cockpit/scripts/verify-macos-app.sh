#!/usr/bin/env bash
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
    cat <<'EOF'
Usage:
  verify-macos-app.sh --app PATH --version VERSION --signature-mode MODE [options]

Required:
  --app PATH                 Application bundle to verify
  --version VERSION          Expected CFBundleShortVersionString
  --signature-mode MODE      auto, adhoc, developer-id-unnotarized, or
                             developer-id-notarized

Expected bundle metadata:
  --identifier ID            CFBundleIdentifier (default: dev.phux.cockpit)
  --name NAME                CFBundleName and CFBundleDisplayName
                             (default: Phux Cockpit)
  --build BUILD              CFBundleVersion (default: VERSION)
  --executable NAME          CFBundleExecutable (default: phux-cockpit)
  --minimum-os VERSION       LSMinimumSystemVersion (default: 11.0)

Policy:
  --entitlements POLICY      absent or allow (default: absent)
  --quarantine POLICY        absent, present, or ignore (default: ignore)
  -h, --help                 Show this help

The executable must be arm64-only. Every mode performs strict deep code-signature
verification and rejects entitlements unless --entitlements allow is explicit.
Both developer-id modes verify the Developer ID Application certificate, team,
and secure timestamp. developer-id-unnotarized does not require a ticket;
developer-id-notarized requires a stapled ticket and successful Gatekeeper
assessment. auto detects ad hoc or Developer ID signing and retains the stricter
notarized policy for Developer ID.
EOF
}

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

require_value() {
    [[ "$#" -ge 2 && -n "$2" ]] || fail "$1 requires a value"
}

valid_plain_value() {
    [[ -n "$1" && "$1" != *[[:cntrl:]]* ]]
}

detail_value() {
    local details="$1"
    local key="$2"
    local line

    while IFS= read -r line; do
        case "${line}" in
            "${key}="*) printf '%s\n' "${line#*=}"; return 0 ;;
        esac
    done <<< "${details}"
    return 1
}

has_developer_id_authority() {
    local details="$1"
    local line

    while IFS= read -r line; do
        case "${line}" in
            'Authority=Developer ID Application: '*) return 0 ;;
        esac
    done <<< "${details}"
    return 1
}

APP=''
EXPECTED_VERSION=''
SIGNATURE_MODE=''
EXPECTED_IDENTIFIER='dev.phux.cockpit'
EXPECTED_NAME='Phux Cockpit'
EXPECTED_BUILD=''
EXPECTED_EXECUTABLE='phux-cockpit'
EXPECTED_MINIMUM_OS='11.0'
QUARANTINE_POLICY='ignore'
ENTITLEMENTS_POLICY='absent'

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --app)
            require_value "$@"
            APP="$2"
            shift 2
            ;;
        --version)
            require_value "$@"
            EXPECTED_VERSION="$2"
            shift 2
            ;;
        --signature-mode)
            require_value "$@"
            SIGNATURE_MODE="$2"
            shift 2
            ;;
        --identifier)
            require_value "$@"
            EXPECTED_IDENTIFIER="$2"
            shift 2
            ;;
        --name)
            require_value "$@"
            EXPECTED_NAME="$2"
            shift 2
            ;;
        --build)
            require_value "$@"
            EXPECTED_BUILD="$2"
            shift 2
            ;;
        --executable)
            require_value "$@"
            EXPECTED_EXECUTABLE="$2"
            shift 2
            ;;
        --minimum-os)
            require_value "$@"
            EXPECTED_MINIMUM_OS="$2"
            shift 2
            ;;
        --quarantine)
            require_value "$@"
            QUARANTINE_POLICY="$2"
            shift 2
            ;;
        --entitlements)
            require_value "$@"
            ENTITLEMENTS_POLICY="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        --*) fail "unknown option: $1" ;;
        *) fail "unexpected argument: $1" ;;
    esac
done

[[ -n "${APP}" ]] || fail '--app is required'
[[ -n "${EXPECTED_VERSION}" ]] || fail '--version is required'
[[ -n "${SIGNATURE_MODE}" ]] || fail '--signature-mode is required'

case "${SIGNATURE_MODE}" in
    auto|adhoc|developer-id-unnotarized|developer-id-notarized) ;;
    *) fail '--signature-mode must be auto, adhoc, developer-id-unnotarized, or developer-id-notarized' ;;
esac
case "${ENTITLEMENTS_POLICY}" in
    absent|allow) ;;
    *) fail '--entitlements must be absent or allow' ;;
esac
case "${QUARANTINE_POLICY}" in
    absent|present|ignore) ;;
    *) fail '--quarantine must be absent, present, or ignore' ;;
esac

valid_plain_value "${APP}" || fail '--app must not be empty or contain control characters'
[[ "${EXPECTED_IDENTIFIER}" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$ ]] ||
    fail '--identifier is malformed'
[[ "${EXPECTED_VERSION}" =~ ^[A-Za-z0-9][A-Za-z0-9.+-]*$ ]] ||
    fail '--version is malformed'
valid_plain_value "${EXPECTED_NAME}" || fail '--name must not be empty or contain control characters'
[[ "${EXPECTED_NAME}" != */* ]] || fail '--name must not contain a slash'
[[ "${EXPECTED_EXECUTABLE}" != */* && "${EXPECTED_EXECUTABLE}" != '.' && "${EXPECTED_EXECUTABLE}" != '..' ]] ||
    fail '--executable must be a file name'
valid_plain_value "${EXPECTED_EXECUTABLE}" || fail '--executable must not contain control characters'
[[ "${EXPECTED_MINIMUM_OS}" =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?$ ]] ||
    fail '--minimum-os must be a dotted numeric version'

if [[ -z "${EXPECTED_BUILD}" ]]; then
    EXPECTED_BUILD="${EXPECTED_VERSION}"
fi
[[ "${EXPECTED_BUILD}" =~ ^[A-Za-z0-9][A-Za-z0-9.+-]*$ ]] || fail '--build is malformed'

[[ "$(/usr/bin/uname -s)" == 'Darwin' ]] || fail 'verification requires macOS'
for tool in /usr/bin/codesign /usr/bin/find /usr/bin/lipo /usr/bin/mktemp /usr/bin/plutil /usr/bin/xattr /bin/rm; do
    [[ -x "${tool}" ]] || fail "required macOS tool not found: ${tool}"
done

[[ -d "${APP}" ]] || fail "application bundle not found: ${APP}"
PLIST="${APP}/Contents/Info.plist"
[[ -f "${PLIST}" ]] || fail "Info.plist not found: ${PLIST}"
/usr/bin/plutil -lint "${PLIST}" >/dev/null || fail 'Info.plist is invalid'

plist_value() {
    local key="$1"
    local value

    value="$(/usr/bin/plutil -extract "${key}" raw -o - "${PLIST}" 2>/dev/null)" ||
        fail "Info.plist is missing ${key}"
    printf '%s\n' "${value}"
}

check_plist_value() {
    local key="$1"
    local expected="$2"
    local actual

    actual="$(plist_value "${key}")"
    [[ "${actual}" == "${expected}" ]] ||
        fail "${key}: expected '${expected}', found '${actual}'"
    printf 'ok: %s = %s\n' "${key}" "${actual}"
}

check_plist_value CFBundleIdentifier "${EXPECTED_IDENTIFIER}"
check_plist_value CFBundleName "${EXPECTED_NAME}"
check_plist_value CFBundleDisplayName "${EXPECTED_NAME}"
check_plist_value CFBundleShortVersionString "${EXPECTED_VERSION}"
check_plist_value CFBundleVersion "${EXPECTED_BUILD}"
check_plist_value CFBundleExecutable "${EXPECTED_EXECUTABLE}"
check_plist_value LSMinimumSystemVersion "${EXPECTED_MINIMUM_OS}"

EXECUTABLE="${APP}/Contents/MacOS/${EXPECTED_EXECUTABLE}"
RESOURCES="${APP}/Contents/Resources"
[[ -f "${EXECUTABLE}" && -x "${EXECUTABLE}" ]] ||
    fail "bundle executable is missing or not executable: ${EXECUTABLE}"
for resource in LICENSE.txt README.txt THIRD_PARTY_NOTICES.md JetBrainsMono-OFL.txt Phux-FFI-THIRD-PARTY.html Phux-FFI-Provenance.json signing-plan.txt cockpit-self-update.sh install-cockpit.sh; do
    [[ -s "${RESOURCES}/${resource}" ]] || fail "required resource is missing: ${resource}"
done
"${ROOT}/scripts/verify-phux-ffi.py" \
    --provenance-file "${RESOURCES}/Phux-FFI-Provenance.json"
ARCHITECTURES="$(/usr/bin/lipo -archs "${EXECUTABLE}" 2>/dev/null)" ||
    fail 'could not inspect executable architectures'
[[ "${ARCHITECTURES}" == 'arm64' ]] ||
    fail "expected an arm64-only executable, found: ${ARCHITECTURES}"
printf 'ok: executable architecture = arm64 only\n'
python3 "${ROOT}/scripts/check-ring-p256-helpers.py" "${EXECUTABLE}" ||
    fail 'ring P-256 mul/sqr helper was dead-stripped; QUIC TLS handshake would SIGILL'

/usr/bin/codesign --verify --deep --strict --verbose=2 "${APP}" ||
    fail 'strict code-signature verification failed'
printf 'ok: strict deep code signature\n'

TEMP_DIR=''
cleanup() {
    if [[ -n "${TEMP_DIR}" && -d "${TEMP_DIR}" ]]; then
        /bin/rm -rf -- "${TEMP_DIR}"
    fi
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

TEMP_ROOT="${TMPDIR:-/tmp}"
TEMP_DIR="$(/usr/bin/mktemp -d "${TEMP_ROOT%/}/verify-macos-app.XXXXXX")" ||
    fail 'could not create temporary directory'
ENTITLEMENTS="${TEMP_DIR}/entitlements.plist"
ENTITLEMENTS_LOG="${TEMP_DIR}/codesign-entitlements.log"
HAS_ENTITLEMENTS='false'
if ! /usr/bin/codesign --display --entitlements "${ENTITLEMENTS}" "${APP}" \
    >/dev/null 2>"${ENTITLEMENTS_LOG}"; then
    fail 'could not inspect code-signature entitlements'
fi
if [[ -e "${ENTITLEMENTS}" ]]; then
    HAS_ENTITLEMENTS='true'
    [[ "${ENTITLEMENTS_POLICY}" == 'allow' ]] || fail 'unexpected code-signature entitlements'
fi

CODE_FILES="${TEMP_DIR}/code-files"
/usr/bin/find "${APP}/Contents" -type f -print0 >"${CODE_FILES}" ||
    fail 'could not enumerate bundle files for entitlement inspection'
while IFS= read -r -d '' code_file; do
    /bin/rm -f -- "${ENTITLEMENTS}"
    /usr/bin/codesign --display --entitlements "${ENTITLEMENTS}" "${code_file}" \
        >/dev/null 2>"${ENTITLEMENTS_LOG}" || true
    if [[ -e "${ENTITLEMENTS}" ]]; then
        HAS_ENTITLEMENTS='true'
        [[ "${ENTITLEMENTS_POLICY}" == 'allow' ]] ||
            fail "unexpected code-signature entitlements in ${code_file}"
    fi
done <"${CODE_FILES}"
printf 'ok: code-signature entitlements = %s (policy: %s)\n' \
    "${HAS_ENTITLEMENTS}" "${ENTITLEMENTS_POLICY}"

SIGNATURE_DETAILS="$(/usr/bin/codesign --display --verbose=4 "${APP}" 2>&1)" ||
    fail 'could not inspect code-signature details'
SIGNATURE_VALUE="$(detail_value "${SIGNATURE_DETAILS}" Signature || true)"
if [[ "${SIGNATURE_VALUE}" == 'adhoc' ]]; then
    DETECTED_SIGNATURE_MODE='adhoc'
elif has_developer_id_authority "${SIGNATURE_DETAILS}"; then
    DETECTED_SIGNATURE_MODE='developer-id'
else
    fail 'signature is neither ad hoc nor Developer ID Application'
fi

case "${SIGNATURE_MODE}" in
    adhoc)
        [[ "${DETECTED_SIGNATURE_MODE}" == 'adhoc' ]] ||
            fail "expected adhoc signature, found ${DETECTED_SIGNATURE_MODE}"
        ;;
    developer-id-unnotarized|developer-id-notarized)
        [[ "${DETECTED_SIGNATURE_MODE}" == 'developer-id' ]] ||
            fail "expected Developer ID signature, found ${DETECTED_SIGNATURE_MODE}"
        ;;
    auto) ;;
esac
printf 'ok: signature class = %s\n' "${DETECTED_SIGNATURE_MODE}"

if [[ "${DETECTED_SIGNATURE_MODE}" == 'developer-id' ]]; then
    TEAM_IDENTIFIER="$(detail_value "${SIGNATURE_DETAILS}" TeamIdentifier || true)"
    [[ "${TEAM_IDENTIFIER}" =~ ^[A-Z0-9]{10}$ ]] ||
        fail 'Developer ID signature has an invalid team identifier'
    TIMESTAMP="$(detail_value "${SIGNATURE_DETAILS}" Timestamp || true)"
    [[ -n "${TIMESTAMP}" && "${TIMESTAMP}" != 'none' ]] ||
        fail 'Developer ID signature has no secure timestamp'

    DEVELOPER_ID_REQUIREMENT='=anchor apple generic and certificate leaf[field.1.2.840.113635.100.6.1.13] exists'
    /usr/bin/codesign --verify --deep --strict --verbose=2 \
        --test-requirement "${DEVELOPER_ID_REQUIREMENT}" "${APP}" ||
        fail 'Developer ID Application certificate verification failed'
    printf 'ok: Developer ID Application certificate (team %s)\n' "${TEAM_IDENTIFIER}"

fi

if [[ "${SIGNATURE_MODE}" == 'developer-id-notarized' ||
      ( "${SIGNATURE_MODE}" == 'auto' && "${DETECTED_SIGNATURE_MODE}" == 'developer-id' ) ]]; then
    [[ -x /usr/bin/xcrun ]] || fail 'required macOS tool not found: /usr/bin/xcrun'
    [[ -x /usr/sbin/spctl ]] || fail 'required macOS tool not found: /usr/sbin/spctl'

    /usr/bin/xcrun stapler validate "${APP}" || fail 'stapled notarization ticket validation failed'
    printf 'ok: stapled notarization ticket\n'
    /usr/sbin/spctl --assess --type execute --verbose=2 "${APP}" ||
        fail 'Gatekeeper execution assessment failed'
    printf 'ok: Gatekeeper execution assessment\n'
fi

ATTRIBUTES="$(/usr/bin/xattr "${APP}" 2>/dev/null)" || fail 'could not inspect extended attributes'
HAS_QUARANTINE='false'
while IFS= read -r attribute; do
    if [[ "${attribute}" == 'com.apple.quarantine' ]]; then
        HAS_QUARANTINE='true'
        break
    fi
done <<< "${ATTRIBUTES}"

case "${QUARANTINE_POLICY}" in
    absent)
        [[ "${HAS_QUARANTINE}" == 'false' ]] || fail 'quarantine attribute must be absent'
        ;;
    present)
        [[ "${HAS_QUARANTINE}" == 'true' ]] || fail 'quarantine attribute must be present'
        ;;
    ignore) ;;
esac
printf 'ok: quarantine = %s (policy: %s)\n' "${HAS_QUARANTINE}" "${QUARANTINE_POLICY}"

printf 'verified: %s\n' "${APP}"
