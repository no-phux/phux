#!/usr/bin/env bash
# Shared by both dispatch-only FFI workflows. The environment supplies the
# workflow/artifact names; uncertain evidence always leaves the build enabled.
set -Eeo pipefail

summary="${RUNNER_TEMP:-/tmp}/ffi-preflight-summary.md"
mkdir -p "$(dirname "$summary")"
: >"$summary"
skip=false
release_reason=""
matched_url=""
matched_id=""
matched_head=""
matched_event=""
matched_names=""

# shellcheck disable=SC2329 # invoked from the EXIT trap
finish() {
    if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
        echo "skip=${skip}" >>"$GITHUB_OUTPUT"
    fi
    if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
        cat "$summary" >>"$GITHUB_STEP_SUMMARY"
    fi
    echo "preflight: skip=${skip}"
}
trap finish EXIT

# shellcheck disable=SC2329 # invoked from the ERR trap
fail_open() {
    local status=$?
    skip=false
    {
        echo "### Preflight could not check"
        echo
        echo "A command failed with status ${status}, so this run will build."
        echo "An uncertain check does not skip the build."
    } >"$summary"
    exit 0
}
trap fail_open ERR

say_build() {
    {
        echo "### Build will run"
        echo
        printf '%s\n' "$1"
    } >"$summary"
    skip=false
    exit 0
}

say_skip() {
    {
        echo "### Existing build"
        echo
        printf '%s\n' "$1"
    } >"$summary"
    skip=true
    exit 0
}

check_archive() {
    local dir="$1" zip="$2" sum="$3" prov="$4" line_prefix="$5"
    local zip_path="$dir/$zip" sum_path="$dir/$sum" prov_path="$dir/$prov"
    local count digest name actual want
    if [[ ! -f "$zip_path" || ! -f "$sum_path" || ! -f "$prov_path" ]]; then
        release_reason="Download of \`${zip}\` was incomplete. The build will run."
        return 1
    fi
    count="$(awk 'NF { n++; if (NF != 2) bad = 1 } END { print bad ? -1 : n + 0 }' "$sum_path")"
    digest="$(awk 'NF { print tolower($1); exit }' "$sum_path")"
    name="$(awk 'NF { print $2; exit }' "$sum_path")"
    name="${name#\*}"
    if [[ "$count" != "1" || "$name" != "$zip" || ! "$digest" =~ ^[0-9a-f]{64}$ ]]; then
        release_reason="Checksum \`${sum}\` does not name \`${zip}\` as one sha256 line. The build will run."
        return 1
    fi
    actual="$(sha256sum "$zip_path" | awk '{ print $1 }')"
    if [[ "$actual" != "$digest" ]]; then
        release_reason="\`${zip}\` does not match its sha256 sidecar. The build will run and replace it."
        return 1
    fi
    if [[ "$line_prefix" == *= ]]; then
        want="${line_prefix}${expected}"
    else
        want="${line_prefix} ${expected}"
    fi
    if ! grep -qx -- "$want" "$prov_path"; then
        release_reason="Provenance \`${prov}\` does not name \`${expected}\`. The build will run and replace it."
        return 1
    fi
}

