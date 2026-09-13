---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-06-06
---

> **Archived 2026-09-09.** Superseded by [ADR-0030](../../docs/adr/0030-engine-delegated-wire-and-projection-consumers.md):
> there is no L2 collection tier. Preserved for the design record; not the
> authoritative source. For current behavior see the ADR and `docs/spec/`.

# L2 Server-Side Architecture

**TL;DR.** Superseded scratch. This file designs how phux-server would have implemented a server-side Collection lifecycle tier (state in the `Registry` alongside Terminals; events via `CollectionEventEmitter` channels to per-Collection subscriber lists; a handler routing create/kill/rename commands). [ADR-0030](../../docs/adr/0030-engine-delegated-wire-and-projection-consumers.md) dissolves that tier — grouping becomes L3 metadata plus client logic, and the one atomic need (multi-terminal teardown) becomes a single L1 op, `KILL_TERMINALS { ids }`. Kept for design history, not as a build target.

---

## Overview

L2 adds named Collection bundles to L1's Terminal-per-pane model. The server must:

1. **Manage Collection state** — create, rename, kill, track membership
2. **Emit typed events** — when commands arrive, when Terminals join/leave, when Collections close
3. **Synchronize subscribers** — every attached client that speaks L2 gets Collection lifecycle updates
4. **Enforce atomic kill** — `KILL_COLLECTION` terminates all member Terminals together

The design maintains the existing single-threaded tokio architecture, keeps domain state (`phux-core::Registry`) separate from I/O state (`phux-server::ServerState`), and extends the per-pane broadcast pattern to Collections.

---

## 1. State Model

### 1.1 Domain: `phux-core::registry::Collection`

Extend `phux-core::Registry` with a fourth `SlotMap`:

```rust
// In phux-core/src/registry.rs

pub struct Collection {
    /// Unique key in the Registry's Collections slot map.
    pub id: CollectionId,
    /// Human-readable name; mutable via RENAME_COLLECTION.
    pub name: String,
    /// Terminals currently in this Collection. A Terminal may belong to
    /// zero or one Collection; removing membership is idempotent.
    pub terminals: Vec<TerminalId>,
    /// Timestamp when the Collection was created (immutable).
    pub created_at: std::time::Instant,
    /// Timestamp of the most recent RENAME (for observability).
    pub renamed_at: Option<std::time::Instant>,
}

pub struct Registry {
    sessions: SlotMap<SessionId, Session>,
    windows:  SlotMap<WindowId, Window>,
    panes:    SlotMap<PaneId, Pane>,
    // NEW:
    collections: SlotMap<CollectionId, Collection>,
}

impl Registry {
    /// Create a new Collection with an optional name.
    /// Returns the assigned CollectionId.
    pub fn create_collection(&mut self, name: Option<String>) -> CollectionId {
        let id = self.collections.insert(Collection {
            id,
            name: name.unwrap_or_else(|| format!("collection-{}", self.collections.len())),
            terminals: Vec::new(),
            created_at: std::time::Instant::now(),
            renamed_at: None,
        });
        id
    }

    /// Add a Terminal to a Collection. No-op if the Terminal is already
    /// a member; returns `Ok(true)` if newly added, `Ok(false)` if already
    /// a member, `Err` if Collection not found.
    pub fn add_terminal_to_collection(
        &mut self,
        collection_id: CollectionId,
        terminal_id: TerminalId,
    ) -> Result<bool, CollectionNotFound> {
        let collection = self.collections.get_mut(collection_id)?;
        if !collection.terminals.contains(&terminal_id) {
            collection.terminals.push(terminal_id);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Remove a Terminal from a Collection. Idempotent.
    pub fn remove_terminal_from_collection(
        &mut self,
        collection_id: CollectionId,
        terminal_id: TerminalId,
    ) -> Result<bool, CollectionNotFound> {
        let collection = self.collections.get_mut(collection_id)?;
        if let Some(pos) = collection.terminals.iter().position(|id| *id == terminal_id) {
            collection.terminals.remove(pos);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Rename a Collection.
    pub fn rename_collection(
        &mut self,
        collection_id: CollectionId,
        new_name: String,
    ) -> Result<(), CollectionNotFound> {
        let collection = self.collections.get_mut(collection_id)?;
        collection.name = new_name;
        collection.renamed_at = Some(std::time::Instant::now());
        Ok(())
    }

    /// Kill a Collection and all its member Terminals. Returns the list
    /// of TerminalIds that were killed (so the caller can issue one
    /// TERMINAL_CLOSED per Terminal + one COLLECTION_CLOSED).
    pub fn kill_collection(
        &mut self,
        collection_id: CollectionId,
    ) -> Result<Vec<TerminalId>, CollectionNotFound> {
        let collection = self.collections.remove(collection_id)?;
        Ok(collection.terminals)
    }
}
```

