//! Submodule for terminal actor internals.

use crate::grid::ConsumerReference;
use phux_resource::mailbox::Outbound;
use phux_resource::replication::ConsumerSync;
use tokio::sync::{mpsc, watch};

pub(super) enum PublicationCancellation {
    Consumer(phux_protocol::ClientId, phux_protocol::ids::StreamId, phux_protocol::ids::BootstrapId),
    Native(super::NativeCursorKey, phux_protocol::ids::BootstrapId, crate::native_state::OpaqueHistoryCursor),
}

pub(super) type ConsumerCancellation = futures_util::future::Join<
    tokio_util::sync::WaitForCancellationFutureOwned,
    std::future::Ready<PublicationCancellation>,
>;

/// Per-consumer state-sync cache (ADR-0018), one per attached consumer on
/// the actor. `!Send` like the terminal it tracks.
pub(super) struct ConsumerSyncState {
    /// Replicated-state delivery against this consumer's
    /// [`ConsumerReference`] (last-synced rendered row bodies and cursor/mode
    /// state). The tick diffs the live grid against it, independent of
    /// libghostty's shared dirty bits.
    pub(super) sync: ConsumerSync<ConsumerReference>,
    /// Outbound mailbox for tick-emitted frames.
    pub(super) outbound: mpsc::Sender<Outbound>,
    /// Wire terminal id for the `ResourceOutput` frame.
    pub(super) wire_terminal_id: u32,
    /// Aggregate-attach gate; false suppresses live output until `ATTACH_READY`.
    pub(super) live_gate: watch::Receiver<bool>,
    /// Whether this consumer negotiated `StateSync`; otherwise the broadcast
    /// pump serves it (unless the test override forces the tick).
    pub(super) wants_state_sync: bool,
    /// Interest owned by this registration's enclosing host generation.
    pub(super) cancellation: tokio_util::sync::CancellationToken,
}

impl std::fmt::Debug for ConsumerSyncState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsumerSyncState")
            .field("wire_terminal_id", &self.wire_terminal_id)
            .field("next_seq", &self.sync.next_seq)
            .field("last_acked_seq", &self.sync.last_acked_seq)
            .field("wants_state_sync", &self.wants_state_sync)
            .finish_non_exhaustive()
    }
}
