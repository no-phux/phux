#!/bin/sh
# Exercise phux-agent-wrap.sh with a stub phux and deterministic fake agents.
# No server is needed; every lifecycle assertion is against complete CLI argv.

set -eu

script_dir=$(CDPATH=; cd -- "$(dirname -- "$0")" && pwd)
wrapper=$script_dir/phux-agent-wrap.sh
grok_wrapper=$script_dir/phux-grok-wrap.sh
tmp=${TMPDIR:-/tmp}/phux-agent-wrap-smoke.$$
argv_log=$tmp/phux-argv.log
agent_log=$tmp/agent.log
tab=$(printf '\t')

cleanup() {
  rm -rf "$tmp"
}
trap cleanup EXIT HUP INT TERM

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  if [ -f "$argv_log" ]; then
    printf 'argv log:\n' >&2
    cat "$argv_log" >&2
  fi
  exit 1
}

reset_logs() {
  : > "$argv_log"
  : > "$agent_log"
}

assert_status() {
  [ "$1" -eq "$2" ] || fail "$3 (got $1, want $2)"
}

assert_line() {
  grep -Fqx "$1" "$argv_log" || fail "$2"
}

assert_no_match() {
  if grep -Fq "$1" "$argv_log"; then
    fail "$2"
  fi
}

assert_once() {
  count=$(grep -Fxc "$1" "$argv_log" || true)
  [ "$count" -eq 1 ] || fail "$2 (got $count)"
}

line_number() {
  grep -Fnx "$1" "$argv_log" | head -n 1 | cut -d: -f1
}

assert_grok_prompt_length() {
  expected_length=$1
  shift
  reset_logs
  prompt_status=0
  STUB_ARGV_LOG=$argv_log STUB_AGENT_LOG=$agent_log STUB_OPEN_RESOURCE=@54 \
  PHUX_AGENT_PHUX_BIN=$stub_phux PHUX_GROK_BIN=$failure_agent PHUX_TERMINAL_ID=3 \
    sh "$grok_wrapper" "$@" || prompt_status=$?
  assert_status "$prompt_status" 7 'Grok prompt-length case changed provider failure'
  assert_line "agent${tab}emit${tab}@54${tab}--type${tab}prompt${tab}--data${tab}{\"length\":$expected_length}" \
    "Grok prompt form $* emitted the wrong character count"
}

mkdir -p "$tmp"

# The stub logs one tab-delimited argv record per call. `session open` returns
# a configurable exact child. In ambiguity mode, parent-addressed lifecycle
# calls fail: @40 models a preexisting child and @42 one racing with @41.
stub_phux=$tmp/phux
cat > "$stub_phux" <<'EOF'
#!/bin/sh
set -u
: "${STUB_ARGV_LOG:?}"
separator=
for arg in "$@"; do
  printf '%s%s' "$separator" "$arg" >> "$STUB_ARGV_LOG"
  separator=$(printf '\t')
done
printf '\n' >> "$STUB_ARGV_LOG"

if [ "${1:-}" = agent ] && [ "${2:-}" = session ] && [ "${3:-}" = open ]; then
  printf '%s\n' "${STUB_OPEN_RESOURCE:-@41}"
  exit 0
fi

if [ "${STUB_AMBIGUOUS_PARENT:-0}" = 1 ]; then
  if [ "${1:-}" = agent ] && [ "${2:-}" = emit ] && [ "${3:-}" = @3 ]; then
    exit 73
  fi
  if [ "${1:-}" = agent ] && [ "${2:-}" = session ] && \
     [ "${3:-}" = close ] && [ "${4:-}" = @3 ]; then
    exit 73
  fi
fi
EOF
chmod +x "$stub_phux"

success_agent=$tmp/success-agent
cat > "$success_agent" <<'EOF'
#!/bin/sh
printf 'ran %s\n' "$*" >> "${STUB_AGENT_LOG:?}"
exit 0
EOF
chmod +x "$success_agent"

failure_agent=$tmp/failure-agent
cat > "$failure_agent" <<'EOF'
#!/bin/sh
printf 'ran %s\n' "$*" >> "${STUB_AGENT_LOG:?}"
exit 7
EOF
chmod +x "$failure_agent"

# Ordinary identity: preserve a nonzero provider outcome and clear only the
# pane whose identity was set.
reset_logs
status=0
STUB_ARGV_LOG=$argv_log STUB_AGENT_LOG=$agent_log \
PHUX_AGENT_PHUX_BIN=$stub_phux PHUX_TERMINAL_ID=3 \
  sh "$wrapper" --name claude --kind claude -- "$failure_agent" hello || status=$?
assert_status "$status" 7 'wrapper did not preserve a nonzero agent status'
grep -Fq 'ran hello' "$agent_log" || fail 'nonzero fake agent did not run'
assert_line "agent${tab}set${tab}@3${tab}--name${tab}claude${tab}--kind${tab}claude" \
  'identity set did not target @3'
