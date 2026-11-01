//! Per-consumer replicated-state delivery, independent of resource kind
//! (ADR-0018, ADR-0042).
//!
//! A kind supplies its reference type `R` (what one consumer's mirror holds)
//! and a delta builder that diffs live state against an `R`. The builder,
//! the mailbox, and frame construction stay with the kind; [`ConsumerSync`]
//! owns the rest of one consumer's delivery state:
//!
//! - output `seq` allocation from the bootstrap cut;
//! - `FRAME_ACK` admission: matching generation, strictly newer than the
//!   last ack, below the next `seq`. A rejected ack changes nothing;
//! - an RTT sample from the newest emit instant an ack covers, which drives
//!   the adaptive cadence ([`RttEstimator::desired_tick_interval`]);
//! - the `needs_initial_emit` and `behind` walk flags;
//! - the loss-tolerant model: a snapshot of `R` per emitted `seq`, the acked
//!   reference advanced only on ack, and the retransmit decision.
//!
//! Emit instants and pending snapshots are capped at [`MAX_EMIT_INSTANTS`],
//! so a consumer that never acks cannot grow them.

mod cadence;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use phux_protocol::ids::{BootstrapId, StreamId};
use tokio::time::Instant;

pub use cadence::{
    DEFAULT_TICK_INTERVAL, MAX_TICK_INTERVAL, MIN_TICK_INTERVAL, RETRANSMIT_DEFAULT,
    RETRANSMIT_MAX, RETRANSMIT_MIN, RTT_EMA_ALPHA, RttEstimator, TICK_RESET_DEADBAND,
    adaptive_tick_interval, retransmit_timeout,
};

/// Cap on [`ConsumerSync::emit_instants`] (and pending refs), for a
/// consumer that never acks: about 5 s of ticks at the 20 ms floor.
pub const MAX_EMIT_INSTANTS: usize = 256;

/// Why [`ConsumerSync::ack`] refused a `FRAME_ACK`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckRejection {
    /// At or below the last ack: cumulative, so nothing new.
    Stale {
        /// The highest seq already acked.
        last_acked_seq: u64,
    },
    /// At or beyond the next seq: never emitted.
    BeyondEmitted {
        /// The seq the next emit will carry.
        next_seq: u64,
    },
}

/// One consumer's replicated-state delivery state, generic over the kind's
/// reference type `R`.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent per-consumer state flags (needs_initial_emit, behind, \
              loss_tolerant); collapsing them into an enum would obscure the \
              per-flag lifecycle each drives"
)]
pub struct ConsumerSync<R> {
    /// Emit-once reference: the delta builder diffs against it and advances
    /// it on emit.
    pub reference: R,
    /// Logical protocol-0.7 subscription identity.
    pub stream_id: StreamId,
    /// Replica generation currently receiving live output.
    pub bootstrap_id: BootstrapId,
    /// Per-consumer `RESOURCE_OUTPUT` sequence, starting at 1.
    pub next_seq: u64,
    /// Highest acked `seq`; `0` before any ack.
    pub last_acked_seq: u64,
    /// Set at registration: walk once on the next tick even if the resource
    /// is clean.
    pub needs_initial_emit: bool,
    /// A tick skipped this consumer on a full mailbox, so its reference is
    /// behind live state; keeps the walk going on a clean resource until it
    /// is served.
    pub behind: bool,
    /// Smoothed RTT for the adaptive cadence.
    pub rtt: RttEstimator,
    /// Emit instants of in-flight `seq`s, for RTT on ack. Pruned on ack and
    /// capped at [`MAX_EMIT_INSTANTS`].
    pub emit_instants: BTreeMap<u64, Instant>,
    /// Loss-tolerant model (ADR-0042): diff against [`Self::acked_reference`],
    /// advance only on ack, and retransmit after a timeout, so drops on a
    /// forwarded leg self-heal. Off by default (emit-once).
    pub loss_tolerant: bool,
    /// The last-acked reference (loss-tolerant only).
    pub acked_reference: R,
    /// Snapshots of emitted, un-acked frames by `seq`; an ack advances
    /// [`Self::acked_reference`] to the highest covered one. Capped at
    /// [`MAX_EMIT_INSTANTS`].
    pub pending_refs: BTreeMap<u64, R>,
}

impl<R> std::fmt::Debug for ConsumerSync<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsumerSync")
            .field("next_seq", &self.next_seq)
            .field("last_acked_seq", &self.last_acked_seq)
            .field("loss_tolerant", &self.loss_tolerant)
            .finish_non_exhaustive()
    }
}

impl<R> ConsumerSync<R> {
    /// A freshly registered emit-once consumer whose first emit carries
    /// `next_seq` (the bootstrap cut). It is walked on the next tick.
    #[must_use]
    pub fn new(
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        next_seq: u64,
        reference: R,
        acked_reference: R,
    ) -> Self {
        Self {
            reference,
            stream_id,
            bootstrap_id,
            next_seq,
            last_acked_seq: 0,
            needs_initial_emit: true,
            behind: false,
            // Cold-start cadence until the first ack round-trip.
            rtt: RttEstimator::default(),
            emit_instants: BTreeMap::new(),
            loss_tolerant: false,
            acked_reference,
            pending_refs: BTreeMap::new(),
        }
    }

    /// Whether an ack names this consumer's current stream generation.
    #[must_use]
    pub fn matches_generation(&self, stream_id: StreamId, bootstrap_id: BootstrapId) -> bool {
        self.stream_id == stream_id && self.bootstrap_id == bootstrap_id
    }

