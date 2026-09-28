//! Which hub consumer asked for each satellite resource this hub spawned
//! (ADR-0109, `docs/spec/L1.md` §9.1).
//!
//! A satellite sees all hub consumers as one connection, so it cannot tell
//! the spawner's use from another consumer's. This hub-side ledger records
//! the spawning consumer and the instance token the resource came back
//! bound to, and notes other consumers' use; a conditional kill with
//! `UNATTACHED_SINCE_SPAWN` is relayed only when it vouches under the
//! kill's own token. Bounded (oldest first); an unknown resource refuses
//! the kill, since vouching wrongly would kill someone's pane.

use std::collections::{HashMap, VecDeque};

use phux_protocol::ids::{SatelliteHost, ServerInstance};

use super::ClientId;

/// Most satellite spawns the ledger remembers.
pub(super) const MAX_SATELLITE_SPAWNS: usize = 1024;

/// A satellite resource, by host and satellite-local id.
type Key = (SatelliteHost, u32);

/// One remembered satellite spawn.
#[derive(Debug, Clone, Copy)]
struct Record {
    /// Insertion order, to tell a live record from a stale queue entry.
    seq: u64,
    /// The hub consumer whose spawn created the resource.
    spawner: ClientId,
    /// The satellite's instance token the spawn reply was bound to; `None`
    /// for an unbound spawn.
    instance: Option<ServerInstance>,
    /// Another hub consumer has attached or used it through this hub.
    used_by_other: bool,
}

/// The hub's record of satellite spawns it relayed.
#[derive(Debug, Default)]
pub(super) struct SatelliteSpawnLedger {
    records: HashMap<Key, Record>,
    /// Keys in insertion order. Holds at most [`MAX_SATELLITE_SPAWNS`]
    /// entries, so `records` never holds more.
    order: VecDeque<(u64, Key)>,
    next_seq: u64,
}

impl SatelliteSpawnLedger {
    /// Record that `spawner` spawned `host`'s `id` bound to `instance`
    /// (replacing a reused id's record).
    pub(super) fn record_spawn(
        &mut self,
        host: SatelliteHost,
        id: u32,
        instance: Option<ServerInstance>,
        spawner: ClientId,
    ) {
        let seq = self.next_seq;
        self.next_seq += 1;
        let key = (host, id);
        self.records.insert(
            key.clone(),
            Record {
                seq,
                spawner,
                instance,
                used_by_other: false,
            },
        );
        self.order.push_back((seq, key));
        self.evict_oldest();
    }

    /// Drop the oldest queue entries past the cap, and each one's record if
    /// the entry is still the live one for its key.
    fn evict_oldest(&mut self) {
        while self.order.len() > MAX_SATELLITE_SPAWNS {
            let Some((seq, key)) = self.order.pop_front() else {
                return;
            };
            if self
                .records
                .get(&key)
                .is_some_and(|record| record.seq == seq)
            {
                self.records.remove(&key);
            }
        }
    }

    /// Note that `client` is attaching or using `host`'s resource `id`
    /// through this hub. Only a consumer other than the spawner counts.
    pub(super) fn note_use(&mut self, host: &SatelliteHost, id: u32, client: ClientId) {
        if let Some(record) = self.records.get_mut(&(host.clone(), id))
            && record.spawner != client
        {
            record.used_by_other = true;
        }
    }

    /// Whether this hub spawned it under `instance` and only the spawner has
    /// used it since.
    #[must_use]
    pub(super) fn vouches_unattached(
        &self,
        host: &SatelliteHost,
        id: u32,
        instance: ServerInstance,
    ) -> bool {
        self.records
            .get(&(host.clone(), id))
            .is_some_and(|record| record.instance == Some(instance) && !record.used_by_other)
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_SATELLITE_SPAWNS, SatelliteSpawnLedger};
    use crate::state::ClientId;
    use phux_protocol::ids::{SatelliteHost, ServerInstance};

    fn edge() -> SatelliteHost {
        SatelliteHost::new("edge")
    }

    const TOKEN: ServerInstance = ServerInstance::new([1; 16]);

    #[test]
    fn vouches_only_for_its_own_spawns_until_another_consumer_uses_them() {
        let mut ledger = SatelliteSpawnLedger::default();
        assert!(
            !ledger.vouches_unattached(&edge(), 9, TOKEN),
            "unknown resource"
        );
        ledger.record_spawn(edge(), 9, Some(TOKEN), ClientId(1));
        ledger.note_use(&edge(), 9, ClientId(1));
        assert!(
            ledger.vouches_unattached(&edge(), 9, TOKEN),
            "the spawner's own use"
        );
        ledger.note_use(&edge(), 9, ClientId(2));
        assert!(
            !ledger.vouches_unattached(&edge(), 9, TOKEN),
            "another consumer's"
        );
        assert!(!ledger.vouches_unattached(&SatelliteHost::new("other"), 9, TOKEN));
    }

    #[test]
    fn a_stale_or_missing_token_never_vouches() {
        let mut ledger = SatelliteSpawnLedger::default();
        ledger.record_spawn(edge(), 9, Some(TOKEN), ClientId(1));
        let restarted = ServerInstance::new([2; 16]);
        assert!(
            !ledger.vouches_unattached(&edge(), 9, restarted),
            "a token from another id space"
        );
        ledger.record_spawn(edge(), 10, None, ClientId(1));
        assert!(
            !ledger.vouches_unattached(&edge(), 10, TOKEN),
            "an unbound spawn vouches for nothing"
        );
    }

    #[test]
    fn a_reused_id_starts_a_fresh_record() {
        let mut ledger = SatelliteSpawnLedger::default();
        ledger.record_spawn(edge(), 9, Some(TOKEN), ClientId(1));
        ledger.note_use(&edge(), 9, ClientId(2));
        let restarted = ServerInstance::new([2; 16]);
        ledger.record_spawn(edge(), 9, Some(restarted), ClientId(3));
        assert!(ledger.vouches_unattached(&edge(), 9, restarted));
        assert!(!ledger.vouches_unattached(&edge(), 9, TOKEN));
    }

    #[test]
    fn the_oldest_record_is_forgotten_past_the_cap() {
        let mut ledger = SatelliteSpawnLedger::default();
        for id in 0..=u32::try_from(MAX_SATELLITE_SPAWNS).unwrap() {
            ledger.record_spawn(edge(), id, Some(TOKEN), ClientId(1));
        }
        assert!(!ledger.vouches_unattached(&edge(), 0, TOKEN), "evicted");
        assert!(ledger.vouches_unattached(&edge(), 1, TOKEN));
        assert_eq!(ledger.records.len(), MAX_SATELLITE_SPAWNS);
        // Re-recording one id many times cannot grow the queue past the cap.
        for _ in 0..(2 * MAX_SATELLITE_SPAWNS) {
            ledger.record_spawn(edge(), 5, Some(TOKEN), ClientId(1));
        }
        assert!(ledger.order.len() <= MAX_SATELLITE_SPAWNS);
        assert!(ledger.vouches_unattached(&edge(), 5, TOKEN));
    }
}
