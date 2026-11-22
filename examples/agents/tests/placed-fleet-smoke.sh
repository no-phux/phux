#!/usr/bin/env bash
# Hermetic control-flow smoke for orchestrate-placed-fleet. The fake phux
# records argv and returns canonical small JSON shapes; no server or agent CLI
# is required.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/phux-fleet-smoke.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
log="$tmp/argv.log"
fake="$tmp/phux"

cat >"$fake" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
verb="$1"
shift
line="$verb"
for arg in "$@"; do
    line+=$'\t'"$arg"
done
printf '%s\n' "$line" >>"$PHUX_FAKE_LOG"

case "$verb" in
    new)
        printf '{"session":"smoke-fleet","terminal_id":10}\n'
        ;;
    launch)
        if [[ "${1:-}" == "builder-test" ]]; then
            printf '{"schema_version":1,"terminal_id":11,"integration":"builder-test","plugin":"fake","argv":["builder"]}\n'
        else
            printf '{"schema_version":1,"terminal_id":12,"integration":"reviewer-test","plugin":"fake","argv":["reviewer"]}\n'
        fi
        ;;
    spawn)
        printf '{"terminal_id":13,"satellite":null}\n'
        ;;
    move-pane|swap-pane)
        printf '{"schema_version":1,"operation":"%s"}\n' "$verb"
        ;;
    watch)
        target="${*: -1}"
        if [[ "$target" == "@11" ]]; then
            printf '{"event":"asked","terminal":"@11","id":"approve","question":"Approve the patch?","suggestions":["yes","no"],"elapsed_seconds":2}\n'
        else
            printf '{"event":"idle","terminal":"%s"}\n' "$target"
        fi
        # Remain a stream until the example's bounded timer terminates us.
        sleep 30
        ;;
    *)
        echo "fake phux: unexpected verb $verb" >&2
        exit 64
        ;;
esac
FAKE
chmod +x "$fake"

output="$tmp/output.txt"
PHUX="$fake" \
PHUX_FAKE_LOG="$log" \
PHUX_FLEET_SESSION="smoke-fleet" \
PHUX_BUILDER_INTEGRATION="builder-test" \
PHUX_REVIEWER_INTEGRATION="reviewer-test" \
PHUX_FLEET_CWD="$tmp" \
PHUX_WATCH_SECONDS=1 \
    "$ROOT/examples/agents/orchestrate-placed-fleet" >"$output"

# Placement keeps user-facing divider semantics in argv.
grep -F $'launch\tbuilder-test\t--json\t--target\t@10\t--split\tvertical' "$log" >/dev/null
grep -F $'launch\treviewer-test\t--json\t--target\t@11\t--split\thorizontal' "$log" >/dev/null
grep -F $'spawn\t--json\t--target\t@10\t--split\thorizontal' "$log" >/dev/null

# Existing-pane topology plus one server-wide and two pane-scoped bounded
# watch subprocesses ran concurrently.
grep -F $'move-pane\t@12\t@13\t--split\tvertical' "$log" >/dev/null
grep -F $'swap-pane\t@11\t@12\t--json' "$log" >/dev/null
[[ "$(grep -c '^watch' "$log")" -eq 3 ]]
grep -Fx $'watch\t--json' "$log" >/dev/null

# Asked payloads become human guidance; the script never emits a focus command.
grep -F 'Approve the patch? (suggestions: yes, no)' "$output" >/dev/null
grep -F 'press C-a q' "$output" >/dev/null
grep -F 'C-a Q to return' "$output" >/dev/null
if grep -Eq '^(take|give|focus|paste)([[:space:]]|$)' "$log"; then
    echo "unsafe/non-local command appeared in orchestration argv" >&2
    cat "$log" >&2
    exit 1
fi

printf 'placed-fleet smoke: argv, bounds, ask surfacing, and focus advisory verified\n'
