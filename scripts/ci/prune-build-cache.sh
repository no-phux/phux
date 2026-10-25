#!/usr/bin/env bash
# Keep the newest entry per lane/release prefix. Extracted from ci.yml so the
# workflow stays below the authored-file size gate; policy is unchanged.
set -euo pipefail
keep_newest() {
    gh api --paginate \
        "repos/${GITHUB_REPOSITORY}/actions/caches?key=${1}&sort=created_at&direction=desc&per_page=100" \
        --jq '.actions_caches[].id' | tail -n +2 | while read -r id; do
        echo "deleting ${1} entry ${id}"
        # A 404 means another main run already pruned it.
        gh api -X DELETE "repos/${GITHUB_REPOSITORY}/actions/caches/${id}" --silent || true
        sleep 0.25
    done
}
for prefix in linux-arm64-mbx-phux-check- linux-arm64-mbx-phux-test- \
    v0-rust-phux-uniffi-Linux- linux-x64-mbx-phux-icount- v0-rust-ffi-android-Linux-x64- \
    v0-rust-release-deps-Linux-x64- v0-rust-release-deps-Linux-arm64- \
    v0-rust-release-deps-Darwin-arm64- v0-rust-mini-v1-desktop-release- \
    mini-v1-ffi-xcframework-zig-; do
    keep_newest "$prefix"
done
