//! A lock-free log-linear histogram.
//!
//! Values below 32 get their own bucket; from 32 upward each power-of-two
//! octave is split into eight sub-buckets by the three bits after the
//! leading one. That is 32 + 59 * 8 = 504 buckets covering all of `u64`,
//! with a worst-case relative error of one eighth of an octave (about 9%)
//! on any reported percentile. Recording is a single relaxed `fetch_add`
//! per bucket plus the running count / sum / min / max, so it is safe to
//! call from any thread on a hot path.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// Number of buckets in every [`Histogram`].
pub const BUCKETS: usize = 32 + 59 * 8;

/// Where exact (one value per bucket) tracking ends and the log-linear
/// octaves begin.
const LINEAR_LIMIT: u64 = 32;
/// [`LINEAR_LIMIT`] as a bucket count.
const LINEAR_BUCKETS: usize = 32;
/// `log2(LINEAR_LIMIT)`: the first octave exponent in the log-linear range.
const FIRST_OCTAVE: usize = 5;
/// Sub-buckets per octave (three bits after the leading one).
const SUBS: usize = 8;

/// Map a value to its bucket index.
#[allow(
    clippy::cast_possible_truncation,
    reason = "every cast here is of a value already bounded below 64"
)]
const fn bucket_index(v: u64) -> usize {
    if v < LINEAR_LIMIT {
        return v as usize;
    }
    let e = 63 - v.leading_zeros() as usize;
    let sub = ((v >> (e - 3)) & 7) as usize;
    LINEAR_BUCKETS + (e - FIRST_OCTAVE) * SUBS + sub
}

/// Inclusive lower bound of a bucket.
const fn bucket_lower(idx: usize) -> u64 {
    if idx < LINEAR_BUCKETS {
        return idx as u64;
    }
    let e = FIRST_OCTAVE + (idx - LINEAR_BUCKETS) / SUBS;
    let sub = ((idx - LINEAR_BUCKETS) % SUBS) as u64;
    (1_u64 << e) | (sub << (e - 3))
}

/// Inclusive upper bound of a bucket (saturating at `u64::MAX`).
const fn bucket_upper(idx: usize) -> u64 {
    if idx < LINEAR_BUCKETS {
        return idx as u64;
    }
    let e = FIRST_OCTAVE + (idx - LINEAR_BUCKETS) / SUBS;
    let width = 1_u64 << (e - 3);
    bucket_lower(idx).saturating_add(width - 1)
}

/// A fixed-layout histogram of `u64` samples.
pub struct Histogram {
    buckets: [AtomicU64; BUCKETS],
    count: AtomicU64,
    sum: AtomicU64,
    min: AtomicU64,
    max: AtomicU64,
}

impl std::fmt::Debug for Histogram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Histogram")
            .field("count", &self.count.load(Relaxed))
            .field("sum", &self.sum.load(Relaxed))
            .finish_non_exhaustive()
    }
}

