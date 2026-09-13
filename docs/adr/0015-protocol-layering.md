---
audience: contributors
stability: stable
last-reviewed: 2026-05-28
---

# 0015 — Protocol layering: L1 substrate, L2 collections, L3 metadata

**TL;DR.** The phux wire is three tiers. L1 carries Terminals (PTY plus libghostty `Terminal` plus identity, with bytes-out and structured input-in). L2 is optional Collections — named lifecycle bundles of Terminals. L3 is an optional opaque key-value metadata store. Sessions, windows, panes, layouts, focus are TUI-consumer conventions implemented via L3, not wire concepts. Federation and automation are cross-cuts, not layers.

Status: Accepted
Date: 2026-05-26
Superseded in part by [ADR-0030](./0030-engine-delegated-wire-and-projection-consumers.md): the L2 Collection tier is dissolved; the wire is L1 plus optional L3, grouping is L3 metadata plus client logic, and the one atomic group operation is the L1 verb KILL_TERMINALS.
See [ADR-0102](./0102-resources-the-server-serves-kinds.md) for L1's current scope: it is the resource substrate, not Terminals alone.

## Context

Through ADR-0013 the wire became asymmetric — bytes for pane content,
structured events for input. That decision settled *how* a Terminal is
carried across the wire. It did not settle *what shape* the surrounding
protocol takes: today the wire's session-graph vocabulary (sessions,
windows, panes, layout trees, focus) is interleaved with the terminal
vocabulary as if they were the same kind of thing.

They are not. A Terminal is a primitive: PTY plus libghostty `Terminal`
plus identity. A session-of-windows-of-panes is one *consumer's
preferred way* to present a collection of Terminals on a display.
Conflating them costs the protocol three properties phux's vision
(see [`docs/vision.md`](../vision.md)) needs:

- **A consumer that doesn't want sessions and windows cannot speak the
  protocol without inheriting them.** An agent driving Terminals
  programmatically has no use for "window" or "pane focus" but must
  still parse `WINDOW_OPENED`, `LAYOUT_CHANGED`, `FOCUS_CHANGED` to be
  conforming. Every TUI-shaped opinion in the wire taxes every non-TUI
  consumer.

- **Federation is harder than it needs to be.** A session graph that
  knows about windows and layouts has to federate windows and layouts.
  A terminal-and-metadata graph just federates terminals; the layout
  is whatever the consumer paints with its own metadata. The latter
  scales to a fleet; the former is a coordination problem.

- **Evolution drags everything.** Adding a new TUI feature (tabbed
  layouts, floating overlays, "pinned" terminals) under the current
  shape requires new wire messages, new conformance language, new
  forward-compat reservations. None of that should touch the wire if
  the wire knows about Terminals and nothing else.

The shape this ADR establishes was settled in the design discussion
that produced [`docs/vision.md`](../vision.md). This document is the
formal version.

## Decision

The phux wire is **three layers**, each addressable separately, each
referencing only the layers below it.

### L1 — Terminal

A managed terminal: a PTY (or a snapshot-only replay surface), a
libghostty `Terminal` that parses the PTY's bytes into canonical
state, and an attached I/O surface.

A Terminal carries:

- **Stable identity** — `TerminalId`, monotonic, never reused, federation-
  routable (see [ADR-0016](./0016-terminal-id-as-wire-primary.md) and
  the `LOCAL` / `SATELLITE` tag union per [ADR-0007](./0007-mosh-class-transport-and-satellites.md)).
- **Observable output** — `TERMINAL_OUTPUT { terminal_id, seq, bytes }`,
  the VT byte stream emitted by the PTY after server-side capability
  rewriting per the subscribing consumer.
- **Initial-state snapshot** — `TERMINAL_SNAPSHOT { terminal_id, cols,
  rows, vt_replay_bytes, scrollback_bytes? }`, a self-contained byte
  sequence that reconstructs current grid state on a fresh
  `libghostty_vt::Terminal`.
- **Structured input** — `INPUT_KEY`, `INPUT_MOUSE`, `INPUT_FOCUS`,
  `INPUT_PASTE`, `INPUT_RAW`, all addressed to a `TerminalId`. Encoder
  state per terminal lives on the server (ADR-0006, ADR-0008).
