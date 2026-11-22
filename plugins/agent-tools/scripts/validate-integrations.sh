#!/bin/sh
# integration-common.sh (sourced below) defines the agent_tools_* variables;
# they look unassigned here because sources are not followed across files.
# shellcheck disable=SC2154
. "$(CDPATH=; cd -- "$(dirname -- "$0")" && pwd)/integration-common.sh"

require() {
  label=$1
  value=$2
  file=$3
  if [ -z "$value" ]; then
    printf 'missing %s in %s\n' "$label" "$file" >&2
    exit 1
  fi
}

count=0
codex=0
claude=0
for file in "$agent_tools_root"/integrations/*.toml; do
  [ -e "$file" ] || continue
  require schema_version "$(top_key schema_version "$file")" "$file"
  require id "$(top_key id "$file")" "$file"
  require display_name "$(top_key display_name "$file")" "$file"
  require kind "$(top_key kind "$file")" "$file"
  require package_version "$(top_key package_version "$file")" "$file"
  require package_status "$(top_key package_status "$file")" "$file"
  require first_party "$(top_key first_party "$file")" "$file"
  require detect.mode "$(section_key detect mode "$file")" "$file"
  require detect.command "$(section_key detect command "$file")" "$file"
  require detect.env "$(section_key detect env "$file")" "$file"
  require launch.command "$(section_key launch command "$file")" "$file"
  require link.mode "$(section_key link mode "$file")" "$file"
  require link.default_session "$(section_key link default_session "$file")" "$file"
  require session_identity.mode "$(section_key session_identity mode "$file")" "$file"
  require session_identity.native_env "$(section_key session_identity native_env "$file")" "$file"
  case "$(top_key id "$file")" in
    codex) codex=1 ;;
    claude-code) claude=1 ;;
  esac
  count=$((count + 1))
done

if [ "$count" -eq 0 ]; then
  printf 'no integration templates found under %s/integrations\n' "$agent_tools_root" >&2
  exit 1
fi
if [ "$codex" -ne 1 ] || [ "$claude" -ne 1 ]; then
  printf 'codex and claude-code first-party integration packages are required\n' >&2
  exit 1
fi

printf 'validated %s integration templates\n' "$count"
