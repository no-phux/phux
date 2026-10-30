#!/usr/bin/env bash
# Time a clean public install through the first agent phux reports.
#
# Method: one sample. The clock is CLOCK_MONOTONIC (python3
# time.monotonic_ns), so a wall-clock step does not move the number.
# Phases run back to back and the total is the same end minus the start,
# not a sum of rounded seconds. The agent phase polls `phux agent list
# --json` every 0.2s; that interval is the quantization of that phase.
# Runner checkout and anything before the install curl are outside the
# clock. The install is the default public installer into a private HOME.
# The same shell then puts ~/.local/bin on PATH. Opening a new terminal
# is not counted. The script does not shorten phux's agent startup grace
# and does not send telemetry.
#
#   bash scripts/first-run-timing.sh            # public install, real timing
#   bash scripts/first-run-timing.sh --self-test
set -euo pipefail

LIMIT_NS=60000000000
POLL_S=0.2
AGENT_CAP_NS=20000000000
INSTALL_URL="${PHUX_INSTALL_URL:-https://phux.sh/install}"

timing_result() {
  local ns="$1"
  if (( ns >= LIMIT_NS )); then
    printf 'fail\n'
  else
    printf 'pass\n'
  fi
}

seconds() {
  python3 -c 'import sys; print(f"{int(sys.argv[1]) / 1e9:.3f}")' "$1"
}

print_summary() {
  printf '\n'
  printf 'clock: CLOCK_MONOTONIC (python3 time.monotonic_ns), one sample\n'
  printf 'fail:  total >= 60.000s, or no agent kind and state\n'
  printf 'poll:  agent list every %ss\n' "$POLL_S"
  printf 'omit:  runner setup and checkout; a new login shell after install\n'
  printf '\n'
  printf '%-24s %10s\n' 'phase' 'seconds'
  printf '%-24s %10s\n' 'download and install' "$(seconds "$install_ns")"
  printf '%-24s %10s\n' 'first launch' "$(seconds "$launch_ns")"
  printf '%-24s %10s\n' 'agent session' "$(seconds "$agent_ns")"
  printf '%-24s %10s\n' 'total' "$(seconds "$total_ns")"
  printf '%-24s %10s\n' 'result' "$result"
  if [[ -n ${version:-} ]]; then
    printf '%-24s %10s\n' 'phux' "$version"
  fi
}

self_test() {
  [[ $(timing_result 59999999999) == pass ]]
  [[ $(timing_result 60000000000) == fail ]]
  [[ $(timing_result 60000000001) == fail ]]
  install_ns=1000000000
  launch_ns=2000000000
  agent_ns=3000000000
  total_ns=6000000000
  result=pass
  version='phux 0.0.0'
  summary="$(print_summary)"
  grep -Fq 'download and install' <<<"$summary"
  grep -Fq 'first launch' <<<"$summary"
  grep -Fq 'agent session' <<<"$summary"
  grep -Fq '1.000' <<<"$summary"
  grep -Fq '6.000' <<<"$summary"
  grep -Fq 'pass' <<<"$summary"
  echo "first-run timing self-test passed"
}

if [[ ${1:-} == --self-test ]]; then
  self_test
  exit 0
fi
if [[ $# -ne 0 ]]; then
  echo "usage: $0 [--self-test]" >&2
  exit 2
fi

command -v python3 >/dev/null 2>&1 || {
  echo "python3 is required for the monotonic clock" >&2
  exit 1
}
command -v curl >/dev/null 2>&1 || {
  echo "curl is required to fetch the public installer" >&2
  exit 1
}

now_ns() {
  python3 -c 'import time; print(time.monotonic_ns())'
}

install_ns=0
launch_ns=0
agent_ns=0
total_ns=0
result=fail
version=""
work=""
phux_bin=""
phase=install
t0=0
phase_mark=0

cleanup() {
  local status=$?
  if [[ -n $phux_bin && -x $phux_bin ]]; then
    "$phux_bin" kill --server >/dev/null 2>&1 || true
  fi
  if [[ -n $work && -d $work ]]; then
    rm -rf "$work"
  fi
  exit "$status"
}
trap cleanup EXIT

finish() {
  print_summary
  exit "$1"
}

fail() {
  local end phase_ns
  end="$(now_ns)"
  total_ns=$((end - t0))
  phase_ns=$((end - phase_mark))
  case "$phase" in
    install) install_ns=$phase_ns ;;
    launch) launch_ns=$phase_ns ;;
    agent) agent_ns=$phase_ns ;;
  esac
  printf 'phux-first-run: %s\n' "$1" >&2
  result=fail
  finish 1
}

