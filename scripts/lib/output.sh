#!/bin/sh
# Human status only. Keep data, help, and child-tool output untouched.

phux_color_enabled() {
  [ -z "${NO_COLOR:-}" ] || return 1
  [ "${TERM:-}" != dumb ] || return 1
  case "${CLICOLOR_FORCE:-}" in ''|0) ;; *) return 0 ;; esac
  [ "${CLICOLOR:-}" != 0 ] || return 1
  [ -t 2 ]
}

# A subshell keeps rendering variables out of the caller's namespace. printf
# always uses a fixed format: percent signs and backslashes in messages are data.
phux_message() (
  phux_label="$1"
  shift
  if phux_color_enabled; then
    case "$phux_label" in
      error) phux_style=31 ;;
      warn) phux_style=33 ;;
      ok) phux_style=32 ;;
      *) phux_style=36 ;;
    esac
    printf '\033[1mphux\033[0m: \033[%sm%s\033[0m: %s\n' "$phux_style" "$phux_label" "$*" >&2
  else
    printf 'phux: %s: %s\n' "$phux_label" "$*" >&2
  fi
)

phux_error() { phux_message error "$@"; }
phux_warn() { phux_message warn "$@"; }
phux_info() { phux_message info "$@"; }
phux_ok() { phux_message ok "$@"; }
phux_hint() {
  printf '%s\n' "$*" | while IFS= read -r phux_line; do
    printf '  hint: %s\n' "${phux_line#"${phux_line%%[![:space:]]*}"}" >&2
  done
}
