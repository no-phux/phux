---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-13
---

# Data model

**TL;DR.** `phux-core` holds sessions, resources, arrangements, and the
pure `apply` function that turns an organization command into a
transition. Keys name records forever; slotmap ids are live handles for
one server run. `phux-server` commits each transition to the organization
store before mirroring it in memory, and keeps clients and I/O beside the
registry in `ServerState`.

---

The server is a graph of long-lived records with stable identity. The domain
(`phux-core`) uses one `SlotMap` per record type rather than `Rc<RefCell<>>`
because:

- Stable IDs map to wire identities.
- Cross-references such as a client's active terminal use IDs rather than
  borrowed references, avoiding aliasing.
- Deletion is `O(1)` and slotmap's generational keys catch use-after-free in
  tests.

## Keys and handles

Every session, resource, and arrangement carries a 16-byte random key
(`SessionKey`, `ResourceKey`, `ArrangementKey`) that names it forever and is
what the organization store persists. The slotmap `SessionId` and
`ResourceId` stay as live handles: `Registry` keeps `HashMap<Key, Id>`
indices so a key resolves to the current handle, and `IdBridge` maps handles
to wire ids. Handles never reach the store; keys never appear in terminal
I/O. `phux-core` does not generate randomness: the server mints keys with
`getrandom` and passes them into `apply` as `FreshKeys`
([ADR-0160](../adr/0160-sessions-own-resources-and-window-is-retired.md)).
`phux-core` does not depend on `phux-protocol`; it owns its own `Command`,
`Refusal`, and `InstanceId`, and the server maps between the two
([ADR-0011](../adr/0011-protocol-core-independence.md)).

## Resources and kinds

Everything the server serves is a resource. `ResourceKind` is a
`#[non_exhaustive]` enum with two variants today, `Terminal` and
`AgentSession`; a consumer that meets a kind it does not know treats the
resource as opaque, never as a Terminal. The kind fixes which facet the
descriptor carries: a Terminal carries `TerminalFacet { dims, cwd, title }`;
an AgentSession carries `AgentFacet { provider, native_id, state }` and a
parent. Exactly the facet named by `kind` is populated, and the `Registry`
is the only constructor
([ADR-0102](../adr/0102-resources-the-server-serves-kinds.md)).

Every resource also carries its key, the session it belongs to, its exit
policy (`Remove` or `Retain`), and its current or most recent `Activation`.
An activation records its id, the server instance that started it, its
`ExecState`, its `Outcome`, and for a Terminal the `ProcIdentity { pid,
pgid, start_raw, boot_id }` that startup reconciliation compares against
the live process table
([ADR-0162](../adr/0162-organization-records-persist-and-activations-reconcile.md)).

## Sessions and arrangements

A session is a record: key, name, `SessionLifetime` (`Persistent` or
`Ephemeral`), the `awaiting_organization` intake mark, its members in
creation order, an optional arrangement, descriptive attributes, and a
revision. Every resource is a member of exactly one session. There is no
`Window` type; what clients show as tabs are groups in the arrangement.
The authority record names the persistent Inbox session.

`arrangement.rs` holds `LayoutNode<L>`, a binary split tree generic over
its leaf, with `split`, `remove`, and `leaves`. A persisted arrangement is a
list of `Group { id, name, tree: LayoutNode<EntryRef> }`, where an
`EntryRef` names a resource by key; `validate(members)` refuses a tree that
names a non-member, repeats an entry id, or carries a ratio outside
`(0, 1)`. Group and entry ids are random 16 bytes. ADR-0012's "binary
split, not n-ary" rule applies to this tree. Clients edit arrangements in
handle space; `phux-client-runtime` translates keys to handles on read and
back on replace.

