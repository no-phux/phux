//! The input-lease ledgers (ADR-0033, "take the wheel"): who currently
//! owns keystroke delivery to a local pane, and — on a federation hub —
//! which hub consumer owns it over a satellite pane (phux-v45.7).
//!
//! Two maps that were flat on [`super::ServerState`] live here because
//! they share one lifetime — a *holder's* connection. An entry appears
//! when a client's `ACQUIRE_INPUT` is granted and both disappear together
//! in `ServerState::detach`, which is exactly the invariant that must not
//! drift: a disconnect that strands either ledger leaves a pane nobody can
//! type into. That teardown is now one call
//! ([`LeaseTable::release_all_for`]) instead of two open-coded `retain`s in
//! `state::client`.
//!
//! # Ownership boundary
//!
//! This type owns the *ledgers*, not the protocol around them. Everything
//! that decides whether a lease may change hands — the relay round trip to
//! the satellite, the `TerminalControl(Seized)` notification to an evicted
//! holder, the `Released` broadcast at disconnect — stays in
//! `runtime::commands` / `runtime::client` and calls in through the
//! delegating accessors on `ServerState` (see `state::leases`).
//!
//! Both fields are private: unlike `state::config` and
//! `state::client_table`, nothing on `ServerState` needs to borrow-split a
//! lease map against another field, so every read and write goes through a
//! method here.
//!
//! Nothing here is `async` and nothing awaits, so the state lock can never
//! be held across a suspension point through this type.
//!
//! The struct and every method are `pub(super)`: the accessors the runtime
//! calls stay on `ServerState`, so the crate's public surface is unchanged
//! and both maps stay exactly as unreachable from outside `state` as they
//! were as private fields.

use std::collections::{BTreeMap, HashMap, HashSet};

use phux_core::ids::ResourceId;
use tokio::sync::mpsc;

use super::client::ClientId;
use crate::mailbox::Outbound;

/// One hub-side satellite input lease (phux-v45.7, phux-v45.13).
///
/// Records which hub consumer holds the relayed ADR-0033 lease over a
/// satellite terminal **and** that consumer's outbound mailbox. The
/// mailbox is what lets a SEIZE takeover by a *different* hub consumer
/// notify the evicted prior holder directly (a hub-synthesized
/// `TerminalControl(Seized)` event, mirroring the local takeover
/// broadcast) — the satellite cannot do it, because every hub consumer
/// reaches it through the link's single client identity, so its own lease
/// change reads as a same-identity re-acquire.
#[derive(Debug, Clone)]
pub(crate) struct SatelliteLease {
    /// The hub consumer that holds the lease.
    pub(crate) holder: ClientId,
    /// The holder's outbound mailbox, for the eviction notification.
    pub(crate) out_tx: mpsc::Sender<Outbound>,
}

