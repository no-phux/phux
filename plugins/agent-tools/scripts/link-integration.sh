#!/bin/sh
# integration-common.sh (sourced below) defines the agent_tools_* variables;
# they look unassigned here because sources are not followed across files.
# shellcheck disable=SC2154
. "$(CDPATH=; cd -- "$(dirname -- "$0")" && pwd)/integration-common.sh"

mkdir -p "$agent_tools_state_dir"
printf 'id\taction\tpackage_version\tstate_file\tsession_identity\n'
package_ids "$@" | while IFS= read -r id; do
  [ -n "$id" ] || continue
  file=$(integration_file_for_id "$id") || {
    printf 'unknown integration package: %s\n' "$id" >&2
    exit 1
  }
  name=$(top_key display_name "$file")
  version=$(top_key package_version "$file")
  detect_command=$(section_key detect command "$file")
  native_env=$(section_key session_identity native_env "$file")
  native_session_id=$(env_value "$native_env")
  if [ -z "$native_session_id" ]; then
    native_session_id=${PHUX_AGENT_NATIVE_SESSION_ID:-}
  fi
  default_session=$(section_key link default_session "$file")
  phux_session=${PHUX_AGENT_SESSION_TARGET:-$default_session}
  state_file=$(integration_state_file "$id")
  tmp=$state_file.tmp
  {
    printf 'id=%s\n' "$id"
    printf 'display_name=%s\n' "$name"
    printf 'package_version=%s\n' "$version"
    printf 'linked_at=%s\n' "$(utc_now)"
    printf 'manifest=%s\n' "$file"
    printf 'detect_command=%s\n' "$detect_command"
    printf 'phux_session=%s\n' "$phux_session"
    printf 'native_session_id=%s\n' "$native_session_id"
  } > "$tmp"
  mv -f "$tmp" "$state_file"
  if [ -n "$native_session_id" ]; then
    session_identity="native:$native_session_id"
  else
    session_identity="phux:$phux_session"
  fi
  printf '%s\tlinked\t%s\t%s\t%s\n' "$id" "$version" "$state_file" "$session_identity"
done
