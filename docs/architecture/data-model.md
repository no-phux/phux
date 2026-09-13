---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-12
---

# Data model

**TL;DR.** The in-process types the server manipulates: the resources it
owns (a Terminal is the first kind, an agent session the second), the
parent bindings between them, the grouping metadata over them, and the
attached clients. Pure-data `phux-core::Registry` on slotmaps with
generational keys; I/O state lives separately on
`phux-server::ServerState`. This shape is distinct from the wire; the
bridge crosses at `IdBridge`. Grouping is metadata over resources, not a
built collection type.

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

## Resources and kinds

Everything the server serves is a `ResourceDescriptor`. `ResourceKind` is a
`#[non_exhaustive]` enum with two variants today, `Terminal` and
`AgentSession`; a consumer that meets a kind it does not know treats the
resource as opaque, never as a Terminal. The kind fixes which facet the
descriptor carries: a Terminal carries `TerminalFacet { dims, cwd, title }`
and a window slot; an AgentSession carries `AgentFacet { provider,
native_id, state }` and a parent. Exactly the facet named by `kind` is
populated, and the `Registry` is the only constructor
([ADR-0102](../adr/0102-resources-the-server-serves-kinds.md)).

## Grouping is metadata, not a collection tier

There is no L2 collection tier; see [`../spec/L2.md`](../spec/L2.md).
Grouping a set of resources — what a user thinks of as a session — is L3
metadata plus client logic over the [L3 metadata model](../spec/L3.md),
keyed by an opaque grouping identity. `GroupId` is retained only as that
opaque key, not as a lifecycle entity the server creates, names, or tears
down. The lone irreducible group operation — atomic multi-resource teardown —
is a single L1 op (`KILL_RESOURCES`) rather than a tier. `GroupId`'s
retention as an opaque grouping key is settled, not a remnant awaiting
removal (bead phux-0bmc closed as resolved-by-rename).

The `Registry`'s `Session` and `Window` types are the in-process carriers of
that grouping metadata. They are domain bookkeeping, not a wire tier: under
[ADR-0017](../adr/0017-tui-not-protocol-privileged.md) the session,
window, pane-focus, and layout vocabulary is a TUI-consumer convention stored
as L3 metadata, never a protocol-privileged concept.

```rust
// phux-core::registry::Registry — domain only, no I/O.
pub struct Registry {
    sessions:  SlotMap<SessionId,  Session>,             // grouping metadata, not a wire tier
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
pub struct AgentFacet    { provider, native_id: Option<String>, state: Option<String> }
// ResourceId is the slotmap key; location and kind are orthogonal.
// LayoutNode is a binary split tree of ResourceId leaves; only a Terminal
// occupies a window slot. Per ADR-0017 the whole tree (LayoutNode + Window
// + active-slot focus) is a TUI-consumer convention stored in L3 metadata,
// not a wire concept. ADR-0012's "binary split, not n-ary" decision applies
// to the TUI's tree, not the wire.
```

The PTY handle and `libghostty_vt::Terminal` for a Terminal are not fields
of the descriptor. They are server-side concerns and hang off `ResourceId`
in side tables in `phux-server`. Keeping the descriptor free of I/O is what
lets `phux-core` stay `forbid(unsafe_code)` and ship without an async
runtime.

## The binding graph

A resource's `parent` is set at creation and never changes.
`Registry::new_agent_session(parent, facet)` is the only way to create a
bound resource: it refuses an unknown parent (`UnknownResource`) or a parent
of another kind (`ParentKindMismatch`), so an AgentSession always hangs off
a live Terminal and a Terminal has no parent. One level only: a child holds
no children of its own. There is no inverse index; `Registry::children
(parent)` scans the resource slotmap, which is O(N) in resource count and
fine for the same reason session lookup is (below).

Removal cascades downward and never upward. `Registry::remove_resource(id)`
removes the resource, then every resource whose `parent` is `id`, then
vacates the Terminal's window slot; `remove_window` and `remove_session`
run the same cascade for every slot they hold. Removing a child never
touches the parent ([ADR-0104](../adr/0104-parent-bindings-are-l1-lifecycle.md)).

## Server-side state

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
    // Per-scope L3 key/value store (Terminal, group, global).
    metadata:            MetadataStore,
    next_client_id:      u64,
}

// phux-server::resource — what the table holds for any kind.
pub struct ResourceHandle {
    pub kind: ResourceKind, pub parent: Option<ResourceId>,
    pub output: broadcast::Sender<PaneOutput>,
    pub consumer_attach, consumer_detach, consumer_ack,    // ADR-0018 consumers
    pub subscribe_to_events, unsubscribe_from_events,      // semantic events
    pub upgrade, control,                                  // ADR-0032, ADR-0033
    pub facet: ResourceFacetHandle,   // non_exhaustive: Terminal | AgentSession
}

pub struct AttachedClient {
    pub id:      ClientId,           // server-assigned, monotonic
    pub session: phux_core::SessionId,
    pub tx:      tokio::sync::mpsc::Sender<OutboundFrame>,
}
```

`ServerState` is shared across tasks behind a single `std::sync` mutex; see
[threading and I/O](./threading.md) for why a synchronous mutex is safe on
the current-thread runtime and how `KILL_RESOURCES` applies atomically under
one acquisition.

The engine side of a resource is `ResourceCore`: kind, parent, wire id, the
checked output sequence, the output broadcast sender, the event-subscriber
registry and fan-out, the cancel token and exit notification, and the
control mailbox. Each engine (`resource::terminal::TerminalActor`,
`resource::agent_session`) embeds one, keeps its own kind-specific state
beside it, and builds the `ResourceHandle` in its constructor. Runtime code
never holds a `TerminalHandle` on its own: it holds a `ResourceHandle` and
calls `ResourceHandle::terminal()` where a grid, PTY, or input operation is
needed — the one place a request aimed at a resource of another kind becomes
a `WrongResourceKind` error.

Teardown runs under one lock acquisition. `KILL_RESOURCES` resolves every
wire id and cancels every engine inside a single `with_mut`, so no other
command interleaves between the first and last removal. A Terminal engine
that observes PTY EOF fires its exit notification; the exit watcher then
gathers the subscribers, reaps the domain entity through
`ServerState::reap_terminal` (cascading to the window and session when they
empty), and forgets the `ResourceTable` entry, all in the same critical
section, before the `RESOURCE_CLOSED` sends are awaited.

Session name lookup goes through `Registry::sessions()` rather than a side
index — it is O(N) in session count, which is fine: session count is small
(single digits typical, double digits worst-case) and an extra index would
have to be kept consistent across cascading deletes.

## Status

No remaining target-versus-shipped gaps in the in-process types this
document owns. Parent cascade announces `CloseReason::ParentClosed`, the
runtime calls `Registry::new_agent_session` and writes `AgentFacet.state`,
and `ResourceId` is the key name on both sides of `IdBridge`.

| Gap | Today | Owner | Tracked |
|---|---|---|---|