verify_release() {
    local view draft target names prefix zip_suffix prov_suffix line_prefix
    local zip prov sum dir proof spec
    local -a specs=()
    if ! view="$(gh release view "$RELEASE_TAG" --repo "$GH_REPO" --json isDraft,targetCommitish,assets)"; then
        release_reason="No GitHub release \`${RELEASE_TAG}\` exists yet. The build will run and attach once it does, as it does today."
        return 1
    fi
    draft="$(jq -r '.isDraft' <<<"$view")"
    target="$(jq -r '.targetCommitish' <<<"$view")"
    if [[ "$target" != "$expected" ]]; then
        release_reason="Release \`${RELEASE_TAG}\` records target \`${target}\`, not \`${expected}\`. phux-mobile's fetcher requires \`target_commitish\` to be that commit, so the build will run."
        return 1
    fi
    names="$(jq -r '.assets[].name' <<<"$view")"
    while IFS='|' read -r prefix zip_suffix prov_suffix line_prefix; do
        [[ -n "$prefix" ]] || continue
        zip="${prefix}${RELEASE_TAG}${zip_suffix}"
        prov="${prefix}${RELEASE_TAG}${prov_suffix}"
        sum="${zip}.sha256"
        if ! grep -Fxq -- "$zip" <<<"$names" \
            || ! grep -Fxq -- "$sum" <<<"$names" \
            || ! grep -Fxq -- "$prov" <<<"$names"; then
            release_reason="Release \`${RELEASE_TAG}\` is missing \`${zip}\`, its checksum, or its provenance. The build will run and attach."
            return 1
        fi
        specs+=("${prefix}|${zip_suffix}|${prov_suffix}|${line_prefix}")
    done <<<"$RELEASE_SPECS"
    if [[ "${#specs[@]}" -eq 0 ]]; then
        release_reason="Preflight has no release archive specs. The build will run."
        return 1
    fi
    dir="$(mktemp -d)"
    proof=""
    for spec in "${specs[@]}"; do
        IFS='|' read -r prefix zip_suffix prov_suffix line_prefix <<<"$spec"
        zip="${prefix}${RELEASE_TAG}${zip_suffix}"
        prov="${prefix}${RELEASE_TAG}${prov_suffix}"
        sum="${zip}.sha256"
        if ! gh release download "$RELEASE_TAG" --repo "$GH_REPO" --dir "$dir" \
            --pattern "$zip" --pattern "$sum" --pattern "$prov"; then
            rm -rf "$dir"
            release_reason="Could not download \`${zip}\` from \`${RELEASE_TAG}\`. The build will run."
            return 1
        fi
        if ! check_archive "$dir" "$zip" "$sum" "$prov" "$line_prefix"; then
            rm -rf "$dir"
            return 1
        fi
        proof+="- \`${zip}\` sha256 matches, and provenance names \`${expected}\`"$'\n'
    done
    rm -rf "$dir"
    {
        printf 'Skipping the %s build for %s.\n\n' "$PREFLIGHT_LABEL" "$expected"
        printf 'Release [%s](https://github.com/%s/releases/tag/%s) already has verified archives for this commit:\n\n' \
            "$RELEASE_TAG" "$GH_REPO" "$RELEASE_TAG"
        printf '%s\n' "$proof"
        if [[ "$draft" == "true" ]]; then
            echo "The release is still a draft. The archives already verify, so the build is skipped anyway."
            echo
        fi
        echo "Attach is a step inside the build job and would upload these same bytes again (\`--clobber\`, then \`cmp\`). Skipping the build skips that step because the release already holds them."
        echo
        echo "\`notify-mobile\` still runs. It only sends \`client_payload[tag]\` and does not read the build job's outputs or artifacts."
    }
}

run_has_artifacts() {
    local run_id="$1" head="$2"
    local payload count name untagged
    local -a bare_names=() want=()
    if ! payload="$(gh api "repos/${GH_REPO}/actions/runs/${run_id}/artifacts?per_page=100")"; then
        echo "preflight: could not list artifacts for run ${run_id}" >&2
        return 1
    fi
    read -r -a bare_names <<<"$BARE_ARTIFACTS"
    for name in "${bare_names[@]}"; do
        want+=("${name}${suffix}")
    done
    matched_names=""
    for name in "${want[@]}"; do
        count="$(jq -r --arg n "$name" '[.artifacts[] | select(.name == $n and .expired == false)] | length' <<<"$payload")"
        [[ "$count" == "1" ]] || return 1
        matched_names+="${name}"$'\n'
    done
    if [[ -z "$suffix" ]]; then
        [[ "$head" == "$expected" ]] || return 1
        # Require positive proof this run did not build a tag. Historical
        # Android runs lack the marker entirely; absence is not an untagged
        # build, because run headSha can differ from the tag's built commit.
        if ! untagged="$(gh api "repos/${GH_REPO}/actions/runs/${run_id}/jobs?per_page=100" \
            --jq '[.jobs[].steps[]? | select(.name == "Validate release tag") | .conclusion] == ["skipped"]')"; then
            echo "preflight: could not read jobs for run ${run_id}" >&2
            return 1
        fi
        if [[ "$untagged" != "true" ]]; then
            echo "preflight: run ${run_id} has no unique untagged-build proof; ignoring its unsuffixed artifacts" >&2
            return 1
        fi
    fi
}

find_run() {
    local commit_json main_json rows run_id head event url
    if ! commit_json="$(gh run list --repo "$GH_REPO" --workflow "$WORKFLOW_FILE" \
        --commit "$expected" --status success --limit 20 \
        --json databaseId,headSha,event,url,createdAt)"; then
        return 2
    fi
    main_json='[]'
    if [[ -n "$suffix" ]]; then
        if ! main_json="$(gh run list --repo "$GH_REPO" --workflow "$WORKFLOW_FILE" \
            --branch main --event workflow_dispatch --status success --limit 50 \
            --json databaseId,headSha,event,url,createdAt)"; then
            return 2
        fi
    fi
    if ! rows="$(jq -s -r --arg self "${GITHUB_RUN_ID:-0}" '
        add
        | map(select((.databaseId | tostring) != $self))
        | sort_by(.createdAt) | reverse
        | unique_by(.databaseId)
        | .[]
        | [.databaseId, .headSha, .event, .url] | @tsv
    ' <(printf '%s\n' "$commit_json") <(printf '%s\n' "$main_json"))"; then
        return 2
    fi
    while IFS=$'\t' read -r run_id head event url; do
        [[ -n "$run_id" ]] || continue
        if run_has_artifacts "$run_id" "$head"; then
            matched_id="$run_id"
            matched_head="$head"
            matched_event="$event"
            matched_url="$url"
            return 0
        fi
    done <<<"$rows"
    return 1
}

