//! Submodule for terminal actor internals.

use super::tick::RttEstimator;
use crate::grid::ConsumerReference;
use phux_protocol::ids::{BootstrapId, StreamId};
use phux_resource::mailbox::Outbound;
use tokio::sync::{mpsc, watch};

/// Cap on [`ConsumerSyncState::emit_instants`] (and pending refs), for a
/// consumer that never acks: about 5 s of ticks at the 20 ms floor.
pub const MAX_EMIT_INSTANTS: usize = 256;

/// Per-consumer state-sync cache (ADR-0018), one per attached consumer on
/// the actor. `!Send` like the terminal it tracks.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent per-consumer state flags (needs_initial_emit, behind, \
              wants_state_sync, loss_tolerant); collapsing them into an enum would \
              obscure the per-flag lifecycle each drives"
)]
pub struct ConsumerSyncState {
    /// Last-synced rendered row bodies and cursor/mode state. The tick diffs
    /// the live grid against it and advances it on emit, independent of
    /// libghostty's shared dirty bits.
    pub reference: ConsumerReference,
    /// Outbound mailbox for tick-emitted frames.
    pub outbound: mpsc::Sender<Outbound>,
    /// Wire terminal id for the `ResourceOutput` frame.
    pub wire_terminal_id: u32,
    /// Logical protocol-0.7 subscription identity.
    pub stream_id: StreamId,
    /// Replica generation currently receiving live output.
    pub bootstrap_id: BootstrapId,
    /// Aggregate-attach gate; false suppresses live output until `ATTACH_READY`.
    pub live_gate: watch::Receiver<bool>,
    /// Per-consumer `RESOURCE_OUTPUT` sequence, starting at 1.
    pub next_seq: u64,
    /// Highest acked `seq`; `0` before any ack.
    pub last_acked_seq: u64,
    /// Set at registration: walk once on the next tick even if the terminal
    /// is clean.
    pub needs_initial_emit: bool,
    /// A tick skipped this consumer on a full mailbox, so its reference is
    /// behind the grid; keeps the walk going on a clean terminal until it
    /// is served.
    pub behind: bool,
    /// Smoothed RTT for the adaptive cadence.
    pub rtt: RttEstimator,
    /// Emit instants of in-flight `seq`s, for RTT on ack. Pruned on ack and
    /// capped at [`MAX_EMIT_INSTANTS`].
    pub emit_instants: std::collections::BTreeMap<u64, tokio::time::Instant>,
    /// Whether this consumer negotiated `StateSync`; otherwise the broadcast
    /// pump serves it (unless the test override forces the tick).
    pub wants_state_sync: bool,
    /// Loss-tolerant model (ADR-0042): diff against [`Self::acked_reference`],
    /// advance only on ack, and retransmit after a timeout, so drops on a
    /// forwarded leg self-heal. Off by default (emit-once).
    pub loss_tolerant: bool,
    /// The last-acked reference (loss-tolerant only).
    pub acked_reference: ConsumerReference,
    /// Snapshots of emitted, un-acked frames by `seq`; an ack advances
    /// [`Self::acked_reference`] to the highest covered one. Capped at
    /// [`MAX_EMIT_INSTANTS`].
    pub pending_refs: std::collections::BTreeMap<u64, ConsumerReference>,
}

impl std::fmt::Debug for ConsumerSyncState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsumerSyncState")
            .field("wire_terminal_id", &self.wire_terminal_id)
            .field("next_seq", &self.next_seq)
            .field("last_acked_seq", &self.last_acked_seq)
            .field("wants_state_sync", &self.wants_state_sync)
            .finish_non_exhaustive()
    }
}
