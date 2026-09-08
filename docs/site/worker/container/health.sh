#!/bin/bash

# Cloudflare's port readiness check requires a valid HTTP response. Keep it on
# a separate port so the public WebSocket path remains byte-transparent.
printf 'HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n'
