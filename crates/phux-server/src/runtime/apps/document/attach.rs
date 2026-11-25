//! Bootstrapping a viewer onto a document resource: the newest document
//! as one cut, then every later document live. A viewer that falls behind
//! is fenced and recut from the newest document: it skips intermediate
//! documents, never silently.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::engine::{DocumentResource as _, SnapshotRequest, TreeSnapshot};
use bytes::Bytes;
use phux_protocol::caps::{BootstrapLimits, BootstrapStreamProfile};
use phux_protocol::wire::frame::{CommandResult, ErrorCode, FrameKind};

use super::DocumentApi;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::mailbox::Outbound;
use crate::runtime::apps::producer_stream::{
    self, LagRecovery, PumpCore, StreamAttach, StreamCut, StreamIds,
};
use crate::runtime::session_attach::{
    SessionCapture,
    staging::{CaptureFailure, SessionStaging},
};
use crate::state::ClientId;

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
    let cut = DocumentCut::of::<A>(&document.target, &cut);
    let bytes = cut.payload.len();
    if bytes > staging.budget.remaining_bytes() {
        return Err("document exceeds aggregate bootstrap source budget"
            .to_owned()
            .into());
    }
    if let Some(bytes) = &cut.document {
        A::validate_bootstrap(&document.target, bytes).map_err(CaptureFailure::Failed)?;
    }
    let ids = StreamIds {
        terminal_id: pane.wire_terminal_id.clone(),
        stream_id: capture.stream_id,
        bootstrap_id: capture.bootstrap_id,
    };
    let frames =
        staging.collect_frames(producer_stream::bootstrap_frames(&cut, ids, capture.limits))?;
    staging.append(frames, bytes)?;
    let (cancel, done, last_seq, prior) = capture.host.replace(pane.terminal_id, capture.bootstrap_id);
    if let Some((_, done, _)) = prior {
        done.cancelled().await;
    }
    last_seq.store(cut.base_seq, Ordering::Release);
    let pump_done = done.drop_guard();
    let pump = PumpCore {
        host: capture.host.clone(),
        core: pane.terminal_id,
        out_tx: capture.out_tx.clone(),
        terminal_id: pane.wire_terminal_id.clone(),
        stream_id: capture.stream_id,
        bootstrap_id: capture.bootstrap_id,
        limits: capture.limits,
        cancel,
        last_seq,
    };
    let recovery = Recut::of::<A>(document.snapshot.clone(), document.target.clone());
    let base_seq = cut.base_seq;
    staging.park(
        &capture,
        &pane,
        Box::pin(async move {
            let _done = pump_done;
            pump.run(recovery, live, base_seq).await;
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
    host: &crate::runtime::apps::StreamHost,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    core: phux_core::ids::ResourceId,
    handle: &crate::resource::ResourceHandle,
    out_tx: &mpsc::Sender<Outbound>,
    limits: BootstrapLimits,
    connection_token: &CancellationToken,
) -> CommandResult {
    let Ok(document) = handle.document::<A>() else {
        return CommandResult::error(ErrorCode::WrongResourceKind, format!("not a {}", A::LABEL));
    };
    let attach = StreamAttach {
        host,
        client_id,
        terminal_id,
        core,
        handle,
        out_tx,
        limits,
        connection_token,
    };
    let recovery = Recut::of::<A>(document.snapshot.clone(), document.target.clone());
    let cut = async {
        let Some(cut) = snapshot(&document.snapshot).await else {
            return Err(CommandResult::error(
                ErrorCode::TerminalNotFound,
                format!("the {} closed before its snapshot was cut", A::LABEL),
            ));
        };
        Ok(DocumentCut::of::<A>(&document.target, &cut))
    };
    producer_stream::attach_stream(attach, cut, recovery).await
}

/// The newest document, or `None` when the tree is gone.
async fn snapshot(cut: &mpsc::Sender<SnapshotRequest>) -> Option<TreeSnapshot> {
    let (reply, rx) = oneshot::channel();
    cut.send(SnapshotRequest { reply }).await.ok()?;
    rx.await.ok()
}

/// One cut of a document, framed as its kind frames it. An empty payload
/// (a tree or surface before its first publish) has no chunk.
struct DocumentCut {
    base_seq: u64,
    profile: BootstrapStreamProfile,
    /// The document as cut, for the staging check.
    document: Option<Bytes>,
    payload: Bytes,
}

impl DocumentCut {
    fn of<A: DocumentApi>(target: &str, cut: &TreeSnapshot) -> Self {
        Self {
            base_seq: cut.base_seq,
            profile: A::PROFILE,
            document: cut.document.clone(),
            payload: A::bootstrap_payload(target, cut.document.as_ref()),
        }
    }
}

impl StreamCut for DocumentCut {
    fn base_seq(&self) -> u64 {
        self.base_seq
    }

    fn profile(&self) -> BootstrapStreamProfile {
        self.profile
    }

    /// The payload in byte slices of the negotiated budget.
    fn chunks(&self, ids: StreamIds, limits: BootstrapLimits) -> impl Iterator<Item = FrameKind> {
        let budget = usize::try_from(limits.max_chunk_bytes())
            .unwrap_or(64 * 1024)
            .max(1);
        let payload = self.payload.clone();
        (0..payload.len())
            .step_by(budget)
            .enumerate()
            .map(move |(index, start)| {
                let end = payload.len().min(start + budget);
                ids.chunk(
                    u32::try_from(index).unwrap_or(u32::MAX),
                    payload.slice(start..end),
                )
            })
    }
}

/// A viewer the broadcast lapped wants the newest document, not the ones it
/// missed: fence the generation at the last document delivered and
/// bootstrap it again from a fresh cut.
struct Recut {
    snapshot: mpsc::Sender<SnapshotRequest>,
    /// What the resource was opened on, and how a recut frames its payload.
    target: Arc<str>,
    payload: fn(&str, Option<&Bytes>) -> Bytes,
    profile: BootstrapStreamProfile,
    label: &'static str,
}

impl Recut {
    fn of<A: DocumentApi>(snapshot: mpsc::Sender<SnapshotRequest>, target: Arc<str>) -> Self {
        Self {
            snapshot,
            target,
            payload: A::bootstrap_payload,
            profile: A::PROFILE,
            label: A::LABEL,
        }
    }
}

impl LagRecovery for Recut {
    async fn recover(&self, pump: &mut PumpCore, cursor: u64, skipped: u64) -> Option<u64> {
        let cut = snapshot(&self.snapshot).await?;
        warn!(
            resource = %pump.terminal_id,
            kind = self.label,
            skipped,
            last_delivered = cursor,
            newest = cut.base_seq,
            "document viewer fell behind; recut from the newest document"
        );
        let cut = DocumentCut {
            base_seq: cut.base_seq,
            profile: self.profile,
            payload: (self.payload)(&self.target, cut.document.as_ref()),
            document: cut.document,
        };
        pump.republish(cursor, &cut).await.ok()
    }
}