- **Resize** — `RESIZE { terminal_id, cols, rows }`.
- **Lifecycle** — `SPAWN`, `ATTACH_TERMINAL`, `DETACH_TERMINAL`,
  `TERMINAL_CLOSED { reason, exit_status? }`.
- **Structured event stream** — `TERMINAL_EVENT { terminal_id, event }`
  where `event` is a tagged union surfacing parsed OSC and synthesized
  PTY events: `Title`, `Cwd`, `CommandStart`, `CommandEnd { exit_code,
  duration }`, `HyperlinkStart`, `HyperlinkEnd`, `Bell`, `Clipboard`,
  `Progress`, `UserNotification`, `MouseShape`, `Custom`. SPEC.md §7.7
  already defines this union; this ADR elevates it from chrome support
  to a load-bearing L1 surface.

L1 is the **substrate**. Every conforming consumer speaks L1. Server-
side, L1 is the always-on service.

### L2 — Collection

A named lifecycle bundle of Terminals. A Terminal may belong to zero
or one Collection. Killing a Collection kills its members. Detaching
all clients from a Collection leaves the Collection and its Terminals
alive.

A Collection carries:

- **Stable identity** — `CollectionId`, federation-routable like
  `TerminalId`.
- **Name** — a string the consumer chose, optional.
- **Membership** — the set of `TerminalId`s that belong to it.
- **Lifecycle** — `CREATE_COLLECTION`, `ADD_TERMINAL_TO_COLLECTION`,
  `REMOVE_TERMINAL_FROM_COLLECTION`, `RENAME_COLLECTION`,
  `KILL_COLLECTION`, `LIST_COLLECTIONS`, and the
  `COLLECTION_OPENED` / `COLLECTION_CLOSED` / `COLLECTION_RENAMED`
  / `COLLECTION_MEMBERSHIP_CHANGED` event family.

L2 is an **optional** server service. A phux server may decline to
mount L2 (an L1-only substrate deployment); a conforming client may
decline to speak L2 (an agent ignoring the grouping concept).
Conformance is per-tier (see "Conformance" below).

### L3 — Metadata

A typed key-value store the server hosts and does not interpret.
Scopes:

- `Terminal { terminal_id, key, value }`
- `Collection { collection_id, key, value }`
- `Global { key, value }`

Operations: `GET_METADATA`, `SET_METADATA`, `DELETE_METADATA`,
`LIST_METADATA`, and `METADATA_CHANGED { scope, key }` for
subscribers. Values are opaque bytes with a recommended convention
of CBOR-encoded structured data; the server enforces nothing beyond
size limits.

L3 is **how the reference TUI maintains "windows" and "layouts"** —
by writing a layout tree blob into a Collection's metadata under a
well-known key. The protocol does not define what a window is, what
a layout is, what focus means. It defines storage. Conventions live
in the consumer's design docs.

L3 is **optional**. A server may decline to mount it. A consumer may
ignore it.

### Cross-cutting: Federation

Federation is not a layer. It is an **addressing scheme** that runs
through L1, L2, and L3 identities uniformly:

```
TerminalId   = tagged_union { LOCAL { id: u32 }, SATELLITE { host: SatelliteHost, id: u32 } }
CollectionId = tagged_union { LOCAL { id: u32 }, SATELLITE { host: SatelliteHost, id: u32 } }
```

A v0.1 server constructs `LOCAL` identities only. A v0.2+ hub server
routes between satellites. The wire bytes are stable across that
transition; the routing matrix on the server changes. See
[ADR-0007](./0007-mosh-class-transport-and-satellites.md) for the
hub-and-spoke roadmap.

### Cross-cutting: Automation

Server-side rules that subscribe to L1 events and fire actions are an
**optional service**, not a layer. A v0.1 server may ship without
automation; a v0.2 server may add it. The wire surface is small:
register a rule, list rules, delete a rule. Rules reference L1 events
and may produce L1 effects (spawn, kill, input) and out-of-band
effects (run a host command). Details deferred to a follow-on ADR.

### Conformance tiers

`HELLO` declares the layers the consumer speaks:

```
HELLO {
    versions: list<VersionRange>,
    client_caps: ClientCapabilities,
    layers: bitset<Layer>,   // { L1, L2, L3 }
}
```

