#!/usr/bin/env bash
# Drive the real app up to N concurrent shells and report where it stops
# (the pty ceiling is an SDK constant, so only opening shells proves it).
#
#   ./scripts/drive-shell-ceiling.sh --want 8      # open 8 terminals, or fail
#   ./scripts/drive-shell-ceiling.sh --want 8 --keep
# measures: concurrent shell ceiling and per-shell process cost
#
# Exit 0 means every terminal exists and is running; exit 1 names the one
# that did not appear or whose spawn was rejected. Every step asserts the
# pattern absent before and present after (`expect_change`). The shell-limit
# badge prints the compiled-in `max_live_shells`. Serial only: see
# scripts/lib/app-instance.sh.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
. "${ROOT}/scripts/lib/measure.sh"
WORK="${TMPDIR:-/tmp}/phux-cockpit-ceiling.$$"
WANT=8
KEEP=0
MEASURE=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --want) WANT="$2"; shift 2 ;;
        --keep) KEEP=1; shift ;;
        --measure) MEASURE=1; shift ;;
        -h|--help) sed -n '2,/^set -euo pipefail/{ /^set -euo pipefail/!p; }' "$0"; exit 0 ;;
        *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done

mkdir -p "$WORK"
APP_PID=""
cleanup() {
    if [[ -n "$APP_PID" && "$KEEP" == "0" ]]; then
        app_instance_stop "$APP_PID"
    fi
    if [[ -n "$APP_PID" && "$KEEP" == "1" ]]; then
        measure_print_retained_run "$APP_PID" "$NATIVE" "$MEASURE_DROPBOX"
    fi
    [[ "$KEEP" == "1" ]] || rm -rf "$WORK"
}
trap cleanup EXIT

# An already-built CLI can be passed in (needed while build.zig.zon points
# the SDK at a local `.path`); the CLI need not match a patched SDK.
NATIVE="${NATIVE:-$("${ROOT}/scripts/build-automation-cli.sh")}"
printf 'root: %s\n' "$ROOT"
printf 'cli:  %s\n' "$NATIVE"
printf 'cli fingerprint: %s\n' "$("$NATIVE" version)"

# A shell that prints one line (a local pane only goes live on output) and
# then holds its pty open quietly.
cat >"${WORK}/hold.sh" <<'HOLD'
#!/bin/sh
printf 'shell up\n'
exec cat
HOLD
chmod +x "${WORK}/hold.sh"
printf 'command = /bin/sh %s\nfont-size = 13\n' "${WORK}/hold.sh" >"${WORK}/config"

measure_launch_isolated "$WORK" "${WORK}/config" "${WORK}/app.log"
APP_PID="$MEASURE_APP_PID"

"$NATIVE" automate wait >/dev/null

# Bind every later read to the instance we launched.
app_instance_bind "$NATIVE" "$APP_PID"

# The compiled-in ceiling, read back out of the running binary.
report_ceiling() {
    local limit
    limit="$(app_instance_snapshot \
        | grep -o 'Shell limit reached: [0-9]* running terminals' \
        | head -1 | cut -d' ' -f4 || true)"
    printf 'compiled-in shell ceiling (read out of the running binary): %s\n' \
        "${limit:-<the badge renders only once a spawn is refused>}"
}
report_ceiling

expect_change() {
    local what="$1" pattern="$2"; shift 2
    if ! "$NATIVE" automate assert --absent "$pattern" >/dev/null 2>&1; then
        printf 'NEGATIVE CONTROL FAILED: %s already matches before the action.\n' "$pattern" >&2
        printf 'This assertion cannot prove %s did anything. Fix the assertion.\n' "$what" >&2
        return 1
    fi
    "$@" >/dev/null
    if ! "$NATIVE" automate assert --timeout-ms 5000 "$pattern" >/dev/null; then
        printf 'FAILED: %s did not produce %s\n' "$what" "$pattern" >&2
        return 1
    fi
    printf '  ok: %s\n' "$what"
}

