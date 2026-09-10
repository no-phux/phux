---
audience: humans, contributors, agents, consumers
stability: evolving
last-reviewed: 2026-09-09
---

# How phux works

**TL;DR.** phux serves resources: server-owned things with a kind, an output stream, a bootstrap, an event stream, metadata, and an optional parent. A terminal is the first kind; an agent's session is the second. A person and their agents observe and drive the same resources as peers. The wire carries identity, lifecycle, transport, opaque bytes per kind, and metadata; each consumer runs the engine for the kinds it shows. phux is pre-alpha and spec-first.

---

## The short version

A resource in phux belongs to the server, not to the window currently showing it. A terminal is one. The structured event stream of an agent running in that terminal is another. You can detach, attach from another client, read either from a script, or let an agent drive the terminal, without creating a second copy of anything.

The familiar multiplexer is one view of that model: splits, focus, status, and keybindings for a person. The headless CLI gives programs selectors, snapshots, input, events, and JSON results. Both operate on the same resources, and neither has standing the other lacks.

The protocol stays small by carrying identity, lifecycle, transport, opaque bytes per kind, and metadata. It does not turn the wire into a second model of any kind. The engine for each kind runs at both ends, so a consumer projects the structure it needs from the real stream.

## Maturity: pre-alpha, spec-first

