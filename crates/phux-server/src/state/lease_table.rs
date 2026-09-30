//! The input-lease ledgers (ADR-0033, "take the wheel"): who owns keystroke
//! delivery to a local pane, and on a hub, which consumer owns it over a
//! satellite pane.
//!
//! Both clear together on the holder's detach
//! ([`LeaseTable::release_all_for`]); a stranded entry leaves a pane nobody
//! can type into. The protocol around the ledgers (relays, `Seized` and
//! `Released` notifications) lives in the runtime, behind `ServerState`
//! accessors. Everything here is `pub(super)` and sync.

use std::collections::{BTreeMap, HashMap, HashSet};

use phux_core::ids::ResourceId;
use tokio::sync::mpsc;

use super::client::ClientId;
use crate::mailbox::Outbound;

/// One hub-side satellite input lease: the holding hub consumer and its
/// mailbox, so a SEIZE by another consumer can notify the evicted holder
/// (the satellite sees only the link's single identity and cannot).
#[derive(Debug, Clone)]
pub(crate) struct SatelliteLease {
    /// The hub consumer that holds the lease.
    pub(crate) holder: ClientId,
    /// The holder's outbound mailbox, for the eviction notification.
    pub(crate) out_tx: mpsc::Sender<Outbound>,
}

/// The local and hub-side input-lease ledgers.
#[derive(Debug)]
pub(super) struct LeaseTable {
    /// Per-pane lease: only the holder's input reaches the PTY (others are
    /// dropped but still acked). Absent means `Open`.
    input: HashMap<ResourceId, ClientId>,
    /// Hub-side lease per satellite terminal (L1 §9.1). All hub consumers
    /// share one identity on the satellite, so the hub gates relayed input
    /// and lease commands here before forwarding. Carries the holder's
    /// mailbox for SEIZE notifications.
    satellite: BTreeMap<(phux_protocol::ids::SatelliteHost, u32), SatelliteLease>,
    /// Proxied satellite `ATTACH_RESOURCE`s. A relayed frame must match an
    /// exact `(client, host, terminal)` entry, so a consumer cannot address
    /// a satellite pane it never attached.
    satellite_proxy_attaches: HashSet<(ClientId, phux_protocol::ids::SatelliteHost, u32)>,
    /// Armed TTL expiry per pane (ADR-0033 `ttl_ms`). A woken timer acts only
    /// if its generation is still on file; generations come from a counter
    /// that never resets, and superseding an entry aborts its timer, so at
    /// most one timer lives per lease.
    expiry: HashMap<ResourceId, ExpiryEntry>,
    /// Source of expiry generations; never reset, so each is unique.
    next_expiry_generation: u64,
    /// Live timer-task count, for the regression test that re-arms do not
    /// accumulate tasks (cheap enough to keep ungated).
    expiry_task_counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Satellite terminals with an `ACQUIRE_INPUT` relay in flight; see
    /// [`Self::mirror_satellite_lease_event`].
    satellite_acquire_pending: HashSet<(phux_protocol::ids::SatelliteHost, u32)>,
    /// The satellite's `seq` of the last mirrored Acquired/Seized per
    /// terminal, when the link carries stamps.
    satellite_last_mirrored_seq: HashMap<(phux_protocol::ids::SatelliteHost, u32), u64>,
}

/// One pane's armed TTL timer: its generation and, once spawned, its abort
/// handle.
#[derive(Debug)]
struct ExpiryEntry {
    generation: u64,
    /// `None` until the caller spawns the timer. A refresh in that window
    /// makes `attach_expiry_abort` report stale, and the caller aborts the
    /// handle itself.
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

    /// Whether `client` holds a proxied attach over `(host, terminal)`.
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

    /// The lease holder for `terminal`, or `None` when `Open`.
    #[must_use]
    pub(super) fn holder(&self, terminal: ResourceId) -> Option<ClientId> {
        self.input.get(&terminal).copied()
    }

    /// Whether another client's lease blocks `client`'s input.
    #[must_use]
    pub(super) fn blocked(&self, terminal: ResourceId, client: ClientId) -> bool {
        self.input
            .get(&terminal)
            .is_some_and(|holder| *holder != client)
    }

