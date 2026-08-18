---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-08-02
---

# Vision

**TL;DR.** The long arc beyond the current substrate cut: agents as a primary consumer category, federation as the default deployment shape, and lazy state synchronization as the wire's destination semantics. This is a direction, not a schedule; the value of writing it down is that the wire was shaped to leave room for it. For what phux is today, read CONCEPTS.md.

---

These are directions, not delivery dates. The wire shipped today was shaped by them, so the forward compatibility below is already in the bytes rather than a later retrofit. For the present-tense model — what phux is and what works now — [`CONCEPTS.md`](./CONCEPTS.md) is the owner.

## Why now

Two structural changes, neither of which existed when tmux's
architecture was set, make a different design possible.

**libghostty is reusable as a library.** A bytes-in / structure-out
terminal emulator that the server and the client can both run
identically, with no re-parsing in between. Modern terminal protocols —
the Kitty keyboard protocol, true colour, OSC 8 hyperlinks, OSC 133
prompt boundaries, image protocols, mouse pixel-precision — pass
through end-to-end because the same parser sits on both ends. tmux,
screen, and zellij predate this and re-parse VT mid-path, which is
where those features degrade. phux carries the bytes and re-runs the
same engine, so there is no second parser to fall behind.

**Agents became a consumer category.** Programs that drive terminals —
Claude Code, Cursor's agent, anything orchestrating a developer
workflow — now sit alongside humans as first-class consumers. They want
primitives, not opinions: a `command-end` event with an exit code, not
a grid to scrape; a terminal spawned on a remote box and observed from
a control plane, not an SSH-into-a-named-tmux-session ritual. The
existing multiplexers were not built for this, and the gap widens as
agents proliferate.

What falls out of taking both seriously at once is a terminal control
plane rather than a better multiplexer — the rest of this document is
where that leads.

---

## Distributed by design

phux is not a single-machine tool that one day might support remote
attach. It is a control plane from day one. The local Unix socket and
the federated hub are *the same wire*. Identity is portable across
hosts because federation is in the addressing scheme, not bolted on
later.

A satellite is a phux server running on another machine. A hub
federates them: an authenticated consumer connecting to the hub can
list Terminals on any satellite, spawn new Terminals on any
satellite, observe and drive them. The transport between hub and
satellite is whatever works. Direct consumer attach over QUIC and WSS
ships today, and a hub routes Terminal-scoped operations to enrolled
satellites — including over SSH-stdio (`ssh HOST phux stdio-bridge`).
Satellite session/window joins are intentionally not federated;
satellites stay Terminal-scoped. The wire is oblivious to the transport
beneath it.

This shape is the answer to "what is this for, beyond a better tmux." A
fleet of agents working on a fleet of cloud boxes needs terminals as
first-class addressable resources: accessible from one place, observable
in real time, persistent across disconnect. tmux would have to replace
its wire to get there; phux's wire is already pointed at it.

## Lazy state synchronization as the wire's destination

Lazy state synchronization of libghostty terminal state ships as an opt-in
output mode for custom consumers ([ADR-0018](../ADR/0018-lazy-state-synchronization.md)).
The server synthesizes the minimum VT transition from each consumer's last
reference state. Bundled TUI and web clients still request raw output, which is
the lowest-latency path for their current use cases. Federation adds the links
where StateSync becomes the expected default rather than changing its shape.

The research note (now archived) captures the algorithm composition:
[`../research/archive/2026-05-26-state-sync-algorithm.md`](../research/archive/2026-05-26-state-sync-algorithm.md).

---

## Two consumer surfaces, both on the arc

Before those surfaces diverge further, one authority boundary is explicit:
durable work identity and evidence belong to a Phux coordinator, while clients
own presentation. Objective, Run, WorkSession, Artifact, and Signal are not TUI
layout vocabulary and are not inferred independently by each client. The
proposed contract and its delivery order live in
[ADR-0092](../ADR/0092-durable-work-coordinator-authority.md). Nothing in the
current release claims that durable-work surface already ships.

### The reference TUI

The shape users expect from a multiplexer. Sessions, windows, panes,
splits, status bar, keybindings, prefix table. The user-facing
vocabulary is tmux's because it's what people know.
[`consumers/tui.md`](./consumers/tui.md) is the surface doc.

The command palette, the session/window pickers, the window tab strip in the
status bar (default-on, with a toggleable sidebar window list), and which-key
prefix discovery all ship. What remains is the polish the 2026-08 UX audit
named: a copy-mode search affordance over the shipped server-side
find-in-scrollback primitive, and finishing the one-grammar consistency work
across the CLI surface. None of this touches the wire; it is client chrome
over the substrate.

### The agent surface

The agent's universe is *terminals and events*: spawn a build, wait
for the OSC 133 command-end event, read the exit code, kill the
terminal, move on. Two ways to reach it ship today — the headless
`phux` CLI verbs (`run`, `wait`, `watch`, `send-keys`, `snapshot`, …)
and the [`phux-mcp`](./consumers/mcp.md) adapter. The current CLI consumes
server-derived convenience snapshots and exposes versioned JSON; richer
carry-your-own-engine consumers can project locally. Neither makes structured
screen state the canonical synchronization tier
([ADR-0030](../ADR/0030-engine-delegated-wire-and-projection-consumers.md)).
The typed Rust handle over the same wire already exists as the
`phux-client` library crate over `phux-protocol`.

What's still on the arc here is the *ergonomic* layer past that crate:
JSON-over-HTTP if non-Rust agents that can't speak MCP become a real
consumer category, and a richer agent SDK that follows the phux-web
pattern — carry your own engine and project structured state locally
rather than read it off the wire.

[`consumers/agents.md`](./consumers/agents.md) owns the agent projection
surface (verb set and JSON contracts);
[`consumers/mcp.md`](./consumers/mcp.md) covers the MCP adapter.

---

## Milestones

What works today and the current substrate state are in
[`CONCEPTS.md`](./CONCEPTS.md); this list is the forward arc only.

- **Federation deepens.** Hub-to-satellite routing, QUIC, WSS, and
  SSH-stdio ship today; what remains is lazy state sync as the
  federation default per ADR-0018 and richer joins beyond
  Terminal-scoped relay.
- **v0.x and beyond — second consumer.** A native GUI consumer
  would show the substrate isn't TUI-shaped; a tmux control-mode adapter
  ([ADR-0010](../ADR/0010-frontend-agnostic-tmux-cc-reserved.md)), that
  it isn't phux-shaped. Two such consumers already exist: phux-web
  ([`consumers/web.md`](./consumers/web.md)), a browser client that
  carries its own engine and projects locally — the reference shape for
  the rest — and the recorder (`phux rec`,
  [`consumers/recording.md`](./consumers/recording.md)), which showed
  the substrate isn't consumer-shaped.

---

## What phux is, on purpose, not

The no-list survived two reframes — first from "better tmux" to
"libghostty multiplexer," then from "multiplexer" to "terminal
control plane." It survives because each item is about keeping the
substrate honest, not about being a smaller anything. The full list
with rationale lives in [`../CONTRIBUTING.md`](../CONTRIBUTING.md);
the headlines:

- No embedded scripting language.
- No in-process plugin host. Plugins are external packages declared in
  config, not code loaded into the server.
- No tmux-style copy-mode clone.
- No homegrown crypto.
- No format-template DSL.
