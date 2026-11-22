//! What `watch` remembers between passes: which boxes had no phux and
//! which failed to join, so each is probed and reported once and a failing
//! one is retried slowly.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// How long `watch` waits before trying a box whose join failed again.
pub(super) const RETRY_AFTER: Duration = Duration::from_secs(30);

/// How long `watch` waits before probing a box that had no phux again: one
/// probed while still activating may not have its loadout's files yet.
pub(super) const REPROBE_AFTER: Duration = Duration::from_secs(5);

/// What `watch` remembers between passes.
#[derive(Debug, Default)]
pub(super) struct Memory {
    /// Boxes that had no phux, and when they were last probed.
    no_phux: BTreeMap<String, Instant>,
    /// Boxes whose last join failed: when, and why.
    failed: BTreeMap<String, (Instant, String)>,
}

impl Memory {
    /// Whether this pass leaves box `id` alone.
    pub(super) fn waits_on(&self, id: &str) -> bool {
        self.no_phux
            .get(id)
            .is_some_and(|at| at.elapsed() < REPROBE_AFTER)
            || self
                .failed
                .get(id)
                .is_some_and(|(at, _)| at.elapsed() < RETRY_AFTER)
    }

    /// Record a probe that found no phux; `true` the first time, the one
    /// worth reporting.
    pub(super) fn lacks_phux(&mut self, id: &str) -> bool {
        self.no_phux.insert(id.to_owned(), Instant::now()).is_none()
    }

    /// Record a failed join; `true` when `reason` is new and worth reporting.
    pub(super) fn fail(&mut self, id: &str, reason: &str) -> bool {
        let previous = self
            .failed
            .insert(id.to_owned(), (Instant::now(), reason.to_owned()));
        previous.is_none_or(|(_, old)| old != reason)
    }

    /// Box `id` has phux now; probe it like any other.
    pub(super) fn found_phux(&mut self, id: &str) {
        self.no_phux.remove(id);
    }

    /// Box `id` joined; a later failure is new again.
    pub(super) fn joined(&mut self, id: &str) {
        self.failed.remove(id);
    }

    pub(super) fn forget_gone(&mut self, live: &BTreeSet<&str>) {
        self.no_phux.retain(|id, _| live.contains(id.as_str()));
        self.failed.retain(|id, _| live.contains(id.as_str()));
    }
}
