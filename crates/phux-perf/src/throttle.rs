//! Rate limiting for degradation warnings.
//!
//! A stall that logs once per event floods the log and hides itself; one
//! that logs at debug is invisible at the default filter. A [`Throttle`]
//! lets a hot path emit a `warn!` at most once per interval and report how
//! many events it swallowed in between, so the log carries the fact and
//! the magnitude without carrying the volume.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

/// Process-wide monotonic epoch so a `static` throttle can store an
/// `Instant` as a `u64`.
fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn now_ns() -> u64 {
    u64::try_from(epoch().elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Admit at most one emission per `interval`, counting the rest.
#[derive(Debug)]
pub struct Throttle {
    interval_ns: u64,
    /// `0` means "never emitted".
    last_ns: AtomicU64,
    suppressed: AtomicU64,
}

impl Throttle {
    /// A throttle that admits one emission per `interval`.
    #[must_use]
    #[allow(
        clippy::cast_lossless,
        reason = "u32 -> u64 in a const fn, where `From` is unavailable"
    )]
    pub const fn new(interval: Duration) -> Self {
        let interval_ns = interval
            .as_secs()
            .saturating_mul(1_000_000_000)
            .saturating_add(interval.subsec_nanos() as u64);
        Self {
            interval_ns,
            last_ns: AtomicU64::new(0),
            suppressed: AtomicU64::new(0),
        }
    }

    /// Should the caller emit now? `Some(n)` says yes and hands back the
    /// number of events suppressed since the last emission; `None` says the
    /// event was counted and the caller should stay quiet.
    pub fn admit(&self) -> Option<u64> {
        // Add one so a first-ever call is never mistaken for "never emitted".
        let now = now_ns().saturating_add(1);
        let last = self.last_ns.load(Relaxed);
        if last != 0 && now.saturating_sub(last) < self.interval_ns {
            self.suppressed.fetch_add(1, Relaxed);
            return None;
        }
        if self
            .last_ns
            .compare_exchange(last, now, Relaxed, Relaxed)
            .is_err()
        {
            // Lost the race to another thread that just emitted.
            self.suppressed.fetch_add(1, Relaxed);
            return None;
        }
        Some(self.suppressed.swap(0, Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_call_emits_then_suppresses_until_interval() {
        let t = Throttle::new(Duration::from_secs(3600));
        assert_eq!(t.admit(), Some(0));
        assert_eq!(t.admit(), None);
        assert_eq!(t.admit(), None);
        assert_eq!(t.suppressed.load(Relaxed), 2);
    }

    #[test]
    fn zero_interval_always_emits_and_reports_suppressed() {
        let t = Throttle::new(Duration::ZERO);
        assert_eq!(t.admit(), Some(0));
        assert_eq!(t.admit(), Some(0));
    }

    #[test]
    fn suppressed_count_is_handed_back_on_the_next_emission() {
        let t = Throttle::new(Duration::from_millis(20));
        assert_eq!(t.admit(), Some(0));
        assert_eq!(t.admit(), None);
        assert_eq!(t.admit(), None);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(t.admit(), Some(2));
    }
}