assert_line "agent${tab}clear${tab}@3" 'identity clear did not target @3'

# Successful one-turn Grok: the parent already has @40 and gets a racing @42,
# while this open returns @41. Every emit/close must use @41, never ambiguous @3.
reset_logs
printf 'hello grok\n' > "$tmp/prompt.txt"
status=0
STUB_ARGV_LOG=$argv_log STUB_AGENT_LOG=$agent_log \
STUB_OPEN_RESOURCE=@41 STUB_AMBIGUOUS_PARENT=1 \
STUB_EXISTING_CHILD=@40 STUB_RACING_CHILD=@42 \
PHUX_AGENT_PHUX_BIN=$stub_phux PHUX_GROK_BIN=$success_agent PHUX_TERMINAL_ID=3 \
  sh "$grok_wrapper" --no-alt-screen --prompt-file "$tmp/prompt.txt" || status=$?
assert_status "$status" 0 'successful Grok wrapper changed the provider outcome'
assert_line "agent${tab}session${tab}open${tab}@3${tab}--provider${tab}grok" \
  'one-turn Grok did not open under @3'
assert_line "agent${tab}emit${tab}@41${tab}--type${tab}session_start${tab}--data${tab}{\"provider\":\"grok\"}" \
  'session_start did not carry the full provider record'
assert_line "agent${tab}emit${tab}@41${tab}--type${tab}prompt${tab}--data${tab}{\"length\":11}" \
  'prompt did not carry the truthful file character count'
assert_line "agent${tab}emit${tab}@41${tab}--type${tab}stop" \
  'successful turn did not emit stop'
assert_line "agent${tab}emit${tab}@41${tab}--type${tab}session_end${tab}--data${tab}{\"reason\":\"completed\"}" \
  'successful turn did not emit terminal session_end'
assert_line "agent${tab}session${tab}close${tab}@41" \
  'successful turn did not close the exact opened child'
assert_no_match "agent${tab}emit${tab}@3" 'lifecycle emit used the ambiguous parent'
assert_no_match "agent${tab}session${tab}close${tab}@3" \
  'session close used the ambiguous parent'

stop_line=$(line_number "agent${tab}emit${tab}@41${tab}--type${tab}stop")
end_line=$(line_number "agent${tab}emit${tab}@41${tab}--type${tab}session_end${tab}--data${tab}{\"reason\":\"completed\"}")
close_line=$(line_number "agent${tab}session${tab}close${tab}@41")
clear_line=$(line_number "agent${tab}clear${tab}@3")
if [ "$stop_line" -ge "$end_line" ] || [ "$end_line" -ge "$close_line" ] || \
   [ "$close_line" -ge "$clear_line" ]; then
  fail 'success cleanup order was not stop, session_end, close, clear'
fi
grep -Fq "ran --no-alt-screen --prompt-file $tmp/prompt.txt" "$agent_log" || \
  fail 'Grok wrapper changed the provider argv'

# Grok 1.0.30 documents `-p, --single <PROMPT>` plus JSON content-block input.
# Exercise separate and equals forms so argument consumption cannot drift.
assert_grok_prompt_length 5 -p hello
assert_grok_prompt_length 11 --single 'hello world'
assert_grok_prompt_length 5 --single=hello
assert_grok_prompt_length 5 --prompt-json '["x"]'
assert_grok_prompt_length 5 '--prompt-json=["x"]'
assert_grok_prompt_length 11 "--prompt-file=$tmp/prompt.txt"

# A provider failure is terminal but not completion: no stop, reasoned
# session_end, then exact-child close and parent identity clear.
reset_logs
status=0
provider=$(printf 'grok"lab\\one\013x')
STUB_ARGV_LOG=$argv_log STUB_AGENT_LOG=$agent_log STUB_OPEN_RESOURCE=@51 \
PHUX_AGENT_PHUX_BIN=$stub_phux PHUX_TERMINAL_ID=3 \
  sh "$wrapper" --name grok --kind "$provider" --stream-single-turn \
    --prompt-length 4 -- "$failure_agent" fail || status=$?
assert_status "$status" 7 'one-turn wrapper did not preserve provider failure'
assert_no_match "${tab}stop" 'failed turn falsely emitted stop'
assert_line "agent${tab}session${tab}open${tab}@3${tab}--provider${tab}$provider" \
  'open did not preserve the open-vocabulary provider'
assert_line "agent${tab}emit${tab}@51${tab}--type${tab}session_start${tab}--data${tab}{\"provider\":\"grok\\\"lab\\\\one\\u000bx\"}" \
  'session_start did not JSON-escape its provider'
