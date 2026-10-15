#!/usr/bin/env bash
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$(CDPATH='' cd -- "${ROOT}/../.." && pwd)"
EXPECTED_TAG="${1:-}"

IFS= read -r version < "${ROOT}/version.txt"
if [[ ! "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    printf 'error: version.txt does not contain a semantic version: %s\n' "${version}" >&2
    exit 1
fi

read_zon_version() {
    awk -F '"' '/^[[:space:]]*\.version = / { print $2; exit }' "$1"
}

for file in app.zon build.zig.zon; do
    marker_count="$(grep -Ec '^[[:space:]]*\.version = "[^"]+",[[:space:]]*// x-release-please-version[[:space:]]*$' "${ROOT}/${file}" || true)"
    if [[ "${marker_count}" != "1" ]]; then
        printf 'error: %s must have exactly one annotated release version\n' "${file}" >&2
        exit 1
    fi
    actual="$(read_zon_version "${ROOT}/${file}")"
    if [[ "${actual}" != "${version}" ]]; then
        printf 'error: %s version %s does not match version.txt %s\n' \
            "${file}" "${actual:-<missing>}" "${version}" >&2
        exit 1
    fi
done

# Absent while Cockpit is frozen (ADR-0167). A restored release-please
# package must record the same version as version.txt.
if jq -e 'has("clients/cockpit")' "${REPO_ROOT}/.release-please-manifest.json" >/dev/null; then
    manifest_version="$(jq -er '."clients/cockpit"' "${REPO_ROOT}/.release-please-manifest.json")"
    if [[ "${manifest_version}" != "${version}" ]]; then
        printf 'error: release manifest version %s does not match version.txt %s\n' \
            "${manifest_version}" "${version}" >&2
        exit 1
    fi
fi

if [[ -n "${EXPECTED_TAG}" ]]; then
    tag="${EXPECTED_TAG#refs/tags/}"
    if [[ "${tag}" != "cockpit-v${version}" ]]; then
        printf 'error: release tag %s does not match version.txt %s\n' \
            "${tag}" "${version}" >&2
        exit 1
    fi
fi

printf 'Release version %s is synchronized.\n' "${version}"
