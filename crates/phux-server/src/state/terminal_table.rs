//! The per-pane actor table: every map keyed on a live pane's identity,
//! plus the `JoinSet` that owns the pane actors' futures.
//!
//! Five maps that were flat fields on [`super::ServerState`] live here
//! because they share one lifetime: an entry appears when
//! `ServerState::register_terminal_handle` records a freshly-spawned actor
//! and disappears when the pane is reaped. Keeping them together makes the
//! "forget everything about this pane" and "forget everything about this
//! client" teardowns single calls instead of four open-coded map
//! operations spread across `state::reap` and `state::client`, which is
//! where they drifted out of step before.
//!
//! # Ownership boundary
//!
//! This type owns the *bookkeeping*, not the policy. Anything that needs a
//! second cluster stays on `ServerState` and calls in:
//!
//! * `register_terminal_handle` / `spawn_terminal_actor` mint a wire id
//!   from `state::id_space` in the same breath as the insert, so they stay
//!   on `ServerState` and hand the already-interned pieces down here.
//! * `reap_terminal`'s bookkeeping also retires wire ids and per-Terminal
//!   metadata; it calls [`TerminalTable::forget_terminal`] for the five
//!   maps and keeps the rest.
//! * `build_upgrade_blob` and `request_pane_handoff` are `async` and must
//!   not be — nothing on this type awaits, so the state lock can never be
//!   held across a suspension point through it.
//!
//! The struct and every method are `pub(super)`: the accessors the runtime
//! calls stay on `ServerState` (see `state::terminals`), so the crate's
//! public surface is unchanged and three of these five maps
//! (`tokens`, `tasks`, `pumps`) stay exactly as unreachable from outside
//! `state` as they were when they were private fields.

use std::collections::HashMap;
use std::future::Future;