### 1.2 I/O: `phux-server::state::ServerState` extensions

Extend `ServerState` to track:

```rust
// In phux-server/src/state.rs

pub struct ServerState {
    // ... existing fields ...
    pub registry: Registry,
    pub attached: HashMap<ClientId, AttachedClient>,
    
    // NEW:
    /// Per-Collection broadcast channel for lifecycle events.
    /// When a Collection is created, a (Sender, Receiver) pair is allocated
    /// and stored here; subscribers (per ATTACH per L2 tier) receive a Receiver.
    /// On KILL_COLLECTION the Sender is dropped, signaling EOF to all subscribers.
    pub collection_events: HashMap<CollectionId, broadcast::Sender<CollectionLifecycleEvent>>,
    
    /// Clients subscribed to L2 events (per HELLO's declared LayerSet).
    /// Used to fan-out COLLECTION_* frames to all L2-capable clients.
    pub l2_subscribers: HashSet<ClientId>,
}

/// Lifecycle event for a Collection.
#[derive(Debug, Clone)]
pub enum CollectionLifecycleEvent {
    /// A Terminal was added to the Collection.
    MemberAdded { terminal_id: TerminalId },
    /// A Terminal was removed from the Collection.
    MemberRemoved { terminal_id: TerminalId },
    /// The Collection was renamed.
    Renamed { new_name: String },
    /// The Collection is being destroyed (EOF signal; no payload).
    Closing,
}
```

### 1.3 Per-Collection state lifecycle

When `CREATE_COLLECTION` arrives:

1. Registry allocates `Collection`, stores in `collections` SlotMap.
2. Runtime allocates `broadcast::channel()` and stores Sender in `collection_events`.
3. Clients that speak L2 receive `COLLECTION_OPENED` immediately (cached snapshot).

When a Terminal attaches to the Collection:

1. Registry updates `Collection::terminals`.
2. Runtime broadcasts `CollectionLifecycleEvent::MemberAdded` to the channel.
3. L2-speaking clients receive `COLLECTION_MEMBERSHIP_CHANGED { terminal_id, added: true }`.

When `KILL_COLLECTION` arrives:

1. Registry removes `Collection`, returns list of member TerminalIds.
2. Runtime broadcasts `CollectionLifecycleEvent::Closing` (all subscribers get EOF).
3. For each member Terminal:
   - TerminalActor is signaled to shut down.
   - Once shutdown completes, `TERMINAL_CLOSED` is emitted to all L1 subscribers.
4. After all Terminals are closed, `COLLECTION_CLOSED` is emitted to L2 subscribers.

---

## 2. Event Emission

### 2.1 CollectionEventEmitter (new utility)

A per-Collection event emitter lives in the runtime. It provides:

```rust
// New: phux-server/src/collection_emitter.rs

pub struct CollectionEventEmitter {
    /// The CollectionId this emitter is bound to.
    collection_id: CollectionId,
    /// Broadcast sender for lifecycle events.
    tx: broadcast::Sender<CollectionLifecycleEvent>,
}

impl CollectionEventEmitter {
    /// Create a new emitter for a Collection.
    pub fn new(collection_id: CollectionId) -> Self {
        let (tx, _rx) = broadcast::channel(DEFAULT_COLLECTION_BROADCAST_DEPTH);
        Self { collection_id, tx }
    }

    /// Emit a lifecycle event to all subscribers.
    pub fn emit(&self, event: CollectionLifecycleEvent) {
        let _ = self.tx.send(event); // Silently drop if no subscribers.
    }

    /// Get a receiver for subscribing a client.
    pub fn subscribe(&self) -> broadcast::Receiver<CollectionLifecycleEvent> {
        self.tx.subscribe()
    }

    /// Get a reference to the underlying Sender (for ServerState storage).
    pub fn sender(&self) -> broadcast::Sender<CollectionLifecycleEvent> {
        self.tx.clone()
    }
}
```

