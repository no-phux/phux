//! Consumer acknowledgment and cadence belong to the admitted generation.

use super::PublicationOwner;
use crate::actor::{ClientId, DEFAULT_TICK_INTERVAL};
use phux_resource::replication::AckRejection;
use tracing::trace;

impl PublicationOwner {
    /// Handle a cumulative `FRAME_ACK` for `seq`.
    ///
    /// Under emit-once the reference already advanced on emit, so the ack
    /// tracks `last_acked_seq`, refreshes the cursor/mode capture, and feeds
    /// RTT. Stale or unknown acks are ignored. Returns `true` when a fresh
    /// RTT sample was folded in (the loop then re-evaluates the cadence).
    pub(in crate::actor) fn on_generation_frame_ack(
        &mut self,
        client_id: ClientId,
        stream_id: phux_protocol::ids::StreamId,
        bootstrap_id: phux_protocol::ids::BootstrapId,
        seq: u64,
    ) -> bool {
        let Some(consumer) = self.consumers.get(&client_id) else {
            return false;
        };
        if consumer.cancellation.is_cancelled()
            || !consumer.sync.matches_generation(stream_id, bootstrap_id)
        {
            trace!(
                ?client_id,
                ?stream_id,
                ?bootstrap_id,
                seq,
                "FRAME_ACK for stale stream generation; dropping",
            );
            return false;
        }
        self.on_frame_ack(client_id, seq)
    }

    pub(in crate::actor) fn on_frame_ack(&mut self, client_id: ClientId, seq: u64) -> bool {
        // The test override that forces every consumer onto the tick.
        let force_all_consumers = self.force_tick;
        let Some(consumer) = self.consumers.get_mut(&client_id) else {
            // An ack racing detach; steady-state, not a misuse.
            trace!(
                ?client_id,
                seq, "FRAME_ACK for unregistered consumer; dropping"
            );
            return false;
        };
        if consumer.cancellation.is_cancelled() {
            return false;
        }
        // A raw consumer acks the pump's own seq space, unrelated to this
        // state; folding it in would corrupt accounting.
        if !force_all_consumers && !consumer.wants_state_sync {
            trace!(
                ?client_id,
                seq, "FRAME_ACK for raw-broadcast consumer; not a tick ack, dropping"
            );
            return false;
        }
        let sample = match consumer.sync.ack(seq) {
            Ok(sample) => sample,
            Err(rejection) => {
                trace_rejected_ack(client_id, seq, rejection);
                return false;
            }
        };
        if let Some(sample) = sample {
            crate::perf::CONSUMER_ACK_RTT.record_duration(sample);
            trace_rtt_sample(client_id, seq, sample, consumer.sync.rtt.smoothed());
        }
        trace!(
            ?client_id,
            seq, "FRAME_ACK applied: last_acked_seq advanced"
        );
        sample.is_some()
    }

    /// The shared tick interval: the minimum desired interval over all
    /// consumers, so the fastest peer sets the cadence (slower peers just see
    /// more empty diffs). [`DEFAULT_TICK_INTERVAL`] with no samples.
    pub(in crate::actor) fn adaptive_tick_interval(&self) -> std::time::Duration {
        self.consumers
            .values()
            .map(|s| s.sync.rtt.desired_tick_interval())
            .min()
            .unwrap_or(DEFAULT_TICK_INTERVAL)
    }
}

/// Trace why a tick ack was dropped; a rejected ack changes nothing.
fn trace_rejected_ack(client_id: ClientId, seq: u64, rejection: AckRejection) {
    match rejection {
        AckRejection::Stale { last_acked_seq } => trace!(
            ?client_id,
            seq, last_acked_seq, "FRAME_ACK older/duplicate; dropping",
        ),
        AckRejection::BeyondEmitted { next_seq } => trace!(
            ?client_id,
            seq, next_seq, "FRAME_ACK beyond emitted sequence; dropping"
        ),
    }
}

/// Trace the RTT sample an applied ack folded into the EMA.
fn trace_rtt_sample(
    client_id: ClientId,
    seq: u64,
    sample: std::time::Duration,
    srtt: Option<std::time::Duration>,
) {
    trace!(
        ?client_id,
        seq,
        rtt_ms = sample.as_secs_f64() * 1000.0,
        srtt_ms = srtt.map(|d| d.as_secs_f64() * 1000.0),
        "FRAME_ACK: RTT sample folded into EMA",
    );
}
