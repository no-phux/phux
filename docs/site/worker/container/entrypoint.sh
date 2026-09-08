#!/bin/bash
set -euo pipefail

printf 'container_start uid=%s gid=%s proxy_port=8080 server_port=8081 health_port=8082\n' "$(id -u)" "$(id -g)"

/usr/bin/env -i PATH=/usr/bin:/bin \
  /opt/internal/socat TCP4-LISTEN:8080,bind=0.0.0.0,reuseaddr,fork TCP:127.0.0.1:8081 &
proxy_pid=$!

/usr/bin/env -i \
  HOME=/tmp \
  USER=guest \
  LOGNAME=guest \
  PATH=/usr/local/bin:/usr/bin:/bin \
  SHELL=/usr/local/bin/phux-shell \
  TERM=xterm-256color \
  XDG_CACHE_HOME=/tmp/.cache \
  XDG_CONFIG_HOME=/tmp/.config \
  XDG_RUNTIME_DIR=/tmp \
  PHUX_WS_ADDR=127.0.0.1:8081 \
  /usr/local/bin/phux server --session default &
server_pid=$!

health_pid=
cleanup() {
  set +e
  kill "$proxy_pid" "$server_pid" 2>/dev/null
  wait "$proxy_pid" "$server_pid" 2>/dev/null
  if [[ -n $health_pid ]]; then
    kill "$health_pid" 2>/dev/null
    wait "$health_pid" 2>/dev/null
  fi
}
trap cleanup EXIT
trap 'exit 0' TERM INT

for _ in {1..150}; do
  if exec 3<>/dev/tcp/127.0.0.1/8081 2>/dev/null; then
    exec 3>&- 3<&-
    break
  fi
  sleep 0.1
done
if ! kill -0 "$server_pid" 2>/dev/null || ! exec 3<>/dev/tcp/127.0.0.1/8081 2>/dev/null; then
  printf 'container_start_failed phase=server_port\n' >&2
  exit 1
fi
exec 3>&- 3<&-

/usr/bin/env -i PATH=/usr/bin:/bin \
  /opt/internal/socat TCP4-LISTEN:8082,bind=0.0.0.0,reuseaddr,fork EXEC:/usr/local/libexec/health,pipes &
health_pid=$!
printf 'container_ready health_port=8082 websocket_port=8080\n'

set +e
wait "$server_pid"
exit $?