### 2.2 Broadcast depth constant

```rust
/// Default capacity of the per-Collection broadcast channel.
/// Sized for burst tolerance (e.g., many Terminals added/removed in quick succession).
pub const DEFAULT_COLLECTION_BROADCAST_DEPTH: usize = 64;
```

### 2.3 Event timing

Events are emitted **synchronously** in the command handler:

| Trigger | Event | Emitted by | Broadcast target |
|---------|-------|-----------|-----------------|
| ATTACH Terminal to Collection | `MemberAdded` | runtime L1 handler | Collection event channel |
| REMOVE Terminal from Collection | `MemberRemoved` | runtime L2 handler | Collection event channel |
| RENAME_COLLECTION | `Renamed` | runtime L2 handler | Collection event channel |
| KILL_COLLECTION (each member) | `TERMINAL_CLOSED` | TerminalActor shutdown sequence | L1 subscribers + L2 subscribers |
| KILL_COLLECTION (final) | `Closing` then `COLLECTION_CLOSED` | runtime L2 handler | L2 subscribers |

---

## 3. Runtime Handler Structure

### 3.1 L2 command dispatcher

Add to `phux-server/src/runtime.rs`:

```rust
/// Handle an L2 command frame.
async fn handle_l2_command(
    state: &Arc<Mutex<ServerState>>,
    client_id: ClientId,
    command: L2Command,
) -> Result<CommandResult, CommandError> {
    let mut srv = state.lock().unwrap();

    // Check L2 capability.
    let client = srv.attached.get(&client_id)
        .ok_or(CommandError::NotAttached)?;
    
    if !client.capabilities.layers.contains(Layer::L2) {
        return Err(CommandError::LayerNotSupported);
    }

    match command {
        L2Command::CreateCollection { name } => {
            let collection_id = srv.registry.create_collection(name);
            
            // Allocate broadcast channel for the Collection.
            let emitter = CollectionEventEmitter::new(collection_id);
            srv.collection_events.insert(collection_id, emitter.sender());
            
            // Emit frame to all L2 subscribers.
            let frame = Frame::CollectionOpened {
                collection_id: wire_id(collection_id),
                name: srv.registry.collections[collection_id].name.clone(),
            };
            broadcast_to_l2_subscribers(&mut srv, frame);
            
            Ok(CommandResult::CollectionCreated { collection_id: wire_id(collection_id) })
        }

        L2Command::RenameCollection { collection_id, name } => {
            srv.registry.rename_collection(core_id(collection_id), name.clone())?;
            
            let frame = Frame::CollectionRenamed {
                collection_id,
                new_name: name,
            };
            broadcast_to_l2_subscribers(&mut srv, frame);
            
            Ok(CommandResult::Ok)
        }

        L2Command::KillCollection { collection_id } => {
            let core_coll_id = core_id(collection_id);
            let terminal_ids = srv.registry.kill_collection(core_coll_id)?;
            
            // Drop the broadcast channel (signals EOF to subscribers).
            srv.collection_events.remove(&core_coll_id);
            
            // Emit TERMINAL_CLOSED for each member.
            for term_id in &terminal_ids {
                let wire_term_id = srv.resolve_terminal_wire_id(*term_id)?;
                let frame = Frame::TerminalClosed {
                    terminal_id: wire_term_id,
                    exit_status: None, // Killed by command, not process exit.
                };
                broadcast_to_all_subscribers(&mut srv, frame);
                
                // Signal the TerminalActor to shut down.
                // (This is separate; the actor will emit the actual close notification.)
            }
            
            // Emit COLLECTION_CLOSED.
            let frame = Frame::CollectionClosed { collection_id };
            broadcast_to_l2_subscribers(&mut srv, frame);
            
            Ok(CommandResult::Ok)
        }

        L2Command::AddTerminalToCollection { collection_id, terminal_id } => {
            let newly_added = srv.registry.add_terminal_to_collection(
                core_id(collection_id),
                core_id(terminal_id),
            )?;
            
            if newly_added {
                // Broadcast the membership change.
                let frame = Frame::CollectionMembershipChanged {
                    collection_id,
                    terminal_id,
                    added: true,
                };
                broadcast_to_l2_subscribers(&mut srv, frame);
            }
            
            Ok(CommandResult::Ok)
        }

        L2Command::RemoveTerminalFromCollection { collection_id, terminal_id } => {
            let was_removed = srv.registry.remove_terminal_from_collection(
                core_id(collection_id),
                core_id(terminal_id),
            )?;
            
            if was_removed {
                let frame = Frame::CollectionMembershipChanged {
                    collection_id,
                    terminal_id,
                    added: false,
                };
                broadcast_to_l2_subscribers(&mut srv, frame);
            }
            
            Ok(CommandResult::Ok)
        }

        L2Command::ListCollections => {
            // Snapshot current collections and send back.
            let collections: Vec<_> = srv.registry.collections.iter()
                .map(|(id, coll)| {
                    (wire_id(id), coll.name.clone(), coll.terminals.len())
                })
                .collect();
            
            Ok(CommandResult::CollectionList { collections })
        }
    }
}

/// Helper: broadcast frame to all L2-capable clients.
fn broadcast_to_l2_subscribers(srv: &mut ServerState, frame: Frame) {
    for &client_id in &srv.l2_subscribers {
        if let Some(client) = srv.attached.get(&client_id) {
            let _ = client.tx.try_send(Outbound::Frame(frame.clone()));
        }
    }
}
```

