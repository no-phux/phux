#!/bin/sh
# integration-common.sh (sourced below) defines the agent_tools_* variables;
# they look unassigned here because sources are not followed across files.
# shellcheck disable=SC2154
. "$(CDPATH=; cd -- "$(dirname -- "$0")" && pwd)/integration-common.sh"

printf 'id\tdisplay_name\tkind\tpackage_version\tdetect_mode\tdetect_command\tsession_identity\n'
for file in "$agent_tools_root"/integrations/*.toml; do
  [ -e "$file" ] || continue
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$(top_key id "$file")" \
    "$(top_key display_name "$file")" \
    "$(top_key kind "$file")" \
    "$(top_key package_version "$file")" \
    "$(section_key detect mode "$file")" \
    "$(section_key detect command "$file")" \
    "$(section_key session_identity mode "$file")"
done
