# shellcheck shell=sh
# Sourced library, not executed: no shebang by design.
set -eu

agent_tools_root=${PHUX_PLUGIN_ROOT:-$(CDPATH=; cd -- "$(dirname -- "$0")/.." && pwd)}
agent_tools_state_dir=${PHUX_AGENT_TOOLS_STATE_DIR:-"$agent_tools_root/state/integrations"}
agent_tools_default_packages=${PHUX_AGENT_PACKAGES:-${PHUX_AGENT_PACKAGE:-"codex claude-code"}}

top_key() {
  key=$1
  file=$2
  awk -F '=' -v key="$key" '
    $1 ~ "^[[:space:]]*" key "[[:space:]]*$" {
      value = $2
      sub(/^[[:space:]]*/, "", value)
      sub(/[[:space:]]*$/, "", value)
      sub(/^"/, "", value)
      sub(/"$/, "", value)
      print value
      exit
    }
  ' "$file"
}

section_key() {
  section=$1
  key=$2
  file=$3
  awk -F '=' -v section="[$section]" -v key="$key" '
    $0 == section { in_section = 1; next }
    /^\[/ { in_section = 0 }
    in_section && $1 ~ "^[[:space:]]*" key "[[:space:]]*$" {
      value = $2
      sub(/^[[:space:]]*/, "", value)
      sub(/[[:space:]]*$/, "", value)
      sub(/^"/, "", value)
      sub(/"$/, "", value)
      print value
      exit
    }
  ' "$file"
}

state_key() {
  key=$1
  file=$2
  [ -f "$file" ] || return 0
  awk -F '=' -v key="$key" '
    $1 == key {
      print substr($0, length(key) + 2)
      exit
    }
  ' "$file"
}

integration_file_for_id() {
  want=$1
  for file in "$agent_tools_root"/integrations/*.toml; do
    [ -e "$file" ] || continue
    if [ "$(top_key id "$file")" = "$want" ]; then
      printf '%s\n' "$file"
      return 0
    fi
  done
  return 1
}

integration_state_file() {
  id=$1
  printf '%s/%s.link\n' "$agent_tools_state_dir" "$id"
}

command_exists() {
  command_name=$1
  search_path=${PHUX_AGENT_TOOLS_PATH:-$PATH}
  old_ifs=$IFS
  IFS=:
  for dir in $search_path; do
    if [ -x "$dir/$command_name" ]; then
      IFS=$old_ifs
      return 0
    fi
  done
  IFS=$old_ifs
  return 1
}

env_value() {
  name=$1
  case "$name" in
    ""|*[!A-Za-z0-9_]*|[0-9]*)
      return 0
      ;;
  esac
  eval "printf '%s' \"\${$name:-}\""
}

package_ids() {
  if [ "$#" -gt 0 ]; then
    printf '%s\n' "$@"
  else
    printf '%s\n' "$agent_tools_default_packages" | tr ' ' '\n' | sed '/^$/d'
  fi
}

utc_now() {
  date -u '+%Y-%m-%dT%H:%M:%SZ'
}
