//! The live half of an `AgentSession` attach: forward records after the cut.
//! A lagged consumer is replayed from the ring; if the ring already evicted
//! them, the generation is tombstoned (`OUTBOUND_GAP`) and re-bootstrapped:
//! an explicit gap, never silence.

use bytes::Bytes;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::runtime::apps::producer_stream::{LagRecovery, PumpCore, RepublishEnd};
use phux_agent_session::session::BootstrapRequest;

/// Recover a lagged consumer from a validated cut after its cursor: replay
/// the retained suffix, or republish the full ring when part of it was
/// evicted.
pub(super) struct ReplayThenRepublish {
    /// The session's cut mailbox, for recovering a lagged window.
    pub(super) bootstrap: mpsc::Sender<BootstrapRequest>,
}

impl LagRecovery for ReplayThenRepublish {
    async fn recover(&self, pump: &mut PumpCore, cursor: u64, skipped: u64) -> Option<u64> {
        // The retained suffix, or the full ring when part of the suffix was
        // evicted. `None` when the session is gone.
        let cut = super::capture_cut(&self.bootstrap, Some(cursor))
            .await
            .ok()?;
        if cut.resume_after_seq == Some(cursor) {
            debug!(
                session = %pump.terminal_id,
                skipped,
                recovered = cut.records.len(),
                "agent session consumer fell behind; replaying the skipped records"
            );
            replay(pump, cursor, cut.records).await?;
            return Some(cut.base_seq);
        }
        warn!(
            session = %pump.terminal_id,
            skipped,
            last_delivered = cursor,
            "agent session consumer fell behind evicted records; publishing a replacement"
        );
        match pump.republish(cursor, &cut).await {
            Ok(base_seq) => Some(base_seq),
            Err(RepublishEnd::Fenced) => None,
            Err(RepublishEnd::CutShort(next)) => {
                // The state table already names `next`; a later attach's
                // tombstone for it is dropped by the client's fence.
                warn!(
                    session = %pump.terminal_id,
                    bootstrap_id = ?next,
                    "agent session replacement ended before its bootstrap was delivered"
                );
                None
            }
        }
    }
}

/// Forward a contiguous suffix after `cursor`, one record per frame.
async fn replay(pump: &PumpCore, cursor: u64, records: Vec<Bytes>) -> Option<()> {
    let mut seq = cursor;
    for record in records {
        if pump.cancel.is_cancelled() {
            return None;
        }
        seq += 1;
        if !pump.forward(seq, record).await {
            return None;
        }
    }
    Some(())
}
