#!/usr/bin/env bash
# Keep the executable dsr fallback contract synchronized across the release
# workflow and its operator documentation. This is intentionally offline: the
# failure ordering must be reviewable and deterministic without release access.
set -uo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$(CDPATH='' cd -- "${ROOT}/../.." && pwd)"
WORKFLOW="${REPO_ROOT}/.github/workflows/cockpit-release.yml"
DOC="${ROOT}/docs/dsr-release-fallback.md"
RELEASE_CONFIG="${REPO_ROOT}/release-please-config.json"

pass=0
fail=0

ok() {
    pass=$((pass + 1))
    printf 'ok   %s\n' "$1"
}

bad() {
    fail=$((fail + 1))
    printf 'FAIL %s\n' "$1" >&2
}

line_of() {
    local needle="$1" file="$2" matches
    matches="$(grep -nF -- "${needle}" "${file}" | cut -d: -f1)"
    if [[ ! "${matches}" =~ ^[0-9]+$ ]]; then
        printf 'error: expected exactly one %q in %s, found lines %q\n' \
            "${needle}" "${file#${REPO_ROOT}/}" "${matches:-<none>}" >&2
        return 1
    fi
    printf '%s\n' "${matches}"
}

contains_once() {
    local what="$1" needle="$2" file="$3" count
    count="$(grep -Fc -- "${needle}" "${file}" || true)"
    if [[ "${count}" == 1 ]]; then
        ok "${what}"
    else
        bad "${what}: expected one occurrence, found ${count}"
    fi
}

required="$(awk '
    /^      HOMEBREW_TAP_DEPLOY_KEY:/ { in_tap_secret = 1; next }
    in_tap_secret && /^        required:/ { print $2; exit }
' "${WORKFLOW}")"
if [[ "${required}" == false ]]; then
    ok 'workflow_call admits a keyless replay'
else
    bad "workflow_call HOMEBREW_TAP_DEPLOY_KEY required value is ${required:-missing}, not false"
fi

assets="$(line_of '- name: Attach and verify release assets' "${WORKFLOW}")" || assets=''
upload="$(line_of 'gh release upload "${RELEASE_TAG}"' "${WORKFLOW}")" || upload=''
uploaded="$(line_of 'uploaded_this_run=true' "${WORKFLOW}")" || uploaded=''
download="$(line_of 'gh release download "${RELEASE_TAG}"' "${WORKFLOW}")" || download=''
compare_guard="$(line_of 'if [[ "${uploaded_this_run}" == "true" ]]; then' "${WORKFLOW}")" || compare_guard=''
compare="$(line_of 'cmp "clients/cockpit/zig-out/release/${archive}" "${existing}/${archive}"' "${WORKFLOW}")" || compare=''
restore="$(line_of 'cp "${existing}/${archive}" "${existing}/${image}" "${existing}/SHA256SUMS" clients/cockpit/zig-out/release/' "${WORKFLOW}")" || restore=''
gate="$(line_of '- name: Require Homebrew tap deploy key before publication' "${WORKFLOW}")" || gate=''
tap_checkout="$(line_of 'repository: no-phux/homebrew-tap' "${WORKFLOW}")" || tap_checkout=''
tap_generation="$(line_of 'bash .github/scripts/gen-phux-cockpit-cask.sh' "${WORKFLOW}")" || tap_generation=''
tap_mutation="$(line_of 'bash .github/scripts/commit-update.sh Casks/phux-cockpit.rb' "${WORKFLOW}")" || tap_mutation=''
tap_intended="$(line_of 'intended_cask_blob="$(git hash-object Casks/phux-cockpit.rb)"' "${WORKFLOW}")" || tap_intended=''
tap_fetch="$(line_of 'git fetch origin main' "${WORKFLOW}")" || tap_fetch=''
tap_remote="$(line_of 'remote_cask_blob="$(git rev-parse origin/main:Casks/phux-cockpit.rb)"' "${WORKFLOW}")" || tap_remote=''
remote_proof="$(line_of 'if [[ "${remote_cask_blob}" != "${intended_cask_blob}" ]]; then' "${WORKFLOW}")" || remote_proof=''
publication="$(line_of 'gh release edit "${RELEASE_TAG}" --draft=false' "${WORKFLOW}")" || publication=''

