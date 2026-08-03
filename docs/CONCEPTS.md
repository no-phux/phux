---
audience: humans, contributors, agents, consumers
stability: evolving
last-reviewed: 2026-08-02
---

# How phux works

**TL;DR.** phux makes a terminal an addressable object that can outlive any one
view. A person, browser, script, or agent can observe and drive the same live
terminal as peer consumers. The wire preserves the terminal stream; each
consumer turns that stream into the structured view it needs. phux is
pre-alpha and spec-first.

---

## The short version

A terminal in phux belongs to the server, not to the window currently showing
it. You can detach, attach from another client, inspect it from a script, or
let an agent drive it without creating a second copy of the session.

The familiar multiplexer is one view of that model. It gives a person splits,
focus, status, and keybindings. The headless CLI gives programs selectors,
snapshots, input, events, and JSON results. Both operate on the same terminals.

The protocol stays small by carrying terminal identity, lifecycle, input,
output, and metadata. It does not turn the wire into a second terminal model.
The same libghostty engine runs at each end, so each consumer can render or
project structure locally from the real stream.

## Maturity: pre-alpha, spec-first

phux is pre-alpha. The protocol is pre-1.0 (the current version is pinned in `phux-protocol` and mirrored by [`spec/`](./spec/README.md); a CI gate keeps the two in sync) and the spec leads the code: several behaviors are designed and written down before they are built, and a few shipped behaviors still diverge from the target the spec describes. This document distinguishes what runs today from a stated direction. Where they disagree, the divergence is marked inline and pointed at the ADR that decides it.

What runs today: a server that spawns PTY-backed terminals and parses them with libghostty, a reference TUI that attaches over the wire, a browser client (phux-web), and a headless verb set a script or an agent can drive. The shipped CLI verbs are catalogued in [`QUICKSTART.md`](./QUICKSTART.md). Hub-and-spoke federation can dial configured satellites, aggregate their Terminal inventory, spawn there, and relay Terminal-scoped operations; satellite sessions/windows are intentionally not joined into the hub's local model.

This document owns the maturity fact. Other docs link here rather than restating it.

---

## Terminals, not panes

A terminal runs a process, parses bytes into a grid of cells, accepts structured input, and reports observable events such as title, bell, output activity, lifecycle, and agent asks. That object is the whole model. A person looking at one and a program driving one are holding the same thing two different ways. Working-directory and command-boundary events are part of the target surface, not fully emitted today.

Sessions, windows, panes, and splits — the tmux vocabulary — are not on the wire. They are how a consumer arranges terminals for a person, expressed as metadata and client logic. An agent does not need that vocabulary to spawn a terminal, drive it, and read its exit code; an orchestrator may opt into the same L3 layout convention to place or reshape panes for a human without gaining authority over that human's local focus.

The reason the consumer set drives the model: humans want windows and splits, agents want to spawn and tear down terminals and read their events, CI fleets want event streams, and control planes want resources addressable across machines. The terminal is the one thing all of them need. Everything else is a per-consumer arrangement.

The reference TUI ships in-tree because a substrate is only real if something rides it.

---

## The engine is delegated; structure is a projection

The server and rendering clients run the same terminal engine, libghostty: the server holds canonical state and a rendering client holds a local mirror. The same parser runs on both ends, and terminal synchronization does not depend on a second cell model in between.

Structured state is a projection, not the canonical synchronization tier. A rendering consumer can compute it from its local engine; the current CLI and MCP adapter also use server-derived convenience snapshots and results such as `GET_SCREEN`. Those replies make headless tools practical without defining a second screen model that every terminal stream must traverse. Older multiplexers re-parse VT mid-path; phux keeps terminal bytes and the shared engine as the authoritative path.

This is the central design commitment. See [ADR-0013 (libghostty bytes on the wire)](../ADR/0013-libghostty-bytes-on-wire.md), [ADR-0004 (libghostty-vt as the canonical grid)](../ADR/0004-libghostty-vt-as-grid.md), [ADR-0008 (use libghostty types directly)](../ADR/0008-use-libghostty-types-directly.md), and [ADR-0030 (engine-delegated wire and projection consumers)](../ADR/0030-engine-delegated-wire-and-projection-consumers.md).