### 3.2 Integration with ATTACH

When a client attaches with L2 capability:

```rust
async fn handle_attach(
    state: &Arc<Mutex<ServerState>>,
    client_id: ClientId,
    target: AttachTarget,
    capabilities: ClientCapabilities,
) -> Result<(), AttachError> {
    let mut srv = state.lock().unwrap();

    // ... existing ATTACH logic ...

    // NEW: if L2 is enabled, send all active Collections.
    if capabilities.layers.contains(Layer::L2) {
        srv.l2_subscribers.insert(client_id);
        
        // Send a COLLECTION_OPENED for each active Collection (state replay).
        for (coll_id, collection) in srv.registry.collections.iter() {
            let frame = Frame::CollectionOpened {
                collection_id: wire_id(coll_id),
                name: collection.name.clone(),
            };
            client.tx.send(Outbound::Frame(frame)).await?;
            
            // Send COLLECTION_MEMBERSHIP_CHANGED for each member (state replay).
            for term_id in &collection.terminals {
                let wire_term_id = srv.resolve_terminal_wire_id(*term_id)?;
                let frame = Frame::CollectionMembershipChanged {
                    collection_id: wire_id(coll_id),
                    terminal_id: wire_term_id,
                    added: true,
                };
                client.tx.send(Outbound::Frame(frame)).await?;
            }
        }
    }

    Ok(())
}
```

### 3.3 Integration with DETACH

When a client detaches, remove from `l2_subscribers`:

```rust
async fn handle_detach(
    state: &Arc<Mutex<ServerState>>,
    client_id: ClientId,
) -> Result<(), DetachError> {
    let mut srv = state.lock().unwrap();
    srv.l2_subscribers.remove(&client_id);
    // ... existing DETACH logic ...
}
```

---

## 4. Command Flow Examples

### 4.1 CREATE_COLLECTION (C → S)

```
Client sends: COMMAND { command_id: 42, COMMAND_L2::CreateCollection { name: "build" } }

Runtime:
  1. Parse command, route to handle_l2_command.
  2. Acquire ServerState lock.
  3. Registry allocates Collection, stores in collections SlotMap.
  4. Runtime allocates broadcast::channel and stores Sender in collection_events.
  5. Emit COLLECTION_OPENED to all L2 subscribers.
  6. Release lock.
  7. Return success to client as COMMAND_RESULT { command_id: 42, Ok(...) }.
```