The server MUST omit messages from layers the consumer did not
declare. A consumer MUST NOT send messages from layers it did not
declare. Servers advertise the layers they implement in `HELLO_OK`'s
`server_caps`.

Three consumer shapes are defined:

- **L1 consumer.** Agents, recorders, CI orchestrators. Sees
  Terminals and Terminal events. Never sees Collections, never sees
  metadata.
- **L1+L3 consumer.** Native GUIs that arrange Terminals their own
  way and may use metadata for cross-client agreement. May or may
  not use Collections.
- **L1+L2+L3 consumer.** The reference TUI. Tmux-shaped UX.

## Rationale

### Why L1 is fundamental and the others are not

A Terminal is what exists in the world: a process running under a
PTY, emitting bytes a parser turns into a grid. Every consumer needs
this. There is no smaller useful primitive.

Collections and metadata are server-side conveniences that *could*
be done client-side at a cost. We bake them in because the cost is
load-bearing for the consumers we care about:

- Collections give a server-enforced lifecycle invariant (kill bundle
  → kill members) that every consumer would otherwise re-derive from
  metadata + per-member kills. Server-side is one cheap operation;
  client-side is N round trips with race windows.
- Metadata gives consumers a place to agree on conventions across
  attaches and across clients. The alternative — every consumer
  maintains its own state per detach/reattach — loses multi-client
  agreement (the only thing layout-on-the-wire was buying us, the
  only thing the alternative would lose).

Both pay back the cost of being in the protocol many times over.
Layout, by contrast, would not — see "Why not L3 = layout" below.

### Why not pure metadata (no Collection concept)

A more minimalist substrate would drop Collection entirely: every
Terminal has tags; "kill all terminals tagged 'work'" is a query.
This was seriously considered (see VISION.md and the design
discussion that produced this ADR).

The case for: smaller surface, more flexible (overlap, multiple
groupings), no first-class "session" concept that pre-decides
hierarchy.

The case against, which carried: Collections earn their place by
carrying a server-enforced lifecycle invariant (atomic kill, server-
consistent membership view) that's awkward to reconstruct from
metadata + queries. Users also genuinely expect "my session" to be a
durable named thing, not a saved query.

Worth revisiting if a strong agent-orchestration use case shows up
that needs many overlapping groupings of the same Terminal. Today,
single-parent Collection + metadata tags is enough.

### Why not L3 = layout (the previously-considered shape)

An earlier draft of this layering put **Layout** at L3: a server-
side service that stored a layout tree per Collection and emitted
`LAYOUT_CHANGED` events. The rationale was multi-client shared
layout (pair-programming, "I see what you see" cross-attach).

That shape did not survive review. Three failures:

1. **Layout is meaningless to most consumers.** An agent does not
   want one. A recorder does not want one. A native GUI may want
   its own arrangement. Putting it on the wire taxes every consumer
   that ignores it.
2. **The use case is narrow.** Multi-client shared layout is real
   but uncommon. It does not justify the protocol surface.
3. **Metadata covers it.** A TUI that wants shared layouts can
   store the tree as an L3 metadata blob keyed by collection; other
   clients reading the same key get the same view. The server
   doesn't need to know what a layout is.

The current `LayoutNode` / `LAYOUT_CHANGED` / `FOCUS_CHANGED` /
`WINDOW_*` messages in SPEC.md become *TUI-consumer conventions*,
documented in the TUI's design doc, implemented via L3 metadata
reads and writes. They leave the wire entirely.

### Why structured terminal events are at L1, not L2

OSC 133 prompt boundaries, OSC 7 cwd, OSC 0/1/2 title — these are
how an L1-only agent answers "did the build finish, what was the
exit code, what directory am I in." They are *the* headline feature
of the substrate for non-TUI consumers. SPEC.md already defines
them in `OSC_EVENT`; this ADR re-anchors them as load-bearing for
agents, not chrome support for GUIs.

### Why the cross-cuts aren't layers

Federation is an *addressing scheme* applied uniformly to every
identity. Calling it a layer overstates it — the wire bytes don't
change between local and federated; only routing does. It's a
property, not a tier.

