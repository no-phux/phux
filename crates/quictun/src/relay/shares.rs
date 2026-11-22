//! Bounded in-flight counts per key: how many admissions one source address
//! may have in progress, and how many consumers one route may have bridged.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// At most `share` held units per key.
#[derive(Debug)]
pub(crate) struct Shares<K> {
    share: usize,
    in_flight: Mutex<HashMap<K, usize>>,
}

impl<K: Hash + Eq + Clone> Shares<K> {
    /// A counter allowing `share` (at least one) units per key.
    pub(crate) fn new(share: usize) -> Self {
        Self {
            share: share.max(1),
            in_flight: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<K, usize>> {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Take one of `key`'s units, if any is left.
    pub(crate) fn try_take(self: &Arc<Self>, key: K) -> Option<Share<K>> {
        let mut in_flight = self.lock();
        let count = in_flight.entry(key.clone()).or_insert(0);
        let admitted = *count < self.share;
        if admitted {
            *count += 1;
        }
        drop(in_flight);
        admitted.then(|| Share {
            shares: Arc::clone(self),
            key,
        })
    }
}

/// One held unit; returned on drop, and the map entry goes with the last one
/// so the map stays bounded by what is in flight.
#[derive(Debug)]
pub(crate) struct Share<K: Hash + Eq + Clone> {
    shares: Arc<Shares<K>>,
    key: K,
}

impl<K: Hash + Eq + Clone> Drop for Share<K> {
    fn drop(&mut self) {
        let mut in_flight = self.shares.lock();
        if let Some(count) = in_flight.get_mut(&self.key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                in_flight.remove(&self.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_holds_at_most_its_share_and_gets_it_back_on_drop() {
        let shares = Arc::new(Shares::new(2));
        let first = shares.try_take("a").unwrap();
        let _second = shares.try_take("a").unwrap();
        assert!(shares.try_take("a").is_none(), "share spent");
        assert!(shares.try_take("b").is_some(), "keys are independent");
        drop(first);
        assert!(shares.try_take("a").is_some(), "returned on drop");
    }

    #[test]
    fn the_map_holds_only_keys_in_flight() {
        let shares = Arc::new(Shares::new(1));
        drop(shares.try_take(7).unwrap());
        assert!(shares.lock().is_empty());
    }
}