work="$(mktemp -d)"
export HOME="$work/home"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_STATE_HOME="$HOME/.local/state"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_RUNTIME_DIR="$work/run"
mkdir -p "$HOME" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
unset PHUX_SOCKET PHUX_PROFILE PHUX_INSTALL_DIR
export SHELL="${SHELL:-/bin/sh}"

phase=install
t0="$(now_ns)"
phase_mark=$t0
curl -fsSL "$INSTALL_URL" | sh || fail "public install failed"
phux_bin="$HOME/.local/bin/phux"
[[ -x $phux_bin ]] || fail "installer did not write $phux_bin"
export PATH="$HOME/.local/bin:$PATH"
install_ns=$(($(now_ns) - phase_mark))

phase=launch
phase_mark="$(now_ns)"
version="$("$phux_bin" --version 2>&1 | head -n 1)" || fail "phux --version failed"
if ! "$phux_bin" doctor >"$work/doctor.out" 2>&1; then
  fail "phux doctor failed: $(cat "$work/doctor.out")"
fi
if ! "$phux_bin" server --ensure >"$work/ensure.out" 2>&1; then
  fail "phux server --ensure failed: $(cat "$work/ensure.out")"
fi
launch_ns=$(($(now_ns) - phase_mark))

phase=agent
phase_mark="$(now_ns)"
stub="$work/bin/claude"
mkdir -p "$work/bin"
cat > "$stub" <<'EOF'
#!/bin/sh
printf 'phux-stub-agent ready\n'
while true; do sleep 30; done
EOF
chmod 755 "$stub"
if ! created="$("$phux_bin" new --json -s first-agent -c "$work" -- "$stub" 2>"$work/new.err")"; then
  fail "phux new failed: $(cat "$work/new.err")"
fi
terminal_id="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["terminal_id"])' <<<"$created")" \
  || fail "phux new JSON had no terminal_id: $created"
seen=0
while (( $(now_ns) - phase_mark < AGENT_CAP_NS )); do
  if "$phux_bin" agent list --json >"$work/agents.json" 2>"$work/agents.err"; then
    if python3 - "$work/agents.json" <<'PY'
import json
import sys
doc = json.load(open(sys.argv[1]))
for agent in doc.get("agents") or []:
    kind = (agent.get("agent") or {}).get("kind")
    state = agent.get("state")
    if kind == "claude" and isinstance(state, str) and state:
        raise SystemExit(0)
raise SystemExit(1)
PY
    then
      seen=1
      break
    fi
  fi
  sleep "$POLL_S"
done
end="$(now_ns)"
agent_ns=$((end - phase_mark))
total_ns=$((end - t0))
if [[ $seen -ne 1 ]]; then
  printf 'phux-first-run: stub agent was not reported\n' >&2
  printf '--- phux new ---\n%s\n' "$created" >&2
  printf '--- agent list ---\n' >&2
  cat "$work/agents.json" >&2 || true
  cat "$work/agents.err" >&2 || true
  "$phux_bin" snapshot --tail 40 "@${terminal_id}" >&2 || true
  result=fail
  finish 1
fi
result="$(timing_result "$total_ns")"
if [[ $result != pass ]]; then
  printf 'phux-first-run: total %s >= 60.000s\n' "$(seconds "$total_ns")" >&2
  finish 1
fi
finish 0
