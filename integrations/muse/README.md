# phux for Muse

First-party integration that lets Muse (Meta's AI agent) drive phux:
persistent terminals, terminal-hosted agents, and federated workloads —
from a headless agent environment.

## What this is

Muse runs headless: no terminal UI, no plugin host, no lifecycle hooks.
This integration is the operating contract for a Muse instance driving
phux, plus the scripts that prove the contract holds:

- **`SKILL.md`** — the operating guide. When to use phux, how to drive it
  via the CLI, and the safety rules. Written for Muse's capabilities
  (shell execution, file I/O, finite observations).
- **`../../.agents/lib/phux-agent.sh`** — general, harness-agnostic shell
  library. Source it and drive phux with short functions (`phux_spawn`,
  `phux_run`, `phux_wait`, `phux_snap`, `phux_ls`, `phux_kill`, `phux_do`)
  instead of raw CLI incantations. `phux_init` ensures the binary and
  server, starting the server when needed. Not Muse-specific: any agent
  or script can use it.
- **`scripts/setup.sh`** — idempotent environment check. Verifies the
  `phux` binary, `jq`, the server (auto-started), and the tool versions
  this integration was built against.
- **`scripts/smoke.sh`** — end-to-end drive test. Spawns a session, runs a
  command, snapshots output, waits for exit, and cleans up. If this passes,
  Muse can use phux.
- **`test/`** — structural gates: every file the integration promises
  exists, is executable where it must be, and parses.

## Prerequisites

- A `phux` binary on `PATH` (source build or release tarball; see
  `../../docs/INSTALL.md`).
- A running phux server for your user. `phux` starts one on demand; no
  manual daemon management is needed.
- The tool versions in `../../mise.toml` (or `flake.nix`) if you are
  building from source.

## Quickstart

```sh
# 1. Verify the environment
./scripts/setup.sh

# 2. Prove Muse can drive phux end to end
./scripts/smoke.sh

# 3. Read the operating guide
cat SKILL.md
```

## How Muse uses phux

Muse drives phux through the CLI, following `SKILL.md`:

1. **Discover** — `phux agent list --json`, `phux ls --json`
2. **Target** — prefer exact `@N` pane selectors
3. **Read** — `phux snapshot --json @N` before acting
4. **Act** — `phux run --json --timeout SECS @N "COMMAND"`
5. **Observe** — `phux wait` / `watch` / `agent wait` with finite timeouts
6. **Verify** — re-read state; a quiet pane is not proof of completion

The MCP adapter (`phux mcp`) exposes the same surface over stdio for
MCP-capable hosts; see `../../.agents/skills/using-phux-mcp/SKILL.md`.
This integration targets the CLI path, which is what Muse drives today.

## Federated use

One phux server per host; `phux` CLI commands address sessions across
the federation the server knows about. Multiple Muse instances (or Muse
alongside other agents) coordinate through the same primitives the
other integrations use: `phux_mail_send`/`phux_mail_read` for messaging
and `phux_claim_take`/`phux_claim_release` for shared checkouts. See
`SKILL.md` for the coordination protocol.

## Gates

```sh
npm test   # structural tests
```

## Versioning

`CHANGELOG.md` tracks this integration. The `version` in `package.json`
is the integration's own version, independent of the phux release.
