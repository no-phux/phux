//! Monotone counters and last-value gauges.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// A monotonically increasing count: events, bytes, drops.
#[derive(Debug)]
pub struct Counter(AtomicU64);

impl Counter {
    /// A zeroed counter, usable in a `static`.
    #[must_use]
    pub const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// Add `n`.
    pub fn add(&self, n: u64) {
        self.0.fetch_add(n, Relaxed);
    }

    /// Add one.
    pub fn incr(&self) {
        self.add(1);
    }

    /// Current total.
    #[must_use]
    pub fn get(&self) -> u64 {
        self.0.load(Relaxed)
    }

    /// Return to zero.
    pub fn reset(&self) {
        self.0.store(0, Relaxed);
    }
}

impl Default for Counter {
    fn default() -> Self {
        Self::new()
    }
}

/// A last-value reading: attached clients, live panes, queue depth.
#[derive(Debug)]
pub struct Gauge(AtomicU64);

impl Gauge {
    /// A zeroed gauge, usable in a `static`.
    #[must_use]
    pub const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// Replace the reading.
    pub fn set(&self, v: u64) {
        self.0.store(v, Relaxed);
    }

    /// Current reading.
    #[must_use]
    pub fn get(&self) -> u64 {
        self.0.load(Relaxed)
    }

    /// Return to zero.
    pub fn reset(&self) {
        self.0.store(0, Relaxed);
    }
}

impl Default for Gauge {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_accumulates_and_resets() {
        let c = Counter::new();
        c.incr();
        c.add(4);
        assert_eq!(c.get(), 5);
        c.reset();
        assert_eq!(c.get(), 0);
    }

    #[test]
    fn gauge_is_last_value() {
        let g = Gauge::new();
        g.set(7);
        g.set(3);
        assert_eq!(g.get(), 3);
    }
}