phux is pre-alpha. The protocol is pre-1.0 (the version is pinned in `phux-protocol` and mirrored by [`spec/`](./spec/README.md); a CI gate keeps the two in sync) and the spec leads the code: behaviors are written down before they are built. This document describes the target state the ADRs decide. The [Status](#status) table at the end lists every place the reference implementation has not caught up; nothing else in this document marks a gap.

What runs today: a server that spawns PTY-backed terminals and parses them with libghostty, accepts agent session records from a hook shim, and enforces the parent binding between the two; a reference TUI that attaches over the wire; a browser client (phux-web); and a headless verb set with an MCP adapter that a script or an agent can drive. The shipped CLI verbs are catalogued in [`QUICKSTART.md`](./QUICKSTART.md). Hub-and-spoke federation dials configured satellites, aggregates their resource inventory, spawns there, and relays resource-scoped operations; satellite sessions and windows are intentionally not joined into the hub's local model. A hub does report each satellite's sessions as a separate host-qualified inventory, so a selector can group by host without the hub adopting foreign ids ([ADR-0107](../ADR/0107-satellite-sessions-are-listed-never-adopted.md)).

This document owns the maturity fact. Other docs link here rather than restating it.

---

## Resources, not panes

A resource is a server-owned, addressable thing. Every resource has:

- a `ResourceId` and a `ResourceKind`;
- a lifecycle: spawned, then closed with a reason (`Exited`, `Killed`, `ParentClosed`, `ServerShutdown`);
- an ordered, opaque output stream with a codec negotiated per stream;
- a bootstrap: a replaceable replica generation a consumer loads before it reads live bytes;
- a kind-defined input channel;
- a tagged event stream;
- an L3 metadata scope;
- an optional parent, set at spawn and immutable.

Terminal is the first kind: a PTY child and a libghostty engine, with columns, rows, a title, and a working directory. Those fields are the Terminal facet, and the operations that only make sense against them (input atoms, resize, screen and state reads, history, input leases, signals, file drops, transcription) are facet operations. Sending one to a resource of another kind is refused with `WRONG_RESOURCE_KIND`.

AgentSession is the second kind: the structured event stream of an agent harness, with a provider name, an optional native session id, and a derived state. It is producer-fed. The harness's hook shim appends records; the server stamps sequence and time, retains a bounded ring, and derives working, blocked, and done from the record types. An agent session always has a Terminal parent, the pane the agent runs in. Closing the parent closes the child with `ParentClosed`, atomically, under the same lock that serves `KILL_RESOURCES`. Closing a child never touches the parent. Bindings are one level deep.

Sessions, windows, panes, and splits are not on the wire. "Pane" stays a TUI and CLI word for a Terminal-kind resource in a layout slot, expressed as metadata and client logic. An agent needs none of that vocabulary to spawn a terminal, drive it, and read its exit code; an orchestrator may opt into the same L3 layout convention to place panes for a human without gaining authority over that human's local focus.

The reason the consumer set drives the model: humans want windows and splits, agents want to spawn resources and read their events, fleets want event streams, and control planes want resources addressable across machines. The resource is the one thing all of them need. Everything else is a per-consumer arrangement.

The reference TUI ships in-tree because a substrate is only real if something rides it.

---

## The engine is delegated per kind

phux does not own the semantics of any kind. For Terminal, libghostty owns the mapping from bytes to a grid of cells and from input atoms to bytes, and both ends run that engine: the server holds canonical state and a rendering client holds a local replica. For AgentSession, the engine is the `AgentEventsJsonlV1` codec: one JSON record per line, a closed set of record types, and a state rule the spec states. Neither kind is re-encoded into a second model on the wire.

Structured state is a projection, not the synchronization tier. A consumer that carries the engine computes it locally; the CLI and the MCP adapter also use server-derived convenience snapshots and results such as `GET_SCREEN`. Those replies make headless tools practical without defining a screen model that every stream must traverse. The rule generalizes per kind: a new kind brings its own codec and its own projection, not new structured wire surface.

This is the central design commitment. See [ADR-0102 (resources: the server serves kinds)](../ADR/0102-resources-the-server-serves-kinds.md), [ADR-0013 (libghostty bytes on the wire)](../ADR/0013-libghostty-bytes-on-wire.md), [ADR-0004 (libghostty-vt as the canonical grid)](../ADR/0004-libghostty-vt-as-grid.md), [ADR-0008 (use libghostty types directly)](../ADR/0008-use-libghostty-types-directly.md), and [ADR-0030 (engine-delegated wire and projection consumers)](../ADR/0030-engine-delegated-wire-and-projection-consumers.md).

In practice:

- Server to client: opaque bytes per kind (bootstrap and live output). Client to server: structured input atoms for a Terminal, appended records for an AgentSession.
- Terminal input types are libghostty's, re-exported directly. There is no parallel phux input enum.
- A libghostty pin bump lights up new terminal features on both ends at once. A codec version does the same for a record kind.

---

## What the wire carries

The wire carries resource **identity**, resource **lifecycle** (spawn with a kind and an optional parent, close with a reason, atomic multi-resource teardown, and the parent cascade), **transport** framing and capability negotiation, **opaque bytes per kind**, and **metadata** the server stores without interpreting. Structured screen state is not its synchronization model. Convenience commands may return snapshots and command results for headless consumers; panes and layouts remain consumer projections.

The spec organizes this as layers declared at connect time:

| Tier | Concept | Carries |
|---|---|---|
| **L1** | Resource | Spawn, close, bootstrap, live output, and per-kind append. The Terminal facet's structured input, resize, snapshots, and engine-derived events. The AgentSession facet's records and derived state. |
| **L3** | Metadata | Opaque key-value pairs scoped to a resource or globally. Consumers store conventions here (TUI layout, window and session names, group membership, agent identity). The server stores; it does not interpret. |

There is no L2 collection tier. Group lifecycle is metadata plus client logic, with two exceptions the server enforces because a consumer cannot enforce them alone. `KILL_RESOURCES { ids }` tears down a set of resources all-or-nothing under the single state lock, so no observer sees a partial group. The parent binding closes every child when its parent closes, under that same lock, so an agent session cannot outlive the terminal it ran in. Atomicity earns operations, not a tier. See [ADR-0030](../ADR/0030-engine-delegated-wire-and-projection-consumers.md), [ADR-0015 (protocol layering)](../ADR/0015-protocol-layering.md), and [ADR-0104 (parent bindings are L1 lifecycle)](../ADR/0104-parent-bindings-are-l1-lifecycle.md).

Create is `SPAWN_RESOURCE` plus a metadata key; rename is a metadata SET. `GroupId` is a documented opaque grouping key, not a lifecycle tier.

The wire surface itself is owned by the spec: L1 by [`spec/L1.md`](./spec/L1.md), the metadata model and grouping conventions by [`spec/L3.md`](./spec/L3.md), and the byte-level codec by [`spec/appendix-encoding.md`](./spec/appendix-encoding.md).

---

## Identity is federation-ready

Every resource is addressed by a `ResourceId` that is either `Local { id }` or `Satellite { host, id }`. Location is orthogonal to kind. A normal server constructs local ids. A federation hub also constructs satellite ids when it retags aggregate inventory, spawn replies, and relayed frames. A non-hub server rejects a satellite id cleanly with `UnsupportedSatelliteRoute` rather than misreading it.

Concretely: `Local { id: 42 }` names resource 42 on the server you are talking to. `Satellite { host: "prod-box-3", id: 42 }` names resource 42 on the configured satellite keyed by the opaque hub-local token `prod-box-3`. The CLI renders these as `@42` and `prod-box-3/@42`; both are accepted as direct selectors once they appear in the server's inventory. Satellite resources stay resource-scoped: the hub does not merge remote session or window identities or chain routes through another satellite. The relay rewrites only the routing id at each hop and forwards payloads opaquely.

See [ADR-0102](../ADR/0102-resources-the-server-serves-kinds.md) and [ADR-0007 (Mosh-class transport and satellites)](../ADR/0007-mosh-class-transport-and-satellites.md).

---

## Consumers are peers; carry your own engine per kind

The reference TUI, the browser client, the Cockpit client over the C ABI, and the agent surface are peers. None has protocol-level standing: if a consumer needs a capability the wire does not provide, the answer is an ADR that extends the spec, not a consumer-shaped hook on the wire ([ADR-0017 (TUI not protocol-privileged)](../ADR/0017-tui-not-protocol-privileged.md)).

A consumer that wants structured state carries the engine for the kinds it shows and projects locally. phux-web is that pattern in shipping code for the Terminal kind: it compiles to WASM, loads `ghostty-vt.wasm`, speaks the exact wire codec over WebSocket, and computes its rendered view from engine state it owns. A consumer that shows agent sessions parses `AgentEventsJsonlV1` records the same way; the TUI's sidebar and fleet overlay read an agent's state from that stream when a session exists and from metadata otherwise. A consumer that does not render a kind skips it: phux-web lists non-terminal resources and draws none of them. See [ADR-0025 (browser web client)](../ADR/0025-browser-web-client.md), [ADR-0103 (agent session resource and producer-fed streams)](../ADR/0103-agent-session-resource-and-producer-fed-streams.md), and the consumer docs: [`consumers/web.md`](./consumers/web.md), [`consumers/tui.md`](./consumers/tui.md), [`consumers/agents.md`](./consumers/agents.md).

The agent surface is the headless CLI verb set plus the [`phux-mcp`](./consumers/mcp.md) adapter over it (its `tools/list` catalog is the authoritative tool inventory). An agent reads structured state through versioned JSON, creates and explicitly places terminals with `new`, `launch`, and `spawn`, and may serialize existing-pane topology edits through the shared L3 layout convention. `phux agent session open`, `phux agent emit`, and `phux agent log` are the producer and reader verbs for the second kind, and `%name` resolves an agent session. Observation is bounded `wait` plus event `watch`; a blocked human question reuses the advisory `Asked` event. The TUI alone applies `next-attention` and return navigation to its client-local focus, so neither CLI nor MCP can move a human's viewport. These are consumer projections, not wire contracts or scheduler semantics. The library behind the CLI is the `phux-client` crate over `phux-protocol`. The verb catalog, JSON contracts, and orchestration safety rules are owned by [`consumers/agents.md`](./consumers/agents.md) and the runnable [`phux-agent-cli` skill](../examples/skills/phux-agent-cli/SKILL.md).

Some L1 commands return engine-derived snapshots a consumer could also compute locally: `GET_SCREEN`, `GET_TERMINAL_STATE`, `SUBSCRIBE_RESOURCE_EVENTS`, and the pushed `AgentEvent` frame. Read these as a convenience for consumers that have not yet adopted the carry-your-own-engine pattern, not as a normative structured contract and not as license to grow new structured wire surface. [`spec/L1.md`](./spec/L1.md) owns that surface.

---

## Positioning: a substrate for all work

phux is a substrate, a wire and per-kind engines that resources ride, with a reference TUI as its first product on top. The argument for it is architectural, not a feature list: because the engine for each kind is shared and never re-encoded, the wire carries no second model that can drift or lose fidelity, and the same bytes serve a human, a browser, and an agent without privileging any of them. A terminal was the first thing worth serving this way. An agent's session was the second, because the same person already held both, one in a pane and one in a hook payload, and the two had separate lifecycles for no reason.

The reference TUI matters as the adoption surface that bootstraps a population of resources on the wire, and it is worth real product investment. Its distinguishing trait is the wire itself: attach and detach, remoting, and a human and their agents holding the same live resources. ADR-0017 keeps that investment from corrupting the substrate: the TUI's needs land as metadata conventions and client logic, never as new wire surface. See [ADR-0009 (positioning)](../ADR/0009-phux-vs-mux-positioning.md), [ADR-0030](../ADR/0030-engine-delegated-wire-and-projection-consumers.md), and [ADR-0102](../ADR/0102-resources-the-server-serves-kinds.md).

---

## Status

Target-versus-shipped gaps open as of the last review. Each row names the ADR that owns the target and the bead that tracks the work; the rest of this document describes the target.

| Gap | Today | Owner | Tracked |
|---|---|---|---|
| Working-directory and command-boundary events as an L1 Terminal-facet frame | `TERMINAL_EVENT` has no codec entry. `cwd_changed`, `command_started`, and `command_finished` reach consumers only through the `SUBSCRIBE_RESOURCE_EVENTS` gate path. | [ADR-0015](../ADR/0015-protocol-layering.md), [ADR-0102](../ADR/0102-resources-the-server-serves-kinds.md) | phux-ue2r |
| On-disk output journal and crash recovery | The server keeps every resource in memory. Nothing is journaled and there is no recovery flag. | [ADR-0092](../ADR/0092-durable-work-coordinator-authority.md) | phux-p91i |
| Workload authentication enforcement | The `phux-workload/v1` profile is allocated in the spec. The reference server accepts no proof and enforces no scope matrix. | [ADR-0098](../ADR/0098-workload-proof-and-closed-scope-authority.md) | phux-cockpit-p1q.11.2 |
| Cockpit projection of agent sessions | Cockpit lists Terminal-kind resources only; AgentSession children are not shown under their parent. | [ADR-0103](../ADR/0103-agent-session-resource-and-producer-fed-streams.md) | phux-am9y.25 |

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