    /// Grant the lease to `client`, returning a preempted prior holder.
    pub(super) fn acquire(&mut self, terminal: ResourceId, client: ClientId) -> Option<ClientId> {
        self.input.insert(terminal, client)
    }

    /// Release the lease if `client` holds it; `true` if released.
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

    /// Invalidate `terminal`'s expiry entry (aborting its timer) and, when
    /// `scheduled`, mint a fresh generation for a new timer. The single
    /// place TTL tracking changes.
    pub(super) fn refresh_expiry(&mut self, terminal: ResourceId, scheduled: bool) -> Option<u64> {
        Self::abort(self.expiry.remove(&terminal));
        if !scheduled {
            return None;
        }
        // From the never-reset counter, so no two grants share a generation.
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

    /// Store the spawned timer's abort handle if `generation` is still
    /// current; `false` tells the caller to abort the handle itself.
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

    /// Whether `generation` is still current (checked by a woken timer).
    #[must_use]
    pub(super) fn expiry_is_current(&self, terminal: ResourceId, generation: u64) -> bool {
        self.expiry
            .get(&terminal)
            .is_some_and(|e| e.generation == generation)
    }

    /// Remove the expiry entry without aborting: for the timer's own firing,
    /// which a self-abort could truncate mid-send. External supersession
    /// uses [`Self::refresh_expiry`].
    pub(super) fn clear_expiry(&mut self, terminal: ResourceId) {
        self.expiry.remove(&terminal);
    }

    /// A clone of the live-timer-task counter.
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

    /// The hub consumer holding the satellite lease (L1 §9.1), if any.
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

    /// Record `client` (and mailbox) as the satellite lease holder,
    /// returning the evicted lease when a different consumer held it.
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

    /// Release the satellite lease if `client` holds it; `true` if removed.
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

    /// Drop every lease `client` holds, local and hub-side (called from
    /// `detach` after the runtime has broadcast and relayed the releases).
    pub(super) fn release_all_for(&mut self, client: ClientId) {
        for pane in self.held_by(client) {
            Self::abort(self.expiry.remove(&pane));
        }
        self.input.retain(|_, holder| *holder != client);
        self.satellite.retain(|_, lease| lease.holder != client);
    }

    /// Clear the satellite lease whoever holds it, mirroring a satellite's
    /// `Released`/`Expired`. Returns the evicted holder.
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

    /// Mark an `ACQUIRE_INPUT` relay in flight. Clear it on failure; on
    /// success the next mirrored event clears it.
    pub(super) fn mark_satellite_acquire_pending(
        &mut self,
        host: phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) {
        self.satellite_acquire_pending.insert((host, terminal));
    }

    /// Clear the pending mark after a failed relay.
    pub(super) fn clear_satellite_acquire_pending(
        &mut self,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) {
        self.satellite_acquire_pending
            .remove(&(host.clone(), terminal));
    }

    /// Forget the mirror-ordering bookkeeping for a satellite terminal that
    /// closed; no event for it can arrive again. Without this the map keeps
    /// one entry per satellite terminal that ever held a lease, for the
    /// hub's lifetime, and a satellite naming fresh ids grows it at will.
    pub(super) fn forget_satellite_terminal(
        &mut self,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) {
        let key = (host.clone(), terminal);
        self.satellite_last_mirrored_seq.remove(&key);
        self.satellite_acquire_pending.remove(&key);
    }

    /// Entries in the mirror-ordering bookkeeping.
    #[cfg(test)]
    pub(super) fn satellite_mirror_entries(&self) -> usize {
        self.satellite_last_mirrored_seq.len() + self.satellite_acquire_pending.len()
    }

    /// Mirror a satellite `terminal_control` into the hub ledger (the
    /// satellite owns the timer). `is_end` is true for Released/Expired.
    /// Returns the evicted holder when an end was applied.
    ///
    /// The acquire reply can beat an earlier Released/Expired over the link:
    ///
    /// * **Pending**: the first event after a relayed acquire consumes the
    ///   mark; an end arriving then is stale and swallowed.
    /// * **Seq**: otherwise an end applies only if its `seq` is newer than
    ///   the last mirrored Acquired/Seized.
    ///
    /// A lost confirming event could let one genuine end be swallowed, but
    /// only once.
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