Automation subscribes to events and produces effects. It is a
*service* a server may mount alongside the layer services. Calling
it a layer would imply consumers route messages through it, which
they don't.

## Tradeoffs

### What this is worse at than the old shape

- **Re-spec'ing in-flight.** SPEC.md grew up describing
  `WindowId`, `PaneId`, `LAYOUT_CHANGED` as wire concepts. Moving
  those into TUI-consumer conventions is a non-trivial edit, and
  every reference to those names in ADRs (especially 0010, 0012)
  needs to be re-read with the new framing. See "Doc impact" below.
- **Pre-shipping the TUI's convention vocabulary.** Until the TUI's
  design doc enumerates the L3 metadata keys it writes ("phux.tui
  .layout/v1", "phux.tui.window_order/v1"), implementers can't
  shadow the TUI. This is a near-term gap, not a structural cost.
- **Multi-client TUI agreement via metadata, not via a typed
  protocol.** The wire doesn't validate that two TUIs writing the
  layout blob agree on its schema. Schema drift becomes the TUI's
  problem, with the usual versioned-key answer.

### What this is better at

- **Substrate consumers cost nothing.** An agent SDK is L1-only;
  the server never sends it a `COLLECTION_OPENED` it has to ignore.
- **Federation surface stays small.** Federation has to route three
  identity types, not a session graph.
- **Evolution is asymmetric in the right direction.** Adding TUI
  features (tabbed layouts, floating panes, pinned terminals)
  never touches the wire. Adding substrate features (a new
  Terminal event, a new lifecycle field) touches one layer.
- **The "tmux replacement" story and the "agent control plane"
  story share a substrate** instead of forking the project.

## Doc impact

This ADR forces edits in several places, listed for the cascade
that follows it:

- **SPEC.md** — restructured by tier. L1 messages, L2 messages, L3
  messages. Conformance is per-tier. `WindowId`, `PaneId`,
  `LAYOUT_CHANGED`, `FOCUS_CHANGED`, `WINDOW_*` and the `LayoutNode`
  type leave the normative spec and become TUI-consumer conventions
  documented in the TUI's design doc.
- **README.md** — reframed: substrate + reference TUI.
- **ARCHITECTURE.md** — reorganized around the three layers; the
  per-pane actor becomes the per-terminal actor; the L2/L3 services
  are described as optional mounts.
- **DESIGN.md** — narrowed to the TUI consumer's surface, plus its
  L3 metadata conventions.
- **ADR-0007 (satellites)** — re-anchored: the satellite tag lives
  on every identity, not just `SessionId`. Promote forward-compat
  reservation to a normative addressing scheme.
- **ADR-0010 (tmux-CC reserved)** — re-anchored: tmux-CC is one L4-
  equivalent consumer (presentation-only) alongside the native TUI
  and a future GUI. Nothing reserved; nothing protocol-privileged.
- **ADR-0012 (binary-split layout)** — re-scoped: the layout tree is
  a TUI convention, not a wire type. The "binary split, not n-ary"
  decision continues to apply *to the TUI*, not to the wire.

## Alternatives considered

- **Keep the current single-tier protocol; add an L1-only profile
  via capability bits.** The capability machinery would have to grow
  to cover every TUI-shaped message that an L1 consumer wants
  omitted. Hard to evolve cleanly. Drifts toward the layered design
  one capability bit at a time, with worse names.

- **Make everything metadata.** Drop Collection entirely; even L1
  becomes "the server hosts terminals and exposes metadata."
  Genuinely minimalist, but Collection earns its place by carrying
  a lifecycle invariant the alternative reconstructs from many
  small operations (see "Why not pure metadata" above). Worth
  revisiting if agent orchestration outgrows single-parent grouping.

- **Two tiers (L1 + everything else).** Lump Collection, layout,
  workspace, chrome into one upper tier. Fails the test of
  "consumers should be able to declare what they want": a GUI
  client that uses Collections but not the TUI's layout convention
  has nowhere to land. The three-tier shape gives each piece its
  own opt-in axis.

- **Five+ tiers (terminal / collection / metadata / automation /
  presentation).** Over-fits. Automation is a service, not a layer;
  presentation is client-side, not a protocol concern. The three
  tiers above are the lines that earn their place.
