//! Hub-side satellite lease ownership, acquire markers, and mirror ordering.

use super::{ClientId, Federation, Outbound, SatelliteHost, SatelliteLease, SatelliteTerminal, mpsc};

impl Federation {
    /// The hub consumer holding the satellite lease, if any.
    #[must_use]
    pub(crate) fn satellite_lease_holder(
        &self,
        host: &SatelliteHost,
        terminal: u32,
    ) -> Option<ClientId> {
        self.leases
            .get(&(host.clone(), terminal))
            .map(|lease| lease.holder)
    }

    /// Record `client` (and mailbox) as the satellite lease holder after the
    /// satellite acked, returning the evicted lease to notify when a
    /// different consumer held it.
    pub(crate) fn set_satellite_lease(
        &mut self,
        host: SatelliteHost,
        terminal: u32,
        client: ClientId,
        out_tx: mpsc::Sender<Outbound>,
    ) -> Option<SatelliteLease> {
        let prior = self.leases.insert(
            (host, terminal),
            SatelliteLease {
                holder: client,
                out_tx,
            },
        );
        prior.filter(|lease| lease.holder != client)
    }

    /// Release the satellite lease over `(host, terminal)` if `client` holds
    /// it; `true` when an entry was removed.
    pub(crate) fn release_satellite_lease(
        &mut self,
        host: &SatelliteHost,
        terminal: u32,
        client: ClientId,
    ) -> bool {
        let key = (host.clone(), terminal);
        if self.leases.get(&key).map(|lease| lease.holder) != Some(client) {
            return false;
        }
        self.leases.remove(&key);
        true
    }

    /// Every satellite lease `client` holds (for disconnect relays).
    #[must_use]
    pub(crate) fn satellite_leases_held_by(&self, client: ClientId) -> Vec<SatelliteTerminal> {
        self.leases
            .iter()
            .filter(|(_, lease)| lease.holder == client)
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Drop every satellite lease `client` holds (its detach, after the
    /// runtime relayed the releases).
    pub(in crate::state) fn release_satellite_leases_for(&mut self, client: ClientId) {
        self.leases.retain(|_, lease| lease.holder != client);
        if let Some(owner) = self.consumer_registrations.remove(&client) {
            owner.cancel();
        }
        self.acquire_pending
            .retain(|_, (_, owner)| *owner != client);
        self.proxy_attaches.retain(|(known, _, _)| *known != client);
    }

    /// Mark an `ACQUIRE_INPUT` relay in flight. Clear it on failure; on
    /// success the next mirrored event clears it.
    pub(crate) fn mark_satellite_lease_acquire_pending(
        &mut self,
        host: SatelliteHost,
        terminal: u32,
        client: ClientId,
    ) -> u64 {
        self.next_acquire += 1;
        self.acquire_pending
            .insert((host, terminal), (self.next_acquire, client));
        self.next_acquire
    }

    /// Clear the pending mark set by
    /// [`Self::mark_satellite_lease_acquire_pending`] after a failed relay.
    pub(crate) fn clear_satellite_lease_acquire_pending(
        &mut self,
        host: &SatelliteHost,
        terminal: u32,
        marker: u64,
    ) {
        let key = (host.clone(), terminal);
        if self
            .acquire_pending
            .get(&key)
            .is_some_and(|(current, _)| *current == marker)
        {
            self.acquire_pending.remove(&key);
        }
    }

    /// Forget the mirror-ordering bookkeeping for a satellite terminal that
    /// closed; no event for it can arrive again. Without this the maps keep
    /// one entry per satellite terminal that ever held a lease, for the
    /// hub's lifetime, and a satellite naming fresh ids grows them at will.
    pub(crate) fn forget_satellite_lease_terminal(&mut self, host: &SatelliteHost, terminal: u32) {
        let key = (host.clone(), terminal);
        self.last_mirrored_seq.remove(&key);
        self.acquire_pending.remove(&key);
        self.leases.remove(&key);
        self.proxy_attaches
            .retain(|(_, known, id)| known != host || *id != terminal);
    }

    /// Entries in the satellite lease-mirror ordering state.
    #[cfg(test)]
    pub(crate) fn satellite_lease_mirror_entries(&self) -> usize {
        self.last_mirrored_seq.len() + self.acquire_pending.len()
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
    pub(crate) fn mirror_satellite_lease_event(
        &mut self,
        host: &SatelliteHost,
        terminal: u32,
        is_end: bool,
        seq: Option<u64>,
    ) -> Option<ClientId> {
        let key = (host.clone(), terminal);
        let was_pending = self.acquire_pending.remove(&key).is_some();
        if was_pending && is_end {
            return None;
        }
        if !was_pending
            && is_end
            && let Some(seq) = seq
            && let Some(last) = self.last_mirrored_seq.get(&key)
            && seq <= *last
        {
            return None;
        }
        if !is_end {
            if let Some(seq) = seq {
                self.last_mirrored_seq.insert(key, seq);
            }
            return None;
        }
        self.leases.remove(&key).map(|lease| lease.holder)
    }
}
