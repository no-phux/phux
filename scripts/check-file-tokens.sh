#!/usr/bin/env bash
# Pre-commit gate: reject staged files larger than the agent token budget.
#
# Tokens are estimated as bytes/4 of the staged blob. Only files agents author
# are checked: binaries, lockfiles, and generated or vendored files are exempt.
# Mark generated or vendored paths in .gitattributes (`linguist-generated=true`
# or `linguist-vendored=true`) to exempt them; files whose header says
# "@generated", "Code generated", or "DO NOT EDIT" are exempt automatically.
#
# A merge may carry an oversized blob unchanged at the same path from either
# parent. New content, including manual resolutions and automatic combinations,
# must meet the limit; ordinary commits keep the same ratchet.
set -euo pipefail

max_tokens=5000
max_bytes=$((max_tokens * 4))

# MERGE_HEAD contains full commit object IDs, not refs or abbreviated IDs.
is_commit_oid() {
    [[ "$1" =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]] || return 1
    [[ "$(git cat-file -t "$1" 2>/dev/null)" == commit ]]
}

read_merge_heads() {
    local parent
    while IFS= read -r parent || [[ -n "$parent" ]]; do
        is_commit_oid "$parent" || return 1
        merge_parents+=("$parent")
    done <"$1"
    ((${#merge_parents[@]} > 0))
}

load_merge_parents() {
    local merge_head head
    merge_head="$(git rev-parse --git-path MERGE_HEAD)"
    [[ -f "$merge_head" ]] || return 1
    read_merge_heads "$merge_head" || return 1
    head="$(git rev-parse --verify HEAD)" || return 1
    is_commit_oid "$head" || return 1
    merge_parents+=("$head")
}

# Invalid or absent merge metadata grants no exclusions, including from HEAD.
merge_parents=()
load_merge_parents || merge_parents=()

is_lockfile() {
    case "${1##*/}" in
        *.lock | *.lockb | *-lock.json | *-lock.yaml | *.lock.json | go.sum | \
            Package.resolved | npm-shrinkwrap.json | gradle.lockfile)
            return 0
            ;;
    esac
    return 1
}

has_exempt_attribute() {
    git check-attr --cached linguist-generated linguist-vendored -- "$1" |
        grep -E ': (set|true)$' >/dev/null
}

has_generated_header() {
    # Inspect only the prefix, but drain the blob so pipefail cannot turn an
    # exemption into a platform-dependent SIGPIPE. Keep producer/read failures.
    git cat-file blob ":$1" 2>/dev/null |
        { head -c 2048 && cat >/dev/null; } |
        LC_ALL=C grep -iE '@generated|code generated|auto-?generated|do not edit' >/dev/null
}

parent_has_blob() {
    local parent="$1" path="$2" staged_blob="$3" parent_blob
    parent_blob=$(git rev-parse --verify "${parent}:$path" 2>/dev/null) || return 1
    [[ "$staged_blob" == "$parent_blob" ]]
}

# Only byte-identical staged content at the same path is inherited.
inherited_from_a_parent() {
    local parent staged_blob
    ((${#merge_parents[@]} > 0)) || return 1
    staged_blob=$(git rev-parse --verify ":$1") || return 1
    for parent in "${merge_parents[@]}"; do
        parent_has_blob "$parent" "$1" "$staged_blob" && return 0
    done
    return 1
}

is_exempt() {
    is_lockfile "$1" || has_exempt_attribute "$1" || has_generated_header "$1" ||
        inherited_from_a_parent "$1"
}

oversized=()
while IFS=$'\t' read -r -d '' added _deleted path; do
    # numstat reports binary files as "-".
    [[ "$added" == "-" ]] && continue
    bytes=$(git cat-file -s ":$path")
    ((bytes > max_bytes)) || continue
    is_exempt "$path" && continue
    oversized+=("$(printf '  %s  ~%d tokens (%d bytes)' "$path" $((bytes / 4)) "$bytes")")
done < <(git diff --cached --numstat --no-renames -z --diff-filter=ACMR)

((${#oversized[@]} == 0)) && exit 0

cat >&2 <<EOF
pre-commit: ${#oversized[@]} staged file(s) exceed the ${max_tokens}-token limit (tokens = bytes/4, so ${max_bytes} bytes):
$(printf '%s\n' "${oversized[@]}")

Split each file listed above before committing:
  1. Find its seams: group the contents by responsibility (types, parsing,
     I/O, state, rendering, CLI wiring, tests, fixtures, doc sections).
  2. Move each group into its own file beside the original, named for what it
     does (e.g. session.rs -> session/{mod,parse,store,render}.rs), keeping
     every resulting file under ${max_tokens} tokens (${max_bytes} bytes).
  3. Keep the original path as a thin entry point that imports or re-exports
     the pieces, so callers and the public API do not change:
       Rust: \`mod parse; pub use parse::*;\`   TS/JS: \`export * from "./parse"\`
       Swift: new files in the same module      Python: package + __init__.py
       Shell: \`source\` helper files             Markdown: an index linking sub-pages
  4. Move tests with the code they cover; put large fixtures or data tables in
     their own files and load them instead of inlining them.
  5. Split by meaning. Do not minify, strip comments, or join lines to get
     under the limit.
  6. \`git add\` the new files and the slimmed original, then commit again.

If a flagged file is generated, vendored, or a lockfile rather than authored,
mark it in .gitattributes instead of splitting it:
  path/to/file linguist-generated=true
EOF
exit 1
