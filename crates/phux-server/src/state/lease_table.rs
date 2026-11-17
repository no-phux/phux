//! The local input-lease ledger (ADR-0033, "take the wheel"): who owns
//! keystroke delivery to a local pane. The hub-side satellite ledger lives
//! in [`super::federation`].
//!
//! Both clear together on the holder's detach
//! (`ServerState::release_all_input_leases`); a stranded entry leaves a pane
//! nobody can type into. The protocol around the ledger (relays, `Seized`
//! and `Released` notifications) lives in the runtime, behind `ServerState`
//! accessors. Everything here is `pub(super)` and sync.

use std::collections::HashMap;

use phux_core::ids::ResourceId;

use super::client::ClientId;

/// The local input-lease ledger.
#[derive(Debug, Default)]
pub(crate) struct LeaseTable {
    /// Per-pane lease: only the holder's input reaches the PTY (others are
    /// dropped but still acked). Absent means `Open`.
    input: HashMap<ResourceId, ClientId>,
    /// Per-pane lease epoch (ADR-0163 §7): the number of holder changes,
    /// so `0` until the first grant. Kept after the lease is released so
    /// epochs never repeat while the pane lives.
    epochs: HashMap<ResourceId, u64>,
    /// Armed TTL expiry per pane (ADR-0033 `ttl_ms`). A woken timer acts only
    /// if its generation is still on file; generations come from a counter
    /// that never resets, and superseding an entry aborts its timer, so at
    /// most one timer lives per lease.
    expiry: HashMap<ResourceId, ExpiryEntry>,
    /// Source of expiry generations; never reset, so each is unique.
    next_expiry_generation: u64,
    /// Live timer-task count, for the regression test that re-arms do not
    /// accumulate tasks.
    #[cfg(test)]
    expiry_task_counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
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

impl LeaseTable {
    // -- local pane leases ----------------------------------------------

    /// The client currently holding `terminal`'s input lease, or `None` when
    /// the pane is `Open`.
    #[must_use]
    pub(crate) fn holder(&self, terminal: ResourceId) -> Option<ClientId> {
        self.input.get(&terminal).copied()
    }

    /// Whether another client's lease blocks `client`'s input.
    #[must_use]
    pub(crate) fn blocked(&self, terminal: ResourceId, client: ClientId) -> bool {
        self.input
            .get(&terminal)
            .is_some_and(|holder| *holder != client)
    }

    /// Grant the lease to `client`, returning the prior holder. A change of
    /// holder opens a new epoch.
    pub(super) fn acquire(&mut self, terminal: ResourceId, client: ClientId) -> Option<ClientId> {
        let prior = self.input.insert(terminal, client);
        if prior != Some(client) {
            self.bump_epoch(terminal);
        }
        prior
    }

    /// Release the lease if `client` holds it; `true` if released, which
    /// opens a new epoch.
    pub(super) fn release(&mut self, terminal: ResourceId, client: ClientId) -> bool {
        if self.input.get(&terminal) == Some(&client) {
            self.input.remove(&terminal);
            self.bump_epoch(terminal);
            true
        } else {
            false
        }
    }

    /// The pane's current lease epoch; `0` before its first grant.
    #[must_use]
    pub(super) fn epoch(&self, terminal: ResourceId) -> u64 {
        self.epochs.get(&terminal).copied().unwrap_or(0)
    }

    /// Forget a pane that closed: no lease change can follow.
    pub(super) fn forget_resource(&mut self, terminal: ResourceId) {
        Self::abort(self.expiry.remove(&terminal));
        self.input.remove(&terminal);
        self.epochs.remove(&terminal);
    }

    fn bump_epoch(&mut self, terminal: ResourceId) {
        let epoch = self.epochs.entry(terminal).or_insert(0);
        *epoch = epoch.saturating_add(1);
    }

    /// Every pane whose input lease `client` currently holds (for disconnect
    /// broadcasts).
    #[must_use]
    pub(crate) fn held_by(&self, client: ClientId) -> Vec<ResourceId> {
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
    pub(crate) fn expiry_is_current(&self, terminal: ResourceId, generation: u64) -> bool {
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

    /// A clone of the live TTL-timer-task counter.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn expiry_task_counter(&self) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
        std::sync::Arc::clone(&self.expiry_task_counter)
    }

    /// Abort `entry`'s timer task, if it had one attached yet.
    fn abort(entry: Option<ExpiryEntry>) {
        if let Some(abort) = entry.and_then(|e| e.abort) {
            abort.abort();
        }
    }

    // -- disconnect teardown --------------------------------------------

    /// Drop every local lease `client` holds (called from `detach` after the
    /// runtime has broadcast the releases). Returns the panes released, each
    /// now in a new epoch.
    pub(super) fn release_all_for(&mut self, client: ClientId) -> Vec<ResourceId> {
        let held = self.held_by(client);
        for pane in &held {
            Self::abort(self.expiry.remove(pane));
            self.input.remove(pane);
            self.bump_epoch(*pane);
        }
        held
    }
}
