//! Bootstrapping a viewer onto a document resource: the newest document
//! as one cut, then every later document live. A viewer that falls behind
//! is fenced and recut from the newest document: it skips intermediate
//! documents, never silently.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use phux_protocol::caps::{BootstrapLimits, BootstrapStreamProfile};
use phux_protocol::ids::{BootstrapId, StreamId};
use phux_protocol::wire::frame::{CommandResult, ErrorCode, FrameKind, TombstoneReason};
use phux_tree::{DocumentResource as _, SnapshotRequest, TreeSnapshot};

use super::DocumentApi;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::mailbox::Outbound;
use crate::resource::ResourceOutput;
use crate::runtime::session_attach::{
    SessionCapture,
    staging::{CaptureFailure, SessionStaging},
};
use crate::state::{ClientId, SharedState};

/// Capture without allocating an independent attach generation or publishing.
/// This uses the same cut, framing and lag-recovery pump as `ATTACH_RESOURCE`.
pub(super) async fn stage_session<A: DocumentApi>(
    capture: SessionCapture<'_>,
    staging: &mut SessionStaging,
    pane: crate::state::AttachSnapshotPane,
) -> Result<(), CaptureFailure> {
    let document = pane
        .handle
        .document::<A>()
        .map_err(|error| error.to_string())?;
    let live = pane.handle.output.subscribe();
    let cut = snapshot(&document.snapshot).await.ok_or_else(|| {
        CaptureFailure::Closed(format!("{} closed before its snapshot", A::LABEL))
    })?;
    let bytes = cut.document.as_ref().map_or(0, Bytes::len);
    if bytes > staging.budget.remaining_bytes() {
        return Err("document exceeds aggregate bootstrap source budget"
            .to_owned()
            .into());
    }
    if let Some(bytes) = &cut.document {
        A::validate_bootstrap(bytes).map_err(CaptureFailure::Failed)?;
    }
    let frames = staging.collect_frames(bootstrap_frames(
        &pane.wire_terminal_id,
        capture.stream_id,
        capture.bootstrap_id,
        A::PROFILE,
        &cut,
        capture.limits,
    ))?;
    staging.append(frames, bytes)?;
    let (cancel, done, last_seq, prior) = capture.state.with_mut(|s| {
        s.replace_attach_terminal_pump(capture.client_id, pane.terminal_id, capture.bootstrap_id)
    });
    if let Some((_, done, _)) = prior {
        done.cancelled().await;
    }
    last_seq.store(cut.base_seq, Ordering::Release);
    let pump_done = done.drop_guard();
    let pump = LivePump {
        state: capture.state.clone(),
        client_id: capture.client_id,
        core: pane.terminal_id,
        snapshot: document.snapshot.clone(),
        profile: A::PROFILE,
        label: A::LABEL,
        out_tx: capture.out_tx.clone(),
        terminal_id: pane.wire_terminal_id.clone(),
        stream_id: capture.stream_id,
        bootstrap_id: capture.bootstrap_id,
        limits: capture.limits,
        cancel,
        last_seq,
    };
    staging.park(
        &capture,
        &pane,
        Box::pin(async move {
            let _done = pump_done;
            pump.run(live, cut.base_seq).await;
        }),
    );
    Ok(())
}

/// Bootstrap a viewer at one cut of the newest document, then live.
#[allow(
    clippy::too_many_arguments,
    reason = "one ordered subscription/cut/retirement/publication transaction with negotiated context"
)]
pub(crate) async fn attach_document<A: DocumentApi>(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    core: phux_core::ids::ResourceId,
    handle: &crate::resource::ResourceHandle,
    out_tx: &mpsc::Sender<Outbound>,
    limits: BootstrapLimits,
    connection_token: &CancellationToken,
) -> CommandResult {
    let Ok(document) = handle.document::<A>() else {
        return CommandResult::Error {
            code: ErrorCode::WrongResourceKind,
            message: format!("not a {}", A::LABEL),
        };
    };
    let Some(bootstrap_id) = state.with_mut(|s| s.next_attach_terminal_bootstrap_id(client_id))
    else {
        return CommandResult::Error {
            code: ErrorCode::ResourceExhausted,
            message: "ATTACH_RESOURCE bootstrap id space exhausted".to_owned(),
        };
    };
    let stream_id = crate::runtime::stream_id_from(client_id.0);
    // Subscribe before the cut so nothing published between them is lost.
    let live = handle.output.subscribe();
    crate::runtime::pump::stop_output(state, client_id, core).await;
    let Some(cut) = snapshot(&document.snapshot).await else {
        return CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: format!("the {} closed before its snapshot was cut", A::LABEL),
        };
    };

    let (cancel, done, last_seq, prior) =
        state.with_mut(|s| s.replace_attach_terminal_pump(client_id, core, bootstrap_id));
    let pump_done = done.clone().drop_guard();
    if let Some((prior_id, prior_done, prior_seq)) = prior {
        prior_done.cancelled().await;
        let tombstone = FrameKind::BootstrapTombstone {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id: prior_id,
            reason: TombstoneReason::ExplicitReattach,
            last_valid_seq: prior_seq.load(Ordering::Acquire),
        };
        if out_tx.send(Outbound::Frame(tombstone)).await.is_err() {
            return CommandResult::Error {
                code: ErrorCode::ResourceExhausted,
                message: "the client went away before its replacement bootstrap".to_owned(),
            };
        }
    }
    for frame in bootstrap_frames(
        terminal_id,
        stream_id,
        bootstrap_id,
        A::PROFILE,
        &cut,
        limits,
    ) {
        if out_tx.send(Outbound::Frame(frame)).await.is_err() {
            return CommandResult::Error {
                code: ErrorCode::ResourceExhausted,
                message: "the client went away mid-bootstrap".to_owned(),
            };
        }
    }
    last_seq.store(cut.base_seq, Ordering::Release);

    let pump = LivePump {
        state: state.clone(),
        client_id,
        core,
        snapshot: document.snapshot.clone(),
        profile: A::PROFILE,
        label: A::LABEL,
        out_tx: out_tx.clone(),
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        limits,
        cancel,
        last_seq,
    };
    let connection_cancel = connection_token.clone();
    let task = tokio::task::spawn_local(async move {
        let _done = pump_done;
        tokio::select! {
            () = connection_cancel.cancelled() => {}
            () = pump.run(live, cut.base_seq) => {}
        }
    });
    state.with_mut(|s| {
        s.track_terminal_output_pump(client_id, core, task.abort_handle(), done, None);
    });
    CommandResult::Ok
}

