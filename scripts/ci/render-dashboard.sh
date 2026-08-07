#!/usr/bin/env bash
# Regenerate the ci-metrics branch's human dashboard (DASHBOARD.md) and the
# compact machine rollup (site/summary.json) from the NDJSON store written by
# collect-runs.sh. Everything here is derived — safe to delete and re-render.
#
# DASHBOARD.md answers "where do the CI minutes go" at a glance: recent runs,
# per-workflow medians/p95s, per-job queue+wall, the slowest steps, cache hit
# rates, and the latest cold-build / binary-size / dependency snapshots with
# deltas. site/summary.json carries the same rollups (plus short histories
# for sparklines) for phux-site's /ci page, which fetches it from
# raw.githubusercontent.com on the ci-metrics branch.
#
# Usage: render-dashboard.sh <metrics-dir>
set -euo pipefail

dir="${1:?usage: render-dashboard.sh <metrics-dir>}"
now=$(date -u +%s)
generated=$(date -u +%Y-%m-%dT%H:%M:%SZ)

# `|| true`, not `|| echo []`: under pipefail a missing glob fails the
# pipeline AFTER jq already printed "[]", and a second [] would corrupt the
# capture into two JSON documents.
all=$(cat "$dir"/runs/*.ndjson 2>/dev/null | jq -cs '.' || true)
[ -n "$all" ] || all='[]'
if [ "$(jq 'length' <<<"$all")" -eq 0 ]; then
    echo "render-dashboard: no records yet in ${dir}/runs/ — nothing to render"
    exit 0
fi

# One big jq program computes every rollup once; bash only formats markdown.
summary=$(jq -c --argjson now "$now" --arg generated "$generated" '
    def ts: (.created_at // "1970-01-01T00:00:00Z") | fromdateiso8601;
    def median: sort | if length == 0 then null else .[(length - 1) / 2 | floor] end;
    def p95: sort | if length == 0 then null else .[(length - 1) * 0.95 | floor] end;
    def r1: if . == null then null else . * 10 | round / 10 end;
    # A run that was skipped (draft PR) or cancelled (superseded push) never
    # reached a verdict, so counting it against success understates CI health.
    # Rate over concluded runs only; the rest are reported as their own column.
    def concluded: map(select(.conclusion == "success" or .conclusion == "failure"));
    def rate: . as $c
        | if ($c | length) == 0 then null
          else ($c | map(select(.conclusion == "success")) | length)
               / ($c | length) * 100 | round end;

    (map(select(.kind == "run")) | sort_by(-ts)) as $runs
    | ($runs | map(select(ts > $now - 2592000))) as $runs30
    | (map(select(.kind == "phases")) | sort_by(-ts)) as $phases
    | ($phases | map(select(ts > $now - 2592000))) as $phases30
    | (map(select(.kind == "build-timings")) | sort_by(-ts)) as $timings
    | (map(select(.kind == "binary-size")) | sort_by(-ts)) as $sizes
    | (map(select(.kind == "deps")) | sort_by(-ts)) as $deps

    | {schema: 1, generated_at: $generated,
       recent_runs: ($runs | .[:30] | map({workflow, event, branch, conclusion,
           created_at, duration_s,
           runner_s: ([.jobs[].duration_s // 0] | add)})),

       workflows_30d: ($runs30 | group_by(.workflow) | map({
           workflow: .[0].workflow,
           runs: length,
           concluded: (concluded | length),
           success_rate: (concluded | rate),
           main_concluded: (map(select(.branch == "main")) | concluded | length),
           main_success_rate: (map(select(.branch == "main")) | concluded | rate),
           not_run: (map(select(.conclusion != "success" and .conclusion != "failure")) | length),
           median_s: ([.[].duration_s // empty] | median),
           p95_s: ([.[].duration_s // empty] | p95),
           runner_minutes: ([.[].jobs[].duration_s // 0] | add / 60 | round)})
           | sort_by(-.runner_minutes)),

       ci_jobs_30d: ($runs30 | map(select(.workflow == "ci") | .jobs[])
           | group_by(.name) | map({
               job: .[0].name,
               runs: length,
               median_queue_s: ([.[].queued_s // empty] | median),
               median_s: ([.[].duration_s // empty] | median),
               p95_s: ([.[].duration_s // empty] | p95)})
           | sort_by(-.median_s)),

       slow_steps: ($runs30 | map(select(.workflow == "ci") | .jobs[]) | .[:80]
           | map(. as $j | .steps[] | select(.conclusion == "success")
               | {job: $j.name, step: .name, seconds})
           | group_by(.job + "|" + .step)
           | map({job: .[0].job, step: .[0].step,
                  median_s: ([.[].seconds // empty] | median), n: length})
           | map(select(.median_s != null and .median_s >= 5))
           | sort_by(-.median_s) | .[:12]),

       cache: ($phases30 | map(select(.cache_hit != null))
           | group_by(.workflow + "|" + .job) | map({
               workflow: .[0].workflow, job: .[0].job, samples: length,
               hit_rate: ((map(select(.cache_hit)) | length) / length * 100 | round)})),

       phase_medians: ($phases30 | map(. as $r | .phases[]
               | {workflow: $r.workflow, job: $r.job, phase: .name, seconds})
           | group_by(.workflow + "|" + .job + "|" + .phase)
           | map({workflow: .[0].workflow, job: .[0].job, phase: .[0].phase,
                  median_s: ([.[].seconds] | median), n: length})
           | sort_by(.workflow, .job, -.median_s)),

       slow_tests: ($phases | map(select(.slow_tests | length > 0)) | .[0]
           | if . == null then [] else {sha: .sha[:9], tests: .slow_tests[:10]} end),

       build_timings: ($timings | group_by(.profile) | map({
           profile: .[0].profile,
           latest: (.[0] | {sha: .sha[:9], created_at, total_wall_s, units,
                            cpu_seconds, slowest_units: (.slowest_units[:8]),
                            build_script_runs: (.build_script_runs[:5]),
                            terminal_units: (.terminal_units[:5])}),
           previous_total_s: (.[1].total_wall_s? // null),
           history: (.[:12] | map({sha: .sha[:9], created_at, total_wall_s}) | reverse)})),

       binary_size: {
           latest: ($sizes[0] | if . == null then null else
               {sha: .sha[:9], created_at, bins, bloat_crates: (.bloat_crates[:10])} end),
           previous_bins: ($sizes[1].bins? // []),
           history: ($sizes | .[:12] | map({sha: .sha[:9], created_at, bins}) | reverse)},

       deps: {
           latest: ($deps[0] | if . == null then null else
               {sha: .sha[:9], created_at, locked_packages, direct_deps,
                workspace_members, proc_macros, build_scripts,
                duplicates: (.duplicates | length),
                duplicate_list: (.duplicates | map(.name))} end),
           previous: ($deps[1] | if . == null then null else
               {locked_packages, duplicates: (.duplicates | length)} end),
           history: ($deps | .[:12] | map({sha: .sha[:9], created_at,
               locked_packages, duplicates: (.duplicates | length)}) | reverse)}}
    ' <<<"$all")

mkdir -p "$dir/site"
jq '.' <<<"$summary" >"$dir/site/summary.json"

# --- markdown ----------------------------------------------------------------

hms() { # seconds (may be float/null) -> "3m42s" / "42s" / "-"
    local s=${1%%.*}
    if [ -z "$s" ] || [ "$s" = "null" ]; then printf -- '-'
    elif [ "$s" -ge 60 ]; then printf '%dm%02ds' $((s / 60)) $((s % 60))
    else printf '%ss' "$s"; fi
}

{
    echo "# phux CI dashboard"
    echo
    echo "Generated ${generated} by the ci-metrics workflow. Do not edit —"
    echo "every table is re-rendered from \`runs/*.ndjson\` on each update."
    echo "Machine rollup: [\`site/summary.json\`](site/summary.json), rendered live at"
    echo "<https://phux.phall.io/ci>."
    echo
    echo "## Workflows, last 30 days"
    echo
    echo "Success is over **concluded** runs only. Skipped (draft PRs, which the"
    echo "workflow deliberately does not run) and cancelled (superseded pushes)"
    echo "runs never reached a verdict and are counted under \"not run\"."
    echo
    echo "| workflow | runs | concluded | success | main | not run | median | p95 | runner minutes |"
    echo "|---|---:|---:|---:|---:|---:|---:|---:|---:|"
    while IFS=$'\t' read -r wf runs conc ok mainconc mainok notrun med p95 mins; do
        # A workflow with no concluded run has no rate. jq nulls are coalesced
        # to the literal "null" below: @tsv would emit an EMPTY field, and
        # `read` with a tab IFS collapses runs of tabs, silently shifting every
        # later column left. Same trap the binary-size table documents.
        if [ "$mainok" = "null" ]; then main_cell="--"; else main_cell="${mainok}% (${mainconc})"; fi
        if [ "$ok" = "null" ]; then ok_cell="--"; else ok_cell="${ok}%"; fi
        echo "| ${wf} | ${runs} | ${conc} | ${ok_cell} | ${main_cell} | ${notrun} | $(hms "$med") | $(hms "$p95") | ${mins} |"
    done < <(jq -r '.workflows_30d[] | [.workflow, .runs, .concluded, (.success_rate // "null"),
        .main_concluded, (.main_success_rate // "null"), .not_run,
        (.median_s // "null"), (.p95_s // "null"), (.runner_minutes // "null")] | @tsv' <<<"$summary")
    echo
    echo "## ci jobs, last 30 days"
    echo
    echo "| job | runs | median queue | median wall | p95 wall |"
    echo "|---|---:|---:|---:|---:|"
    while IFS=$'\t' read -r job runs q med p95; do
        echo "| ${job} | ${runs} | $(hms "$q") | $(hms "$med") | $(hms "$p95") |"
    done < <(jq -r '.ci_jobs_30d[] | [.job, .runs, (.median_queue_s // "null"),
        (.median_s // "null"), (.p95_s // "null")] | @tsv' <<<"$summary")
    echo
    echo "## Slowest ci steps (median, last 30 days)"
    echo
    echo "| job | step | median | samples |"
    echo "|---|---|---:|---:|"
    while IFS=$'\t' read -r job step med n; do
        echo "| ${job} | ${step} | $(hms "$med") | ${n} |"
    done < <(jq -r '.slow_steps[] | [.job, .step, (.median_s // "null"), .n] | @tsv' <<<"$summary")
    echo
    echo "## Cargo phases inside the lanes (median, last 30 days)"
    echo
    echo "| workflow / job | phase | median | samples |"
    echo "|---|---|---:|---:|"
    while IFS=$'\t' read -r wf job phase med n; do
        echo "| ${wf} / ${job} | ${phase} | $(hms "$med") | ${n} |"
    done < <(jq -r '.phase_medians[] | [.workflow, .job, .phase, (.median_s // "null"), .n] | @tsv' <<<"$summary")
    echo
    echo "## Cache effectiveness (last 30 days)"
    echo
    echo "| workflow / job | rust-cache hit rate | samples |"
    echo "|---|---:|---:|"
    while IFS=$'\t' read -r wf job rate n; do
        if [ "$rate" = "null" ]; then rate_cell="--"; else rate_cell="${rate}%"; fi
        echo "| ${wf} / ${job} | ${rate_cell} | ${n} |"
    done < <(jq -r '.cache[] | [.workflow, .job, (.hit_rate // "null"), .samples] | @tsv' <<<"$summary")
    echo

    if jq -e '.build_timings | length > 0' <<<"$summary" >/dev/null; then
        echo "## Cold build (observatory)"
        echo
        while IFS= read -r bt; do
            profile=$(jq -r '.profile' <<<"$bt")
            total=$(jq -r '.latest.total_wall_s' <<<"$bt")
            prev=$(jq -r '.previous_total_s // empty' <<<"$bt")
            delta=""
            if [ -n "$prev" ]; then
                delta=" (previous: $(hms "$prev"))"
            fi
            echo "### ${profile}: $(hms "$total")${delta} — $(jq -r '.latest.units' <<<"$bt") units at \`$(jq -r '.latest.sha' <<<"$bt")\`"
            echo
            echo "| slowest units | wall |"
            echo "|---|---:|"
            jq -r '.latest.slowest_units[] | "| `\(.name)` | \(.seconds)s |"' <<<"$bt"
            echo
        done < <(jq -c '.build_timings[]' <<<"$summary")
    fi

    if jq -e '.binary_size.latest != null' <<<"$summary" >/dev/null; then
        echo "## Release binary size"
        echo
        echo "| binary | size | previous |"
        echo "|---|---:|---:|"
        while IFS=$'\t' read -r name bytes prev; do
            prev_h="-"
            # @tsv renders a null previous as an EMPTY field, not "null" —
            # treat both as "no prior record" instead of printing 0.0 MiB.
            [ -n "$prev" ] && [ "$prev" != "null" ] && prev_h="$((prev / 1024 / 1024)).$(((prev % 1048576) * 10 / 1048576)) MiB"
            echo "| \`${name}\` | $((bytes / 1024 / 1024)).$(((bytes % 1048576) * 10 / 1048576)) MiB | ${prev_h} |"
        done < <(jq -r '.binary_size as $b | $b.latest.bins[] | .name as $n
            | [$n, .bytes, (($b.previous_bins[] | select(.name == $n) | .bytes) // null)] | @tsv' <<<"$summary")
        echo
    fi

    if jq -e '.deps.latest != null' <<<"$summary" >/dev/null; then
        echo "## Dependency graph"
        echo
        jq -r '.deps as $d | $d.latest |
            "- locked packages: **\(.locked_packages)**\(if $d.previous then " (previous: \($d.previous.locked_packages))" else "" end) — \(.workspace_members) workspace members, \(.direct_deps) direct deps
- duplicate versions: **\(.duplicates)**\(if $d.previous then " (previous: \($d.previous.duplicates))" else "" end)
- proc-macro crates: \(.proc_macros); build-script crates: \(.build_scripts)"' <<<"$summary"
        echo
    fi

    if jq -e '.slow_tests | type == "object"' <<<"$summary" >/dev/null; then
        echo "## Slowest tests (latest instrumented run, \`$(jq -r '.slow_tests.sha' <<<"$summary")\`)"
        echo
        echo "| test | wall |"
        echo "|---|---:|"
        jq -r '.slow_tests.tests[] | "| `\(.binary)::\(.test)` | \(.seconds)s |"' <<<"$summary"
        echo
    fi

    echo "## Recent runs"
    echo
    echo "| when | workflow | event | branch | result | wall | runner time |"
    echo "|---|---|---|---|---|---:|---:|"
    while IFS=$'\t' read -r when wf event branch result wall runner; do
        echo "| ${when} | ${wf} | ${event} | ${branch} | ${result} | $(hms "$wall") | $(hms "$runner") |"
    done < <(jq -r '.recent_runs[] | [(.created_at[:16] | sub("T"; " ")), .workflow, .event,
        (.branch // "-" | .[:32]), .conclusion, .duration_s, .runner_s] | @tsv' <<<"$summary")
    echo
    echo "---"
    echo
    echo "Query the raw store directly, e.g. every recorded ci run's wall time:"
    echo
    echo '```sh'
    echo "git fetch origin ci-metrics && git show origin/ci-metrics:runs/$(date -u +%Y-%m).ndjson \\"
    echo "  | jq -r 'select(.kind == \"run\" and .workflow == \"ci\") | [.created_at, .conclusion, .duration_s] | @tsv'"
    echo '```'
} >"$dir/DASHBOARD.md"

echo "render-dashboard: wrote ${dir}/DASHBOARD.md and ${dir}/site/summary.json"