In practice:

- The wire is asymmetric. Server to client: terminal bytes (output and snapshots). Client to server: structured input atoms (key, mouse, focus, paste).
- Input types are libghostty's, re-exported directly. There is no parallel phux input enum.
- A libghostty pin bump lights up new terminal features on both ends at once.

---

## What the wire carries

The wire carries terminal **identity**, terminal **lifecycle** (including one atomic multi-terminal teardown operation), **transport** framing and capability negotiation, **opaque terminal bytes**, and **metadata** the server stores without interpreting. Structured screen state is not its normative synchronization model. Convenience commands may return snapshots and command results for headless consumers; panes and layouts remain consumer projections.

The spec organizes this as layers declared at connect time:

| Tier | Concept | Carries |
|---|---|---|
| **L1** | Terminal | The PTY plus its libghostty Terminal: output bytes, structured input, snapshots, resize, close, bell, and engine-derived events. The reference server emits a subset today; working-directory and command-boundary emission remain incomplete. |
| **L3** | Metadata | Opaque key-value pairs scoped to a terminal or globally. Consumers store conventions here (TUI layout, window and session names, group membership). The server stores; it does not interpret. |

There is no L2 collection tier. Group lifecycle — "these terminals belong together, tear them down as a unit" — is metadata plus client logic, with one exception. The one thing a consumer cannot do correctly on its own is an atomic group teardown: a client killing N terminals one at a time exposes intermediate states to other observers. That single irreducible need is met by one L1 operation, `KILL_TERMINALS { ids }` (command tag `0x09`), which the server applies all-or-nothing under its single state lock. Atomicity earns one operation, not a tier. See [ADR-0030](../ADR/0030-engine-delegated-wire-and-projection-consumers.md) and [ADR-0015 (protocol layering)](../ADR/0015-protocol-layering.md).

The wire surface itself is owned by the spec: L1 by [`spec/L1.md`](./spec/L1.md), the metadata model and grouping conventions by [`spec/L3.md`](./spec/L3.md), and the byte-level codec by [`spec/appendix-encoding.md`](./spec/appendix-encoding.md).

The dissolution of the collection lifecycle tier shipped in protocol 0.3.0: the `CREATE_SESSION`, `RENAME_SESSION`, and `KILL_COLLECTION` verbs are gone from the wire. Create is `SPAWN_TERMINAL` plus a metadata key, rename is a metadata SET, and atomic group teardown is the single `KILL_TERMINALS` operation. `GroupId` is retained as a documented opaque grouping key, not a lifecycle tier (settled: bead phux-0bmc closed as resolved-by-rename — it is not a remnant slated for removal). The decomposition is decided in [ADR-0030](../ADR/0030-engine-delegated-wire-and-projection-consumers.md).

---

## Identity is federation-ready

Every terminal is addressed by a `TerminalId` that is either `LOCAL { id }` or `SATELLITE { host, id }`. A normal server constructs local ids. A federation hub also constructs satellite ids when it retags aggregate inventory, spawn replies, and relayed frames. A non-hub server still rejects a satellite id cleanly with `UnsupportedSatelliteRoute` rather than misreading it.

Concretely: `TerminalId::Local { id: 42 }` names terminal 42 on the server you are talking to. `TerminalId::Satellite { host: "prod-box-3", id: 42 }` names terminal 42 on the configured satellite keyed by the opaque hub-local token `prod-box-3`. The CLI renders these as `@42` and `prod-box-3/@42`; both are accepted as direct selectors after they appear in the server's inventory. Satellite panes stay Terminal-scoped: the hub does not merge remote session/window identities or chain routes through another satellite.

The forward-compatible tagged shape paid off without a second addressing scheme: the same `TerminalId` the early decoder accepted now carries a routable destination on a hub. The relay rewrites only the routing id at each hop and forwards VT payloads opaquely, preserving ADR-0007's boundary.

See [ADR-0016 (TerminalId as wire primary)](../ADR/0016-terminal-id-as-wire-primary.md) and [ADR-0007 (Mosh-class transport and satellites)](../ADR/0007-mosh-class-transport-and-satellites.md).

---

## Consumers are peers; carry your own engine

