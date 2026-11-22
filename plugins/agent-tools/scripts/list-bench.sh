# shellcheck shell=sh
# bench-common.sh (sourced below) defines the bench_* variables; shellcheck
# lints one file at a time and cannot follow the dynamic source path.
# shellcheck disable=SC2154
. "$(CDPATH=; cd -- "$(dirname -- "$0")" && pwd)/bench-common.sh"

if [ ! -f "$bench_state" ]; then
  printf 'phux-agent-bench: no state file at %s\n' "$bench_state" >&2
  exit 66
fi

cat "$bench_state"
