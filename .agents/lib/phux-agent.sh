#!/bin/sh
# Source this harness-neutral library. Requires phux and jq on PATH.
# Helpers run in subshells: caller variables, options and traps stay intact.
# PHUX_SOCKET / PHUX_PROFILE select the server using the normal CLI contract.
# Only phux_spawn (via `new`) may auto-start it; reads and cleanup never do.
# phux_spawn prints an exact local @N selector, not a session name.

_phux_err() { printf 'phux-agent: %s\n' "$*" >&2; }

_phux_require() {
  command -v phux >/dev/null 2>&1 || { _phux_err 'phux not on PATH'; return 1; }
  command -v jq >/dev/null 2>&1 || { _phux_err 'jq not on PATH'; return 1; }
}

# Read-only readiness check. `status` deliberately reports no_server instead
# of starting a daemon. Startup belongs to the CLI's create path, not a probe.
phux_init() (
  _phux_require || return 1
  out=$(phux status --json) || { _phux_err "$out"; return 1; }
  printf '%s\n' "$out" | jq -e '.running == true' >/dev/null
)

_phux_target() (
  case "$1" in
    @|@0*|@*[!0-9]*|[!@]*|'') _phux_err 'target must be an exact local @N selector'; return 2 ;;
  esac
  self=${PHUX_TERMINAL_ID:-}
  [ -z "$self" ] && return 0
  self=${self#@}
  case "$self" in
    ''|0*|*[!0-9]*) _phux_err 'invalid hosting PHUX_TERMINAL_ID; refusing to operate'; return 2 ;;
  esac
  [ "$self" != "${1#@}" ] || { _phux_err 'refusing to operate on the hosting pane'; return 2; }
)

_phux_timeout() {
  case "$1" in
    ''|*[!0-9]*|0*) _phux_err 'timeout must be a positive integer in seconds'; return 2 ;;
  esac
}

# Capture before parsing: POSIX pipelines otherwise hide the CLI's failure.
_phux_json() (
  _phux_require || return 1
  out=$(phux "$@") || { code=$?; _phux_err "$out"; return "$code"; }
  printf '%s\n' "$out"
)

# Create an ephemeral session with a POSIX shell; print its seed pane's @N.
phux_spawn() (
  [ "$#" -eq 1 ] || { _phux_err 'usage: phux_spawn <name>'; return 2; }
  out=$(_phux_json new --json -s "$1" --ephemeral -- /bin/sh) || return $?
  printf '%s\n' "$out" | jq -er '
    .terminal_id | select(type == "number" and . > 0 and . == floor) | "@\(.)"'
)

# Print output even on child failure, and preserve the CLI's exit status:
# child exit, 125 run timeout, or refusal/transport failure. Never retry input.
phux_run() (
  [ "$#" -ge 2 ] && [ "$#" -le 3 ] || { _phux_err 'usage: phux_run <@N> <command> [timeout]'; return 2; }
  _phux_target "$1" || return 2
  timeout=${3:-${PHUX_AGENT_TIMEOUT:-20}}
  _phux_timeout "$timeout" || return 2
  _phux_require || return 1
  code=0
  out=$(phux run --json --timeout "$timeout" "$1" "$2") || code=$?
  if printf '%s\n' "$out" | jq -e '(.output | type == "string") and (.exit_code | type == "number")' >/dev/null; then
    printf '%s\n' "$out" | jq -r '.output' || return 1
    return "$code"
  fi
  _phux_err "$out"
  [ "$code" -ne 0 ] || code=1
  return "$code"
)

# Print visible pane text. Empty screens are successful; malformed JSON is not.
phux_snap() (
  [ "$#" -eq 1 ] || { _phux_err 'usage: phux_snap <@N>'; return 2; }
  _phux_target "$1" || return 2
  out=$(_phux_json snapshot --json "$1") || return $?
  printf '%s\n' "$out" | jq -er '.lines | select(type == "array") | join("\n")'
)

# Text may match a command echo, not completion. Exit 124 means wait timeout.
phux_wait() (
  [ "$#" -ge 2 ] && [ "$#" -le 3 ] || { _phux_err 'usage: phux_wait <@N> <text> [timeout]'; return 2; }
  _phux_target "$1" || return 2
  timeout=${3:-${PHUX_AGENT_TIMEOUT:-20}}
  _phux_timeout "$timeout" || return 2
  _phux_json wait --json --timeout "$timeout" --until "$2" "$1" >/dev/null
)

# Print session names. A successful empty list is distinct from a failed probe.
phux_ls() (
  [ "$#" -eq 0 ] || { _phux_err 'usage: phux_ls'; return 2; }
  out=$(_phux_json ls --json) || return $?
  printf '%s\n' "$out" | jq -er '.sessions | select(type == "array") | map(.name) | join("\n")'
)

# Kill only the exact pane. Its ephemeral session ends with its last process.
phux_kill() (
  [ "$#" -eq 1 ] || { _phux_err 'usage: phux_kill <@N>'; return 2; }
  _phux_target "$1" || return 2
  _phux_json kill --yes "$1" >/dev/null
)

# Spawn, run, snapshot, clean up. Traps are scoped to this subshell, never
# interpolated shell code. Failure and signals also clean up the owned pane.
phux_do() (
  [ "$#" -ge 2 ] && [ "$#" -le 3 ] || { _phux_err 'usage: phux_do <name> <command> [timeout]'; return 2; }
  _phux_timeout "${3:-${PHUX_AGENT_TIMEOUT:-20}}" || return 2
  pane=$(phux_spawn "$1") || return $?
  trap 'phux_kill "$pane" >/dev/null || :' 0
  trap 'exit 130' INT
  trap 'exit 143' TERM
  code=0
  phux_run "$pane" "$2" "${3:-${PHUX_AGENT_TIMEOUT:-20}}" >/dev/null &&
    phux_snap "$pane" || code=$?
  trap - 0
  if ! phux_kill "$pane" && [ "$code" -eq 0 ]; then
    return 1
  fi
  return "$code"
)
