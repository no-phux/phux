#!/bin/sh
# Route Grok through the identity wrapper, adding a truthful AgentSession
# lifecycle only when argv makes the process itself a single turn.
set -eu

script_dir=$(CDPATH=; cd -- "$(dirname -- "$0")" && pwd)
agent_wrapper=$script_dir/phux-agent-wrap.sh
grok_bin=${PHUX_GROK_BIN:-grok}
single_turn=no

for arg in "$@"; do
  case "$arg" in
    --) break ;;
    -p|--single|--single=*|--prompt-file|--prompt-file=*|--prompt-json|--prompt-json=*)
      single_turn=yes
      ;;
  esac
done

if [ "$single_turn" = yes ]; then
  exec sh "$agent_wrapper" --name grok --kind grok --stream-single-turn -- "$grok_bin" "$@"
fi

PHUX_AGENT_STREAM_SINGLE_TURN=0 \
  exec sh "$agent_wrapper" --name grok --kind grok -- "$grok_bin" "$@"