The reference TUI, the browser client, and the agent surface are peers. None has protocol-level standing: if a consumer needs a capability the wire does not provide, the answer is an ADR that extends the spec, not a consumer-shaped hook on the wire ([ADR-0017 (TUI not protocol-privileged)](../ADR/0017-tui-not-protocol-privileged.md)).

The reference pattern for a consumer that wants structured state is to carry its own engine and project locally. phux-web is that pattern in shipping code: it compiles to WASM, loads `ghostty-vt.wasm`, speaks the exact wire codec over WebSocket, and computes its rendered view from engine state it owns. An agent SDK should follow the same shape — run the engine, project to structured state locally — rather than ask the wire for a structured-state service. See [ADR-0025 (browser web client)](../ADR/0025-browser-web-client.md), [ADR-0030](../ADR/0030-engine-delegated-wire-and-projection-consumers.md), and the consumer docs: [`consumers/web.md`](./consumers/web.md), [`consumers/tui.md`](./consumers/tui.md), [`consumers/agents.md`](./consumers/agents.md).

The agent surface today is the headless CLI verb set plus the [`phux-mcp`](./consumers/mcp.md) adapter over it (its `tools/list` catalog is the authoritative tool inventory). An agent reads structured state through versioned JSON, creates and explicitly places terminals with `new`/`launch`/`spawn`, and may serialize existing-pane topology edits through the shared L3 layout convention. Observation is bounded `wait` plus event `watch`; a blocked human question reuses the advisory `Asked` event. The TUI alone applies `next-attention`/return navigation to its client-local focus, so neither CLI nor MCP can move a human's viewport. These are consumer projections, not wire contracts or scheduler semantics. The library behind the CLI is the `phux-client` crate over `phux-protocol`; it exists today rather than being a future SDK. The verb catalog, JSON contracts, and orchestration safety rules are owned by [`consumers/agents.md`](./consumers/agents.md) and the runnable [`phux-agent-cli` skill](../examples/skills/phux-agent-cli/SKILL.md).

Some L1 commands return engine-derived snapshots a consumer could also compute locally — `GET_SCREEN`, `GET_TERMINAL_STATE`, `SUBSCRIBE_TERMINAL_EVENTS`, and the pushed `AgentEvent` frame. Read these as a convenience for consumers that have not yet adopted the carry-your-own-engine pattern, not as a normative structured contract and not as license to grow new structured wire surface. [`spec/L1.md`](./spec/L1.md) owns that surface.

---

## Positioning: a substrate, not a product

phux is a substrate — a wire and an engine that terminals ride — with a reference TUI as its first product on top. The argument for it is architectural, not a feature race: because the engine is shared and never re-encoded, the wire does not carry a second terminal model that can drift or lose fidelity, and the same bytes serve a human, a browser, and an agent without any of them being privileged.

The reference TUI matters as the adoption surface that bootstraps a population of terminals-on-the-wire, and it is worth real product investment. Its distinguishing trait is the wire itself — attach and detach, remoting, and humans and their agents sharing the same live terminals — rather than local splits, which it also has. ADR-0017 is what keeps that investment from corrupting the substrate: the TUI's needs land as metadata conventions and client logic, never as new wire surface. See [ADR-0009 (phux vs. mux positioning)](../ADR/0009-phux-vs-mux-positioning.md) and [ADR-0030](../ADR/0030-engine-delegated-wire-and-projection-consumers.md).

---

## Where to go next

| You want to | Read |
|---|---|
| Run it | [`QUICKSTART.md`](./QUICKSTART.md) |
| Understand the wire bytes | [`spec/README.md`](./spec/README.md) |
| Understand how the server is built | [`architecture/README.md`](./architecture/README.md) |
| Drive it from an agent | [`consumers/agents.md`](./consumers/agents.md) |
| Use the browser client | [`consumers/web.md`](./consumers/web.md) |
| Understand the TUI surface | [`consumers/tui.md`](./consumers/tui.md) |
| See why we decided X | [`../ADR/README.md`](../ADR/README.md) |
| Read the long arc | [`vision.md`](./vision.md) |
| Contribute | [`../CONTRIBUTING.md`](../CONTRIBUTING.md) |
