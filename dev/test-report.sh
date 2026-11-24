#!/usr/bin/env bash
# Run the whole unit pool to the end and say what is actually new.
#
#   dev/test-report.sh [--base <ref>] [nextest args...]
#
# Unlike `just test`, this does not stop at the first failure, and it reads the
# result back: with --base every failure is rerun on that ref, so "this also
# fails on main" is a measurement rather than a claim. Exit 0 means nothing
# unexplained failed (with --base: nothing that passes on the base ref).
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

base=
if [[ "${1:-}" == --base ]]; then
    base="${2:?--base needs a ref}"
    shift 2
fi

# Nextest's store sits under the workspace root, whatever CARGO_TARGET_DIR is.
work="$root/target/nextest/report"
mkdir -p "$work"
rm -f "$work/junit.xml" "$work/run.log"

# Failed tests from a nextest JUnit report, one `binary-id test-name` per line.
failed_tests() {
    python3 -B -c '
import sys
import xml.etree.ElementTree as ET
for suite in ET.parse(sys.argv[1]).getroot().iter("testsuite"):
    for case in suite.iter("testcase"):
        if case.find("failure") is not None or case.find("error") is not None:
            print(suite.get("name"), case.get("name"))
' "$1" | sort -u
}

run_nextest() {
    # The explicit scrub is for --base: an older ref may predate the Cargo runner.
    PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE=600 bash "$root/dev/test-env-runner.sh" --all \
        cargo nextest run --profile report --no-fail-fast --no-tests=warn "$@"
}

print_section() {
    [[ -s "$2" ]] || return 0
    printf '\n%s\n' "$1"
    sed 's/^/  /' "$2"
}

# Rerun `$1`'s failures on the base ref, in a throwaway worktree that keeps its
# own build directory so a second report does not start cold. Writes the ones
# that also fail there to `$2`. A test missing on the base did not fail there.
rerun_on_base() {
    local list="$1" out="$2" tree filter binary test report
    tree="$(mktemp -d "${TMPDIR:-/tmp}/phux-test-base.XXXXXX")"
    rmdir "$tree"
    git worktree add --quiet --detach "$tree" "$base"
    filter=
    while read -r binary test; do
        filter="${filter:+$filter + }(binary_id(=$binary) & test(=$test))"
    done <"$list"
    phux_info "rerunning $(wc -l <"$list" | tr -d ' ') failure(s) on $base"
    (
        cd "$tree"
        export CARGO_TARGET_DIR="$root/target/test-report-base"
        cp "$root/.config/nextest.toml" .config/nextest.toml
        run_nextest --workspace "${features[@]}" -E "$filter"
    ) >"$work/base.log" 2>&1 || true
    report="$tree/target/nextest/report/junit.xml"
    if [[ -f "$report" ]]; then
        failed_tests "$report" >"$out"
    else
        phux_warn "the rerun on $base produced no report; see $work/base.log"
        : >"$out"
    fi
    git worktree remove --force "$tree"
}

# The pool's feature set: `just test-report` passes the recipe's
# TEST_FEATURES so this shares `just test`'s build.
read -r -a features <<<"${PHUX_TEST_FEATURES:---features phux-server/measure}"

args=("$@")
if [[ ${#args[@]} -eq 0 ]]; then
    args=(--workspace "${features[@]}")
fi

status=0
run_nextest "${args[@]}" >"$work/run.log" 2>&1 || status=$?
grep -E '^ +Summary ' "$work/run.log" || true
if [[ ! -f "$work/junit.xml" ]]; then
    tail -30 "$work/run.log" >&2
    phux_error "test-report: nextest wrote no report (exit $status); see $work/run.log"
    exit 1
fi

failed="$work/failed.txt" unexplained="$work/unexplained.txt"
failed_tests "$work/junit.xml" >"$failed"
cp "$failed" "$unexplained"
: >"$work/also-on-base.txt"
if [[ -n "$base" && -s "$unexplained" ]]; then
    rerun_on_base "$unexplained" "$work/also-on-base.txt"
    comm -23 "$unexplained" "$work/also-on-base.txt" >"$work/introduced.txt"
    cp "$work/introduced.txt" "$unexplained"
fi

print_section "Failed, also failing on ${base:-base}:" "$work/also-on-base.txt"
print_section 'Failed, UNEXPLAINED:' "$unexplained"
printf '\nFull output: %s\n' "$work/run.log"

if [[ -s "$unexplained" ]]; then
    phux_error "test-report: $(wc -l <"$unexplained" | tr -d ' ') unexplained failure(s)."
    exit 1
fi
if [[ "$status" -ne 0 && ! -s "$failed" ]]; then
    tail -30 "$work/run.log" >&2
    phux_error "test-report: nextest failed (exit $status) with no failing test; see $work/run.log"
    exit 1
fi
phux_ok 'test-report: nothing unexplained failed.'
