#!/bin/sh
#
# smoke-agent-wrap.sh (phux-r82.11) — exercise phux-agent-wrap.sh with a
# stub `phux` and a fake agent, asserting the record-write path.
#
# Runs the wrapper against a stub `phux` binary that logs its argv and a
# fake agent that exits successfully, then checks that:
#   1. `phux agent set @<pane> --name <name> --kind <kind>` ran at launch;
#   2. the fake agent ran;
#   3. `phux agent clear @<pane>` ran on exit, pinned to the SAME pane;
#   4. the wrapper forwarded the agent's exit status;
#   5. with NO pane target, the wrapper writes NOTHING (never clobbers a
#      focused pane's record) yet still launches the agent.
#
# Needs no phux server and leaves no state behind.

set -eu

script_dir=$(CDPATH=; cd -- "$(dirname -- "$0")" && pwd)
wrapper=$script_dir/phux-agent-wrap.sh
grok_wrapper=$script_dir/phux-grok-wrap.sh
tmp=${TMPDIR:-/tmp}/phux-agent-wrap-smoke.$$

cleanup() {
  rm -rf "$tmp"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$tmp"
argv_log=$tmp/phux-argv.log
agent_log=$tmp/agent.log

# Stub `phux`: append its full argv (tab-separated) to the log.
stub_phux=$tmp/phux
cat > "$stub_phux" <<EOF
#!/bin/sh
printf '%s\t' "\$@" >> "$argv_log"
printf '\n' >> "$argv_log"
EOF
chmod +x "$stub_phux"

# Fake agent: record that it ran, echo its own args, exit non-zero to
# prove the wrapper forwards the status.
fake_agent=$tmp/fake-agent
cat > "$fake_agent" <<EOF
#!/bin/sh
printf 'ran %s\n' "\$*" >> "$agent_log"
exit 7
EOF
chmod +x "$fake_agent"

# A pane target is required for the wrapper to write a record. Pin it via
# PHUX_TERMINAL_ID (the automatic path) so the wrapper self-targets `@3`.
status=0
PHUX_AGENT_PHUX_BIN=$stub_phux \
PHUX_TERMINAL_ID=3 \
  sh "$wrapper" --name claude --kind claude -- "$fake_agent" hello || status=$?

if [ "$status" -ne 7 ]; then
  printf 'FAIL: wrapper did not forward agent exit status (got %s, want 7)\n' "$status" >&2
  exit 1
fi

if [ ! -f "$agent_log" ] || ! grep -q 'ran hello' "$agent_log"; then
  printf 'FAIL: fake agent did not run\n' >&2
  exit 1
fi

# The set line must carry the pane target AND the exact flags, in a single
# invocation. `@3` derives from PHUX_TERMINAL_ID.
if ! grep -q 'agent	set	@3	--name	claude	--kind	claude' "$argv_log"; then
  printf 'FAIL: agent set was not invoked as: set @3 --name claude --kind claude\n' >&2
  printf 'argv log:\n' >&2
  cat "$argv_log" >&2
  exit 1
fi

# Clear must target the SAME pane the launch-time set used, never a bare
# (focused-pane) clear that could delete a sibling agent's record.
if ! grep -q 'agent	clear	@3' "$argv_log"; then
  printf 'FAIL: agent clear was not pinned to the launch pane (@3)\n' >&2
  printf 'argv log:\n' >&2
  cat "$argv_log" >&2
  exit 1
fi

# Grok's prompt-file mode paints no reliable screen or title while it works.
# Its provider wrapper therefore opts into the generic one-turn stream.
: > "$argv_log"
: > "$agent_log"
grok_status=0
PHUX_AGENT_PHUX_BIN=$stub_phux \
PHUX_GROK_BIN=$fake_agent \
PHUX_TERMINAL_ID=3 \
  sh "$grok_wrapper" --no-alt-screen --prompt-file prompt.txt || grok_status=$?

if [ "$grok_status" -ne 7 ]; then
  printf 'FAIL: Grok wrapper did not forward agent exit status (got %s, want 7)\n' "$grok_status" >&2
  exit 1
fi

tab=$(printf '\t')
for expected in \
  "agent${tab}session${tab}open${tab}@3${tab}--provider${tab}grok" \
  "agent${tab}emit${tab}@3${tab}--type${tab}session_start" \
  "agent${tab}emit${tab}@3${tab}--type${tab}prompt" \
  "agent${tab}emit${tab}@3${tab}--type${tab}stop" \
  "agent${tab}session${tab}close${tab}@3" \
  "agent${tab}clear${tab}@3"
do
  if ! grep -q "$expected" "$argv_log"; then
    printf 'FAIL: one-turn Grok lifecycle missing %s\n' "$expected" >&2
    cat "$argv_log" >&2
    exit 1
  fi
done

stop_line=$(grep -n "agent${tab}emit${tab}@3${tab}--type${tab}stop" "$argv_log" | cut -d: -f1)
close_line=$(grep -n "agent${tab}session${tab}close${tab}@3" "$argv_log" | cut -d: -f1)
clear_line=$(grep -n "agent${tab}clear${tab}@3" "$argv_log" | cut -d: -f1)
if [ "$stop_line" -ge "$close_line" ] || [ "$close_line" -ge "$clear_line" ]; then
  printf 'FAIL: one-turn cleanup must emit stop, close its session, then clear identity\n' >&2
  cat "$argv_log" >&2
  exit 1
fi

if ! grep -q 'ran --no-alt-screen --prompt-file prompt.txt' "$agent_log"; then
  printf 'FAIL: Grok wrapper changed the provider argv\n' >&2
  exit 1
fi

# Ordinary interactive Grok stays detector-driven. Opening a stream around a
# long-lived TUI would pin it working for its entire process lifetime.
: > "$argv_log"
PHUX_AGENT_PHUX_BIN=$stub_phux \
PHUX_GROK_BIN=$fake_agent \
PHUX_TERMINAL_ID=3 \
  sh "$grok_wrapper" --no-alt-screen >/dev/null 2>&1 || :
if grep -q "agent${tab}session" "$argv_log" || grep -q "agent${tab}emit" "$argv_log"; then
  printf 'FAIL: interactive Grok incorrectly opened a one-turn stream\n' >&2
  cat "$argv_log" >&2
  exit 1
fi

# Safety case: with no resolvable pane target, the wrapper must NOT touch
# any record (a focused-pane guess would clobber a sibling), yet must still
# run the agent and forward its status.
notarget_log=$tmp/phux-argv-notarget.log
stub_phux_nt=$tmp/phux-nt
cat > "$stub_phux_nt" <<EOF
#!/bin/sh
printf '%s\t' "\$@" >> "$notarget_log"
printf '\n' >> "$notarget_log"
EOF
chmod +x "$stub_phux_nt"

nt_status=0
env -u PHUX_TERMINAL_ID -u PHUX_AGENT_TARGET \
  PHUX_AGENT_PHUX_BIN="$stub_phux_nt" \
  sh "$wrapper" --name claude --kind claude -- "$fake_agent" hi 2>/dev/null || nt_status=$?

if [ "$nt_status" -ne 7 ]; then
  printf 'FAIL: no-target wrapper did not forward agent exit status (got %s, want 7)\n' "$nt_status" >&2
  exit 1
fi

if [ -f "$notarget_log" ]; then
  printf 'FAIL: no-target wrapper wrote a record (would clobber a focused pane)\n' >&2
  printf 'argv log:\n' >&2
  cat "$notarget_log" >&2
  exit 1
fi

printf 'agent wrap smoke ok\n'
