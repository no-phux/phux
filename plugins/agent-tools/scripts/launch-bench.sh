# shellcheck shell=sh
# bench-common.sh (sourced below) defines the bench_* variables; shellcheck
# lints one file at a time and cannot follow the dynamic source path.
# shellcheck disable=SC2154
. "$(CDPATH=; cd -- "$(dirname -- "$0")" && pwd)/bench-common.sh"

mkdir -p "$bench_state_dir"
tmp=$bench_state.tmp
err=$bench_state.err
printf 'role\tsession\tstatus\ttarget\n' > "$tmp"

for role in $bench_roles; do
  session=$(role_session "$role")
  if phux_cmd new --json -s "$session" --cwd "$bench_workspace" >/dev/null 2>"$err"; then
    status=running
  elif grep -q "already exists" "$err"; then
    status=existing
  else
    cat "$err" >&2
    rm -f "$tmp" "$err"
    exit 1
  fi
  printf '%s\t%s\t%s\t%s\n' "$role" "$session" "$status" "$session" >> "$tmp"
done

rm -f "$err"
mv -f "$tmp" "$bench_state"
cat "$bench_state"
