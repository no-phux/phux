//! Submodule for terminal actor internals.

use crate::grid::ConsumerReference;
use phux_resource::mailbox::Outbound;
use phux_resource::replication::ConsumerSync;
use tokio::sync::{mpsc, watch};

pub use phux_resource::replication::MAX_EMIT_INSTANTS;

/// Per-consumer state-sync cache (ADR-0018), one per attached consumer on
/// the actor. `!Send` like the terminal it tracks.
pub struct ConsumerSyncState {
    /// Replicated-state delivery against this consumer's
    /// [`ConsumerReference`] (last-synced rendered row bodies and cursor/mode
    /// state). The tick diffs the live grid against it, independent of
    /// libghostty's shared dirty bits.
    pub sync: ConsumerSync<ConsumerReference>,
    /// Outbound mailbox for tick-emitted frames.
    pub outbound: mpsc::Sender<Outbound>,
    /// Wire terminal id for the `ResourceOutput` frame.
    pub wire_terminal_id: u32,
    /// Aggregate-attach gate; false suppresses live output until `ATTACH_READY`.
    pub live_gate: watch::Receiver<bool>,
    /// Whether this consumer negotiated `StateSync`; otherwise the broadcast
    /// pump serves it (unless the test override forces the tick).
    pub wants_state_sync: bool,
}

/// Tests read replication fields as they did before the engine moved out.
#[cfg(test)]
impl std::ops::Deref for ConsumerSyncState {
    type Target = ConsumerSync<ConsumerReference>;

    fn deref(&self) -> &Self::Target {
        &self.sync
    }
}

#[cfg(test)]
impl std::ops::DerefMut for ConsumerSyncState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.sync
    }
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