if [[ -n "${RELEASE_TAG}" && -n "${COMMIT_INPUT}" ]]; then
    say_build "Both \`tag\` and \`commit\` were set. The build will run and reject that, as it does today."
fi
if [[ -n "${COMMIT_INPUT}" && ! "${COMMIT_INPUT}" =~ ^[0-9a-f]{40}$ ]]; then
    say_build "\`commit\` is not a full 40-character SHA. The build will run and reject it, as it does today."
fi
tag_re='^v[0-9]+[.][0-9]+[.][0-9]+([-.][0-9A-Za-z][0-9A-Za-z.-]*)?$'
if [[ -n "${RELEASE_TAG}" && ! "${RELEASE_TAG}" =~ $tag_re ]]; then
    say_build "\`tag\` does not match a root release tag. The build will run and reject it, as it does today."
fi

if [[ -n "${RELEASE_TAG}" ]]; then
    if ! expected="$(gh api "repos/${GH_REPO}/commits/${RELEASE_TAG}" --jq .sha)"; then
        say_build "Tag \`${RELEASE_TAG}\` does not resolve. The build will run and report that, as it does today."
    fi
elif [[ -n "${COMMIT_INPUT}" ]]; then
    expected="${COMMIT_INPUT}"
else
    expected="${GITHUB_SHA}"
fi
if [[ ! "${expected}" =~ ^[0-9a-f]{40}$ ]]; then
    say_build "Could not resolve a full commit SHA (\`${expected}\`). The build will run."
fi

suffix=""
if [[ -z "${RELEASE_TAG}" && -n "${COMMIT_INPUT}" && "${COMMIT_INPUT}" != "${GITHUB_SHA}" ]]; then
    suffix="-${expected}"
fi

if [[ -n "${RELEASE_TAG}" ]]; then
    proof_file="$(mktemp)"
    # Current shell, not a substitution: release_reason has to survive a miss.
    if verify_release >"$proof_file"; then
        say_skip "$(cat "$proof_file")"
    fi
    rm -f "$proof_file"
    say_build "${release_reason:-Release \`${RELEASE_TAG}\` is not a verified build of \`${expected}\`. The build will run and attach.}"
fi

find_status=0
find_run || find_status=$?
if [[ "$find_status" -eq 0 ]]; then
    if [[ ! "$matched_url" =~ ^https://github.com/ ]]; then
        matched_url="https://github.com/${GH_REPO}/actions/runs/${matched_id}"
    fi
    name_list=""
    while IFS= read -r name; do
        [[ -n "$name" ]] || continue
        name_list+="- \`${name}\` (not expired)"$'\n'
    done <<<"$matched_names"
    if [[ -n "$suffix" ]]; then
        fetch_note="Those names end in \`-${expected}\`, which is the \`commit=\` artifact \`scripts/ffi-run-for-pin.sh\` downloads. The lookup uses that script's window: 20 successful runs whose head SHA is the commit, and 50 recent successful \`main\` dispatches. This run uploads nothing, so the script skips it and keeps the earlier run."
    elif [[ "$matched_event" != "workflow_dispatch" ]]; then
        fetch_note="The names are unsuffixed because this dispatch's head is the built commit, matching the build job's \`ARTIFACT_SUFFIX\`. \`scripts/ffi-run-for-pin.sh\` accepts an unsuffixed artifact when the run head is the pin and the run is not a \`workflow_dispatch\`, so this earlier run still satisfies that fetch."
    else
        fetch_note="The names are unsuffixed, the run head is the pin, and its release-tag validation was explicitly skipped. A \`commit=\` dispatch from a later main instead requires \`-${expected}\` names; this skip does not hide a missing suffixed artifact."
    fi
    say_skip "$(
        printf 'Skipping the %s build for %s.\n\n' "$PREFLIGHT_LABEL" "$expected"
        printf 'Successful run [%s](%s) of %s (%s, head %s) still has the unexpired artifacts this dispatch would upload:\n\n' \
            "$matched_id" "$matched_url" "$WORKFLOW_FILE" "$matched_event" "$matched_head"
        printf '%s\n' "$name_list"
        printf '\n%s\n\n' "$fetch_note"
        echo "No tag was passed, so the build job's attach step does not run and \`notify-mobile\` stays skipped. Both of those already behave that way when \`inputs.tag\` is empty."
    )"
fi
if [[ "$find_status" -eq 2 ]]; then
    say_build "Could not list earlier runs of \`${WORKFLOW_FILE}\`. The build will run."
fi
want_desc=""
read -r -a bare_names <<<"$BARE_ARTIFACTS"
for name in "${bare_names[@]}"; do
    if [[ -n "$want_desc" ]]; then
        want_desc+=" and "
    fi
    want_desc+="\`${name}${suffix}\`"
done
say_build "No earlier successful run of \`${WORKFLOW_FILE}\` still has unexpired ${want_desc} for \`${expected}\`. The build will run."
