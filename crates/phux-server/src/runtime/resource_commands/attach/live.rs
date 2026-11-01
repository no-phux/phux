//! The live half of an `AgentSession` attach: forward records after the cut,
//! and recover whatever a broadcast lag skipped.
//!
//! A lagged consumer is replayed the skipped records from the session's ring
//! in the same generation. When the ring has already evicted some of them,
//! the generation is tombstoned (`OUTBOUND_GAP`, at the last delivered
//! record) and replaced by a full retained bootstrap: an explicit gap, never
//! silence.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use phux_protocol::caps::BootstrapLimits;
use phux_protocol::ids::{BootstrapId, StreamId};
use phux_protocol::wire::frame::{FrameKind, TombstoneReason};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::bootstrap_frames;
use crate::mailbox::Outbound;
use crate::resource::PaneOutput;
use crate::resource::agent_session::{AgentSessionBootstrap, BootstrapRequest};
use crate::state::{ClientId, SharedState};

/// One consumer's live subscription to an `AgentSession` stream.
pub(super) struct LivePump {
    pub(super) state: SharedState,
    pub(super) client_id: ClientId,
    pub(super) core: phux_core::ids::ResourceId,
    /// The session's cut mailbox, for recovering a lagged window.
    pub(super) bootstrap: mpsc::Sender<BootstrapRequest>,
    pub(super) out_tx: mpsc::Sender<Outbound>,
    pub(super) terminal_id: phux_protocol::ids::ResourceId,
    pub(super) stream_id: StreamId,
    pub(super) bootstrap_id: BootstrapId,
    pub(super) limits: BootstrapLimits,
    pub(super) cancel: CancellationToken,
    /// The last record delivered, shared with a replacing attach's tombstone.
    pub(super) last_seq: Arc<AtomicU64>,
}

impl LivePump {
    /// Forward records after `base_seq` until the consumer detaches, goes
    /// away, or the session closes.
    pub(super) async fn run(mut self, mut live: broadcast::Receiver<PaneOutput>, base_seq: u64) {
        let mut cursor = base_seq;
        loop {
            let output = tokio::select! {
                () = self.cancel.cancelled() => return,
                received = live.recv() => received,
            };
            let next = match output {
                // A batch is cut atomically: at or behind the cursor, every
                // record in it was already delivered.
                Ok(PaneOutput::Live { seq, .. }) if seq <= cursor => Some(cursor),
                Ok(PaneOutput::Live { seq, bytes, .. }) => {
                    self.forward(seq, bytes).await.then_some(seq)
                }
                // A session has no grid to resync.
                Ok(PaneOutput::Resync { .. } | PaneOutput::Control { .. }) => Some(cursor),
                Err(RecvError::Lagged(skipped)) => self.recover(cursor, skipped).await,
                Err(RecvError::Closed) => None,
            };
            let Some(next) = next else { return };
            cursor = next;
        }
    }

    /// Put one live frame on the consumer's mailbox; `false` once it is gone.
    async fn forward(&self, seq: u64, bytes: Bytes) -> bool {
        let frame = FrameKind::ResourceOutput {
            terminal_id: self.terminal_id.clone(),
            stream_id: self.stream_id,
            bootstrap_id: self.bootstrap_id,
            seq,
            bytes,
        };
        if !self.send(frame).await {
            return false;
        }
        self.last_seq.store(seq, Ordering::Release);
        true
    }

    async fn send(&self, frame: FrameKind) -> bool {
        self.out_tx.send(Outbound::Frame(frame)).await.is_ok()
    }

    /// Bring a consumer that lagged at `cursor` up to a fresh cut; the new
    /// cursor, or `None` when the pump must end.
    async fn recover(&mut self, cursor: u64, skipped: u64) -> Option<u64> {
        if self.cancel.is_cancelled() {
            return None;
        }
        let cut = self.cut_after(cursor).await?;
        if cut.resume_after_seq == Some(cursor) {
            debug!(
                session = %self.terminal_id,
                skipped,
                recovered = cut.records.len(),
                "agent session consumer fell behind; replaying the skipped records"
            );
            self.replay(cursor, cut.records).await?;
        } else {
            warn!(
                session = %self.terminal_id,
                skipped,
                last_delivered = cursor,
                "agent session consumer fell behind evicted records; publishing a replacement"
            );
            self.republish(cursor, &cut).await?;
        }
        Some(cut.base_seq)
    }

    /// Cut the session after `cursor`: the retained suffix, or the full ring
    /// when part of the suffix was evicted. `None` when the session is gone.
    async fn cut_after(&self, cursor: u64) -> Option<AgentSessionBootstrap> {
        let (reply, rx) = oneshot::channel();
        let request = BootstrapRequest {
            reply,
            seal: None,
            after_seq: Some(cursor),
        };
        self.bootstrap.send(request).await.ok()?;
        rx.await.ok()
    }

    /// Forward a contiguous suffix after `cursor`, one record per frame.
    async fn replay(&self, cursor: u64, records: Vec<Bytes>) -> Option<()> {
        let mut seq = cursor;
        for record in records {
            seq += 1;
            if !self.forward(seq, record).await {
                return None;
            }
        }
        Some(())
    }

    /// Tombstone the generation at the last delivered record and publish the
    /// full retained ring as its replacement.
    async fn republish(&mut self, cursor: u64, cut: &AgentSessionBootstrap) -> Option<()> {
        let next = self.state.with_mut(|s| {
            s.advance_attach_terminal_pump(self.client_id, self.core, &self.last_seq)
        })?;
        let tombstone = FrameKind::BootstrapTombstone {
            terminal_id: self.terminal_id.clone(),
            stream_id: self.stream_id,
            bootstrap_id: self.bootstrap_id,
            reason: TombstoneReason::OutboundGap,
            last_valid_seq: cursor,
        };
        if !self.send(tombstone).await {
            return None;
        }
        self.bootstrap_id = next;
        for frame in bootstrap_frames(
            &self.terminal_id,
            self.stream_id,
            self.bootstrap_id,
            cut,
            self.limits,
        ) {
            if !self.send(frame).await {
                return None;
            }
        }
        self.last_seq.store(cut.base_seq, Ordering::Release);
        Some(())
    }
}