impl Histogram {
    /// An empty histogram, usable in a `static`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buckets: [const { AtomicU64::new(0) }; BUCKETS],
            count: AtomicU64::new(0),
            sum: AtomicU64::new(0),
            min: AtomicU64::new(u64::MAX),
            max: AtomicU64::new(0),
        }
    }

    /// Record one sample.
    pub fn record(&self, v: u64) {
        self.buckets[bucket_index(v)].fetch_add(1, Relaxed);
        self.count.fetch_add(1, Relaxed);
        self.sum.fetch_add(v, Relaxed);
        self.min.fetch_min(v, Relaxed);
        self.max.fetch_max(v, Relaxed);
    }

    /// Record a duration in microseconds.
    pub fn record_duration(&self, d: std::time::Duration) {
        self.record(crate::duration_us(d));
    }

    /// Record the microseconds elapsed since `start`.
    pub fn record_elapsed(&self, start: Instant) {
        self.record_duration(start.elapsed());
    }

    /// Samples recorded so far.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.count.load(Relaxed)
    }

    /// Return every bucket and running statistic to the empty state.
    pub fn reset(&self) {
        for b in &self.buckets {
            b.store(0, Relaxed);
        }
        self.count.store(0, Relaxed);
        self.sum.store(0, Relaxed);
        self.min.store(u64::MAX, Relaxed);
        self.max.store(0, Relaxed);
    }

    /// Copy the current state out. Only non-empty buckets are kept, so an
    /// idle histogram snapshots to a few dozen bytes.
    #[must_use]
    pub fn snapshot(&self) -> HistogramSnapshot {
        let buckets = self
            .buckets
            .iter()
            .enumerate()
            .filter_map(|(idx, b)| {
                let n = b.load(Relaxed);
                (n > 0).then(|| Bucket {
                    idx: u16::try_from(idx).unwrap_or(u16::MAX),
                    count: n,
                })
            })
            .collect();
        let count = self.count.load(Relaxed);
        HistogramSnapshot {
            count,
            sum: self.sum.load(Relaxed),
            min: if count == 0 {
                0
            } else {
                self.min.load(Relaxed)
            },
            max: self.max.load(Relaxed),
            buckets,
        }
    }
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

/// One non-empty bucket in a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bucket {
    /// Bucket index into the fixed layout.
    pub idx: u16,
    /// Samples in the bucket.
    pub count: u64,
}

/// A point-in-time copy of a [`Histogram`], or the difference between two.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HistogramSnapshot {
    /// Total samples.
    pub count: u64,
    /// Sum of all samples (saturating).
    pub sum: u64,
    /// Smallest sample (exact for a live snapshot; a bucket bound for a delta).
    pub min: u64,
    /// Largest sample (exact for a live snapshot; a bucket bound for a delta).
    pub max: u64,
    /// Non-empty buckets in ascending index order.
    pub buckets: Vec<Bucket>,
}

impl HistogramSnapshot {
    /// Is there nothing here?
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Arithmetic mean, `0` when empty.
    #[must_use]
    pub const fn mean(&self) -> u64 {
        if self.count == 0 {
            0
        } else {
            self.sum / self.count
        }
    }

