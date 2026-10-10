---
audience: humans, agents, consumers, contributors
stability: stable
last-reviewed: 2026-09-30
---

# phux documentation

**TL;DR.** Start with the quickstart to run a terminal, detach, and reattach.
Use the task guides below for configuration, coding agents, and remote access;
the references cover commands, protocols, and implementation.

## Start here

| Your goal | Best first page |
|---|---|
| Run a persistent terminal and reattach | [Quickstart](./QUICKSTART.md) |
| Choose a supported platform and install method | [Install phux](./INSTALL.md) |
| Understand sessions, windows, panes, and persistence | [How phux works](./CONCEPTS.md) |
| Compare phux with tmux, Herdr, or cmux | [When to use phux](./when-to-use.md) |
| Translate tmux or screen habits | [Coming from tmux or screen](./coming-from.md) |

## Use phux

- [Terminal UI](./consumers/tui.md): navigate, split, copy, and search.
- [Configuration](./CONFIG.md): make a small change, validate it, and apply it.
- [Cockpit](./consumers/cockpit.md): use the native macOS interface.
- [Recording](./consumers/recording.md): capture and replay terminal work.
- [All interfaces](./consumers/README.md): distinguish available clients from previews.

## Run coding agents

Start with [coding-agent getting started](./consumers/getting-started.md), then
choose [Claude Code](./consumers/claude.md), [Grok](./consumers/grok.md),
[Pi](./consumers/pi.md), [OpenCode V2](./consumers/opencode-v2.md), or
[MCP](./consumers/mcp.md).
For scripts and advanced automation, use the [agent CLI guide](./consumers/agents.md).

## Connect machines

[Remote access](./remote-access.md) starts with SSH enrollment and attach,
then covers reconnect, pairing, overlays, and relays.
[Troubleshooting](./remote-access.md#troubleshooting) separates server,
route, firewall, and credential failures.

## Performance and comparisons

- [When to use phux](./when-to-use.md): compare workflows with tmux, Herdr, and cmux.
- [Performance](./performance.md): measured results, methodology, and limits.
- [Performance diagnostics](./operations.md#performance-observability): measure
  your running system.

## Troubleshoot and maintain

- [Troubleshooting and recovery](./troubleshooting.md): start from a symptom.
- [Update and roll back](./INSTALL.md#updating): use the owner of your install.
- [Workspace continuity](./operations.md#workspace-continuity-and-update-survival):
  distinguish live upgrade from restoring fresh PTYs.
- [Operations](./operations.md): logs, services, diagnostics, and security boundaries.

## Reference and integration development

- [Generated reference](./reference/README.md): CLI, JSON, configuration, and catalogs.
- [Harness authors](./consumers/harness.md): emit agent lifecycle records.
- [Build a client](./consumers/build-a-client.md): implement a new consumer.
- [Browser client development](./consumers/web.md): demo versus building your own client.
- [Rust client library](./consumers/sdk.md): workspace library and native binding boundaries.
- [Protocol specification](./spec/README.md): normative wire contracts.
- [Architecture](./architecture/README.md) and [architecture decisions](./adr/README.md):
  implementation structure and design rationale.

## Working on the project

[Contributor setup](./SETUP.md) · [Contributor guide](../CONTRIBUTING.md) ·
[Documentation conventions](./CONVENTIONS.md) · [Release process](./RELEASING.md)

The public site at https://phux.sh is generated from this tree. Documentation
is served at https://docs.phux.sh, with the primary documentation home at
`/overview` and this full index at `/docs`.
