---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-09
---

# Data model

**TL;DR.** The in-process types the server manipulates: the resources it
owns (a Terminal is the first kind, an agent session the second), the
grouping metadata over them, and the attached clients. Pure-data
`phux-core::Registry` on slotmaps with generational keys; I/O state lives
separately on `phux-server::ServerState`. This shape is distinct from the
wire; the bridge crosses at `IdBridge`. Grouping is metadata over resources,
not a built collection type.

---

The server is a graph of long-lived nodes with stable identity. The domain
(`phux-core`) uses one `SlotMap` per node type rather than `Rc<RefCell<>>`
because:

- Stable IDs are exactly what the wire protocol needs anyway.
- Cross-references ("this client's active terminal") become an ID, not a
  borrowed reference — no aliasing problem.
- Deletion is `O(1)` and slotmap's generational keys catch use-after-free in
  tests.

The shape splits in two: the *domain* (pure data, in `phux-core`) and the
*attached-client + I/O* state (in `phux-server`). The split is deliberate;
see ADR-0008 and the crate-graph note above.

## Grouping is metadata, not a collection tier

There is no built `Collection` type and no L2 collection lifecycle tier.
Grouping a set of terminals — what a user thinks of as a session — is L3
metadata plus client logic over the [L3 metadata model](../spec/L3.md),
keyed by an opaque grouping identity. `GroupId` is retained only as that
opaque key, not as a lifecycle entity the server creates, names, or tears
down; per [ADR-0030](../../ADR/0030-engine-delegated-wire-and-projection-consumers.md)
(option B) the structured grouping that used to be proposed for the wire is a
consumer-side projection, and the lone irreducible group operation — atomic
multi-terminal teardown — is a single L1 op (`KILL_TERMINALS`) rather than a
tier. `GroupId`'s retention as an opaque grouping key is settled, not a
remnant awaiting removal (bead phux-0bmc closed as resolved-by-rename).

The `Registry`'s `Session` and `Window` types are the in-process carriers of
that grouping metadata. They are domain bookkeeping, not a wire tier: under
[ADR-0017](../../ADR/0017-tui-not-protocol-privileged.md) the session,
window, pane-focus, and layout vocabulary is a TUI-consumer convention stored
as L3 metadata, never a protocol-privileged concept.

```rust
// phux-core::registry::Registry — domain only, no I/O.
pub struct Registry {
    sessions:  SlotMap<SessionId,  Session>,             // grouping metadata, not an L2 tier
    windows:   SlotMap<WindowId,   Window>,              // TUI L3 convention
    resources: SlotMap<ResourceId, ResourceDescriptor>,  // L1 resources of every kind
}

pub struct Session  { id, name, windows: Vec<WindowId>, active: Option<WindowId> }
pub struct Window   { id, session, slots: Vec<ResourceId>, layout: Option<LayoutNode>, active: Option<ResourceId> }
pub struct ResourceDescriptor {
    id, kind: ResourceKind, parent: Option<ResourceId>,
    window: Option<WindowId>,          // Terminal kind only
    terminal: Option<TerminalFacet>,   // present iff kind == Terminal
    agent: Option<AgentFacet>,         // present iff kind == AgentSession
}
pub struct TerminalFacet { dims, cwd, title }
pub struct AgentFacet    { provider, native_id, state }
// ResourceId is the slotmap key (still spelled `TerminalId` in phux-core
// until the wire rename lands); location and kind are orthogonal.
// LayoutNode is a binary split tree of ResourceId leaves; only a Terminal
// occupies a window slot. Per ADR-0017 the whole tree (LayoutNode + Window
// + active-slot focus) is a TUI-consumer convention stored in L3 metadata,
// not a wire concept. ADR-0012's "binary split, not n-ary" decision applies
// to the TUI's tree, not the wire.
```

Exactly the facet named by `kind` is populated, and the `Registry` is the
only constructor of a descriptor. A resource's `parent` is set at creation
and never changes; removing a parent removes its children, and removing a
window or session removes every resource in its slots and their children.
Removing a child never touches the parent.

The PTY handle and `libghostty_vt::Terminal` for a Terminal are not fields
of the descriptor. They are server-side concerns and hang off `ResourceId`
in side tables in `phux-server`. Keeping the descriptor free of I/O is what
lets `phux-core` stay `forbid(unsafe_code)` and ship without an async
runtime.

```rust
// phux-server::state::ServerState — domain + clients + I/O.
pub struct ServerState {
    pub registry:        Registry,
    pub attached:        HashMap<ClientId, AttachedClient>,
    // ResourceTable: one ResourceHandle per live resource, its cancel
    // token, its subscribers, its output pumps, and the engine JoinSet.
    resources:           ResourceTable,
    // Core ids (slotmap keys, generational) <-> wire ids (u32), for
    // sessions, terminals, and windows. All three go through `IdBridge`.
    pub idspace:         IdSpace,
    next_client_id:      u64,
}

// phux-server::resource — what the table holds for any kind.
pub struct ResourceHandle {
    pub kind: ResourceKind, pub parent: Option<ResourceId>,
    pub output: broadcast::Sender<PaneOutput>,
    pub consumer_attach, consumer_detach, consumer_ack,    // ADR-0018 consumers
    pub subscribe_to_events, unsubscribe_from_events,      // semantic events
    pub upgrade, control,                                  // ADR-0032, ADR-0033
    pub facet: ResourceFacetHandle,   // Terminal(TerminalHandle) | ...
}

pub struct AttachedClient {
    pub id:      ClientId,           // server-assigned, monotonic
    pub session: phux_core::SessionId,
    pub tx:      tokio::sync::mpsc::Sender<OutboundFrame>,
}
```

`ServerState` is shared across tasks behind a single `std::sync` mutex; see
[threading and I/O](./threading.md) for why a synchronous mutex is safe on
the current-thread runtime and how `KILL_TERMINALS` applies atomically under
one acquisition.

The engine side of a resource is `ResourceCore`: the checked output
sequence, the output broadcast sender, the event-subscriber registry and
fan-out, the cancel token and exit notification, and the control mailbox.
An engine (today `resource::terminal::TerminalActor`) embeds one, keeps its
own kind-specific state beside it, and builds the `ResourceHandle` in its
constructor. Runtime code never holds a `TerminalHandle` on its own: it
holds a `ResourceHandle` and calls `ResourceHandle::terminal()` where a
grid, PTY, or input operation is needed — the one place a request aimed at
a resource of another kind becomes a `WrongResourceKind` error.

Session name lookup goes through `Registry::sessions()` rather than a side
index — it is O(N) in session count, which is fine: session count is small
(single digits typical, double digits worst-case) and an extra index would
have to be kept consistent across cascading deletes.
