#!/usr/bin/env bash
# Fail when the SDK/engine pins documented in README.md or the shipped notices
# have drifted from the pins build.zig.zon actually resolves. Offline and fast,
# so it belongs beside check-release-version.sh at the top of CI rather than in
# a build job.
#
# This check exists because the drift already happened: README claimed
# phall1/native@f7347de long after build.zig.zon had moved to f3678832.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
ZON="${ROOT}/build.zig.zon"
README="${ROOT}/README.md"
NOTICES="${ROOT}/THIRD_PARTY_NOTICES.md"

# shellcheck source=scripts/lib/zon.sh
source "${ROOT}/scripts/lib/zon.sh"

status=0

check_pin() {
    local name="$1" url read_status slug sha pinned_source notice_source_lines
    # `set -e` would kill the script at this assignment on a non-zero read,
    # before the refusal below could name what went wrong — a silent exit 3.
    # Capture the status instead of inheriting it.
    read_status=0
    url="$(zon_dependency_url "${ZON}" "${name}")" || read_status=$?
    # A .path override is a REFUSAL, not a miss. This check used to read the
    # override's block as never-closing and answer with the NEXT dependency's
    # url — ghostty's — which passes the sha-pinned-archive test below, so the
    # drift gate printed "Pinned native_sdk is ghostty-org/ghostty@..." and
    # exited 0. See scripts/lib/zon.sh and phux-cockpit-yo5.
    if [[ "${read_status}" -eq 3 ]]; then
        printf 'error: .%s is a local override (.path), so there is no pin to check.\n' "${name}" >&2
        printf 'That is fine while developing, but CI must run against a published pin.\n' >&2
        status=1
        return
    fi
    if [[ "${read_status}" -ne 0 || -z "${url}" ]]; then
        printf 'error: build.zig.zon has no .%s dependency with a .url\n' "${name}" >&2
        status=1
        return
    fi

    # https://github.com/<owner>/<repo>/archive/<sha>.tar.gz
    if [[ ! "${url}" =~ ^https://github\.com/([^/]+/[^/]+)/archive/([0-9a-f]{40})\.tar\.gz$ ]]; then
        printf 'error: .%s url is not a github archive pinned by full commit sha: %s\n' \
            "${name}" "${url}" >&2
        status=1
        return
    fi
    slug="${BASH_REMATCH[1]}"
    sha="${BASH_REMATCH[2]}"

    if ! grep -qF "${sha}" "${README}"; then
        printf 'error: README.md does not document the pinned .%s commit %s (%s)\n' \
            "${name}" "${sha}" "${slug}" >&2
        status=1
        return
    fi

    # A stale short sha next to a fresh long one reads as current to a human,
    # so require every short reference to prefix the pin it claims to describe.
    local short
    while read -r short; do
        [[ -n "${short}" ]] || continue
        if [[ "${sha}" != "${short}"* ]]; then
            printf 'error: README.md references %s@%s but the pin is %s\n' \
                "${slug}" "${short}" "${sha}" >&2
            status=1
        fi
    done < <(grep -Eo "${slug}@[0-9a-f]{7,40}" "${README}" | sed "s|${slug}@||")

    pinned_source="Pinned source: https://github.com/${slug}/tree/${sha}"
    notice_source_lines="$(grep -F "Pinned source: https://github.com/${slug}/tree/" "${NOTICES}" || true)"
    if [[ "${notice_source_lines}" != "${pinned_source}" ]]; then
        printf 'error: THIRD_PARTY_NOTICES.md does not record the pinned .%s source: %s\n' \
            "${name}" "${pinned_source}" >&2
        status=1
        return
    fi

    printf 'Pinned %s is %s@%s; README.md and THIRD_PARTY_NOTICES.md agree.\n' \
        "${name}" "${slug}" "${sha}"
}

check_pin native_sdk
check_pin ghostty

exit "${status}"
