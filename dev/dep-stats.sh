#!/usr/bin/env bash
# Dependency-graph observability: counts and shapes that explain build cost
# without compiling anything. Reads `cargo metadata` (locked) and prints a
# markdown block to the step summary, or to stdout when run locally.
#
# Signals and why they matter for CI cost:
#   locked_packages   every crate CI may compile cold
#   duplicate crates  same crate, multiple versions — each compiles separately
#   proc_macros       compile serially early and gate everything downstream
#   build_scripts     each is a compile + an execution (libghostty-vt's zig
#                     shell-out is the workspace's dominant single cost)
#
# Usage: dep-stats.sh   (normally via `just dep-stats`)
set -euo pipefail

meta=$(cargo metadata --format-version 1 --locked 2>/dev/null || cargo metadata --format-version 1)

stats=$(jq -c \
    '(.workspace_members) as $ws
     | (.packages | map(select(.id as $id | $ws | index($id)))) as $members
     | ($members | map(.name)) as $member_names
     | {workspace_members: ($members | length),
        locked_packages: (.packages | length),
        direct_deps: ([$members[].dependencies[].name] | unique
                      | map(select(. as $n | $member_names | index($n) | not))
                      | length),
        proc_macros: ([.packages[] | select(any(.targets[]; .kind | index("proc-macro")))] | length),
        build_scripts: ([.packages[] | select(any(.targets[]; .kind | index("custom-build")))] | length),
        duplicates: (.packages | map(select(.name as $n | $member_names | index($n) | not))
                     | group_by(.name) | map(select(length > 1)
                     | {name: .[0].name, versions: [.[].version]}))}' \
    <<<"$meta")

{
    echo "### dependency graph"
    echo
    jq -r '"- locked packages: **\(.locked_packages)** (\(.workspace_members) workspace members, \(.direct_deps) direct deps)
- proc-macro crates: \(.proc_macros); crates with build scripts: \(.build_scripts)
- duplicate versions: **\(.duplicates | length)**"' <<<"$stats"
    if jq -e '.duplicates | length > 0' <<<"$stats" >/dev/null; then
        echo
        echo "| duplicated crate | versions |"
        echo "|---|---|"
        jq -r '.duplicates[] | "| `\(.name)` | \(.versions | join(", ")) |"' <<<"$stats"
    fi
    echo
} >>"${GITHUB_STEP_SUMMARY:-/dev/stdout}"
