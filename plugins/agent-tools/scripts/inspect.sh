#!/bin/sh
set -eu

printf 'plugin_id=%s\n' "${PHUX_PLUGIN_ID:-}"
printf 'action_id=%s\n' "${PHUX_PLUGIN_ACTION_ID:-}"
printf 'root=%s\n' "${PHUX_PLUGIN_ROOT:-}"
