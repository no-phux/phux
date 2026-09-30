---
audience: humans, consumers, contributors, agents
stability: evolving
last-reviewed: 2026-09-30
---

# Ways to use phux

**TL;DR.** Choose the terminal UI or Cockpit for interactive work, a host
integration or MCP for a coding agent, and the CLI for scripts. The browser
client is available as a demo and developer integration. Desktop contracts
and mobile previews are not equivalent to an installable client. All connect
to the same server-owned terminal model.

## Choose an interface

| You want to | Start with |
|---|---|
| Work interactively in a terminal | [Terminal UI](./tui.md) or the [first-run walkthrough](../QUICKSTART.md) |
| Use the native macOS app | [Cockpit](./cockpit.md) |
| Connect OMP, Claude, Pi, OpenCode, or an MCP host | [Coding-agent getting started](./getting-started.md) |
| Read and drive terminals from a script | [Agent CLI guide](./agents.md) |
| Record a pane or an attached session | [Recording](./recording.md) |
| Try the browser demo or build your own browser client | [Web client](./web.md) |

### Host integrations

- [Claude Code](./claude.md): plugin tools and hooks, plus optional launch shim.
- [OMP](./omp.md): native terminal tools and bounded observations; locally installable.
- [Pi](./pi.md): target selection, saved targets, and fleet awareness.
- [OpenCode V2](./opencode-v2.md): source-loaded plugin; not a published package.
- [MCP adapter](./mcp.md): registration for other tool hosts.

## Build an integration

| You want to | Start with |
|---|---|
| Emit lifecycle events from a harness | [Harness author guide](./harness.md) |
| Speak the wire from a new client | [Build a client](./build-a-client.md) |
| Use the in-tree Rust library or native bindings | [Client library guide](./sdk.md) |
| Understand the accepted GPUIX Solid desktop contract | [Desktop contract](./desktop.md), not yet release-verified |
| Follow mobile client availability | [iOS](./ios.md) and [Android](./android.md) status |

Clients are protocol peers; the TUI has no special protocol privilege
([the design decision](../adr/0017-tui-not-protocol-privileged.md)). For product
boundaries, see [current limitations](../CONCEPTS.md#status). Each guide's
metadata declares its own stability.