use phux_core::ids::TerminalId;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct PumpState {
    token: CancellationToken,
    consumer_generation: u64,
    sequence: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

use super::ClientId;
use crate::terminal_actor::TerminalHandle;

/// Every pane-keyed table the server owns, plus the client subscriptions
/// and output pumps that hang off them.
///
/// Held as a single field on [`super::ServerState`]. Not thread-safe on
/// its own; the surrounding `Mutex<ServerState>` provides synchronization.
#[derive(Debug)]
pub(super) struct TerminalTable {
    /// Per-pane actor handles, keyed by core [`TerminalId`]. The
    /// `TerminalHandle` is `Send`; the underlying `TerminalActor` (which
    /// owns the `!Send` `Terminal`) lives on the `LocalSet` — see
    /// ADR-0014.
    ///
    /// Populated by [`super::ServerState::register_terminal_handle`] after
    /// the actor is spawned. Looked up by the ATTACH handler to request
    /// snapshots and by the input path to forward keystrokes.
    handles: HashMap<TerminalId, TerminalHandle>,
    /// Per-pane cancellation tokens. Cancelling a token fires the matching
    /// `TerminalActor`'s shutdown branch (see `TerminalActor::run`'s
    /// `select!`). Typically a child of the per-server root token, so a
    /// root cancel cascades to every pane in one step.
    ///
    /// Distinct from the prior `oneshot::Sender<()>` shutdown channel:
    /// dropping the token does NOT cancel — cancellation must be explicit
    /// (see `detach_actor`).
    tokens: HashMap<TerminalId, CancellationToken>,
    /// `JoinSet` collecting the `TerminalActor::run` futures spawned via
    /// [`super::ServerState::spawn_terminal_actor`]. Owned at this scope so
    /// cancellation of the per-server root token (or drop of
    /// `ServerState`) aborts every still-running pane actor in one go.
    ///
    /// **Drop-safety note:** `JoinSet<()>` is `Send`, but the futures it
    /// holds are `!Send` (pane actors own a `!Send` `Terminal` per
    /// ADR-0014). They were spawned via `JoinSet::spawn_local`, which is
    /// only legal inside a `LocalSet`. `ServerState` — and therefore this
    /// table — is dropped at the tail of
    /// `runtime::ServerRuntime::run_async` on the same thread that ran the
    /// `LocalSet`, so this `JoinSet`'s `Drop` is always on the spawning
    /// thread — no cross-thread poll of `!Send` futures occurs.
    tasks: JoinSet<()>,
    /// For each pane, the clients currently observing it (and thus
    /// eligible to receive `TERMINAL_OUTPUT` frames for it).
    ///
    /// Empty lists are garbage-collected rather than left behind, so the
    /// map stays bounded across attach/detach churn.
    subscribers: HashMap<TerminalId, Vec<ClientId>>,
    /// Active actor registration generation for each subscribed client/pane.
    /// Runtime teardown uses this to fence delayed detach messages.
    consumer_registrations: HashMap<(ClientId, TerminalId), u64>,
    /// Per-`(client, terminal)` cancellation for the active output emitter,
    /// whether installed by session `ATTACH` or `ATTACH_TERMINAL` (phux-v45.7).
    /// Reattach replaces one entry; client detach/disconnect cancels all of
    /// the client's entries; pane reap cancels the pane's entries.
    pumps: HashMap<(ClientId, TerminalId), PumpState>,
    resize_revisions: HashMap<TerminalId, u64>,
}

impl Default for TerminalTable {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalTable {
    /// Build an empty table.
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            handles: HashMap::new(),
            tokens: HashMap::new(),
            tasks: JoinSet::new(),
            subscribers: HashMap::new(),
            consumer_registrations: HashMap::new(),
            pumps: HashMap::new(),
            resize_revisions: HashMap::new(),
        }
    }

    // -- actor handles ------------------------------------------------

    /// Look up the [`TerminalHandle`] for `terminal`, if registered.
    #[must_use]
    pub(super) fn handle(&self, terminal: TerminalId) -> Option<&TerminalHandle> {
        self.handles.get(&terminal)
    }

    /// Every registered pane id. Cheap relative to [`Self::all_handles`]:
    /// no `TerminalHandle` clones.
    #[must_use]
    pub(super) fn terminal_ids(&self) -> Vec<TerminalId> {
        self.handles.keys().copied().collect()
    }

    /// Clone every registered `(pane, handle)` pair, so callers can talk to
    /// the actors *outside* the `ServerState` lock.
    #[must_use]
    pub(super) fn all_handles(&self) -> Vec<(TerminalId, TerminalHandle)> {
        self.handles
            .iter()
            .map(|(tid, handle)| (*tid, handle.clone()))
            .collect()
    }

    /// Record a freshly-spawned actor's `handle` and shutdown `token`
    /// against `terminal`.
    ///
    /// Overwrites any prior entry. The wire-id allocation that pairs with
    /// this deliberately stays on
    /// [`super::ServerState::register_terminal_handle`] — it belongs to the
    /// id space, not to this table.
    pub(super) fn register(
        &mut self,
        terminal: TerminalId,
        handle: TerminalHandle,
        token: CancellationToken,
    ) {
        self.handles.insert(terminal, handle);
        self.tokens.insert(terminal, token);
    }

    /// Spawn a pane actor's future onto the per-server `JoinSet`.
    ///
    /// Must be called from inside a `LocalSet` (ADR-0014): pane actors own
    /// `!Send` `Terminal`s, so this goes through `JoinSet::spawn_local`.
    /// See the drop-safety note on the `tasks` field for why holding those
    /// futures behind a `Send` `JoinSet` is sound.
    pub(super) fn spawn_actor<F>(&mut self, actor_future: F)
    where
        F: Future<Output = ()> + 'static,
    {
        self.tasks.spawn_local(actor_future);
    }

    /// Cancel `terminal`'s actor token, signalling the `TerminalActor` to
    /// exit, and forget the token. Idempotent.
    ///
    /// The actor task itself is drained from the per-server `JoinSet` when
    /// it returns from `run`; we don't need to touch `tasks` here.
    pub(super) fn detach_actor(&mut self, terminal: TerminalId) {
        if let Some(token) = self.tokens.remove(&terminal) {
            token.cancel();
        }
    }

    // -- subscriptions ------------------------------------------------

    /// Subscribers (snapshot) for `terminal`. Returns an empty slice if no
    /// clients are currently observing the pane.
    #[must_use]
    pub(super) fn subscribers_for(&self, terminal: TerminalId) -> &[ClientId] {
        self.subscribers.get(&terminal).map_or(&[], Vec::as_slice)
    }

    /// Subscribe `client` to `terminal`, deduplicating: a client already on
    /// the list is not pushed twice, so a re-attach cannot double-fan
    /// `TERMINAL_OUTPUT` at it.
    pub(super) fn subscribe(&mut self, client: ClientId, terminal: TerminalId) {
        let subs = self.subscribers.entry(terminal).or_default();
        if !subs.contains(&client) {
            subs.push(client);
        }
    }

    /// Remove `client` from `terminal`'s subscriber list (the
    /// `DETACH_TERMINAL` counterpart of the attach-time registration).
    /// Drops the entry when it empties.
    pub(super) fn unsubscribe(&mut self, client: ClientId, terminal: TerminalId) {
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

    pub(super) fn subscribed_consumer_handles(
        &self,
        client: ClientId,
    ) -> Vec<(TerminalHandle, u64)> {
        self.consumer_registrations
            .iter()
            .filter(|((owner, _), _)| *owner == client)
            .filter_map(|((_, terminal), generation)| {
                self.handle(*terminal)
                    .cloned()
                    .map(|handle| (handle, *generation))
            })
            .collect()
    }

    pub(super) fn record_consumer_registration(
        &mut self,
        client: ClientId,
        terminal: TerminalId,
        generation: u64,
    ) {
        self.consumer_registrations
            .insert((client, terminal), generation);
    }

    pub(super) fn take_consumer_registration(
        &mut self,
        client: ClientId,
        terminal: TerminalId,
    ) -> Option<u64> {
        self.consumer_registrations.remove(&(client, terminal))
    }

    pub(super) fn remove_consumer_registration_if(
        &mut self,
        client: ClientId,
        terminal: TerminalId,
        generation: u64,
    ) -> bool {
        if self.consumer_registrations.get(&(client, terminal)) != Some(&generation) {
            return false;
        }
        self.consumer_registrations.remove(&(client, terminal));
        true
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

    // -- ATTACH_TERMINAL output pumps ---------------------------------

    pub(super) fn register_staged_pump(
        &mut self,
        client: ClientId,
        terminal: TerminalId,
        consumer_generation: u64,
        token: CancellationToken,
        sequence: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) {
        if let Some(previous) = self.pumps.insert(
            (client, terminal),
            PumpState {
                token,
                consumer_generation,
                sequence,
            },
        ) {
            previous.token.cancel();
        }
    }

    pub(super) fn pump_sequence(
        &self,
        client: ClientId,
        terminal: TerminalId,
    ) -> std::sync::Arc<std::sync::atomic::AtomicU64> {
        self.pumps.get(&(client, terminal)).map_or_else(
            || std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            |pump| pump.sequence.clone(),
        )
    }

    pub(super) fn accept_resize_revision(&mut self, terminal: TerminalId, revision: u64) -> bool {
        let current = self.resize_revisions.entry(terminal).or_default();
        if revision <= *current {
            return false;
        }
        *current = revision;
        true
    }

    /// Cancel and forget the `ATTACH_TERMINAL` pump for `(client,
    /// terminal)`, if one is live. Idempotent.
    pub(super) fn cancel_pump(&mut self, client: ClientId, terminal: TerminalId) -> Option<u64> {
        if let Some(pump) = self.pumps.remove(&(client, terminal)) {
            pump.token.cancel();
            Some(pump.consumer_generation)
        } else {
            None
        }
    }

    pub(super) fn cancel_pump_if(
        &mut self,
        client: ClientId,
        terminal: TerminalId,
        generation: u64,
    ) {
        if self
            .pumps
            .get(&(client, terminal))
            .is_some_and(|pump| pump.consumer_generation == generation)
        {
            let _ = self.cancel_pump(client, terminal);
        }
    }

    /// Cancel every terminal output emitter `client` owns so no task keeps
    /// streaming into a dead mailbox.
    pub(super) fn cancel_pumps_for_client(&mut self, client: ClientId) {
        self.pumps.retain(|(owner, _), pump| {
            if *owner == client {
                pump.token.cancel();
                false
            } else {
                true
            }
        });
        self.consumer_registrations
            .retain(|(owner, _), _| *owner != client);
    }

    // -- teardown -----------------------------------------------------

    /// Drop every entry in this table keyed on a now-removed pane.
    ///
    /// Cancels the actor token defensively (the actor has usually already
    /// exited by the time we reap, but a still-live token is cleanly
    /// resolved by the cancel) and cancels the pane's `ATTACH_TERMINAL`
    /// pumps: the broadcast channel is closing anyway, but the cancel keeps
    /// the token map bounded and the teardown prompt.
    ///
    /// The wire-id retirement and the per-Terminal metadata / agent-record
    /// cleanup that pair with this stay on
    /// [`super::ServerState::reap_terminal`] — they are not pane-keyed, they
    /// are keyed on the wire id this pane is about to give up.
    pub(super) fn forget_terminal(&mut self, terminal: TerminalId) {
        self.handles.remove(&terminal);
        if let Some(token) = self.tokens.remove(&terminal) {
            token.cancel();
        }
        self.subscribers.remove(&terminal);
        self.consumer_registrations
            .retain(|(_, pane), _| *pane != terminal);
        self.resize_revisions.remove(&terminal);
        self.pumps.retain(|(_, pane), pump| {
            if *pane == terminal {
                pump.token.cancel();
                false
            } else {
                true
            }
        });
    }
}
