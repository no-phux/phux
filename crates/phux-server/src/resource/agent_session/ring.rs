//! The bounded record ring an [`AgentSession`](super) retains and replays.
//!
//! A byte ceiling (like ADR-0094 scrollback), oldest-first pruning, and a
//! count of what was pruned so a replay can say it starts mid-session.

use bytes::Bytes;

/// A bounded FIFO of stamped records, pruned by total retained bytes.
#[derive(Debug)]
pub struct RecordRing {
    /// Retained records, oldest first, each a complete stamped JSONL line.
    records: std::collections::VecDeque<Bytes>,
    /// Sum of `records`' lengths, maintained incrementally.
    bytes: usize,
    /// Ceiling on [`Self::bytes`], from `defaults.agent-log-bytes`.
    ceiling: usize,
    /// How many records eviction has dropped over this ring's life.
    dropped: u64,
}

impl RecordRing {
    /// A ring holding at most `ceiling` bytes of records.
    #[must_use]
    pub const fn new(ceiling: usize) -> Self {
        Self {
            records: std::collections::VecDeque::new(),
            bytes: 0,
            ceiling,
            dropped: 0,
        }
    }

    /// Append `record` and prune the oldest back under the ceiling. The
    /// newest record is always kept (bounded by the 16 KiB record limit);
    /// eviction is counted in [`Self::dropped`], never an error.
    pub fn push(&mut self, record: Bytes) {
        self.bytes = self.bytes.saturating_add(record.len());
        self.records.push_back(record);
        while self.bytes > self.ceiling && self.records.len() > 1 {
            if let Some(evicted) = self.records.pop_front() {
                self.bytes = self.bytes.saturating_sub(evicted.len());
                self.dropped = self.dropped.saturating_add(1);
            }
        }
    }

    /// The retained records, oldest first.
    pub fn records(&self) -> impl ExactSizeIterator<Item = &Bytes> {
        self.records.iter()
    }

    /// How many records eviction has dropped.
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Bytes currently retained.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    /// How many records are currently retained.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// `true` iff the ring currently retains nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(n: usize) -> Bytes {
        Bytes::from(vec![b'x'; n])
    }

    #[test]
    fn a_ring_under_its_ceiling_retains_everything() {
        let mut ring = RecordRing::new(100);
        ring.push(record(10));
        ring.push(record(20));
        assert_eq!(ring.len(), 2);
        assert_eq!(ring.bytes(), 30);
        assert_eq!(ring.dropped(), 0);
    }

    #[test]
    fn overflow_evicts_the_oldest_and_counts_the_tombstone() {
        let mut ring = RecordRing::new(50);
        for _ in 0..4 {
            ring.push(record(20));
        }
        assert_eq!(ring.len(), 2, "two 20-byte records fit under 50");
        assert_eq!(ring.bytes(), 40);
        assert_eq!(ring.dropped(), 2);
        assert!(
            ring.records().all(|r| r.len() == 20),
            "eviction drops whole records, never fragments"
        );
    }

    #[test]
    fn the_newest_record_survives_a_ceiling_smaller_than_itself() {
        let mut ring = RecordRing::new(4);
        ring.push(record(10));
        ring.push(record(11));
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.records().next().expect("one").len(), 11);
        assert_eq!(ring.dropped(), 1);
    }
}
