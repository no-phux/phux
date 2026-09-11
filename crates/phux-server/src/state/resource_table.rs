//! The per-resource engine table: every map keyed on a live resource's
//! identity, plus the `JoinSet` that owns the engine tasks' futures.
//!
//! The maps share one lifetime: an entry appears when
//! `ServerState::register_resource_handle` records a freshly-spawned engine
//! and disappears when the resource is reaped. Keeping them together makes
//! the "forget everything about this resource" and "forget everything about
//! this client" teardowns single calls instead of open-coded map operations
//! spread across `state::reap` and `state::client`.
//!
//! The table is kind-agnostic: it stores [`ResourceHandle`]s and never
//! looks inside a facet. Kind-specific dispatch happens where the runtime
//! reaches a facet through [`ResourceHandle::terminal`].
//!
//! # Ownership boundary
//!
//! This type owns the *bookkeeping*, not the policy. Anything that needs a
//! second cluster stays on `ServerState` and calls in:
//!
//! * `register_resource_handle` / `spawn_resource_actor` mint a wire id
//!   from `state::id_space` in the same breath as the insert, so they stay
//!   on `ServerState` and hand the already-interned pieces down here.
//! * `reap_terminal`'s bookkeeping also retires wire ids and per-resource
//!   metadata; it calls [`ResourceTable::forget_resource`] for these maps
//!   and keeps the rest.
//! * `build_upgrade_blob` and `request_pane_handoff` are `async` and must
//!   not be — nothing on this type awaits, so the state lock can never be
//!   held across a suspension point through it.
//!
//! The struct and every method are `pub(super)`: the accessors the runtime
//! calls stay on `ServerState` (see `state::terminals`), so the crate's
//! public surface is unchanged and three of these maps
//! (`tokens`, `tasks`, `pumps`) stay unreachable from outside `state`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use phux_protocol::ids::BootstrapId;
use tokio::task::{AbortHandle, JoinSet};
use tokio_util::sync::CancellationToken;

use super::ClientId;
use crate::resource::{ResourceHandle, ResourceId};

/// Last `PANE_OUTPUT` sequence a pump emitted before it was superseded.
///
/// Shared with the pump task so a replacement generation can resume from
/// where the prior one stopped rather than replaying from zero.
pub(super) type LastValidAttachSequence = Arc<AtomicU64>;

/// What a superseded pump generation leaves behind: its bootstrap id (to
/// tombstone), a token that resolves once it has actually exited, and the
/// sequence it reached.
pub(super) type PriorAttachResourcePump = (BootstrapId, CancellationToken, LastValidAttachSequence);

/// The new generation's `(cancel, done, last_valid_seq)` plus whatever it
/// displaced.
pub(super) type AttachResourcePumpReplacement = (
    CancellationToken,
    CancellationToken,
    LastValidAttachSequence,
    Option<PriorAttachResourcePump>,
);

/// One generation of an `ATTACH_RESOURCE` output pump.
///
/// `ATTACH_RESOURCE` used to be idempotent-or-nothing: a second attach for a
/// live `(client, terminal)` was refused so the pane could not double-stream.
/// Negotiated bootstrap makes re-attach meaningful — a client re-bootstraps to
/// change profile or recover — so a generation now *replaces* its predecessor:
/// the prior `cancel` fires, `done` reports when it has drained, and its
/// bootstrap id is tombstoned so late frames from the dead pump are dropped
/// rather than attributed to the new one.
#[derive(Debug)]
pub(super) struct AttachResourceGeneration {
    /// Fires to stop this pump.
    pub(super) cancel: CancellationToken,
    /// Resolves once the pump task has actually exited.
    pub(super) done: CancellationToken,
    /// Highest sequence this generation emitted.
    pub(super) last_valid_seq: LastValidAttachSequence,
    /// Bootstrap id this generation streams under.
    pub(super) bootstrap_id: BootstrapId,
}

/// An output task's lifetime, independent of which bootstrap path created it.
/// Dropping the owner aborts every await in the task, including mailbox sends.
#[derive(Debug)]
struct OutputPumpTask {
    abort: AbortHandle,
    done: CancellationToken,
}