/// Both input-lease ledgers the server owns: the local per-pane leases and
/// the hub-side per-satellite-pane leases.
///
/// Held as a single field on [`super::ServerState`]. Not thread-safe on
/// its own; the surrounding `Mutex<ServerState>` provides synchronization.
#[derive(Debug)]
pub(super) struct LeaseTable {
    /// Per-pane input lease (ADR-0033). When a pane has an entry, only that
    /// `ClientId`'s input reaches the PTY; everyone else's `INPUT_*` /
    /// `ROUTE_INPUT` is dropped at the gate (still acked, per the
    /// fire-and-forget input invariant). Absent = `Open`: any subscriber's
    /// input passes (the back-compat default). Released automatically when
    /// the holder detaches or its connection drops.
    input: HashMap<ResourceId, ClientId>,
    /// Hub-side ledger of which **hub consumer** owns the input lease over
    /// a satellite terminal (phux-v45.7). All hub consumers share the
    /// link's single client identity on the satellite, so the satellite's
    /// own lease map cannot tell them apart: without this ledger, consumer
    /// A's `ACQUIRE_INPUT` over a satellite terminal would not exclude
    /// consumer B's relayed input, and B's `RELEASE_INPUT` would release
    /// A's lease. The hub therefore gates relayed `ACQUIRE_INPUT` /
    /// `RELEASE_INPUT` / `ROUTE_INPUT` / `INPUT_*` on this map *before*
    /// forwarding, and the satellite-side lease (held by the link
    /// identity) keeps excluding the satellite's own local clients.
    /// Entries are keyed `(host, satellite-local id)` and cleared when the
    /// holder detaches (with a detached `RELEASE_INPUT` relayed so the
    /// satellite-side lease follows). Each entry carries the holder's
    /// outbound mailbox so a SEIZE takeover by another hub consumer can
    /// notify the evicted prior holder directly (phux-v45.13) — the
    /// satellite cannot, since it sees only the shared link identity. See
    /// L1 §9.1.
    satellite: BTreeMap<(phux_protocol::ids::SatelliteHost, u32), SatelliteLease>,
    /// Successful satellite `ATTACH_RESOURCE` proxy ownership, mirrored at the
    /// hub authority boundary.
    ///
    /// The hub relays opaque reply and input frames on behalf of a consumer,
    /// and cannot re-derive from the frame alone whether that consumer is
    /// entitled to the satellite terminal it names. A frame must match one of
    /// these exact `(client, host, terminal)` registrations before the hub
    /// forwards it, so a consumer cannot address a satellite pane it never
    /// attached to.
    satellite_proxy_attaches: HashSet<(ClientId, phux_protocol::ids::SatelliteHost, u32)>,
    /// TTL bookkeeping for a pane's input lease (ADR-0033's `ttl_ms`, this
    /// lane): the currently-armed expiry, if any, keyed by pane.
    ///
    /// A timer captures its [`ExpiryEntry::generation`] at spawn time and,
    /// on waking, re-reads this map — it only expires the lease when its
    /// generation is still the one recorded here. Review found the first
    /// cut's generations broken two ways, both fixed by this shape:
    /// * The value used to be derived from `self.expiry_generation.get(&
    ///   terminal)`, which a clear (release, a `ttl_ms = 0` re-acquire,
    ///   disconnect) always resets to empty — so a released-then-reacquired
    ///   pane restarted at generation `1` every time, and a still-sleeping
    ///   dead timer from the *first* lease would spuriously match the
    ///   *second* one's generation. Generations now come from
    ///   [`Self::next_expiry_generation`], a counter that is never reset,
    ///   so no two grants for a pane can ever share one.
    /// * A superseded timer used to keep sleeping until its own deadline
    ///   purely to find itself stale (the "cancel" was passive). Repeated
    ///   re-arms — a Cooperative re-acquire, or a hub consumer relaying
    ///   `ttl_ms` to a satellite — would accumulate one live task per
    ///   grant. Every acquire/release/disconnect that supersedes an entry
    ///   now also aborts its [`ExpiryEntry::abort`] handle, so at most one
    ///   timer task is ever alive for a pane's lease.
    expiry: HashMap<ResourceId, ExpiryEntry>,
    /// Source of every [`ExpiryEntry::generation`] value: incremented, never
    /// reset, so a generation is unique across this table's entire
    /// lifetime, not just relative to the current map entry.
    next_expiry_generation: u64,
    /// Live-timer-task counter for the regression test in
    /// `runtime::commands` proving re-arms do not accumulate sleeping
    /// tasks. Production code never reads it; it costs one extra
    /// `Arc<AtomicUsize>` clone per armed timer even outside tests, which
    /// is cheap enough not to bother `cfg`-gating away.
    expiry_task_counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// `(host, terminal)`s with an `ACQUIRE_INPUT` relay currently in
    /// flight (review round 2, medium finding). See
    /// [`Self::mirror_satellite_lease_ended`].
    satellite_acquire_pending: HashSet<(phux_protocol::ids::SatelliteHost, u32)>,
    /// The satellite's own stamped `seq` of the last Acquired/Seized
    /// mirrored per `(host, terminal)`, when the link carries stamps.
    satellite_last_mirrored_seq: HashMap<(phux_protocol::ids::SatelliteHost, u32), u64>,
}