assert_line "agent${tab}emit${tab}@51${tab}--type${tab}session_end${tab}--data${tab}{\"reason\":\"exit_status_7\"}" \
  'failed turn did not emit a reasoned session_end'
assert_line "agent${tab}session${tab}close${tab}@51" \
  'failed turn did not close its exact child'

# Command lookup failure is preserved as 127 and is never represented as done.
reset_logs
status=0
STUB_ARGV_LOG=$argv_log STUB_OPEN_RESOURCE=@52 \
PHUX_AGENT_PHUX_BIN=$stub_phux PHUX_TERMINAL_ID=3 \
  sh "$wrapper" --name grok --kind grok --stream-single-turn \
    --prompt-length 0 -- "$tmp/does-not-exist" 2> "$tmp/not-found.err" || status=$?
assert_status "$status" 127 'wrapper did not preserve command-not-found status'
assert_no_match "${tab}stop" 'command-not-found falsely emitted stop'
assert_line "agent${tab}emit${tab}@52${tab}--type${tab}session_end${tab}--data${tab}{\"reason\":\"command_not_found\"}" \
  'command-not-found did not emit its terminal reason'
assert_line "agent${tab}session${tab}close${tab}@52" \
  'command-not-found did not close its exact child'

# Signal the wrapper PID, not the child. The wrapper must forward TERM, reap the
# child, return 143, avoid stop, and clean the stream and identity once.
blocking_agent=$tmp/blocking-agent
child_pid_file=$tmp/child.pid
child_signal_log=$tmp/child-signal.log
cat > "$blocking_agent" <<'EOF'
#!/bin/sh
trap 'printf "TERM\n" >> "${STUB_CHILD_SIGNAL_LOG:?}"; exit 0' TERM
printf '%s\n' "$$" > "${STUB_CHILD_PID_FILE:?}"
while :; do sleep 1; done
EOF
chmod +x "$blocking_agent"

reset_logs
STUB_ARGV_LOG=$argv_log STUB_OPEN_RESOURCE=@53 \
STUB_CHILD_PID_FILE=$child_pid_file STUB_CHILD_SIGNAL_LOG=$child_signal_log \
PHUX_AGENT_PHUX_BIN=$stub_phux PHUX_TERMINAL_ID=3 \
  sh "$wrapper" --name grok --kind grok --stream-single-turn \
    --prompt-length 3 -- "$blocking_agent" &
wrapper_pid=$!
attempt=0
while [ ! -s "$child_pid_file" ] && [ "$attempt" -lt 100 ]; do
  sleep 0.05
  attempt=$((attempt + 1))
done
[ -s "$child_pid_file" ] || fail 'signal test child did not start'
child_pid=$(cat "$child_pid_file")
kill -TERM "$wrapper_pid"
status=0
wait "$wrapper_pid" || status=$?
assert_status "$status" 143 'wrapper did not preserve 128+TERM'
grep -Fq TERM "$child_signal_log" || fail 'wrapper did not forward TERM to child'
if kill -0 "$child_pid" 2>/dev/null; then
  fail 'wrapper did not reap the signaled child'
fi
assert_no_match "${tab}stop" 'aborted turn falsely emitted stop'
assert_once "agent${tab}emit${tab}@53${tab}--type${tab}session_end${tab}--data${tab}{\"reason\":\"signal_TERM\"}" \
  'signal path did not emit exactly one terminal session_end'
assert_once "agent${tab}session${tab}close${tab}@53" \
  'signal path did not close its exact child exactly once'
assert_once "agent${tab}clear${tab}@3" \
  'signal path did not clear identity exactly once'

# Ordinary interactive Grok remains detector-driven and opens no stream.
reset_logs
STUB_ARGV_LOG=$argv_log STUB_AGENT_LOG=$agent_log \
PHUX_AGENT_PHUX_BIN=$stub_phux PHUX_GROK_BIN=$failure_agent PHUX_TERMINAL_ID=3 \
  sh "$grok_wrapper" --no-alt-screen >/dev/null 2>&1 || :
assert_no_match "agent${tab}session" 'interactive Grok opened a one-turn stream'
assert_no_match "agent${tab}emit" 'interactive Grok emitted one-turn records'

# With no target, never touch a focused pane, but still run and preserve status.
reset_logs
status=0
env -u PHUX_TERMINAL_ID -u PHUX_AGENT_TARGET \
  STUB_ARGV_LOG="$argv_log" STUB_AGENT_LOG="$agent_log" \
  PHUX_AGENT_PHUX_BIN="$stub_phux" \
  sh "$wrapper" --name claude --kind claude -- "$failure_agent" hi \
    2>/dev/null || status=$?
assert_status "$status" 7 'no-target wrapper changed the provider status'
if [ -s "$argv_log" ]; then
  fail 'no-target wrapper touched phux state'
fi

printf 'agent wrap smoke ok\n'
