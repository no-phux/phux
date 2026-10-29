#!/bin/sh
# integration-common.sh (sourced below) defines the agent_tools_* variables;
# they look unassigned here because sources are not followed across files.
# shellcheck disable=SC2154
. "$(CDPATH=; cd -- "$(dirname -- "$0")" && pwd)/integration-common.sh"

runtime_state() {
  file=$1
  detect_env=$(section_key detect env "$file")
  detect_command=$(section_key detect command "$file")
  if [ "$(env_value "$detect_env")" != "1" ]; then
    printf 'not-probed'
    return 0
  fi
  if command_exists "$detect_command"; then
    printf 'available'
  else
    printf 'unavailable'
  fi
}

printf 'id\tdisplay_name\tpackage_version\tlinked_version\tpackage_state\tdetect_command\truntime_state\tsession_identity\n'
for file in "$agent_tools_root"/integrations/*.toml; do
  [ -e "$file" ] || continue
  id=$(top_key id "$file")
  name=$(top_key display_name "$file")
  version=$(top_key package_version "$file")
  detect_command=$(section_key detect command "$file")
  state_file=$(integration_state_file "$id")
  linked_version=$(state_key package_version "$state_file")
  package_state=missing
  session_identity="-"
  if [ -n "$linked_version" ]; then
    if [ "$linked_version" = "$version" ]; then
      package_state=current
    else
      package_state=outdated
    fi
    native_session_id=$(state_key native_session_id "$state_file")
    phux_session=$(state_key phux_session "$state_file")
    if [ -n "$native_session_id" ]; then
      session_identity="native:$native_session_id"
    else
      session_identity="phux:$phux_session"
    fi
  fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$id" "$name" "$version" "${linked_version:--}" "$package_state" \
    "$detect_command" "$(runtime_state "$file")" "$session_identity"
done
