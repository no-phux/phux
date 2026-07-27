---
audience: humans, agents, consumers, contributors
stability: stable
last-reviewed: 2026-07-15
---

# phux documentation

**TL;DR.** Start with the quickstart if you want a persistent terminal you and
an agent can share. The first half of these docs is task-oriented: install,
run, automate, and configure phux. Protocol, architecture, operations, and
decision records live in clearly separated reference sections when you need
to understand or extend the substrate.

---

## Start here

You do not need to understand the protocol before using phux.

| Your goal | Best first page |
|---|---|
| Run a persistent terminal and reattach to it | [Quickstart](./QUICKSTART.md) |
| Let an agent inspect and drive that same terminal | [Agent CLI guide](./consumers/agents.md) |
| Connect OpenCode to shared phux terminals | [OpenCode integration](./consumers/opencode.md) |
| Connect Pi to shared phux terminals | [Pi integration](./consumers/pi.md) |
| Install through Homebrew, a release, or source | [Install guide](./INSTALL.md) |
| Decide whether phux fits your workflow today | [When to use phux](./when-to-use.md) |
| Change the prefix, keys, status bar, or hooks | [Configuration](./CONFIG.md) |
| Record a pane or a session as a cast, GIF, or APNG | [Recording](./consumers/recording.md) |
| Reach your server from another network | [Remote access](./remote-access.md) |

The shortest path is the quickstart. It gets a real session running first,
then shows the read, act, wait, read loop that makes the same terminal useful
to an agent.

## Two speeds

### Use phux

These pages are for people trying to get work done:

- [Quickstart](./QUICKSTART.md) gets the first shared terminal running.
- [Install](./INSTALL.md) covers every supported installation path.
- [Configuration](./CONFIG.md) owns keybindings, status, and hooks.
- [Remote access](./remote-access.md) reaches a server across networks over an overlay.
- [The reference TUI](./consumers/tui.md) is the interactive terminal guide.
- [Recording](./consumers/recording.md) captures a pane or a whole session as a cast, GIF, or APNG.
- [Agents and the CLI](./consumers/agents.md) is the headless CLI and JSON guide.
- [The OpenCode integration](./consumers/opencode.md) adds OpenCode tools, target selection, and lifecycle metadata.
- [The Pi integration](./consumers/pi.md) adds Pi tools, target selection, and lifecycle metadata.
- [The MCP adapter](./consumers/mcp.md) connects the same controls to MCP clients.

### Understand or extend phux

These are reference material. Read them when you are building against phux,
operating it, or checking why the system has a particular shape:

- [How phux works](./CONCEPTS.md) explains the terminal, wire, and peer-consumer model.
- [Protocol reference](./spec/) is the normative, versioned wire protocol.
- [Consumer interfaces](./consumers/) documents each interface built on the wire.
- [Architecture](./architecture/) explains the process, transport, rendering, and state-sync internals.
- [Operations](./operations.md) owns errors, logging, telemetry, and security boundaries.
- [Decision records](../ADR/) explain why consequential decisions were made.

The distinction is deliberate. A user should be able to install and operate
phux without reading an ADR. A protocol implementer should be able to find the
normative answer without pulling behavior from a tutorial.

## What phux is

phux treats a terminal as an addressable object that can outlive any one view.
A person in the reference TUI, an agent using the CLI, and a browser client can
observe or drive the same terminal through peer interfaces. The terminal
stream stays a terminal stream; structured views are projected by consumers.

phux is pre-alpha. The local TUI, persistent sessions, multi-client attach,
headless commands, and MCP adapter are real. Interfaces may still move, and
some of the longer-range protocol design is intentionally documented before
it ships. [`CONCEPTS.md`](./CONCEPTS.md) owns the exact maturity boundary.

## Working on the project

Contributors should read [`../CONTRIBUTING.md`](../CONTRIBUTING.md). Documentation
structure and review rules live in [`CONVENTIONS.md`](./CONVENTIONS.md); release
procedure lives in [`RELEASING.md`](./RELEASING.md). Code-level API docs are
generated from Rust source with `cargo doc --workspace --all-features`.
