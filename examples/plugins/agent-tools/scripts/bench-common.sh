# shellcheck shell=sh
# Sourced library, not executed: no shebang by design. Several bench_*
# variables are consumed only by the sibling scripts that source this file,
# and shellcheck lints one file at a time.
# shellcheck disable=SC2034
set -eu

bench_root=${PHUX_PLUGIN_ROOT:-$(CDPATH=; cd -- "$(dirname -- "$0")/.." && pwd)}
bench_profile=${PHUX_AGENT_BENCH_PROFILE:-agent-bench}
bench_state_dir=${PHUX_AGENT_BENCH_STATE_DIR:-"$bench_root/state"}
bench_state=${PHUX_AGENT_BENCH_STATE:-"$bench_state_dir/$bench_profile.tsv"}
bench_workspace=${PHUX_AGENT_BENCH_WORKSPACE:-"$PWD"}
bench_roles=${PHUX_AGENT_BENCH_ROLES:-"codex claude-code gemini-cli grok"}
phux_bin=${PHUX_BIN:-phux}

phux_cmd() {
  subcommand=$1
  shift
  if [ -n "${PHUX_SOCKET:-}" ]; then
    "$phux_bin" "$subcommand" --socket "$PHUX_SOCKET" "$@"
  else
    "$phux_bin" "$subcommand" "$@"
  fi
}

role_session() {
  role=$1
  printf '%s-%s\n' "$bench_profile" "$role"
}
