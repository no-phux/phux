# Environment variables

Environment variables phux reads. `phux help environment` prints the same table.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

Every environment variable the `phux` binary reads, from the canonical in-code table that also renders `phux help environment`. A flag always wins over its variable (`--socket` over `PHUX_SOCKET`, `phux server --quic` over `PHUX_QUIC_ADDR`).

| Variable | Meaning |
|---|---|
| `PHUX_SOCKET` | Server socket for the CLI verbs and the server. `--socket` overrides it. Default: $XDG_RUNTIME_DIR/phux/phux.sock; when XDG_RUNTIME_DIR is unset, /run/user/<uid>/phux/phux.sock if that directory exists, else /tmp/phux-$USER/phux.sock. |
| `PHUX_WS_ADDR` | Also accept WebSocket clients on HOST:PORT. Equivalent to `phux server --listen`, which overrides it. |
| `PHUX_WS_SECURE` | Force TLS and token auth on a loopback --listen address, to exercise the remote path locally. |
| `PHUX_WS_ALLOWED_ORIGINS` | Browser origins a plaintext loopback --listen address admits beyond loopback pages: comma-separated exact origins, or `*` behind a proxy that checks origins itself. |
| `PHUX_WS_TLS_CERT` | Operator-supplied server certificate (PEM), instead of the |
| `PHUX_WS_TLS_KEY` | auto-provisioned self-signed pair used off-loopback. |
| `PHUX_WS_TOKENS` | Pairing-token store the server reads and `phux pair` writes. |
| `PHUX_QUIC_ADDR` | Also accept QUIC clients on HOST:PORT. Equivalent to `phux server --quic`, which overrides it. |
| `PHUX_WT_ADDR` | Also accept WebTransport (HTTP/3 over QUIC) clients on HOST:PORT. Equivalent to `phux server --webtransport`. |
| `PHUX_UPLOAD_DIR` | Where the server lands file uploads (PUT_FILE). Default: $XDG_DATA_HOME/phux/uploads, else ~/.local/share/phux/uploads. |
| `PHUX_UPLOAD_MAX_BYTES` | Total bytes of finished and partial uploads the server keeps before refusing more (default 8 GiB; 0 = no limit). |
| `PHUX_UPLOAD_MAX_FILES` | Uploads, finished or partial, the server keeps before refusing a new one (default 10000; 0 = no limit). |
| `PHUX_SSH` | OpenSSH-compatible program used to reach ssh:// hosts and satellites (default: `ssh` on PATH). |
| `PHUX_OVERLAY_ADDRS` | Comma-separated concrete unicast IPs for any private network (e.g. Defguard/WireGuard). Overrides overlay discovery for pair, doctor, and auto-listen. Empty or invalid disables discovery without fallback. First IP is the auto-bind IP. Explicit --listen/--quic still win. Set in the service env and pairing shell; this does not install or configure a VPN. Set, it also keeps `phux pair` links off MagicDNS names. |
| `PHUX_TAILSCALE` | Tailscale-compatible CLI used to detect the overlay address (default: `tailscale` on PATH) for `phux pair`, `phux doctor`, and the server's auto-bound remote listener. When set and PHUX_OVERLAY_ADDRS is unset, it is the only source: a command reporting nothing disables overlay discovery. `phux pair` also reads its `status --json` for the host's MagicDNS name, which its links name instead of the address. |
| `PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE` | Idle limit in seconds (1..=86400) for an auto-spawned server, as if started with `phux server --exit-after-idle`. Unset means no limit. For test harnesses and CI jobs that cannot guarantee their own cleanup runs. |
| `PHUX_ALLOW_NESTED` | Set (non-empty) to let `phux`, `phux attach`, and `phux new` attach from inside one of the same server's panes, which they otherwise refuse (exit 2). |
| `PHUX_LOG` | Write logs to this file (the server tees to it; the client writes only here). |
| `PHUX_DEV` | Set to `1`, `true`, `yes`, or `on` to enable dev telemetry: server heap accounting; in the desktop, also the HUD and owner-only dev socket. Off by default. |
| `PHUX_DESKTOP_DEV_SOCKET` | Override the dev-mode desktop socket used by `phux perf --desktop` and the desktop. Its parent must be owner-only and owned by this user; default: beside the server socket. |
| `PHUX_LOG_FORMAT` | `text` (default) or `json`: the log line format. |
| `RUST_LOG` | tracing level filter, e.g. `phux=debug`. |
| `NO_COLOR` | Set (non-empty) to keep human output plain, including help and diagnostics. Overrides CLICOLOR_FORCE. |
| `CLICOLOR_FORCE` | Set (non-empty, not `0`) to colour human output even when piped. NO_COLOR and TERM=dumb still win. |
| `CLICOLOR` | Set to `0` to disable colour in human output unless forced. Otherwise colour follows each destination stream's terminal. |
| `TERM` | Set to `dumb` to keep human output plain, even when forced. |

Run `phux server --listen 127.0.0.1:8787` to expose a port, with `[policy] mode = "paired"` in config.toml; see `phux server --help` for the remote and TLS details.