/// One pane's armed TTL timer: the generation it was spawned under, and —
/// once the spawning caller has it — the handle to kill it outright. See
/// the [`LeaseTable::expiry`] field doc for why both exist.
#[derive(Debug)]
struct ExpiryEntry {
    generation: u64,
    /// `None` for the brief window between [`LeaseTable::refresh_expiry`]
    /// minting `generation` and the caller finishing `spawn_local` and
    /// calling [`LeaseTable::attach_expiry_abort`] with the handle. A
    /// refresh landing in that window still bumps past this generation, so
    /// `attach_expiry_abort` finds itself stale and the caller aborts the
    /// handle on arrival instead of storing it — the task never has a
    /// window where it is both live and unreachable for cancellation.
    abort: Option<tokio::task::AbortHandle>,
}

impl Default for LeaseTable {
    fn default() -> Self {
        Self::new()
    }
}

impl LeaseTable {
    /// Build an empty ledger pair — every pane starts `Open`.
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            input: HashMap::new(),
            satellite: BTreeMap::new(),
            satellite_proxy_attaches: HashSet::new(),
            expiry: HashMap::new(),
            next_expiry_generation: 0,
            expiry_task_counter: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            satellite_acquire_pending: HashSet::new(),
            satellite_last_mirrored_seq: HashMap::new(),
        }
    }

    // -- satellite proxy attach registrations -----------------------------

    /// Whether `client` holds a proxied `ATTACH_RESOURCE` over `terminal` on
    /// `host`. The gate every relayed reply/input frame passes.
    pub(super) fn has_satellite_proxy_attach(
        &self,
        client: ClientId,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) -> bool {
        self.satellite_proxy_attaches
            .contains(&(client, host.clone(), terminal))
    }

    /// Record that `client` now proxies `terminal` on `host`.
    pub(super) fn register_satellite_proxy_attach(
        &mut self,
        client: ClientId,
        host: phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) {
        self.satellite_proxy_attaches
            .insert((client, host, terminal));
    }

    /// Drop one proxy registration. Idempotent.
    pub(super) fn unregister_satellite_proxy_attach(
        &mut self,
        client: ClientId,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) {
        self.satellite_proxy_attaches
            .remove(&(client, host.clone(), terminal));
    }

    // -- local pane leases ----------------------------------------------

    /// The client currently holding `terminal`'s input lease (ADR-0033), or
    /// `None` if the pane is `Open`.
    #[must_use]
    pub(super) fn holder(&self, terminal: ResourceId) -> Option<ClientId> {
        self.input.get(&terminal).copied()
    }

    /// Whether `client`'s input to `terminal` is blocked by another
    /// client's lease. `false` when the pane is `Open` or `client` is the
    /// holder.
    #[must_use]
    pub(super) fn blocked(&self, terminal: ResourceId, client: ClientId) -> bool {
        self.input
            .get(&terminal)
            .is_some_and(|holder| *holder != client)
    }

    /// Grant `terminal`'s input lease to `client`, returning the prior
    /// holder if the lease was already held (a `Seize` preemption).
    pub(super) fn acquire(&mut self, terminal: ResourceId, client: ClientId) -> Option<ClientId> {
        self.input.insert(terminal, client)
    }

    /// Release `terminal`'s input lease if `client` holds it. Returns
    /// `true` if a lease was actually released. A no-op (returns `false`)
    /// if the pane is `Open` or held by someone else.
    pub(super) fn release(&mut self, terminal: ResourceId, client: ClientId) -> bool {
        if self.input.get(&terminal) == Some(&client) {
            self.input.remove(&terminal);
            true
        } else {
            false
        }
    }

    /// Every pane whose input lease `client` currently holds.
    #[must_use]
    pub(super) fn held_by(&self, client: ClientId) -> Vec<ResourceId> {
        self.input
            .iter()
            .filter_map(|(pane, holder)| (*holder == client).then_some(*pane))
            .collect()
    }

    // -- lease TTL (ADR-0033's `ttl_ms`, this lane) ----------------------

    /// Invalidate `terminal`'s current expiry entry — aborting its timer
    /// task, if one was already attached — and, when `scheduled` is true,
    /// record a fresh, never-before-used generation for the caller to arm
    /// a new timer under. Called on every grant (with `scheduled = ttl_ms
    /// != 0`) and every release/disconnect (`scheduled = false`), so it is
    /// always the single place a pane's TTL tracking changes, and the
    /// single place a stale timer is torn down rather than left to sleep
    /// out its own deadline.
    pub(super) fn refresh_expiry(&mut self, terminal: ResourceId, scheduled: bool) -> Option<u64> {
        Self::abort(self.expiry.remove(&terminal));
        if !scheduled {
            return None;
        }
        // Never derived from the map entry (that is the bug review found:
        // a clear always resets it to empty, so a released-then-reacquired
        // pane could reuse a generation a still-sleeping dead timer had
        // already captured).
        let generation = self.next_expiry_generation.checked_add(1)?;
        self.next_expiry_generation = generation;
        self.expiry.insert(
            terminal,
            ExpiryEntry {
                generation,
                abort: None,
            },
        );
        Some(generation)
    }

    /// Record the just-spawned timer's abort handle for `terminal`,
    /// provided `generation` is still the one on file — nothing
    /// superseded it while the caller was between minting the generation
    /// and finishing `spawn_local`. Returns `false` when it is not, so the
    /// caller aborts the handle itself instead of leaking a task that is
    /// already dead by the generation check.
    #[must_use]
    pub(super) fn attach_expiry_abort(
        &mut self,
        terminal: ResourceId,
        generation: u64,
        abort: tokio::task::AbortHandle,
    ) -> bool {
        match self.expiry.get_mut(&terminal) {
            Some(entry) if entry.generation == generation => {
                entry.abort = Some(abort);
                true
            }
            _ => false,
        }
    }

    /// Whether `generation` is still the one on file for `terminal` — the
    /// check a woken expiry timer makes before acting.
    #[must_use]
    pub(super) fn expiry_is_current(&self, terminal: ResourceId, generation: u64) -> bool {
        self.expiry
            .get(&terminal)
            .is_some_and(|e| e.generation == generation)
    }

    /// Remove `terminal`'s expiry entry without aborting its timer task —
    /// for the timer's *own* legitimate firing, which is already past its
    /// sleep and about to `.await` sending the `Expired` broadcast.
    /// Aborting itself that late is unsafe: tokio drops an aborted task's
    /// future the next time a poll of it would return `Pending`, so a
    /// self-abort racing a momentarily-full mailbox could truncate the
    /// send it is in the middle of. External supersession (a new acquire,
    /// an explicit release, disconnect teardown) is a *different* task
    /// being torn down from the outside — that path stays on
    /// [`Self::refresh_expiry`], which does abort.
    pub(super) fn clear_expiry(&mut self, terminal: ResourceId) {
        self.expiry.remove(&terminal);
    }

    /// A clone of the live-timer-task counter (see the field doc);
    /// `runtime::commands` increments it when it spawns a timer and
    /// decrements it — via a drop guard — when that task ends, aborted or
    /// not.
    #[must_use]
    pub(super) fn expiry_task_counter(&self) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
        std::sync::Arc::clone(&self.expiry_task_counter)
    }

    /// Abort `entry`'s timer task, if it had one attached yet.
    fn abort(entry: Option<ExpiryEntry>) {
        if let Some(abort) = entry.and_then(|e| e.abort) {
            abort.abort();
        }
    }

    // -- hub-side satellite leases (phux-v45.7) -------------------------

    /// The hub consumer currently holding the input lease over satellite
    /// terminal `(host, terminal)` (L1 §9.1), or `None` when free.
    #[must_use]
    pub(super) fn satellite_holder(
        &self,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) -> Option<ClientId> {
        self.satellite
            .get(&(host.clone(), terminal))
            .map(|lease| lease.holder)
    }

    /// Record `client` (with its outbound mailbox `out_tx`) as the
    /// hub-side holder of the satellite lease.
    ///
    /// Returns the **evicted** prior lease when this acquire preempted a
    /// *different* hub consumer (a SEIZE takeover, phux-v45.13). A
    /// re-acquire by the same holder (idempotent cooperative acquire) or a
    /// grant over a free lease returns `None` — nobody was evicted.
    pub(super) fn set_satellite(
        &mut self,
        host: phux_protocol::ids::SatelliteHost,
        terminal: u32,
        client: ClientId,
        out_tx: mpsc::Sender<Outbound>,
    ) -> Option<SatelliteLease> {
        let prior = self.satellite.insert(
            (host, terminal),
            SatelliteLease {
                holder: client,
                out_tx,
            },
        );
        prior.filter(|lease| lease.holder != client)
    }

    /// Release the hub-side satellite lease over `(host, terminal)` if
    /// `client` holds it. Returns `true` when an entry was removed.
    pub(super) fn release_satellite(
        &mut self,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
        client: ClientId,
    ) -> bool {
        let key = (host.clone(), terminal);
        if self.satellite.get(&key).map(|lease| lease.holder) == Some(client) {
            self.satellite.remove(&key);
            true
        } else {
            false
        }
    }

    /// Every satellite lease `client` currently holds.
    #[must_use]
    pub(super) fn satellite_held_by(
        &self,
        client: ClientId,
    ) -> Vec<(phux_protocol::ids::SatelliteHost, u32)> {
        self.satellite
            .iter()
            .filter(|(_, lease)| lease.holder == client)
            .map(|(key, _)| key.clone())
            .collect()
    }

    // -- disconnect teardown --------------------------------------------

    /// Drop every lease `client` holds, local and hub-side, in one step.
    ///
    /// Called from [`super::ServerState::detach`] so a disconnect never
    /// strands the wheel. The runtime broadcasts the `Released` events (via
    /// [`super::ServerState::leases_held_by`]) and relays the detached
    /// `RELEASE_INPUT` per satellite entry (via
    /// [`super::ServerState::satellite_leases_held_by`]) *before* calling
    /// detach; this clears both ledgers regardless of those paths running.
    pub(super) fn release_all_for(&mut self, client: ClientId) {
        for pane in self.held_by(client) {
            Self::abort(self.expiry.remove(&pane));
        }
        self.input.retain(|_, holder| *holder != client);
        self.satellite.retain(|_, lease| lease.holder != client);
    }

    /// Unconditionally clear the hub-side satellite lease over `(host,
    /// terminal)`, regardless of which hub consumer the ledger currently
    /// names.
    ///
    /// Unlike [`Self::release_satellite`], this does not check who holds
    /// it: it is how a `Released`/`Expired` `terminal_control` arriving
    /// *from* the satellite (the source of truth for that lease) mirrors
    /// into the hub's own ledger, so the hub stops gating other hub
    /// consumers against a holder the satellite has already dropped.
    /// Returns the evicted holder, if any.
    pub(super) fn clear_satellite(
        &mut self,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) -> Option<ClientId> {
        self.satellite
            .remove(&(host.clone(), terminal))
            .map(|lease| lease.holder)
    }

    // -- satellite lease mirror ordering (review round 2, medium finding) -

    /// Mark `(host, terminal)`'s lease pending: an `ACQUIRE_INPUT` relay is
    /// in flight for it. Call before awaiting the relay's reply.
    ///
    /// On a *failed* relay, clear this immediately (nothing was installed,
    /// nothing to wait for). On success, leave it set: unlike the reply,
    /// which bypasses the event pump and can resolve well before the
    /// stream catches up, the mirror in
    /// [`Self::mirror_satellite_lease_event`] is what actually clears it —
    /// on the *first* event it observes for this terminal afterward,
    /// whatever that event is. See that method's doc for why.
    pub(super) fn mark_satellite_acquire_pending(
        &mut self,
        host: phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) {
        self.satellite_acquire_pending.insert((host, terminal));
    }

    /// Clear the pending mark without waiting for an event — for a
    /// *failed* relay only (nothing was installed, so there is nothing
    /// for the event stream to confirm). A successful relay leaves the
    /// mark for [`Self::mirror_satellite_lease_event`] to clear instead.
    pub(super) fn clear_satellite_acquire_pending(
        &mut self,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) {
        self.satellite_acquire_pending
            .remove(&(host.clone(), terminal));
    }

    /// Mirror one satellite-originated `terminal_control` transition for
    /// `(host, terminal)` into the hub's own ledger (ADR-0033's `ttl_ms`,
    /// this lane: "the satellite owns the timer, the hub reflects the
    /// satellite's `Expired`/`Released`"). `is_end` is `true` for
    /// Released/Expired, `false` for Acquired/Seized (the only two actions
    /// the caller routes here). Returns the evicted holder when a
    /// Released/Expired was actually applied.
    ///
    /// Two defenses against the ledger going stale (review round 2,
    /// medium finding — reply and event delivery share one link but are
    /// not ordered against each other, so a hub consumer's `ACQUIRE_INPUT`
    /// reply — which bypasses the event pump and installs the new holder
    /// directly, see `runtime::commands::relay_satellite_acquire_input` —
    /// can resolve before an *earlier* Released/Expired for the *prior*
    /// holder has finished arriving):
    ///
    /// * **Pending.** While `(host, terminal)` is marked pending (an
    ///   acquire's reply already resolved, but no event has confirmed it
    ///   yet — see [`Self::mark_satellite_acquire_pending`]), the mark is
    ///   consumed by the *first* event observed for this terminal. If that
    ///   event is a Released/Expired, it is swallowed rather than applied:
    ///   it can only be a stale report the reply already superseded, since
    ///   nothing legitimately ends a lease the hub itself just installed
    ///   before the event stream has said anything about it at all. If it
    ///   is instead the confirming Acquired/Seized, the mark is cleared
    ///   and `seq` recorded normally — the stream has caught up.
    /// * **Seq.** Once nothing is pending, a Released/Expired is applied
    ///   only when its `seq` (when both it and the last mirrored
    ///   Acquired/Seized carry one) is strictly newer than the one on
    ///   file — catching a stale or duplicate event the pending window
    ///   did not, e.g. one replayed after a reconnect.
    ///
    /// This is self-healing rather than provably complete: a pending mark
    /// with no confirming event ever arriving (a lost/gapped event) would
    /// let one *later, genuine* Released/Expired be wrongly swallowed too
    /// — but only once, since swallowing also clears the mark. Formally
    /// closing that gap needs the reply itself to carry ordering evidence
    /// the wire does not provide today; this bounds the exposure to "one
    /// stray no-op" instead of "the ledger can be evicted from under a
    /// live holder," which is the finding this fixes.
    pub(super) fn mirror_satellite_lease_event(
        &mut self,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
        is_end: bool,
        seq: Option<u64>,
    ) -> Option<ClientId> {
        let key = (host.clone(), terminal);
        let was_pending = self.satellite_acquire_pending.remove(&key);
        if was_pending && is_end {
            return None;
        }
        if !was_pending
            && is_end
            && let Some(seq) = seq
            && let Some(last) = self.satellite_last_mirrored_seq.get(&key)
            && seq <= *last
        {
            return None;
        }
        if !is_end {
            if let Some(seq) = seq {
                self.satellite_last_mirrored_seq.insert(key, seq);
            }
            return None;
        }
        self.clear_satellite(host, terminal)
    }
}
