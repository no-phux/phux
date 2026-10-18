---
audience: humans, consumers, contributors, agents
stability: evolving
last-reviewed: 2026-10-18
---

# Ways to use phux

**TL;DR.** Use the TUI or the GPUI desktop for interactive work, a host
integration or MCP for coding agents, and the CLI for scripts. The browser
client is a demo and developer integration. The Zig Cockpit is archived outside main.

## Choose an interface

| You want to | Start with |
|---|---|
| Work interactively in a terminal | [Terminal UI](./tui.md) or the [first-run walkthrough](../QUICKSTART.md) |
| Use the native macOS app | [Desktop](./desktop.md) |
| Review and commit a pane's Git changes in the desktop | [Desktop Changes panel](./desktop-changes.md) |
| Recover the archived Zig client | [Cockpit archive](./cockpit.md) |
| Connect OMP, Claude, Grok, Pi, OpenCode, or an MCP host | [Coding-agent getting started](./getting-started.md) |
| Read and drive terminals from a script | [Agent CLI guide](./agents.md) |
| Record a pane or an attached session | [Recording](./recording.md) |
| Run tasks through mission control | [Mission control](./mission.md) (decided, not built) |
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
| Put a phux terminal in your own native app | [Embed a terminal](./embedding.md) |
| Speak the wire from a new client | [Build a client](./build-a-client.md) |
| Use the in-tree Rust library or native bindings | [Client library guide](./sdk.md) |
| Understand the GPUI desktop contract | [Desktop contract](./desktop.md); Apple-silicon, not release-verified |
| Follow mobile client availability | [iOS](./ios.md) and [Android](./android.md) status |

Clients are protocol peers; the TUI has no special protocol privilege
([design decision](../adr/0017-tui-not-protocol-privileged.md)). See
[current limitations](../CONCEPTS.md#status) for product boundaries and each
guide's metadata for its stability.
