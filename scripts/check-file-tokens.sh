#!/usr/bin/env bash
# Pre-commit gate: reject staged files larger than the agent token budget.
#
# Tokens are estimated as bytes/4 of the staged blob. Only files agents author
# are checked: binaries, lockfiles, and generated or vendored files are exempt.
# Mark generated or vendored paths in .gitattributes (`linguist-generated=true`
# or `linguist-vendored=true`) to exempt them; files whose header says
# "@generated", "Code generated", or "DO NOT EDIT" are exempt automatically.
set -euo pipefail

max_tokens=5000
max_bytes=$((max_tokens * 4))

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
        grep -qE ': (set|true)$'
}

has_generated_header() {
    git cat-file blob ":$1" 2>/dev/null | head -c 2048 |
        LC_ALL=C grep -qiE '@generated|code generated|auto-?generated|do not edit'
}

is_exempt() {
    is_lockfile "$1" || has_exempt_attribute "$1" || has_generated_header "$1"
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