    /// The `p`-th percentile (`0..=100`), reported as the upper bound of the
    /// bucket the rank lands in and clamped to `max`, so it never
    /// under-reports. `0` when empty.
    #[must_use]
    pub fn percentile(&self, p: u8) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let p = u64::from(p.min(100));
        // rank = ceil(p/100 * count), at least 1
        let rank = ((p * self.count).div_ceil(100)).max(1);
        let mut seen = 0_u64;
        for b in &self.buckets {
            seen += b.count;
            if seen >= rank {
                return bucket_upper(usize::from(b.idx)).min(self.max);
            }
        }
        self.max
    }

    /// `self - prev`: the samples recorded between two snapshots of the same
    /// histogram. A counter that went backwards (the histogram was reset in
    /// between) yields `self` unchanged.
    #[must_use]
    pub fn delta(&self, prev: &Self) -> Self {
        if self.count < prev.count {
            return self.clone();
        }
        let mut buckets: Vec<Bucket> = Vec::with_capacity(self.buckets.len());
        let mut prev_iter = prev.buckets.iter().peekable();
        for cur in &self.buckets {
            while prev_iter.peek().is_some_and(|p| p.idx < cur.idx) {
                prev_iter.next();
            }
            let before = prev_iter
                .peek()
                .filter(|p| p.idx == cur.idx)
                .map_or(0, |p| p.count);
            let n = cur.count.saturating_sub(before);
            if n > 0 {
                buckets.push(Bucket {
                    idx: cur.idx,
                    count: n,
                });
            }
        }
        let count = self.count - prev.count;
        let min = buckets
            .first()
            .map_or(0, |b| bucket_lower(usize::from(b.idx)));
        let max = buckets
            .last()
            .map_or(0, |b| bucket_upper(usize::from(b.idx)).min(self.max));
        Self {
            count,
            sum: self.sum.saturating_sub(prev.sum),
            min,
            max,
            buckets,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_bounds_tile_u64_without_gaps() {
        assert_eq!(bucket_lower(0), 0);
        for idx in 1..BUCKETS {
            assert_eq!(
                bucket_upper(idx - 1).saturating_add(1),
                bucket_lower(idx),
                "gap or overlap between buckets {} and {}",
                idx - 1,
                idx
            );
        }
        assert_eq!(bucket_upper(BUCKETS - 1), u64::MAX);
    }

    #[test]
    fn every_value_lands_in_a_bucket_that_contains_it() {
        let probes = [
            0_u64,
            1,
            31,
            32,
            33,
            47,
            48,
            63,
            64,
            1000,
            65_535,
            1 << 20,
            (1 << 40) + 12345,
            u64::MAX / 3,
            u64::MAX,
        ];
        for v in probes {
            let idx = bucket_index(v);
            assert!(idx < BUCKETS, "{v} -> {idx}");
            assert!(
                bucket_lower(idx) <= v && v <= bucket_upper(idx),
                "{v} not in bucket {idx}"
            );
        }
    }

    #[test]
    fn relative_error_stays_under_one_eighth_octave() {
        for v in [40_u64, 100, 777, 5000, 123_456, 9_999_999] {
            let idx = bucket_index(v);
            let width = bucket_upper(idx) - bucket_lower(idx) + 1;
            // width / lower <= 1/8 for every log-linear bucket
            assert!(
                width * 8 <= bucket_lower(idx) + width,
                "bucket {idx} too wide for {v}"
            );
        }
    }

    #[test]
    fn percentiles_of_a_known_distribution() {
        let h = Histogram::new();
        for v in 1..=100_u64 {
            h.record(v);
        }
        let s = h.snapshot();
        assert_eq!(s.count, 100);
        assert_eq!(s.min, 1);
        assert_eq!(s.max, 100);
        assert_eq!(s.mean(), 50);
        // p50 rank 50 lands in bucket [48,51]; upper bound 51 >= true 50.
        let p50 = s.percentile(50);
        assert!((50..=51).contains(&p50), "p50={p50}");
        let p99 = s.percentile(99);
        assert!((99..=100).contains(&p99), "p99={p99}");
        assert_eq!(s.percentile(100), 100);
        assert_eq!(s.percentile(0), 1);
    }

    #[test]
    fn empty_snapshot_is_all_zero() {
        let s = Histogram::new().snapshot();
        assert!(s.is_empty());
        assert_eq!(s.percentile(99), 0);
        assert_eq!(s.min, 0);
        assert_eq!(s.mean(), 0);
    }

    #[test]
    fn delta_isolates_the_interval() {
        let h = Histogram::new();
        for _ in 0..10 {
            h.record(100);
        }
        let a = h.snapshot();
        for _ in 0..5 {
            h.record(10_000);
        }
        let b = h.snapshot();
        let d = b.delta(&a);
        assert_eq!(d.count, 5);
        assert_eq!(d.sum, 50_000);
        assert_eq!(d.buckets.len(), 1);
        assert!(d.min <= 10_000 && 10_000 <= d.max);
        // the old 100s are gone from the interval
        assert!(d.percentile(1) > 100);
    }

    #[test]
    fn delta_after_reset_falls_back_to_current() {
        let h = Histogram::new();
        h.record(5);
        h.record(6);
        let a = h.snapshot();
        h.reset();
        h.record(7);
        let b = h.snapshot();
        assert_eq!(b.delta(&a), b);
    }

    #[test]
    fn snapshot_roundtrips_through_json() {
        let h = Histogram::new();
        h.record(3);
        h.record(300);
        let s = h.snapshot();
        let json = serde_json::to_string(&s).unwrap_or_default();
        let back: HistogramSnapshot = serde_json::from_str(&json).unwrap_or_default();
        assert_eq!(back, s);
    }
}
