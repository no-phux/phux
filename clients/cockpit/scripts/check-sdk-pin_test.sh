#!/usr/bin/env bash
# Hermetic regression coverage for the SDK documentation provenance gate.
set -uo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/check-sdk-pin-test.XXXXXX")"
trap 'rm -rf -- "${WORK}"' EXIT

SDK_SHA="905891de27b0138112d422af6b1ee2c60ee3eef9"
GHOSTTY_SHA="7aa9591746ffa4d2eee458960c76554352832595"
STALE_SHA="ad3f0fae36a7d1380c459c6b23ed12d83cad6a7a"
pass=0
fail=0

mkdir -p "${WORK}/fixture/scripts/lib"
cp "${ROOT}/scripts/check-sdk-pin.sh" "${WORK}/fixture/scripts/check-sdk-pin.sh"
cp "${ROOT}/scripts/lib/zon.sh" "${WORK}/fixture/scripts/lib/zon.sh"

reset_fixture() {
    cat >"${WORK}/fixture/build.zig.zon" <<EOF
.{
    .dependencies = .{
        .native_sdk = .{ .url = "https://github.com/phall1/native/archive/${SDK_SHA}.tar.gz" },
        .ghostty = .{ .url = "https://github.com/ghostty-org/ghostty/archive/${GHOSTTY_SHA}.tar.gz" },
    },
}
EOF
    cat >"${WORK}/fixture/README.md" <<EOF
native-sdk is pinned to phall1/native@${SDK_SHA}.
libghostty-vt is pinned to Ghostty commit ${GHOSTTY_SHA}.
EOF
    cat >"${WORK}/fixture/THIRD_PARTY_NOTICES.md" <<EOF
## Native SDK
Pinned source: https://github.com/phall1/native/tree/${SDK_SHA}

## Ghostty and libghostty-vt
Pinned source: https://github.com/ghostty-org/ghostty/tree/${GHOSTTY_SHA}
EOF
}

check() {
    local what="$1" want_code="$2" want_message="$3" got got_code
    got="$("${WORK}/fixture/scripts/check-sdk-pin.sh" 2>&1)"
    got_code=$?
    if [[ "${got_code}" -eq "${want_code}" ]] &&
        { [[ -z "${want_message}" ]] || grep -qF "${want_message}" <<<"${got}"; }; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
        printf 'FAIL %s\n  want code=%s message=%q\n  got  code=%s output=%q\n' \
            "${what}" "${want_code}" "${want_message}" "${got_code}" "${got}" >&2
    fi
}

reset_fixture
check 'matching README and shipped notices pass' 0 ''

# Historical RED on 2c3af11f: both stale packaged-provenance fixtures exited 0
# because the checker read README.md only. Keeping the current sha elsewhere in
# each notice also proves the gate validates the field, not mere file contents.
reset_fixture
sed "s|native/tree/${SDK_SHA}|native/tree/${STALE_SHA}|" \
    "${WORK}/fixture/THIRD_PARTY_NOTICES.md" >"${WORK}/notice"
printf '\nHistorical pin mentioned outside the provenance field: %s\n' "${SDK_SHA}" >>"${WORK}/notice"
mv "${WORK}/notice" "${WORK}/fixture/THIRD_PARTY_NOTICES.md"
check 'stale Native pinned-source field fails even when the current sha appears elsewhere' 1 \
    'THIRD_PARTY_NOTICES.md does not record the pinned .native_sdk source'

reset_fixture
sed "s|ghostty/tree/${GHOSTTY_SHA}|ghostty/tree/${STALE_SHA}|" \
    "${WORK}/fixture/THIRD_PARTY_NOTICES.md" >"${WORK}/notice"
printf '\nHistorical pin mentioned outside the provenance field: %s\n' "${GHOSTTY_SHA}" >>"${WORK}/notice"
mv "${WORK}/notice" "${WORK}/fixture/THIRD_PARTY_NOTICES.md"
check 'stale Ghostty pinned-source field fails even when the current sha appears elsewhere' 1 \
    'THIRD_PARTY_NOTICES.md does not record the pinned .ghostty source'

reset_fixture
printf 'Pinned source: https://github.com/phall1/native/tree/%s\n' "${STALE_SHA}" \
    >>"${WORK}/fixture/THIRD_PARTY_NOTICES.md"
check 'an ambiguous second Native pinned-source field fails' 1 \
    'THIRD_PARTY_NOTICES.md does not record the pinned .native_sdk source'

reset_fixture
sed "s/${SDK_SHA}/${STALE_SHA}/g" "${WORK}/fixture/README.md" >"${WORK}/readme"
mv "${WORK}/readme" "${WORK}/fixture/README.md"
check 'stale README Native pin still fails' 1 \
    'README.md does not document the pinned .native_sdk commit'

printf '%s passed, %s failed\n' "${pass}" "${fail}"
[[ "${fail}" -eq 0 ]]
