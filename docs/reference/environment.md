---
audience: humans, agents, contributors
stability: evolving
last-reviewed: 2026-08-02
---

# phux environment variables reference

**TL;DR.** The canonical environment-variable table: the socket path, the remote listeners and their TLS and token material, the helper programs (`ssh`, `tailscale`), the auto-spawn idle limit, and logging. Rendered from the same in-code table `phux help environment` uses, so the two cannot disagree.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

Every environment variable the `phux` binary reads, from the canonical in-code table that also renders `phux help environment`. A flag always wins over its variable (`--socket` over `PHUX_SOCKET`, `phux server --quic` over `PHUX_QUIC_ADDR`).

| Variable | Meaning |
|---|---|
| `PHUX_SOCKET` | Server socket for the CLI verbs and the server. `--socket` overrides it. Default: $XDG_RUNTIME_DIR/phux/phux.sock, or /tmp/phux-$USER/phux.sock when XDG_RUNTIME_DIR is unset. |
| `PHUX_WS_ADDR` | Also accept WebSocket clients on HOST:PORT. Equivalent to `phux server --listen`, which overrides it. |
| `PHUX_WS_SECURE` | Force TLS and token auth on a loopback --listen address, to exercise the remote path locally. |
| `PHUX_WS_TLS_CERT` | Operator-supplied server certificate (PEM), instead of the |
| `PHUX_WS_TLS_KEY` | auto-provisioned self-signed pair used off-loopback. |
| `PHUX_WS_TOKENS` | Pairing-token store the server reads and `phux pair` writes. |
| `PHUX_QUIC_ADDR` | Also accept QUIC clients on HOST:PORT. Equivalent to `phux server --quic`, which overrides it. |
| `PHUX_WT_ADDR` | Also accept WebTransport (HTTP/3 over QUIC) clients on HOST:PORT. Equivalent to `phux server --webtransport`. |
| `PHUX_SSH` | OpenSSH-compatible program used to reach ssh:// hosts and satellites (default: `ssh` on PATH). |
| `PHUX_TAILSCALE` | Tailscale-compatible CLI used to detect the overlay address (default: `tailscale` on PATH) for `phux pair`, `phux doctor`, and the server's auto-bound remote listener. When set it is the only source consulted: naming a command that reports nothing turns overlay detection off everywhere. |
| `PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE` | Idle limit in seconds (1..=86400) for an auto-spawned server, as if started with `phux server --exit-after-idle`. Unset means no limit. For test harnesses and CI jobs that cannot guarantee their own cleanup runs. |
| `PHUX_LOG` | Write logs to this file (the server tees to it; the client writes only here). |
| `PHUX_LOG_FORMAT` | `text` (default) or `json`: the log line format. |
| `RUST_LOG` | tracing level filter, e.g. `phux=debug`. |
| `NO_COLOR` | Set (non-empty) to keep colour out of help output. |
| `CLICOLOR_FORCE` | Set (not `0`) to colour help output even when piped. |

Run `phux server --listen 127.0.0.1:8787` to expose a port; see `phux server --help` for the remote and TLS details.