```rust
// phux-core — domain only, no I/O.
pub struct Session {
    key: SessionKey, name: String, lifetime: SessionLifetime,
    awaiting_organization: bool,
    members: Vec<ResourceKey>,           // creation order
    arrangement: Option<ArrangementKey>,
    attrs: Attrs, created: Stamp, revision: Revision,
}
pub struct Resource {
    key: ResourceKey, session: SessionKey, kind: ResourceKind,
    parent: Option<ResourceId>, name: Option<String>, provenance: Provenance,
    launch: Option<LaunchSpec>, on_exit: ExitPolicy,
    activation: Option<Activation>, revision: Revision,
    terminal: Option<TerminalFacet>,   // present iff kind == Terminal
    agent: Option<AgentFacet>,         // present iff kind == AgentSession
}
pub struct Arrangement { key, owner: ArrangementOwner, groups: Vec<Group>, revision }
pub struct Authority   { id: AuthorityId, inbox_session: SessionKey }
```

## Applying a command

`apply(org, command, actor, now, fresh) -> Result<(Transition, Vec<Effect>),
Refusal>` in `apply.rs` is the one implementation of the organization rules.
It takes the current organization snapshot, a command, the actor, the time,
and freshly minted keys, and returns either a refusal or the transition to
commit plus the effects (launch, terminate) for the server to run. It does
no I/O. The invariants it enforces after every step are: one session per
resource; arrangements reference members only; revisions strictly
increase; an ephemeral session is never empty after a commit; the Inbox
session is never ephemeral or deleted; a session with members is never
deleted; a resource whose activation is current or `Lost` is never removed
or relaunched; and an exit under `ExitPolicy::Remove` yields a removal.
Undo applies a receipt's inverse only when the recorded revisions still
match.

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
drops it from its session's members and from every arrangement entry that
names it. Removing a child never touches the parent ([ADR-0104](../adr/0104-parent-bindings-are-l1-lifecycle.md)).

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
    // sessions and resources, through `IdBridge`. The legacy wire window
    // id is interned per session for the synthesized snapshot.
    pub idspace:         IdSpace,
    // Transactional organization store (SQLite, or memory in tests).
    org_store:           Box<dyn OrgStore>,
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

`ServerState::commit(command, actor)` is the only organization mutation
path: run `phux-core::apply`, write the transition, its receipt, and the
journal position to the org store in one transaction, then mirror it in
memory, acknowledge, and publish events. A failed store write refuses the
command and leaves memory unchanged. Startup opens the store, imports a
pre-store registry on first start, resumes from an upgrade blob, then runs
`reconcile.rs` over activations recorded live under an earlier instance,
before seeding anything.

`ServerState` is shared across tasks behind a single `std::sync` mutex; see
[threading and I/O](./threading.md) for why a synchronous mutex is safe on
the current-thread runtime and how `KILL_RESOURCES` applies atomically under
one acquisition.

Each engine embeds a `ResourceCore` (output sequence and broadcast, event
fan-out, cancel token, control mailbox) and builds the `ResourceHandle`;
the facet rule is in [module-structure.md](./module-structure.md#phux-server).

Teardown runs under one lock acquisition. A kill (`KILL_RESOURCE`,
`KILL_RESOURCE_IF`, `KILL_RESOURCES`, `CLOSE_TAB_RESOURCES`) resolves every
wire id inside a single `with_mut` and commits the whole visible teardown
there (`runtime/committed_close.rs`, L1 §5.2): it cancels every engine into
its hangup grace, journals each `pane_closed`, gathers the subscribers,
cancels the output pumps, and reaps each entity through
`ServerState::reap_terminal`, which removes the resource through `commit`
and deletes an ephemeral session it empties in the same transaction, so no
other command interleaves and none sees a killed resource
after the reply. A Terminal engine that observes PTY EOF fires its exit
notification; the exit watcher then does the same gather, journal, and reap
in one critical section before the `RESOURCE_CLOSED` sends are awaited. For a
killed pane the watcher finds the entity already reaped and only fires
`pane-exit`. A natural exit commits the activation's end and outcome; under
`ExitPolicy::Remove` the same transaction removes the resource. Nothing is
respawned. The server may exit once no activation is live and no client is
connected; it waits until no killed process is still in its grace.

Session name lookup uses `Registry::sessions()` rather than a side index.
Its O(N) cost is acceptable for the session count; membership, intake, and
inventory queries that scale with resource count go to the org store's
indexes instead.

## Status

No remaining target-versus-shipped gaps.

| Gap | Today | Owner | Tracked |
|---|---|---|---|
