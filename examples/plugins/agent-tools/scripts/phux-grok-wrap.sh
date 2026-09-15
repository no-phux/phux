#!/bin/sh
# Route Grok through the identity wrapper, adding a truthful AgentSession
# lifecycle only when argv makes the process itself a single turn.
set -eu

script_dir=$(CDPATH=; cd -- "$(dirname -- "$0")" && pwd)
agent_wrapper=$script_dir/phux-agent-wrap.sh
grok_bin=${PHUX_GROK_BIN:-grok}
single_turn=no
prompt_length=0
prompt_value_kind=

record_prompt_length() {
  value=$1
  case "$prompt_value_kind" in
    value) prompt_length=${#value} ;;
    "file")
      if [ -r "$value" ]; then
        prompt_length=$(wc -m < "$value" 2>/dev/null | tr -d '[:space:]')
      fi
      ;;
  esac
  case "$prompt_length" in ''|*[!0-9]*) prompt_length=0 ;; esac
  prompt_value_kind=
}

for arg in "$@"; do
  if [ -n "$prompt_value_kind" ]; then
    record_prompt_length "$arg"
    continue
  fi
  case "$arg" in
    --) break ;;
    -p|--single) single_turn=yes; prompt_value_kind=value ;;
    --prompt-file) single_turn=yes; prompt_value_kind="file" ;;
    --prompt-json) single_turn=yes; prompt_value_kind=value ;;
    --single=*)
      single_turn=yes
      prompt_value_kind=value
      record_prompt_length "${arg#*=}"
      ;;
    --prompt-file=*)
      single_turn=yes
      prompt_value_kind="file"
      record_prompt_length "${arg#*=}"
      ;;
    --prompt-json=*)
      single_turn=yes
      prompt_value_kind=value
      record_prompt_length "${arg#*=}"
      ;;
  esac
done

if [ "$single_turn" = yes ]; then
  exec sh "$agent_wrapper" --name grok --kind grok --stream-single-turn \
    --prompt-length "$prompt_length" -- "$grok_bin" "$@"
fi

PHUX_AGENT_STREAM_SINGLE_TURN=0 \
  exec sh "$agent_wrapper" --name grok --kind grok -- "$grok_bin" "$@"
