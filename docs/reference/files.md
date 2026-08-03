---
audience: humans, agents, contributors
stability: evolving
last-reviewed: 2026-08-02
---

# phux file locations reference

**TL;DR.** Where phux keeps its files: the socket under `$XDG_RUNTIME_DIR` (or `/tmp`), config under `$XDG_CONFIG_HOME`, and logs, TLS material, and pairing tokens under `$XDG_STATE_HOME/phux`. Each rule is pinned to the resolving function by a unit test, so the page moves when the code does.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

phux splits its files across the three XDG base directories: config (hand-written, follows you between machines), runtime (expected to disappear on reboot), and state (survives across runs but is not config). Paths below are symbolic; each rule names its fallback when the environment variable is unset.

## Socket

The Unix domain socket every consumer dials and the server binds:

1. `$PHUX_SOCKET` if set (an explicit `--socket` flag still overrides it);
2. `$XDG_RUNTIME_DIR/phux/phux.sock` if `XDG_RUNTIME_DIR` is set;
3. `/tmp/phux-<user>/phux.sock` otherwise.

The parent directory is created mode `0700`.

## Config

`$XDG_CONFIG_HOME/phux/config.toml`, falling back to `~/.config/phux/config.toml` when `XDG_CONFIG_HOME` is unset. A missing config file is not an error (the embedded defaults apply); there is no global config-path flag — set `XDG_CONFIG_HOME` to isolate configuration for a test or an alternate environment. `phux config path` prints the resolved path.

## State

The state directory is `$XDG_STATE_HOME/phux`, falling back to `~/.local/state/phux` when `XDG_STATE_HOME` is unset or empty:

```
$XDG_STATE_HOME/phux/
├── server.log        # the ONE server log, both spawn paths
├── client-<pid>.log  # per-pid interactive-client log
├── remote-cert.pem   # auto-provisioned remote-consumer certificate
├── remote-key.pem    # its private key (owner-only, 0600)
└── remote-tokens     # pairing-token store (owner-only, 0600)
```

- `server.log` is the canonical server log regardless of how the server was started: the auto-spawn path redirects the daemon's stderr here, and the service unit points its log capture at the same file. `phux logs` and `phux service logs` read it; `PHUX_LOG` tees the server's structured log to an additional file without moving this one.
- `client-<pid>.log` is where an interactive client writes its trace — the TUI owns the alt screen, so the client never logs to stderr. `PHUX_LOG` redirects it. Log files are created mode `0600`.
- `remote-cert.pem` / `remote-key.pem` are the self-signed TLS pair auto-provisioned for remote consumers (ADR-0031); `PHUX_WS_TLS_CERT` / `PHUX_WS_TLS_KEY` substitute an operator-supplied pair. A complete pair is never regenerated, so the pinned fingerprint stays stable.
- `remote-tokens` is the pairing-token store the server reads and `phux pair` appends to; `PHUX_WS_TOKENS` moves it.

## Design intent (not yet implemented)

A `server.pid` file and a `journal/` directory of per-pane PTY output for crash recovery remain design intent; neither path exists today. Workspace archives are written only where `phux workspace save` is pointed.