impl Drop for OutputPumpTask {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

/// Every resource-keyed table the server owns, plus the client
/// subscriptions and output pumps that hang off them.
///
/// Held as a single field on [`super::ServerState`]. Not thread-safe on
/// its own; the surrounding `Mutex<ServerState>` provides synchronization.
#[derive(Debug)]
pub(super) struct ResourceTable {
    /// Per-resource engine handles, keyed by core [`ResourceId`]. The
    /// `ResourceHandle` is `Send`; the engine behind it (the Terminal
    /// engine owns a `!Send` `Terminal`) lives on the `LocalSet` — see
    /// ADR-0014.
    ///
    /// Populated by [`super::ServerState::register_resource_handle`] after
    /// the engine is spawned. Looked up by the ATTACH handler to request
    /// bootstraps and by the input path to forward keystrokes.
    handles: HashMap<ResourceId, ResourceHandle>,
    /// Per-resource cancellation tokens. Cancelling a token fires the
    /// matching engine's shutdown branch (see `TerminalActor::run`'s
    /// `select!`). Typically a child of the per-server root token, so a
    /// root cancel cascades to every resource in one step.
    ///
    /// Dropping the token does NOT cancel — cancellation must be explicit
    /// (see `detach_actor`).
    tokens: HashMap<ResourceId, CancellationToken>,
    /// `JoinSet` collecting the engine futures spawned via
    /// [`super::ServerState::spawn_resource_actor`]. Owned at this scope so
    /// cancellation of the per-server root token (or drop of
    /// `ServerState`) aborts every still-running engine in one go.
    ///
    /// **Drop-safety note:** `JoinSet<()>` is `Send`, but the futures it
    /// holds are `!Send` (the Terminal engine owns a `!Send` `Terminal`
    /// per ADR-0014). They were spawned via `JoinSet::spawn_local`, which
    /// is only legal inside a `LocalSet`. `ServerState` — and therefore
    /// this table — is dropped at the tail of
    /// `runtime::ServerRuntime::run_async` on the same thread that ran the
    /// `LocalSet`, so this `JoinSet`'s `Drop` is always on the spawning
    /// thread — no cross-thread poll of `!Send` futures occurs.
    tasks: JoinSet<()>,
    /// For each resource, the clients currently observing it (and thus
    /// eligible to receive `RESOURCE_OUTPUT` frames for it).
    ///
    /// Empty lists are garbage-collected rather than left behind, so the
    /// map stays bounded across attach/detach churn.
    subscribers: HashMap<ResourceId, Vec<ClientId>>,
    /// Per-`(client, terminal)` cancellation for `ATTACH_RESOURCE` output
    /// pumps (phux-v45.7). `DETACH_RESOURCE` cancels one entry; client
    /// detach / disconnect cancels all of the client's entries; pane reap
    /// cancels the pane's entries. Without the token the pump task (which
    /// holds the client's outbound sender) would keep streaming until the
    /// connection died.
    pumps: HashMap<(ClientId, ResourceId), AttachResourceGeneration>,
    /// All raw-output tasks, including gated aggregate replacements and
    /// `SPAWN_RESOURCE` pumps. A staged and a published pump may coexist until
    /// aggregate commit; terminal detach must retire both.
    output_pumps: HashMap<(ClientId, ResourceId), Vec<OutputPumpTask>>,
    /// Next connection-global bootstrap id for per-terminal attaches, keyed
    /// by client. Monotonic, so a tombstoned generation's id is never reused.
    next_bootstrap: HashMap<ClientId, u64>,
    /// ADR-0109: for each resource created by `SPAWN_RESOURCE`, the
    /// connection that spawned it and whether any other connection has
    /// subscribed to its output or used it since. A resource with no entry was not
    /// spawned by a client (a session seed, a pane rebuilt by an upgrade)
    /// and is never "unattached since spawn". Dropped with the resource.
    spawns: HashMap<ResourceId, SpawnRecord>,
}

/// Who spawned a resource, and whether another connection has attached it
/// since (ADR-0109, `docs/spec/L1.md` §5.2.1).
#[derive(Debug, Clone, Copy)]
struct SpawnRecord {
    /// The connection whose `SPAWN_RESOURCE` created the resource. Its own
    /// subscriptions never count as an attach.
    spawner: ClientId,
    /// Set the first time any other connection subscribes to or uses the
    /// resource; never cleared.
    used_by_other: bool,
}

impl Default for ResourceTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceTable {
    /// Build an empty table.
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            handles: HashMap::new(),
            tokens: HashMap::new(),
            tasks: JoinSet::new(),
            subscribers: HashMap::new(),
            pumps: HashMap::new(),
            output_pumps: HashMap::new(),
            next_bootstrap: HashMap::new(),
            spawns: HashMap::new(),
        }
    }

    // -- actor handles ------------------------------------------------

    /// Look up the [`ResourceHandle`] for `terminal`, if registered.
    #[must_use]
    pub(super) fn handle(&self, terminal: ResourceId) -> Option<&ResourceHandle> {
        self.handles.get(&terminal)
    }

    /// Every registered resource id. Cheap relative to
    /// [`Self::all_handles`]: no `ResourceHandle` clones.
    #[must_use]
    pub(super) fn resource_ids(&self) -> Vec<ResourceId> {
        self.handles.keys().copied().collect()
    }

    /// Clone every registered `(pane, handle)` pair, so callers can talk to
    /// the actors *outside* the `ServerState` lock.
    #[must_use]
    pub(super) fn all_handles(&self) -> Vec<(ResourceId, ResourceHandle)> {
        self.handles
            .iter()
            .map(|(tid, handle)| (*tid, handle.clone()))
            .collect()
    }

    /// Record a freshly-spawned engine's `handle` and shutdown `token`
    /// against `terminal`.
    ///
    /// Overwrites any prior entry. The wire-id allocation that pairs with
    /// this deliberately stays on
    /// [`super::ServerState::register_resource_handle`] — it belongs to the
    /// id space, not to this table.
    pub(super) fn register(
        &mut self,
        terminal: ResourceId,
        handle: ResourceHandle,
        token: CancellationToken,
    ) {
        self.handles.insert(terminal, handle);
        self.tokens.insert(terminal, token);
    }

    /// Spawn an engine's future onto the per-server `JoinSet`.
    ///
    /// Must be called from inside a `LocalSet` (ADR-0014): the Terminal
    /// engine owns a `!Send` `Terminal`, so this goes through
    /// `JoinSet::spawn_local`.
    /// See the drop-safety note on the `tasks` field for why holding those
    /// futures behind a `Send` `JoinSet` is sound.
    pub(super) fn spawn_actor<F>(&mut self, actor_future: F)
    where
        F: Future<Output = ()> + 'static,
    {
        self.tasks.spawn_local(actor_future);
    }

    /// Cancel `terminal`'s engine token, signalling the engine to exit, and
    /// forget the token. Idempotent.
    ///
    /// The actor task itself is drained from the per-server `JoinSet` when
    /// it returns from `run`; we don't need to touch `tasks` here.
    pub(super) fn detach_actor(&mut self, terminal: ResourceId) {
        if let Some(token) = self.tokens.remove(&terminal) {
            token.cancel();
        }
    }

    // -- subscriptions ------------------------------------------------

    /// Subscribers (snapshot) for `terminal`. Returns an empty slice if no
    /// clients are currently observing the pane.
    #[must_use]
    pub(super) fn subscribers_for(&self, terminal: ResourceId) -> &[ClientId] {
        self.subscribers.get(&terminal).map_or(&[], Vec::as_slice)
    }

    /// Subscribe `client` to `terminal`, deduplicating: a client already on
    /// the list is not pushed twice, so a re-attach cannot double-fan
    /// `RESOURCE_OUTPUT` at it.
    ///
    ///
    /// Every path that puts a client on a resource's output goes through
    /// here (`ATTACH_RESOURCE`, a session `ATTACH`'s sweep, the spawner's
    /// own auto-subscription), so this is where a spawned resource learns
    /// it has been attached by someone other than its spawner (ADR-0109).
    /// The mark stays even if that subscription is later rolled back.
    pub(super) fn subscribe(&mut self, client: ClientId, terminal: ResourceId) {
        self.note_use(client, terminal);
        let subs = self.subscribers.entry(terminal).or_default();
        if !subs.contains(&client) {
            subs.push(client);
        }
    }

    // -- spawn provenance (ADR-0109) -----------------------------------

    /// Record that `spawner`'s `SPAWN_RESOURCE` created `terminal`. Called
    /// before anything can subscribe to the new resource.
    pub(super) fn record_spawn(&mut self, terminal: ResourceId, spawner: ClientId) {
        self.spawns.insert(
            terminal,
            SpawnRecord {
                spawner,
                used_by_other: false,
            },
        );
    }

    /// Note that `client` attached or used `terminal`: subscribed to its
    /// output, or named it in a verb that drives or reads it (ADR-0109,
    /// L1 §5.2.1). Only a connection other than the spawner counts.
    pub(super) fn note_use(&mut self, client: ClientId, terminal: ResourceId) {
        if let Some(record) = self.spawns.get_mut(&terminal)
            && record.spawner != client
        {
            record.used_by_other = true;
        }
    }

    /// `true` iff `terminal` was spawned by a client and no connection but
    /// its spawner has attached or used it since.
    #[must_use]
    pub(super) fn unattached_since_spawn(&self, terminal: ResourceId) -> bool {
        self.spawns
            .get(&terminal)
            .is_some_and(|record| !record.used_by_other)
    }

    /// Remove `client` from `terminal`'s subscriber list (the
    /// `DETACH_RESOURCE` counterpart of the attach-time registration).
    /// Drops the entry when it empties.
    pub(super) fn unsubscribe(&mut self, client: ClientId, terminal: ResourceId) {
        if let Some(subs) = self.subscribers.get_mut(&terminal) {
            subs.retain(|c| *c != client);
            if subs.is_empty() {
                self.subscribers.remove(&terminal);
            }
        }
    }

    /// Drop `client` from every subscriber list, garbage-collecting the
    /// lists it emptied so the map doesn't grow unboundedly across
    /// attach/detach churn.
    pub(super) fn drop_client_subscriptions(&mut self, client: ClientId) {
        for subs in self.subscribers.values_mut() {
            subs.retain(|c| *c != client);
        }
        self.subscribers.retain(|_, subs| !subs.is_empty());
    }

    /// Clone the [`ResourceHandle`] of every resource `client` currently
    /// subscribes to (phux-0q8).
    #[must_use]
    pub(super) fn subscribed_handles(&self, client: ClientId) -> Vec<ResourceHandle> {
        self.subscribers
            .iter()
            .filter(|(_, subs)| subs.contains(&client))
            .filter_map(|(terminal, _)| self.handle(*terminal).cloned())
            .collect()
    }

    /// `true` when no pane has any subscriber left — the observable form of
    /// the empty-list GC invariant that [`Self::drop_client_subscriptions`]
    /// and [`Self::unsubscribe`] maintain.
    ///
    /// Test-only: production code asks about one pane
    /// ([`Self::subscribers_for`]), never about the map as a whole, and the
    /// GC invariant is otherwise unobservable from outside this type.
    #[cfg(test)]
    #[must_use]
    pub(super) fn subscriber_map_is_empty(&self) -> bool {
        self.subscribers.is_empty()
    }

    // -- output task lifetimes and ATTACH_RESOURCE generations --------

    pub(super) fn track_output_pump(
        &mut self,
        client: ClientId,
        terminal: ResourceId,
        abort: AbortHandle,
        done: CancellationToken,
    ) {
        let tasks = self.output_pumps.entry((client, terminal)).or_default();
        tasks.retain(|task| !task.done.is_cancelled());
        tasks.push(OutputPumpTask { abort, done });
    }

    /// Abort every output task for this subscription and return their exit
    /// fences. Generation bookkeeping remains available to replacement attach.
    pub(super) fn stop_output_pumps(
        &mut self,
        client: ClientId,
        terminal: ResourceId,
    ) -> Vec<CancellationToken> {
        self.output_pumps
            .remove(&(client, terminal))
            .unwrap_or_default()
            .into_iter()
            .map(|task| task.done.clone())
            .collect()
    }

    /// Install a new `ATTACH_RESOURCE` pump generation for `(client,
    /// terminal)`, displacing any live one.
    ///
    /// Returns the new generation's `(cancel, done, last_valid_seq)` plus, when
    /// one was displaced, the prior generation's bootstrap id, its `done` token
    /// and the sequence it reached. The caller awaits that `done` before
    /// tombstoning the old bootstrap id, so a late frame from the dying pump is
    /// never attributed to the new generation.
    ///
    /// This replaced a register-or-refuse form that returned `None` for a live
    /// pair. Under negotiated bootstrap a second `ATTACH_RESOURCE` is a
    /// meaningful request — re-bootstrap at a different profile, or recover —
    /// so refusing it would strand the client on the old stream.
    pub(super) fn replace_pump(
        &mut self,
        client: ClientId,
        terminal: ResourceId,
        bootstrap_id: BootstrapId,
    ) -> AttachResourcePumpReplacement {
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        let last_valid_seq: LastValidAttachSequence = Arc::new(AtomicU64::new(0));
        let prior = self
            .pumps
            .insert(
                (client, terminal),
                AttachResourceGeneration {
                    cancel: cancel.clone(),
                    done: done.clone(),
                    last_valid_seq: Arc::clone(&last_valid_seq),
                    bootstrap_id,
                },
            )
            .map(|prior| {
                prior.cancel.cancel();
                (prior.bootstrap_id, prior.done, prior.last_valid_seq)
            });
        (cancel, done, last_valid_seq, prior)
    }

    /// Allocate the next per-terminal bootstrap id for `client`.
    ///
    /// `None` once the per-client counter would overflow — the connection has
    /// exhausted its id space and must reconnect rather than reuse an id a
    /// tombstone still refers to.
    pub(super) fn next_bootstrap_id(&mut self, client: ClientId) -> Option<BootstrapId> {
        let next = self.next_bootstrap.entry(client).or_insert(1);
        let raw = *next;
        *next = raw.checked_add(1)?;
        BootstrapId::new(raw)
    }

    /// Cancel and forget the `ATTACH_RESOURCE` pump for `(client,
    /// terminal)`, if one is live. Idempotent.
    pub(super) fn cancel_pump(&mut self, client: ClientId, terminal: ResourceId) {
        self.stop_output_pumps(client, terminal);
        if let Some(generation) = self.pumps.remove(&(client, terminal)) {
            generation.cancel.cancel();
        }
    }

    /// Cancel every `ATTACH_RESOURCE` output pump `client` owns
    /// (phux-v45.7) so no task keeps streaming into a dead mailbox.
    pub(super) fn cancel_pumps_for_client(&mut self, client: ClientId) {
        self.output_pumps.retain(|(owner, _), _| *owner != client);
        self.pumps.retain(|(owner, _), generation| {
            if *owner == client {
                generation.cancel.cancel();
                false
            } else {
                true
            }
        });
        self.next_bootstrap.remove(&client);
    }

    // -- teardown -----------------------------------------------------

    /// Drop every entry in this table keyed on a now-removed resource.
    ///
    /// Cancels the actor token defensively (the actor has usually already
    /// exited by the time we reap, but a still-live token is cleanly
    /// resolved by the cancel) and cancels the pane's `ATTACH_RESOURCE`
    /// pumps: the broadcast channel is closing anyway, but the cancel keeps
    /// the token map bounded and the teardown prompt.
    ///
    /// The wire-id retirement and the per-resource metadata / agent-record
    /// cleanup that pair with this stay on
    /// [`super::ServerState::reap_terminal`] — they are keyed on the wire id
    /// this resource is about to give up.
    pub(super) fn forget_resource(&mut self, terminal: ResourceId) {
        self.output_pumps.retain(|(_, pane), _| *pane != terminal);
        self.handles.remove(&terminal);
        if let Some(token) = self.tokens.remove(&terminal) {
            token.cancel();
        }
        self.subscribers.remove(&terminal);
        self.spawns.remove(&terminal);
        self.pumps.retain(|(_, pane), generation| {
            if *pane == terminal {
                generation.cancel.cancel();
                false
            } else {
                true
            }
        });
    }
}
