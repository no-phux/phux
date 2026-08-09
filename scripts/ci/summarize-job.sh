#!/usr/bin/env bash
# Render one CI job's observability signal into the run's step summary: a
# table of phase timings (from scripts/ci/timed.sh), cache effectiveness,
# target-dir size, and the slowest tests when a nextest junit report exists.
#
# The run page is the whole dashboard (ADR-0080). This used to also emit an
# NDJSON record for a collector that swept it into a metrics branch; that
# store is retired, so nothing here outlives the run.
#
# Runs OUTSIDE `nix develop` (needs only bash + jq + coreutils, all present
# on the hosted runner). Deliberately `if: always()`-safe: every input is
# optional, a failed or skipped lane still renders.
#
# Inputs (environment):
#   PHUX_LANE_DIR      where timed.sh wrote phases.ndjson
#                      (default target/lane-signal)
#   PHUX_CACHE_HIT     rust-cache's cache-hit output ("true"/"false"/"")
#   PHUX_DOCS_ONLY     "true" when the docs-only gate skipped the lane
#   PHUX_JUNIT         path to a nextest junit.xml (optional)
#   GITHUB_*           standard Actions metadata (optional; blank locally)
set -euo pipefail

lane_dir="${PHUX_LANE_DIR:-target/lane-signal}"
mkdir -p "$lane_dir"

# --- gather ------------------------------------------------------------------

phases='[]'
if [ -s "$lane_dir/phases.ndjson" ]; then
    phases=$(jq -cs '.' "$lane_dir/phases.ndjson")
fi

target_size=0
if [ -d target ]; then
    # GNU du has -b; BSD (macOS, local runs) does not. Best-effort.
    target_size=$(du -sb target 2>/dev/null | cut -f1 || true)
    if [ -z "$target_size" ]; then
        target_size=$(( $(du -sk target | cut -f1) * 1024 ))
    fi
fi

# Slowest tests + counts from a nextest junit report. Attribute extraction is
# best-effort text scraping (nextest writes one <testcase .../> tag per line
# attribute-wise); rust test paths do not need XML unescaping in practice.
slow_tests='[]'
junit_stats='null'
junit_path="${PHUX_JUNIT:-}"
if [ -n "$junit_path" ] && [ -s "$junit_path" ]; then
    # awk, not `head -15`: head closes the pipe after 15 lines and a junit
    # with thousands of testcases then kills sort with SIGPIPE, which
    # pipefail turns into a failed lane ("sort: write error").
    slow_tests=$(grep -oE '<testcase [^>]*' "$junit_path" \
        | sed -nE 's/.* name="([^"]*)".* classname="([^"]*)".* time="([^"]*)".*/\2\t\1\t\3/p' \
        | sort -t"$(printf '\t')" -k3,3 -rn | awk 'NR <= 15' \
        | jq -cRs '[split("\n")[] | select(length > 0) | split("\t")
                    | {binary: .[0], test: .[1], seconds: (.[2] | tonumber)}]')
    # grep -c prints the 0 itself (exiting 1), so `|| true` — appending a
    # fallback echo would emit "0\n0" and corrupt the --argjson input.
    tests=$(grep -c '<testcase ' "$junit_path" || true)
    failures=$(grep -c '<failure' "$junit_path" || true)
    flaky=$(grep -c '<flakyFailure' "$junit_path" || true)
    junit_stats=$(jq -cn \
        --argjson tests "${tests:-0}" \
        --argjson failures "${failures:-0}" \
        --argjson flaky "${flaky:-0}" \
        '{tests: $tests, failures: $failures, flaky: $flaky}')
fi

# --- step summary ------------------------------------------------------------

hms() { # seconds -> "3m42s" / "42s"
    local s=$1
    if [ "$s" -ge 60 ]; then printf '%dm%02ds' $((s / 60)) $((s % 60)); else printf '%ds' "$s"; fi
}

summary=$(mktemp)
{
    echo "### ${GITHUB_JOB:-job} lane signal"
    echo
    if [ "${PHUX_DOCS_ONLY:-}" = "true" ]; then
        echo "Docs-only change: compile phases skipped."
        echo
    fi
    if [ "$phases" != "[]" ]; then
        echo "| phase | wall | result |"
        echo "|---|---:|---|"
        while IFS=$'\t' read -r name seconds exit_code; do
            [ "$exit_code" = "0" ] && result=ok || result="failed (exit ${exit_code})"
            echo "| ${name} | $(hms "$seconds") | ${result} |"
        done < <(jq -r '.[] | [.name, .seconds, .exit] | @tsv' <<<"$phases")
        total=$(jq '[.[].seconds] | add' <<<"$phases")
        echo "| **total (timed phases)** | **$(hms "$total")** | |"
        echo
    fi
    case "${PHUX_CACHE_HIT:-}" in
    true) echo "- rust-cache: hit" ;;
    false) echo "- rust-cache: **miss** (cold build)" ;;
    *) echo "- rust-cache: n/a" ;;
    esac
    if [ "${target_size:-0}" -gt 0 ]; then
        echo "- target dir: $((target_size / 1024 / 1024)) MiB"
    fi
    if [ "$junit_stats" != "null" ]; then
        echo "- tests: $(jq -r '"\(.tests) run, \(.failures) failed, \(.flaky) flaky (passed on retry)"' <<<"$junit_stats")"
        echo
        echo "<details><summary>Slowest tests</summary>"
        echo
        echo "| test | wall |"
        echo "|---|---:|"
        jq -r '.[] | "| `\(.binary)::\(.test)` | \(.seconds)s |"' <<<"$slow_tests"
        echo
        echo "</details>"
    fi
    echo
} >"$summary"

cat "$summary" >>"${GITHUB_STEP_SUMMARY:-/dev/stdout}"
rm -f "$summary"
