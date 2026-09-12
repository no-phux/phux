---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-09-12
---

# Vision

**TL;DR.** The long arc: lazy state synchronization as the wire's destination, a durable work coordinator that does not ship today, and federation as the default deployment shape. This is direction, not a schedule. The wire was shaped to leave room for it.

---

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
one place, not an SSH-into-a-named-tmux-session ritual. The existing
multiplexers were not built for this, and the gap widens as agents
proliferate.

Taking both seriously at once is why the wire looks like this. What
phux is today lives in [`CONCEPTS.md`](./CONCEPTS.md); the rest of this
document is where that leads.

## Lazy state synchronization as the wire's destination

Lazy state synchronization of libghostty terminal state ships as an opt-in
output mode for custom consumers ([ADR-0018](../ADR/0018-lazy-state-synchronization.md)).
The server synthesizes the minimum VT transition from each consumer's last
reference state. Bundled TUI and web clients still request raw output, which is
the lowest-latency path for their current use cases. Federation adds the links
where StateSync becomes the expected default rather than changing its shape.

The research note (now archived) captures the algorithm composition:
[`../research/archive/2026-05-26-state-sync-algorithm.md`](../research/archive/2026-05-26-state-sync-algorithm.md).

## A durable work coordinator, not shipped

Durable work identity and evidence belong to a phux coordinator; clients
own presentation. Objective, Run, WorkSession, Artifact, and Signal are not
TUI layout vocabulary and are not inferred independently by each client. The
proposed contract and its delivery order live in
[ADR-0092](../ADR/0092-durable-work-coordinator-authority.md). That surface
does not ship.

## Milestones

What works today is in [`CONCEPTS.md`](./CONCEPTS.md); this list is the
forward arc only.

- **Hub.** Hub-and-spoke routing ships (`host/@N`; the hub does not merge
  remote session or window models). Remaining: lazy state sync as the
  federation default, and richer joins beyond Terminal-scoped relay.
- **Web.** The browser client ships as a carry-your-own-engine consumer
  ([`consumers/web.md`](./consumers/web.md)).
- **Cockpit.** The independently versioned native macOS client ships
  ([`consumers/cockpit.md`](./consumers/cockpit.md)).

## What phux is, on purpose, not

The no-list survived two reframes — first from "better tmux" to
"libghostty multiplexer," then from "multiplexer" to the resource
substrate. It survives because each item is about keeping the
substrate honest, not about being a smaller anything. The full list
with rationale lives in [`../CONTRIBUTING.md`](../CONTRIBUTING.md);
the headlines:

- No embedded scripting language.
- No in-process plugin host. Plugins are external packages declared in
  config, not code loaded into the server.
- No tmux-style copy-mode clone.
- No homegrown crypto.
- No format-template DSL.