if [[ -n "${assets}" && -n "${upload}" && -n "${uploaded}" && -n "${download}" && \
      -n "${compare_guard}" && -n "${compare}" && -n "${restore}" && -n "${gate}" && \
      -n "${tap_checkout}" && -n "${tap_generation}" && -n "${tap_mutation}" && \
      -n "${tap_intended}" && -n "${tap_fetch}" && -n "${tap_remote}" && \
      -n "${remote_proof}" && -n "${publication}" ]] && \
   (( assets < upload && upload < uploaded && uploaded < download && \
      download < compare_guard && compare_guard < compare && compare < restore && \
      restore < gate && \
      gate < tap_checkout && tap_checkout < tap_generation && \
      tap_generation < tap_intended && tap_intended < tap_mutation && \
      tap_mutation < tap_fetch && tap_fetch < tap_remote && \
      tap_remote < remote_proof && remote_proof < publication )); then
    ok 'only newly uploaded assets are byte-compared, then verified remote assets replace local packages'
    ok 'the generated cask blob, real tap mutation, and remote equality proof precede publication'
else
    bad "release order changed: assets=${assets:-?} upload=${upload:-?} uploaded=${uploaded:-?} download=${download:-?} compare-guard=${compare_guard:-?} compare=${compare:-?} restore=${restore:-?} gate=${gate:-?} tap=${tap_checkout:-?} generation=${tap_generation:-?} tap-mutation=${tap_mutation:-?} intended=${tap_intended:-?} fetch=${tap_fetch:-?} remote=${tap_remote:-?} remote-proof=${remote_proof:-?} publication=${publication:-?}"
fi

contains_once 'empty drafts are the only upload path' 'if [[ -z "${existing_assets}" ]]; then' "${WORKFLOW}"
contains_once 'asset comparison state starts false' 'uploaded_this_run=false' "${WORKFLOW}"
partial_guards="$(grep -Fc -- '[[ "${existing_assets}" != "${expected_assets}" ]]' "${WORKFLOW}" || true)"
if [[ "${partial_guards}" == 2 ]]; then
    ok 'draft and published releases both reject partial or unexpected asset sets'
else
    bad "expected two exact-set guards, found ${partial_guards}"
fi
clobbers="$(grep -Fc -- '--clobber' "${WORKFLOW}" || true)"
if [[ "${clobbers}" == 0 ]]; then
    ok 'release assets are never clobbered'
else
    bad "release workflow still contains ${clobbers} clobber invocation(s)"
fi

contains_once 'workflow has one named keyless stop' 'KEYLESS_RELEASE_STOP:' "${WORKFLOW}"
contains_once 'workflow has one deploy-key gate after asset verification' \
    '- name: Require Homebrew tap deploy key' "${WORKFLOW}"
contains_once 'release creation remains draft-first' '"draft": true' "${RELEASE_CONFIG}"
contains_once 'documentation names the keyless stop' 'fail with `KEYLESS_RELEASE_STOP` when the tap deploy key is absent' "${DOC}"
contains_once 'documentation names the credentialed rerun command' \
    '--repo no-phux/phux' "${DOC}"
contains_once 'documentation delegates stale-tap recovery to the real external workflow' \
    '--repo no-phux/homebrew-tap' "${DOC}"

nonexistent_command="$(grep -Fc -- 'bash scripts/gen-formula.sh' "${DOC}" || true)"
if [[ "${nonexistent_command}" == 0 ]]; then
    ok 'documentation invokes no nonexistent Cockpit-local formula generator'
else
    bad 'documentation still invokes nonexistent bash scripts/gen-formula.sh'
fi

printf '%s passed, %s failed\n' "${pass}" "${fail}"
[[ "${fail}" -eq 0 ]]
