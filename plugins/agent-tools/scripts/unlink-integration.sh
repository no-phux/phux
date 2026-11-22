#!/bin/sh
. "$(CDPATH=; cd -- "$(dirname -- "$0")" && pwd)/integration-common.sh"

printf 'id\taction\tstate_file\n'
package_ids "$@" | while IFS= read -r id; do
  [ -n "$id" ] || continue
  integration_file_for_id "$id" >/dev/null || {
    printf 'unknown integration package: %s\n' "$id" >&2
    exit 1
  }
  state_file=$(integration_state_file "$id")
  if [ -f "$state_file" ]; then
    rm -f "$state_file"
    action=unlinked
  else
    action=missing
  fi
  printf '%s\t%s\t%s\n' "$id" "$action" "$state_file"
done
