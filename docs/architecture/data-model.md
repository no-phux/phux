---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-06-06
---

# Data model

**TL;DR.** The in-process types the server manipulates: the terminals it
owns, the grouping metadata over them, and the attached clients. Pure-data
`phux-core::Registry` on slotmaps with generational keys; I/O state lives
separately on `phux-server::ServerState`. This shape is distinct from the
wire; the bridge crosses at `IdBridge`. Grouping is metadata over terminals,
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
    sessions: SlotMap<SessionId, Session>,   // grouping metadata, not an L2 tier
    windows:  SlotMap<WindowId,  Window>,    // TUI L3 convention
    panes:    SlotMap<PaneId,    Pane>,      // L1 terminal
}

pub struct Session  { id, name, windows: Vec<WindowId>, active: Option<WindowId> }
pub struct Window   { id, session, panes: Vec<PaneId>, layout: Option<LayoutNode>, active: Option<PaneId> }
pub struct Pane     { id, window, dims, cwd, title }
// LayoutNode is a binary split tree of PaneId leaves. Per ADR-0017 the
// whole tree (LayoutNode + Window + active-pane focus) is a TUI-consumer
// convention stored in L3 metadata, not a wire concept. ADR-0012's
// "binary split, not n-ary" decision applies to the TUI's tree, not the wire.
```

The PTY handle and `libghostty_vt::Terminal` for a pane are not fields of
`Pane`. They are server-side concerns and hang off `PaneId` in side tables in
`phux-server`. Keeping `Pane` free of I/O is what lets `phux-core` stay
`forbid(unsafe_code)` and ship without an async runtime.

```rust
// phux-server::state::ServerState — domain + clients + I/O.
pub struct ServerState {
    pub registry:        Registry,
    pub attached:        HashMap<ClientId, AttachedClient>,
    pub pane_subscribers: HashMap<PaneId, Vec<ClientId>>,
    // Per-pane input log; merge point for multi-client keystrokes.
    pane_inputs:         HashMap<PaneId, Vec<PaneInput>>,
    // Core ids (slotmap keys, generational) <-> wire ids (u32), for
    // sessions, terminals, and windows. All three go through `IdBridge`.
    pub idspace:         IdSpace,
    next_client_id:      u64,
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

Session name lookup goes through `Registry::sessions()` rather than a side
index — it is O(N) in session count, which is fine: session count is small
(single digits typical, double digits worst-case) and an extra index would
have to be kept consistent across cascading deletes.
