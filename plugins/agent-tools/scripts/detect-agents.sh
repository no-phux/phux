#!/bin/sh
# integration-common.sh (sourced below) defines the agent_tools_* variables;
# they look unassigned here because sources are not followed across files.
# shellcheck disable=SC2154
. "$(CDPATH=; cd -- "$(dirname -- "$0")" && pwd)/integration-common.sh"

if [ "${PHUX_AGENT_TOOLS_DETECT:-0}" != "1" ]; then
  printf 'agent detection disabled\n'
  printf 'set PHUX_AGENT_TOOLS_DETECT=1 to probe opt-in integration templates\n'
  exit 0
fi

printf 'id\tdisplay_name\tcommand\tstate\n'
for file in "$agent_tools_root"/integrations/*.toml; do
  [ -e "$file" ] || continue
  id=$(top_key id "$file")
  name=$(top_key display_name "$file")
  command_name=$(section_key detect command "$file")
  state=missing
  if command_exists "$command_name"; then
    state=available
  fi
  printf '%s\t%s\t%s\t%s\n' "$id" "$name" "$command_name" "$state"
done