### 4.2 ADD_TERMINAL_TO_COLLECTION (C → S)

```
Client sends: COMMAND { COMMAND_L2::AddTerminalToCollection { 
                          collection_id: CollectionId::Local(1), 
                          terminal_id: TerminalId::Local(5) } }

Runtime:
  1. Acquire ServerState lock.
  2. Check that both Collection and Terminal exist.
  3. Registry adds Terminal to Collection::terminals.
  4. Emit COLLECTION_MEMBERSHIP_CHANGED { collection_id: 1, terminal_id: 5, added: true }
     to all L2 subscribers.
  5. Release lock.
  6. Return Ok to client.
```

### 4.3 KILL_COLLECTION (C → S)

```
Client sends: COMMAND { COMMAND_L2::KillCollection { collection_id: 1 } }

Runtime:
  1. Acquire ServerState lock.
  2. Registry removes Collection, returns [TerminalId::Local(5), Local(6), ...].
  3. Drop broadcast Sender for the Collection.
  4. For each member Terminal:
     - Emit TERMINAL_CLOSED { terminal_id: 5, exit_status: None } to all subscribers.
     - Signal TerminalActor to shut down (via handle in panes table).
  5. Emit COLLECTION_CLOSED { collection_id: 1 } to all L2 subscribers.
  6. Release lock.
  7. Return Ok to client.
  
TerminalActor (concurrent):
  - Receives shutdown signal.
  - Closes PTY.
  - Cleans up consumer state.
  - Returns exit code (if available).
```

---

## 5. Subscriber Registration

### 5.1 Per-Collection subscription (future extension)

In a future design, a client might SUBSCRIBE to just one Collection's events:

```rust
// Hypothetical future COMMAND variant:
SUBSCRIBE_COLLECTION { collection_id: CollectionId }

// The runtime would:
let rx = srv.collection_events[&collection_id].subscribe();
// ... spawn a task to forward events from rx to client.tx ...
```

For v0.2 (first L2 ship), all L2-capable clients subscribe to **all** Collections at once (via `l2_subscribers` set). Per-Collection subscription is a v0.3 refinement.

---

## 6. Thread Safety & Synchronization

### 6.1 Arc<Mutex<ServerState>>

- `ServerState` holds both `collection_events` (HashMap of Senders) and `l2_subscribers` (Set).
- Commands acquire the lock briefly, mutate, release.
- Broadcast channels are `Send` and can be cloned; Senders are distributed to subscribers.

### 6.2 TerminalActor kill sequence

When `KILL_COLLECTION` is processed:

1. Runtime signals each TerminalActor via a `shutdown_signal` handle.
2. Actor receives signal, closes PTY, returns exit code.
3. Actor's background task (or a separate task watching for actor completion) emits `TERMINAL_CLOSED` once the actor confirms shut down.
4. Once all Terminals are observed closed, runtime emits `COLLECTION_CLOSED`.

This ensures atomicity: clients see all `TERMINAL_CLOSED` frames before `COLLECTION_CLOSED`.

---

## 7. Wire Format

L2 frames (reserved discriminants per `docs/spec/L2.md`; allocated when L2 lands):

```
COLLECTION_OPENED {
    collection_id: CollectionId,
    name: str,
}

COLLECTION_CLOSED {
    collection_id: CollectionId,
}

COLLECTION_RENAMED {
    collection_id: CollectionId,
    new_name: str,
}

COLLECTION_MEMBERSHIP_CHANGED {
    collection_id: CollectionId,
    terminal_id: TerminalId,
    added: bool,
}
```

Encoding: positional fields per `docs/spec/appendix-encoding.md`. The `added` boolean uses the standard `Option<()>` tag convention: `0x00 = removed`, `0x01 = added`.

---

## 8. Error Handling

| Error | Signal | Handling |
|-------|--------|----------|
| Collection not found | `CommandError::CollectionNotFound` | Return error frame to client; no side effects. |
| Terminal not found | `CommandError::TerminalNotFound` | Return error frame to client; no side effects. |
| Terminal already in Collection | Silent no-op | `ADD_TERMINAL_TO_COLLECTION` succeeds but doesn't re-emit. |
| Terminal not in Collection | Silent no-op | `REMOVE_TERMINAL_FROM_COLLECTION` succeeds but doesn't re-emit. |
| Client lacks L2 capability | `CommandError::LayerNotSupported` | Return error; no side effects. |

