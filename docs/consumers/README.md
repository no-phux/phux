---
audience: humans, consumers, contributors, agents
stability: evolving
last-reviewed: 2026-10-14
---

# Ways to use phux

**TL;DR.** Use the TUI or the GPUI desktop for interactive work, a host
integration or MCP for coding agents, and the CLI for scripts. The browser
client is a demo and developer integration. The Zig Cockpit is frozen.

## Choose an interface

| You want to | Start with |
|---|---|
| Work interactively in a terminal | [Terminal UI](./tui.md) or the [first-run walkthrough](../QUICKSTART.md) |
| Use the native macOS app | [Desktop](./desktop.md) |
| Read the frozen Zig client | [Cockpit](./cockpit.md) |
| Connect OMP, Claude, Grok, Pi, OpenCode, or an MCP host | [Coding-agent getting started](./getting-started.md) |
| Read and drive terminals from a script | [Agent CLI guide](./agents.md) |
| Record a pane or an attached session | [Recording](./recording.md) |
| Try the browser demo or build your own browser client | [Web client](./web.md) |

### Host integrations

- [Claude Code](./claude.md): plugin tools and hooks, plus optional launch shim.
- [Grok](./grok.md): the same MCP tools and lifecycle hooks for Grok Build.
- [OMP](./omp.md): native terminal tools and bounded observations; `omp plugin install @phux/omp`.
- [pi + phux in 60 seconds](./pi-in-60-seconds.md): split a session, point pi at the shell, detach, and come back.
- [Pi](./pi.md): target selection, saved targets, and fleet awareness.
- [OpenCode V2](./opencode-v2.md): native plugin; `opencode plugin add @phux/opencode`.
- [MCP adapter](./mcp.md): registration for other tool hosts.

## Build an integration

| You want to | Start with |
|---|---|
| Emit lifecycle events from a harness | [Harness author guide](./harness.md) |
| Speak the wire from a new client | [Build a client](./build-a-client.md) |
| Use the in-tree Rust library or native bindings | [Client library guide](./sdk.md) |
| Understand the GPUI desktop contract | [Desktop contract](./desktop.md); Apple-silicon, not release-verified |
| Follow mobile client availability | [iOS](./ios.md) and [Android](./android.md) status |

Clients are protocol peers; the TUI has no special protocol privilege
([design decision](../adr/0017-tui-not-protocol-privileged.md)). See
[current limitations](../CONCEPTS.md#status) for product boundaries and each
guide's metadata for its stability.