/// The newest document, or `None` when the tree is gone.
async fn snapshot(cut: &mpsc::Sender<SnapshotRequest>) -> Option<TreeSnapshot> {
    let (reply, rx) = oneshot::channel();
    cut.send(SnapshotRequest { reply }).await.ok()?;
    rx.await.ok()
}

/// The bootstrap frame run for one cut: BEGIN, the document in chunks,
/// then READY. Before the first publish there is no document and no chunk.
fn bootstrap_frames(
    terminal_id: &phux_protocol::ids::ResourceId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    profile: BootstrapStreamProfile,
    cut: &TreeSnapshot,
    limits: BootstrapLimits,
) -> impl Iterator<Item = FrameKind> {
    let budget = usize::try_from(limits.max_chunk_bytes())
        .unwrap_or(64 * 1024)
        .max(1);
    let begin = FrameKind::BootstrapBegin {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        profile,
        cols: 0,
        rows: 0,
        base_seq: cut.base_seq,
    };
    let document = cut.document.clone().unwrap_or_default();
    let owner = terminal_id.clone();
    let chunks = (0..document.len())
        .step_by(budget)
        .enumerate()
        .map(move |(index, start)| FrameKind::BootstrapChunk {
            terminal_id: owner.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: u32::try_from(index).unwrap_or(u32::MAX),
            payload: document.slice(start..document.len().min(start + budget)),
        });
    let ready = FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        history_cursor: None,
    };
    std::iter::once(begin)
        .chain(chunks)
        .chain(std::iter::once(ready))
}

/// One viewer's live subscription to a document.
struct LivePump {
    profile: BootstrapStreamProfile,
    label: &'static str,
    state: SharedState,
    client_id: ClientId,
    core: phux_core::ids::ResourceId,
    snapshot: mpsc::Sender<SnapshotRequest>,
    out_tx: mpsc::Sender<Outbound>,
    terminal_id: phux_protocol::ids::ResourceId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    limits: BootstrapLimits,
    cancel: CancellationToken,
    /// The last document delivered, shared with a replacing attach's tombstone.
    last_seq: Arc<AtomicU64>,
}

impl LivePump {
    /// Forward documents after `base_seq` until the viewer detaches, goes
    /// away, or the tree closes.
    async fn run(mut self, mut live: broadcast::Receiver<ResourceOutput>, base_seq: u64) {
        let mut cursor = base_seq;
        loop {
            let output = tokio::select! {
                () = self.cancel.cancelled() => return,
                received = live.recv() => received,
            };
            let next = match output {
                Ok(ResourceOutput::Live { seq, .. }) if seq <= cursor => Some(cursor),
                Ok(ResourceOutput::Live { seq, bytes, .. }) => {
                    self.forward(seq, bytes).await.then_some(seq)
                }
                Ok(ResourceOutput::Resync { .. } | ResourceOutput::Control { .. }) => Some(cursor),
                Err(RecvError::Lagged(skipped)) => self.recut(cursor, skipped).await,
                Err(RecvError::Closed) => None,
            };
            let Some(next) = next else { return };
            cursor = next;
        }
    }

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

    /// A viewer the broadcast lapped wants the newest document, not the
    /// ones it missed: fence the generation at the last document delivered
    /// and bootstrap it again from a fresh cut.
    async fn recut(&mut self, cursor: u64, skipped: u64) -> Option<u64> {
        if self.cancel.is_cancelled() {
            return None;
        }
        let cut = snapshot(&self.snapshot).await?;
        warn!(
            resource = %self.terminal_id,
            kind = self.label,
            skipped,
            last_delivered = cursor,
            newest = cut.base_seq,
            "document viewer fell behind; recut from the newest document"
        );
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
            self.profile,
            &cut,
            self.limits,
        ) {
            if self.cancel.is_cancelled() || !self.send(frame).await {
                return None;
            }
        }
        self.last_seq.store(cut.base_seq, Ordering::Release);
        Some(cut.base_seq)
    }
}