---

## 9. Testing Strategy

### 9.1 Unit tests

- `registry.rs`: test Collection CRUD (create, add member, remove, rename, kill).
- `collection_emitter.rs`: test broadcast semantics (emit, subscribe, EOF on drop).

### 9.2 Integration tests

- **Scenario 1: Create → Add → Kill**
  1. Spawn a Terminal.
  2. Create a Collection.
  3. Add Terminal to Collection.
  4. Kill Collection.
  5. Verify: all L2 clients see COLLECTION_OPENED → COLLECTION_MEMBERSHIP_CHANGED → TERMINAL_CLOSED → COLLECTION_CLOSED in order.

- **Scenario 2: Multi-Terminal Kill**
  1. Create Collection.
  2. Spawn 3 Terminals, add all to Collection.
  3. Kill Collection.
  4. Verify: clients see 3 TERMINAL_CLOSED before COLLECTION_CLOSED.

- **Scenario 3: Rename**
  1. Create Collection with name "old".
  2. Rename to "new".
  3. Verify: clients see COLLECTION_RENAMED.

- **Scenario 4: L2 Capability Gating**
  1. Attach a client with L2 disabled.
  2. Send COMMAND_L2.
  3. Verify: error, no side effects.

- **Scenario 5: State Replay on Attach**
  1. Create Collection + add member.
  2. New client attaches with L2 enabled.
  3. Verify: client receives COLLECTION_OPENED + COLLECTION_MEMBERSHIP_CHANGED immediately (state catch-up).

---

## 10. Future Extensions

### 10.1 Per-Collection subscriber streams (v0.3)

Clients can `SUBSCRIBE_COLLECTION { collection_id }` to receive only that Collection's events, reducing bandwidth for busy servers with many Collections.

### 10.2 Collection metadata (L3)

Opaque key-value blobs stored per Collection (e.g., TUI-layer window title, color tag). Rides on the existing L3 metadata infrastructure.

### 10.3 Terminal lifecycle within a Collection

Extend `SPAWN_TERMINAL` to auto-add the new Terminal to a Collection:

```
SPAWN_TERMINAL {
    command: optional<list<str>>,
    cwd: optional<str>,
    collection_id: optional<CollectionId>,  // NEW
}
```

### 10.4 Collection membership constraints

A Terminal currently belongs to at most one Collection. Future designs might allow multiple Collections (e.g., tagging) or exclusive membership constraints.

---

## 11. Deployment Notes

- L2 is optional per [ADR-0015](../../docs/adr/0015-protocol-layering.md). A v0.1 server that does not yet implement L2 never allocates `collection_events` or `l2_subscribers` and returns `LayerNotSupported` to any L2 command.
- When L2 lands, `ServerState` initialization should create a default Collection at `CollectionId::Local(1)` to match the wire assumption in `docs/spec/L1.md` §1.1 (spawned Terminals without an explicit `collection_id` go to the default).
- The `CommandResult` enum grows new arms (`CollectionCreated`, `CollectionList`) when L2 messages are wired; existing arms are unaffected.

---

## Summary

- **State:** Collections live in `Registry` (domain), broadcast Senders in `ServerState` (I/O), alongside existing per-pane channels.
- **Events:** `CollectionLifecycleEvent` enum broadcast to per-Collection channels; L2 clients receive wire frames derived from those events.
- **Sync:** `Arc<Mutex<ServerState>>` protects both event channels and `l2_subscribers` set; lock is brief (command processing only).
- **Atomicity:** Kill-all is synchronous within the lock; clients see all `TERMINAL_CLOSED` before `COLLECTION_CLOSED`.
- **Testing:** Unit tests for Registry logic, integration tests for multi-client scenarios and state replay on attach.

This design extends the existing per-pane broadcast pattern (already in use for `TERMINAL_OUTPUT`) to Collections with minimal new machinery.