# `role=tab` widgets exist only once there is more than one tab, so the
# single-tab starting state is asserted through the rendered pane instead.
"$NATIVE" automate assert --timeout-ms 10000 \
    'ready=true' 'role=textbox name="Terminal 1, native terminal, RUNNING' 'dispatch_errors=0' >/dev/null
printf 'structure: ok (Terminal 1 RUNNING)\n'

# What one live shell costs the launched pid: rss (KiB) and open descriptors.
measure() {
    [[ "$MEASURE" == "1" ]] || return 0
    local shells="$1" rss threads fds
    if [[ -z "${MEASURE_BASIS_PRINTED:-}" ]]; then
        MEASURE_BASIS_PRINTED=1
        measure_basis shell_cost \
            "quiet /bin/sh + cat per tab; rss, threads, and fds read from pid ${APP_PID}" \
            "./scripts/drive-shell-ceiling.sh --want ${WANT} --measure"
    fi
    rss="$(ps -o rss= -p "$APP_PID" | tr -d ' ')"
    threads="$(ps -M -p "$APP_PID" | tail -n +2 | wc -l | tr -d ' ')"
    fds="$(lsof -p "$APP_PID" 2>/dev/null | tail -n +2 | wc -l | tr -d ' ')"
    printf 'MEASURED shells=%s rss_kib=%s threads=%s fds=%s\n' "$shells" "$rss" "$threads" "$fds"
}
measure 1

printf 'opening %s terminals with cmd+t...\n' "$WANT"
reached=1
for (( n = 2; n <= WANT; n++ )); do
    if expect_change "cmd+t opens Terminal ${n}" "role=tab name=\"Terminal ${n}," \
        "$NATIVE" automate widget-key phux-cockpit-canvas cmd+t; then
        reached="$n"
        measure "$n"
    else
        printf '\nCEILING: stopped at %s live terminals (wanted %s).\n' "$reached" "$WANT" >&2
        # Now that a spawn HAS been refused the badge exists, so the binary can
        # be asked what ceiling it was compiled against rather than inferred.
        report_ceiling >&2
        "$NATIVE" automate snapshot | grep -o 'Terminal [0-9]*, native terminal, [A-Z ]*' >&2 || true
        exit 1
    fi
done

# A tab that exists proves nothing on its own -- phux-cockpit-pg1 was exactly
# a tab that existed with a dead shell behind it. Every one must be RUNNING,
# and no spawn anywhere may have been rejected.
if ! "$NATIVE" automate assert --absent 'SPAWN REJECTED' >/dev/null; then
    printf 'FAILED: a spawn was rejected even though every tab appeared.\n' >&2
    "$NATIVE" automate snapshot | grep -o 'Terminal [0-9]*, native terminal, [A-Z ]*' >&2 || true
    exit 1
fi
# Count published tab widgets (each tab's name embeds its pane status). The
# strip only publishes tabs that fit, so this is bounded by strip width; the
# per-tab proof is the `expect_change` chain above.
published="$(app_instance_snapshot | grep -c 'role=tab ' || true)"
running="$(app_instance_snapshot | grep -c 'role=tab .*native terminal, RUNNING' || true)"
printf '\ntabs opened and individually proven present: %s\n' "$WANT"
printf 'tabs the strip publishes: %s (it scrolls; the rest are off-strip)\n' "$published"
printf 'of those, RUNNING: %s   SPAWN REJECTED: 0\n' "$running"
if [[ "$running" -lt "$published" ]]; then
    printf 'FAILED: the strip publishes %s tabs but only %s are RUNNING.\n' "$published" "$running" >&2
    "$NATIVE" automate snapshot | grep -o 'role=tab name="Terminal [0-9]*, native terminal, [^;]*' >&2 || true
    exit 1
fi
printf 'ceiling drive: ok (%s concurrent shells, %s of them observably RUNNING)\n' "$WANT" "$running"