    /// Read-only admission of a cumulative ack for `seq`.
    ///
    /// # Errors
    ///
    /// [`AckRejection`] when `seq` is not newer than the last ack or was
    /// never emitted.
    pub const fn admit_ack(&self, seq: u64) -> Result<(), AckRejection> {
        if seq <= self.last_acked_seq {
            return Err(AckRejection::Stale {
                last_acked_seq: self.last_acked_seq,
            });
        }
        if seq >= self.next_seq {
            return Err(AckRejection::BeyondEmitted {
                next_seq: self.next_seq,
            });
        }
        Ok(())
    }

    /// Apply a cumulative ack for `seq`: advance `last_acked_seq`, the acked
    /// reference when loss-tolerant, and the RTT estimate. Returns the RTT
    /// sample folded in, if an emit instant covered `seq`.
    ///
    /// # Errors
    ///
    /// [`AckRejection`] from [`Self::admit_ack`]; nothing changes.
    pub fn ack(&mut self, seq: u64) -> Result<Option<std::time::Duration>, AckRejection> {
        self.admit_ack(seq)?;
        self.last_acked_seq = seq;
        if self.loss_tolerant {
            self.advance_acked_reference(seq);
        }
        Ok(self.fold_rtt_sample(seq))
    }

    /// Advance the acked reference to the snapshot of the highest emitted
    /// `seq` this ack covers, dropping older snapshots (ADR-0042).
    fn advance_acked_reference(&mut self, seq: u64) {
        if let Some((&covered, _)) = self.pending_refs.range(..=seq).next_back()
            && let Some(snapshot) = self.pending_refs.remove(&covered)
        {
            self.acked_reference = snapshot;
        }
        // Drain only covered entries; preserve the in-flight tree in place.
        while self
            .pending_refs
            .first_key_value()
            .is_some_and(|(&key, _)| key <= seq)
        {
            self.pending_refs.pop_first();
        }
    }

    /// Turn the ack into an RTT sample from the newest covered emit instant,
    /// then prune every covered instant.
    fn fold_rtt_sample(&mut self, seq: u64) -> Option<std::time::Duration> {
        let now = Instant::now();
        let rtt_sample = self
            .emit_instants
            .range(..=seq)
            .next_back()
            .map(|(_, &emitted_at)| now.saturating_duration_since(emitted_at));
        while self
            .emit_instants
            .first_key_value()
            .is_some_and(|(&key, _)| key <= seq)
        {
            self.emit_instants.pop_first();
        }
        let sample = rtt_sample?;
        crate::perf::CONSUMER_ACK_RTT.record_duration(sample);
        self.rtt.observe(sample);
        Some(sample)
    }

    /// Start this consumer's tick turn: clear `needs_initial_emit` and
    /// return whether a backpressured delta was owed.
    pub const fn begin_turn(&mut self) -> bool {
        let was_behind = self.behind;
        self.needs_initial_emit = false;
        was_behind
    }

    /// The mailbox was full: the reference is held and the walk continues
    /// on a clean resource until this consumer is served.
    pub const fn mark_behind(&mut self) {
        self.behind = true;
    }

    /// The mailbox had room: this consumer is served this tick.
    pub const fn mark_served(&mut self) {
        self.behind = false;
    }

    /// Whether this consumer must be walked on a clean resource: never
    /// served, owed a backpressured delta, or a loss-tolerant frame is
    /// still un-acked.
    #[must_use]
    pub fn must_walk_when_clean(&self) -> bool {
        self.needs_initial_emit || self.behind || !self.pending_refs.is_empty()
    }

    /// Whether a loss-tolerant frame is un-acked past the retransmit
    /// timeout, measured from the newest emit.
    #[must_use]
    pub fn retransmit_due(&self, now: Instant) -> bool {
        !self.pending_refs.is_empty()
            && self.emit_instants.values().next_back().is_none_or(|last| {
                now.saturating_duration_since(*last) >= retransmit_timeout(self.rtt.smoothed())
            })
    }

    /// Loss-tolerant emit gate: the delta against the acked reference stays
    /// non-empty while a frame is un-acked, so hold it unless there is new
    /// content, a post-backpressure flush, or a due retransmit. Never holds
    /// an emit-once consumer.
    #[must_use]
    pub fn holds_delta(&self, mutated: bool, was_behind: bool) -> bool {
        if !self.loss_tolerant {
            return false;
        }
        let retransmit_due = self.retransmit_due(Instant::now());
        !mutated && !was_behind && !retransmit_due
    }

    /// Take the `seq` for the frame about to ship.
    pub const fn allocate_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        seq
    }

    /// Record a shipped `seq`: its emit instant for RTT on ack and, when
    /// loss-tolerant, the reference it brought the consumer to (`snapshot`
    /// runs only then). Both are bounded by [`MAX_EMIT_INSTANTS`].
    pub fn record_emit(&mut self, seq: u64, snapshot: impl FnOnce() -> R) {
        record_bounded(&mut self.emit_instants, seq, Instant::now());
        if self.loss_tolerant {
            record_bounded(&mut self.pending_refs, seq, snapshot());
        }
    }
}

/// Insert at `seq`, evicting the oldest entries past [`MAX_EMIT_INSTANTS`].
fn record_bounded<V>(map: &mut BTreeMap<u64, V>, seq: u64, value: V) {
    map.insert(seq, value);
    while map.len() > MAX_EMIT_INSTANTS {
        map.pop_first();
    }
}
