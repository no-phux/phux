//! `ATTACH`, `SPAWN_RESOURCE`, and `MOVE_RESOURCE` handling, plus the per-pane
//! output pumps that publish bootstrap generations and live output.

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use phux_core::ResourceId;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ClientCapabilities,
};
use phux_protocol::ids::{BootstrapId, GroupId, StreamId};
use phux_protocol::wire::frame::{
    AttachTarget, DetachReason, ErrorCode, FrameKind, MAX_AGENT_SESSION_RECORD_BYTES, MoveError,
    MoveResult, SpawnError, SpawnResult,
};
use std::ops::ControlFlow;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use tracing::{debug, trace, warn};

use super::{
    SpawnOwnership, prepare_attach, seed_session_with_actor, seed_session_with_pty_and_colors,
    send_error, spawn_pane_with_pty_and_colors,
};
use crate::hub::relay::SatelliteSpawn;
use crate::resource::ResourceHandle;
use crate::runtime::pump::{self, PumpGeneration};
use crate::state::{AttachSnapshotPane, ClientId, Outbound, SharedState};
use crate::terminal_actor::{
    ConsumerAttachRequest, ConsumerDetachRequest, PaneOutput, PwdRequest, ResizeRequest,
    ResizeSender, ResyncAudience, ResyncTarget, SetDefaultColorsRequest, SnapshotRequest,
};

/// Adapt a chunk to a client's capabilities without copying when neither
/// the capabilities nor the bytes require rewriting.
pub(crate) fn downsample_for_caps(
    bytes: &bytes::Bytes,
    caps: phux_protocol::ClientCapabilities,
) -> bytes::Bytes {
    if crate::downsample::caps_pass_through(caps) || memchr::memchr(0x1b, bytes).is_none() {
        bytes.clone()
    } else {
        crate::downsample::rewrite_bytes_with_caps(bytes, caps).into()
    }
}

/// Preserve native live bytes exactly; only synthesized profiles may adapt
/// presentation capabilities.
pub(crate) fn live_bytes_for_profile(
    bytes: &bytes::Bytes,
    caps: phux_protocol::ClientCapabilities,
    profile: phux_protocol::caps::BootstrapStreamProfile,
) -> bytes::Bytes {
    if matches!(
        profile,
        phux_protocol::caps::BootstrapStreamProfile::NativeState { .. }
    ) {
        bytes.clone()
    } else {
        downsample_for_caps(bytes, caps)
    }
}

pub(crate) fn bootstrap_source_ceiling(
    remaining_bytes: usize,
    caps: phux_protocol::ClientCapabilities,
) -> usize {
    if crate::downsample::caps_pass_through(caps) {
        remaining_bytes
    } else {
        // The source and one equally bounded rewrite are live together.
        remaining_bytes / 2
    }
}

#[derive(Debug)]
pub(crate) struct AdaptedBootstrap {
    pub(crate) payloads: Vec<bytes::Bytes>,
    retained_bytes: usize,
    peak_bytes: usize,
}

pub(crate) fn adapt_bootstrap_snapshot(
    snapshot: crate::grid::SnapshotBytes,
    caps: phux_protocol::ClientCapabilities,
    peak_budget: usize,
) -> Result<AdaptedBootstrap, ()> {
    let sources = [snapshot.scrollback, snapshot.bytes];
    let mut remaining_source = sources
        .iter()
        .try_fold(0_usize, |total, source| {
            total.checked_add(source.capacity())
        })
        .ok_or(())?;
    let passthrough = crate::downsample::caps_pass_through(caps);
    if remaining_source > bootstrap_source_ceiling(peak_budget, caps) {
        return Err(());
    }
    let mut peak_bytes = remaining_source;

    let mut retained_output = 0_usize;
    let mut payloads = Vec::new();
    payloads.try_reserve(2).map_err(|_| ())?;
    for source in sources {
        if source.is_empty() {
            remaining_source = remaining_source.checked_sub(source.capacity()).ok_or(())?;
            continue;
        }
        let source_capacity = source.capacity();
        let (output, output_allocation) = if passthrough {
            (bytes::Bytes::from(source), source_capacity)
        } else {
            let rewritten = crate::downsample::rewrite_bytes_with_caps(&source, caps);
            let output_allocation = rewritten.capacity();
            if output_allocation > source_capacity {
                return Err(());
            }
            let peak = retained_output
                .checked_add(remaining_source)
                .and_then(|bytes| bytes.checked_add(output_allocation))
                .ok_or(())?;
            if peak > peak_budget {
                return Err(());
            }
            peak_bytes = peak_bytes.max(peak);
            drop(source);
            (bytes::Bytes::from(rewritten), output_allocation)
        };
        remaining_source = remaining_source.checked_sub(source_capacity).ok_or(())?;
        retained_output = retained_output.checked_add(output_allocation).ok_or(())?;
        payloads.push(output);
    }
    Ok(AdaptedBootstrap {
        payloads,
        retained_bytes: retained_output,
        peak_bytes,
    })
}

pub(crate) const fn bootstrap_stream_profile(
    profile: BootstrapProfile,
) -> Option<BootstrapStreamProfile> {
    match profile {
        BootstrapProfile::NativeState { codec, .. } => {
            Some(BootstrapStreamProfile::NativeState { codec })
        }
        BootstrapProfile::SynthesizedVtStateSync => {
            Some(BootstrapStreamProfile::SynthesizedVtStateSync)
        }
        BootstrapProfile::SynthesizedVtRaw => Some(BootstrapStreamProfile::SynthesizedVtRaw),
        _ => None,
    }
}

pub(crate) const fn stream_id_from(raw: u64) -> StreamId {
    match StreamId::new(raw.saturating_add(1)) {
        Some(id) => id,
        None => unreachable!(),
    }
}

const fn initial_bootstrap_id() -> BootstrapId {
    match BootstrapId::new(1) {
        Some(id) => id,
        None => unreachable!(),
    }
}

pub(crate) const fn next_bootstrap_id(id: BootstrapId) -> BootstrapId {
    let raw = match id.get().checked_add(1) {
        Some(raw) => raw,
        None => 1,
    };
    match BootstrapId::new(raw) {
        Some(next) => next,
        None => unreachable!(),
    }
}

pub(crate) struct OutputPumpStart {
    pub(crate) published_cut: u64,
    pub(crate) replay: Vec<(u64, bytes::Bytes)>,
    pub(crate) live: Option<tokio::sync::broadcast::Receiver<PaneOutput>>,
}

struct SnapshotGate {
    terminal_id: ResourceId,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    wire_terminal_id: phux_protocol::ids::ResourceId,
    /// Terminal facet, for the native publication fence.
    terminal: crate::terminal_actor::TerminalHandle,
    gate: oneshot::Sender<OutputPumpStart>,
    cut: Option<u64>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    native_cursor: Option<crate::native_state::OpaqueHistoryCursor>,
}

/// Connection-wide retention ceiling for an aggregate ATTACH: every pane's
/// bootstrap is held until the atomic publication, so the aggregate is capped
/// at one maximal native prefix rather than scaling with the pane count.
pub(crate) const MAX_STAGED_BOOTSTRAP_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_STAGED_BOOTSTRAP_FRAMES: usize = 4_096 + 2;

/// Maximum panes in one aggregate bootstrap: each needs at least
/// `BEGIN`/`CHUNK`/`READY`, so more cannot fit the frame budget.
pub(crate) const MAX_AGGREGATE_BOOTSTRAP_PANES: usize = MAX_STAGED_BOOTSTRAP_FRAMES / 3;

#[derive(Debug)]
struct BootstrapStagingBudget {
    max_bytes: usize,
    max_frames: usize,
    staged_bytes: usize,
    staged_frames: usize,
}

impl BootstrapStagingBudget {
    const fn with_limits(max_bytes: usize, max_frames: usize) -> Self {
        Self {
            max_bytes,
            max_frames,
            staged_bytes: 0,
            staged_frames: 0,
        }
    }

    const fn remaining_bytes(&self) -> usize {
        self.max_bytes.saturating_sub(self.staged_bytes)
    }

    const fn remaining_frames(&self) -> usize {
        self.max_frames.saturating_sub(self.staged_frames)
    }

    fn append_accounted(
        &mut self,
        staged: &mut Vec<FrameKind>,
        incoming: &mut Vec<FrameKind>,
        incoming_bytes: usize,
    ) -> Result<(), ()> {
        let incoming_frames = incoming.len();
        let next_frames = self.staged_frames.checked_add(incoming_frames).ok_or(())?;
        let next_bytes = self.staged_bytes.checked_add(incoming_bytes).ok_or(())?;
        if next_frames > self.max_frames || next_bytes > self.max_bytes {
            return Err(());
        }
        staged.try_reserve(incoming_frames).map_err(|_| ())?;
        staged.append(incoming);
        self.staged_frames = next_frames;
        self.staged_bytes = next_bytes;
        Ok(())
    }
}

/// Is `profile` publishing native libghostty checkpoints?
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
const fn publishes_native_checkpoints(profile: BootstrapStreamProfile) -> bool {
    matches!(
        profile,
        BootstrapStreamProfile::NativeState {
            codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1
        }
    )
}

/// Why a native checkpoint or publication request produced no reply.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub(crate) enum NativeRequestFailure {
    /// The actor mailbox is closed.
    Unsent,
    /// The actor dropped the reply.
    Dropped,
    /// The actor refused the request.
    Refused(crate::native_state::NativeStateError),
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
impl NativeRequestFailure {
    /// The pane's actor is gone: the pane exited, and `RESOURCE_CLOSED` is
    /// its end. No generation of it can be published, and that is no fault
    /// of the connection.
    const fn actor_gone(&self) -> bool {
        matches!(self, Self::Unsent | Self::Dropped)
    }
}

/// Send the native checkpoint request `make` builds and await its reply.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
async fn request_native_checkpoint(
    terminal: &crate::terminal_actor::TerminalHandle,
    make: impl FnOnce(
        oneshot::Sender<
            Result<
                crate::terminal_actor::NativeBootstrapReply,
                crate::native_state::NativeStateError,
            >,
        >,
    ) -> crate::terminal_actor::NativeBootstrapRequest,
) -> Result<crate::terminal_actor::NativeBootstrapReply, NativeRequestFailure> {
    let (reply_tx, reply_rx) = oneshot::channel();
    terminal
        .native_bootstrap
        .send(make(reply_tx))
        .await
        .map_err(|_| NativeRequestFailure::Unsent)?;
    reply_rx
        .await
        .map_err(|_| NativeRequestFailure::Dropped)?
        .map_err(NativeRequestFailure::Refused)
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(crate) async fn publish_native_bootstrap(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    reply: crate::terminal_actor::NativeBootstrapReply,
) -> Result<(u64, crate::native_state::OpaqueHistoryCursor), ()> {
    let cut = reply.base_seq;
    let cursor = reply.publication_cursor;
    for frame in reply.frames {
        out_tx.send(Outbound::Frame(frame)).await.map_err(|_| ())?;
    }
    Ok((cut, cursor))
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(crate) async fn activate_native_publication(
    handle: &crate::terminal_actor::TerminalHandle,
    owner: u64,
    terminal_id: phux_protocol::ids::ResourceId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    cursor: crate::native_state::OpaqueHistoryCursor,
) -> Result<crate::terminal_actor::NativePublicationReply, NativeRequestFailure> {
    let (reply, publication) = oneshot::channel();
    handle
        .native_publication
        .send(crate::terminal_actor::NativePublicationRequest {
            owner,
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            reply,
        })
        .await
        .map_err(|_| NativeRequestFailure::Unsent)?;
    publication
        .await
        .map_err(|_| NativeRequestFailure::Dropped)?
        .map_err(NativeRequestFailure::Refused)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn synthesized_bootstrap_frames(
    terminal_id: phux_protocol::ids::ResourceId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    profile: BootstrapStreamProfile,
    limits: BootstrapLimits,
    cols: u16,
    rows: u16,
    base_seq: u64,
    payloads: impl IntoIterator<Item = bytes::Bytes>,
) -> Result<Vec<FrameKind>, ()> {
    let mut frames = Vec::new();
    frames.try_reserve(2).map_err(|_| ())?;
    frames.push(FrameKind::BootstrapBegin {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        profile,
        cols,
        rows,
        base_seq,
    });
    let max_chunk = usize::try_from(limits.max_chunk_bytes()).map_err(|_| ())?;
    if max_chunk == 0 {
        return Err(());
    }
    let mut chunk_seq = 0_u32;
    for payload in payloads {
        let mut offset = 0_usize;
        while offset < payload.len() {
            let end = offset.saturating_add(max_chunk).min(payload.len());
            frames.try_reserve(1).map_err(|_| ())?;
            frames.push(FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload: payload.slice(offset..end),
            });
            chunk_seq = chunk_seq.checked_add(1).ok_or(())?;
            offset = end;
        }
    }
    frames.try_reserve(1).map_err(|_| ())?;
    frames.push(FrameKind::BootstrapReady {
        terminal_id,
        stream_id,
        bootstrap_id,
        history_cursor: None,
    });
    Ok(frames)
}

/// How a resync bootstrap met the consumer mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotQueue {
    /// Every bootstrap frame is on the mailbox, with nothing else between them.
    Queued,
    /// The mailbox is full and this is not the final grid: the pump stays
    /// fenced and keeps draining, so a later exit snapshot still reaches it.
    Deferred,
    /// The consumer is gone.
    Closed,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn send_synthesized_bootstrap(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    terminal_id: phux_protocol::ids::ResourceId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    profile: BootstrapStreamProfile,
    limits: BootstrapLimits,
    cols: u16,
    rows: u16,
    base_seq: u64,
    payloads: impl IntoIterator<Item = bytes::Bytes>,
) -> Result<(), ()> {
    let frames = synthesized_bootstrap_frames(
        terminal_id,
        stream_id,
        bootstrap_id,
        profile,
        limits,
        cols,
        rows,
        base_seq,
        payloads,
    )?;
    send_frames_contiguously(out_tx, &frames).await
}

/// Queue a resync bootstrap.
///
/// A fenced pump defers a non-final snapshot rather than park on a full
/// mailbox: parking would leave the later exit snapshot unread when
/// `RESOURCE_CLOSED` takes the next slot, losing the final grid. The exit
/// snapshot (and an unfenced resize) always waits, reserving every frame at
/// once when it fits so close cannot split `BEGIN` from its chunk; one larger
/// than the mailbox goes frame by frame.
pub(crate) async fn queue_resync_bootstrap(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    reason: crate::terminal_actor::ResyncReason,
    frames: Vec<FrameKind>,
    defer_if_full: bool,
) -> SnapshotQueue {
    if frames.is_empty() {
        return SnapshotQueue::Queued;
    }
    let fits = frames.len() <= out_tx.max_capacity();
    let must_deliver =
        !fits || matches!(reason, crate::terminal_actor::ResyncReason::Exit) || !defer_if_full;
    if !must_deliver {
        return match try_queue_frames(out_tx, &frames) {
            Ok(()) => SnapshotQueue::Queued,
            Err(QueueFramesError::Full) => SnapshotQueue::Deferred,
            Err(QueueFramesError::Closed) => SnapshotQueue::Closed,
        };
    }
    match try_queue_frames(out_tx, &frames) {
        Ok(()) => SnapshotQueue::Queued,
        Err(QueueFramesError::Closed) => SnapshotQueue::Closed,
        Err(QueueFramesError::Full) => match send_frames_contiguously(out_tx, &frames).await {
            Ok(()) => SnapshotQueue::Queued,
            Err(()) => SnapshotQueue::Closed,
        },
    }
}

enum QueueFramesError {
    Full,
    Closed,
}

/// Reserve every frame, then send them. A later `send` cannot take a slot
/// in the middle of the bootstrap.
fn try_queue_frames(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    frames: &[FrameKind],
) -> Result<(), QueueFramesError> {
    if frames.is_empty() {
        return Ok(());
    }
    if frames.len() > out_tx.max_capacity() {
        return Err(QueueFramesError::Full);
    }
    let mut permits = out_tx
        .try_reserve_many(frames.len())
        .map_err(|err| match err {
            tokio::sync::mpsc::error::TrySendError::Full(()) => QueueFramesError::Full,
            tokio::sync::mpsc::error::TrySendError::Closed(()) => QueueFramesError::Closed,
        })?;
    for frame in frames {
        let Some(permit) = permits.next() else {
            return Err(QueueFramesError::Closed);
        };
        permit.send(Outbound::Frame(frame.clone()));
    }
    Ok(())
}

async fn send_frames_contiguously(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    frames: &[FrameKind],
) -> Result<(), ()> {
    if frames.is_empty() {
        return Ok(());
    }
    if frames.len() <= out_tx.max_capacity() {
        let Ok(mut permits) = out_tx.reserve_many(frames.len()).await else {
            return Err(());
        };
        for frame in frames {
            let Some(permit) = permits.next() else {
                return Err(());
            };
            permit.send(Outbound::Frame(frame.clone()));
        }
        return Ok(());
    }
    for frame in frames {
        out_tx
            .send(Outbound::Frame(frame.clone()))
            .await
            .map_err(|_| ())?;
    }
    Ok(())
}

/// Queue the in-band resync after a broadcast gap, addressed to `pump` alone
/// so other consumers keep their generation. The pump forwards nothing until
/// the actor accepts it; a closed or persistently full mailbox fails
/// boundedly.
pub(crate) async fn enqueue_output_resync(resize: &ResizeSender, pump: ResyncTarget) -> bool {
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            resize.send(ResizeRequest {
                cols: 0,
                rows: 0,
                cell_px: None,
                resync_clients: true,
                resync_only: true,
                resync_for: Some(pump),
            }),
        )
        .await,
        Ok(Ok(()))
    )
}

/// Why an output pump stopped. The caller decides what to release: a
/// `SPAWN_RESOURCE` pump owns its pane, an ATTACH pump shares it.
#[derive(Debug, Clone, Copy)]
pub(crate) enum PumpFault {
    /// The client's outbound mailbox closed. Nothing is left to serve.
    OutboundClosed,
    /// A `BOOTSTRAP_TOMBSTONE` could not be queued ahead of the replacement
    /// generation, so the client can never learn the old one is void.
    TombstoneNotQueued,
    /// The replacement capture, its publication, or the in-band gap resync
    /// failed: the published generation is unrecoverable.
    GenerationLost,
    /// The actor refused to activate the replacement publication after its
    /// bootstrap frames were already queued.
    PublicationNotActivated,
    /// The outbound mailbox closed mid-replay. The pump abandons the task
    /// without touching shared state.
    ReplayAbandoned,
    /// The pane's actor was gone when the pump asked it for a replacement:
    /// `RESOURCE_CLOSED` is the pane's end; the connection is fine.
    PaneGone,
}

/// Whether a broadcast control frame belongs to a pump's current generation,
/// and whether forwarding it ends that generation.
#[derive(Debug, Clone, Copy)]
struct ControlDisposition {
    /// The frame names this pump's terminal, stream, and generation.
    targets_pump: bool,
    /// Forwarding it voids the generation (a bootstrap, not a history,
    /// tombstone).
    ends_generation: bool,
}

/// The actor's full-grid resync payload: a control event that replaces the
/// published generation rather than extending it.
#[derive(Debug)]
struct PaneResync {
    /// Post-reflow grid width the client mirror adopts.
    cols: u16,
    /// Post-reflow grid height the client mirror adopts.
    rows: u16,
    /// Synthesized grid replay for the replacement bootstrap.
    bytes: bytes::Bytes,
    /// Why the prior generation cannot continue.
    reason: crate::terminal_actor::ResyncReason,
    /// Actor-global raw sequence included by the replacement cut.
    base_seq: u64,
}

/// Map the actor's resync cause onto its wire tombstone reason.
const fn tombstone_reason_for(
    reason: crate::terminal_actor::ResyncReason,
) -> phux_protocol::wire::frame::TombstoneReason {
    match reason {
        crate::terminal_actor::ResyncReason::Resize => {
            phux_protocol::wire::frame::TombstoneReason::Resize
        }
        crate::terminal_actor::ResyncReason::OutboundGap
        | crate::terminal_actor::ResyncReason::Exit => {
            phux_protocol::wire::frame::TombstoneReason::OutboundGap
        }
    }
}

/// Everything one output pump needs for the life of its subscription: the
/// client it serves, the pane it reads, and the negotiated bootstrap shape.
pub(crate) struct OutputPumpContext {
    /// This client's outbound mailbox.
    pub(crate) out_tx: tokio::sync::mpsc::Sender<Outbound>,
    /// Where a lagged pump asks the actor for an in-band resync addressed to
    /// it ([`Self::resync_target`]).
    pub(crate) resize: ResizeSender,
    /// Wire identity of the pane being pumped.
    pub(crate) wire_terminal_id: phux_protocol::ids::ResourceId,
    /// Stream this pump publishes on.
    pub(crate) stream_id: StreamId,
    /// Generation the first published bootstrap carries.
    pub(crate) initial_bootstrap_id: BootstrapId,
    /// Owner of the ordered control frames this pump must honour.
    pub(crate) client_id: ClientId,
    /// Negotiated capabilities every payload is adapted to.
    pub(crate) client_caps: ClientCapabilities,
    /// Negotiated bootstrap stream profile.
    pub(crate) profile: BootstrapStreamProfile,
    /// Negotiated bootstrap bounds.
    pub(crate) limits: BootstrapLimits,
    /// How this pump names itself in the broadcast-lag warning.
    pub(crate) lag_label: &'static str,
    /// Skip a slow consumer to a fresh checkpoint once a chunk is older than
    /// [`pump::STALE_OUTPUT_BUDGET`].
    pub(crate) stale_skip: bool,
    /// Ends the pump between events (a ready event is still handled first).
    pub(crate) cancel: Option<CancellationToken>,
    /// Kept at the last forwarded sequence, so a replacement generation can
    /// tombstone this one at the right point.
    pub(crate) last_seq: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    /// Terminal facet used for native checkpoint capture and publication.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(crate) terminal: crate::terminal_actor::TerminalHandle,
}

impl OutputPumpContext {
    /// Publish the generation's last forwarded sequence, when tracked.
    fn publish_last_seq(&self, generation: &PumpGeneration) {
        if let Some(last_seq) = &self.last_seq {
            last_seq.store(
                generation.last_forwarded_seq(),
                std::sync::atomic::Ordering::Release,
            );
        }
    }

    /// How a gap resync names this pump, and the generation it replaces, to
    /// the actor.
    const fn resync_target(&self, generation: &PumpGeneration) -> ResyncTarget {
        ResyncTarget {
            owner: self.client_id.0,
            stream_id: self.stream_id,
            bootstrap_id: generation.bootstrap_id(),
        }
    }

    /// Replace the published generation for a resync addressed to this pump,
    /// and skip one addressed to other pumps.
    async fn apply_resync(
        &self,
        generation: &mut PumpGeneration,
        output_rx: &mut tokio::sync::broadcast::Receiver<PaneOutput>,
        audience: &ResyncAudience,
        resync: &PaneResync,
    ) -> ControlFlow<Option<PumpFault>> {
        if !generation.takes_resync(audience, self.resync_target(generation)) {
            return ControlFlow::Continue(());
        }
        self.republish_generation(generation, output_rx, resync)
            .await
    }

    /// Frame one output chunk for this client, adapted to its capabilities.
    fn output_frame(
        &self,
        generation: &PumpGeneration,
        seq: u64,
        bytes: &bytes::Bytes,
    ) -> FrameKind {
        FrameKind::ResourceOutput {
            terminal_id: self.wire_terminal_id.clone(),
            stream_id: self.stream_id,
            bootstrap_id: generation.bootstrap_id(),
            seq,
            bytes: live_bytes_for_profile(bytes, self.client_caps, self.profile),
        }
    }

    /// Forward the backlog a publication captured behind its cut, skipping
    /// anything the published bootstrap already covered.
    async fn forward_gated_replay(
        &self,
        generation: &mut PumpGeneration,
        replay: Vec<(u64, bytes::Bytes)>,
    ) -> Result<(), PumpFault> {
        for (seq, bytes) in replay {
            if !generation.forwards(seq) {
                continue;
            }
            let frame = self.output_frame(generation, seq, &bytes);
            if self.out_tx.send(Outbound::Frame(frame)).await.is_err() {
                return Err(PumpFault::ReplayAbandoned);
            }
            generation.note_forwarded(seq);
        }
        Ok(())
    }

    /// Forward one live PTY chunk, dropping anything a tombstone voided or the
    /// published bootstrap already covered. A full mailbox is a gap: fence and
    /// skip to a fresh screen rather than park off the broadcast.
    async fn forward_live(
        &self,
        generation: &mut PumpGeneration,
        seq: u64,
        bytes: &bytes::Bytes,
    ) -> ControlFlow<Option<PumpFault>> {
        if !generation.forwards(seq) {
            return ControlFlow::Continue(());
        }
        let frame = self.output_frame(generation, seq, bytes);
        match pump::try_send_frame(&self.out_tx, frame) {
            pump::MailboxForward::Sent => {
                crate::perf::PUMP_FRAMES.incr();
                crate::perf::PUMP_BYTES.add_len(bytes.len());
                crate::perf::PUMP_FRAME_BYTES.record_len(bytes.len());
                generation.note_forwarded(seq);
                self.publish_last_seq(generation);
                ControlFlow::Continue(())
            }
            pump::MailboxForward::Closed => ControlFlow::Break(Some(PumpFault::OutboundClosed)),
            pump::MailboxForward::Full => {
                self.request_gap_resync(generation, GapCause::Backpressure)
                    .await
            }
        }
    }

    /// Does this ordered control frame name this pump's terminal, stream, and
    /// generation — and does forwarding it end that generation?
    fn classify_control(&self, frame: &FrameKind, bootstrap_id: BootstrapId) -> ControlDisposition {
        let (terminal_id, control_stream_id, control_bootstrap_id, ends_generation) = match frame {
            FrameKind::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } => (terminal_id, *stream_id, *bootstrap_id, true),
            FrameKind::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } => (terminal_id, *stream_id, *bootstrap_id, false),
            _ => {
                return ControlDisposition {
                    targets_pump: false,
                    ends_generation: false,
                };
            }
        };
        ControlDisposition {
            targets_pump: terminal_id == &self.wire_terminal_id
                && control_stream_id == self.stream_id
                && control_bootstrap_id == bootstrap_id,
            ends_generation,
        }
    }

    /// Forward an ordered control frame addressed to this pump's generation.
    async fn forward_control(
        &self,
        generation: &mut PumpGeneration,
        owner: u64,
        frame: FrameKind,
    ) -> ControlFlow<Option<PumpFault>> {
        if owner != self.client_id.0 {
            return ControlFlow::Continue(());
        }
        let disposition = self.classify_control(&frame, generation.bootstrap_id());
        if !disposition.targets_pump {
            return ControlFlow::Continue(());
        }
        if self.out_tx.send(Outbound::Frame(frame)).await.is_err() {
            return ControlFlow::Break(Some(PumpFault::OutboundClosed));
        }
        if disposition.ends_generation {
            generation.retire();
        }
        ControlFlow::Continue(())
    }

    /// Ask the actor for the replacement native checkpoint. Only a refused
    /// capture loses the generation; a closed mailbox or dropped reply means
    /// the actor is already gone, which ends this pump without failing the
    /// connection. `None` means the actor invalidated the cut mid-capture (a
    /// reflow, a replacement child, or teardown): a resync or the pane's
    /// close follows, so the generation is not lost.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn capture_native_checkpoint(
        &self,
        bootstrap_id: BootstrapId,
    ) -> Result<Option<crate::terminal_actor::NativeBootstrapReply>, PumpFault> {
        let captured = request_native_checkpoint(&self.terminal, |reply| {
            crate::terminal_actor::NativeBootstrapRequest {
                owner: self.client_id.0,
                terminal_id: self.wire_terminal_id.clone(),
                stream_id: self.stream_id,
                bootstrap_id,
                limits: self.limits,
                max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
                max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
                reply,
            }
        })
        .await;
        let reply = match captured {
            Ok(reply) => reply,
            Err(NativeRequestFailure::Unsent | NativeRequestFailure::Dropped) => {
                return Err(PumpFault::PaneGone);
            }
            Err(NativeRequestFailure::Refused(crate::native_state::NativeStateError::Resize)) => {
                debug!(
                    terminal_id = ?self.wire_terminal_id,
                    "native checkpoint invalidated mid-capture; awaiting the resync"
                );
                return Ok(None);
            }
            Err(NativeRequestFailure::Refused(error)) => {
                warn!(
                    terminal_id = ?self.wire_terminal_id,
                    %error,
                    "native checkpoint resync refused"
                );
                let _ = self
                    .out_tx
                    .send(Outbound::Frame(FrameKind::Error {
                        request_id: None,
                        code: ErrorCode::CodecUnavailable,
                        message: "native checkpoint resync failed".to_owned(),
                    }))
                    .await;
                return Err(PumpFault::GenerationLost);
            }
        };
        Ok(Some(reply))
    }

    /// Tombstone the live generation, publish its native replacement, and
    /// adopt the post-cut receiver the actor fenced it behind. `false` means
    /// the actor invalidated the capture: the generation stays retired.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn republish_native_generation(
        &self,
        generation: &mut PumpGeneration,
        output_rx: &mut tokio::sync::broadcast::Receiver<PaneOutput>,
        prior_bootstrap_id: BootstrapId,
        reason: crate::terminal_actor::ResyncReason,
    ) -> Result<bool, PumpFault> {
        if generation.is_active()
            && self
                .out_tx
                .send(Outbound::Frame(self.tombstone_frame(generation, reason)))
                .await
                .is_err()
        {
            return Err(PumpFault::TombstoneNotQueued);
        }
        // The id advances only once the replacement frames are queued.
        let bootstrap_id = next_bootstrap_id(prior_bootstrap_id);
        let Some(reply) = self.capture_native_checkpoint(bootstrap_id).await? else {
            generation.retire();
            return Ok(false);
        };
        let (cut, cursor) = publish_native_bootstrap(&self.out_tx, reply)
            .await
            .map_err(|()| PumpFault::GenerationLost)?;
        generation.set_bootstrap_id(bootstrap_id);
        let publication = activate_native_publication(
            &self.terminal,
            self.client_id.0,
            self.wire_terminal_id.clone(),
            self.stream_id,
            bootstrap_id,
            cursor,
        )
        .await
        .map_err(|failure| {
            // The pane exited while this pump was blocked publishing its
            // replacement to a slow consumer: the pane ended, the client
            // did not.
            if failure.actor_gone() {
                PumpFault::PaneGone
            } else {
                PumpFault::PublicationNotActivated
            }
        })?;
        // Unfence before the replay so it passes the same `forwards` gate as
        // live output: a replay entry at or behind the cut is already in the
        // checkpoint, and resending it is a `DuplicateSequence` the client
        // detaches on.
        generation.republished_at(cut);
        *output_rx = publication.live;
        self.forward_gated_replay(generation, publication.replay)
            .await?;
        self.publish_last_seq(generation);
        // Chunks queued behind the replay waited on it, not on the consumer.
        generation.restart_staleness_clock();
        Ok(true)
    }

    /// `BOOTSTRAP_TOMBSTONE` retiring the generation this pump publishes.
    fn tombstone_frame(
        &self,
        generation: &PumpGeneration,
        reason: crate::terminal_actor::ResyncReason,
    ) -> FrameKind {
        FrameKind::BootstrapTombstone {
            terminal_id: self.wire_terminal_id.clone(),
            stream_id: self.stream_id,
            bootstrap_id: generation.bootstrap_id(),
            reason: tombstone_reason_for(reason),
            last_valid_seq: generation.last_forwarded_seq(),
        }
    }

    /// Publish the synthesized-VT replacement generation for a resync. A
    /// live generation is tombstoned first (L1 §4.6), in the same queued
    /// batch, so a deferred resync retires nothing.
    async fn republish_synthesized_generation(
        &self,
        generation: &mut PumpGeneration,
        resync: &PaneResync,
    ) -> ControlFlow<Option<PumpFault>> {
        let payload = downsample_for_caps(&resync.bytes, self.client_caps);
        let bootstrap_id = next_bootstrap_id(generation.bootstrap_id());
        let Ok(mut frames) = synthesized_bootstrap_frames(
            self.wire_terminal_id.clone(),
            self.stream_id,
            bootstrap_id,
            self.profile,
            self.limits,
            resync.cols,
            resync.rows,
            resync.base_seq,
            [payload],
        ) else {
            return ControlFlow::Break(Some(PumpFault::OutboundClosed));
        };
        if generation.is_active() {
            frames.insert(0, self.tombstone_frame(generation, resync.reason));
        }
        match queue_resync_bootstrap(&self.out_tx, resync.reason, frames, generation.is_fenced())
            .await
        {
            SnapshotQueue::Queued => {
                generation.set_bootstrap_id(bootstrap_id);
                generation.republished_at(resync.base_seq);
                self.publish_last_seq(generation);
                ControlFlow::Continue(())
            }
            // Still fenced; the exit snapshot is later on this broadcast.
            SnapshotQueue::Deferred => ControlFlow::Continue(()),
            SnapshotQueue::Closed => ControlFlow::Break(Some(PumpFault::OutboundClosed)),
        }
    }

    /// Replace the published generation after the actor resynchronized the
    /// pane. Even an unchanged cut replaces it: resync is a control event.
    async fn republish_generation(
        &self,
        generation: &mut PumpGeneration,
        output_rx: &mut tokio::sync::broadcast::Receiver<PaneOutput>,
        resync: &PaneResync,
    ) -> ControlFlow<Option<PumpFault>> {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if publishes_native_checkpoints(self.profile) {
            let prior_bootstrap_id = generation.bootstrap_id();
            return match self
                .republish_native_generation(
                    generation,
                    output_rx,
                    prior_bootstrap_id,
                    resync.reason,
                )
                .await
            {
                Ok(true) => ControlFlow::Continue(()),
                Ok(false) => self.await_superseded_resync(generation).await,
                Err(fault) => ControlFlow::Break(Some(fault)),
            };
        }
        self.republish_synthesized_generation(generation, resync)
            .await
    }

    /// The actor invalidated this pump's capture and the generation is
    /// already tombstoned. Stay fenced and ask for the resync, under the gap
    /// retry budget, so a lost broadcast cannot freeze the client; a closing
    /// pane ends the pump quietly instead.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn await_superseded_resync(
        &self,
        generation: &mut PumpGeneration,
    ) -> ControlFlow<Option<PumpFault>> {
        if generation.fence_for_gap() {
            return ControlFlow::Continue(());
        }
        self.send_gap_resync(generation).await
    }

    /// A dropped broadcast window leaves the client's mirror stale: fence the
    /// generation (before the request, even if it fails, so gapped frames are
    /// never forwarded), ask for an in-band resync, and lose the generation if
    /// none can be delivered.
    async fn request_gap_resync(
        &self,
        generation: &mut PumpGeneration,
        cause: GapCause,
    ) -> ControlFlow<Option<PumpFault>> {
        crate::perf::PUMP_LAGGED.incr();
        if generation.fence_for_gap() {
            debug!(
                terminal_id = ?self.wire_terminal_id,
                %cause,
                "{} lagged again while a resync was already in flight; waiting",
                self.lag_label,
            );
            return ControlFlow::Continue(());
        } else if matches!(cause, GapCause::Stale(_)) {
            // A slow link goes stale every cycle by design; `pump.lagged`
            // counts it.
            debug!(
                terminal_id = ?self.wire_terminal_id,
                %cause,
                "{} fell behind; skipping to a fresh checkpoint",
                self.lag_label,
            );
        } else {
            warn!(
                terminal_id = ?self.wire_terminal_id,
                %cause,
                "{} lagged; requesting in-band resync",
                self.lag_label,
            );
        }
        crate::perf::PUMP_GAP_RESYNC.incr();
        self.send_gap_resync(generation).await
    }

    /// Ask the actor for this pump's resync. A gone actor ends the pump
    /// quietly (`RESOURCE_CLOSED` follows); a stuck one loses the generation.
    async fn send_gap_resync(
        &self,
        generation: &mut PumpGeneration,
    ) -> ControlFlow<Option<PumpFault>> {
        generation.note_resync_requested();
        if enqueue_output_resync(&self.resize, self.resync_target(generation)).await {
            return ControlFlow::Continue(());
        }
        if self.resize.is_closed() {
            return ControlFlow::Break(None);
        }
        self.fail_unrecoverable_gap().await
    }

    /// The resync asked for at the last gap has not arrived within
    /// [`pump::GAP_RESYNC_RETRY`]: ask again, or the fenced client would sit
    /// on a screen that never changes.
    async fn retry_gap_resync(
        &self,
        generation: &mut PumpGeneration,
    ) -> ControlFlow<Option<PumpFault>> {
        debug!(
            terminal_id = ?self.wire_terminal_id,
            attempt = generation.gap_attempts(),
            "{} is still waiting on its in-band resync; re-requesting",
            self.lag_label,
        );
        self.send_gap_resync(generation).await
    }

    /// The gap spent its whole request budget without the actor ever
    /// broadcasting a replacement generation.
    async fn abandon_unanswered_gap(
        &self,
        generation: &PumpGeneration,
    ) -> ControlFlow<Option<PumpFault>> {
        warn!(
            terminal_id = ?self.wire_terminal_id,
            attempts = generation.gap_attempts(),
            "{} never received the in-band resync it asked for; failing the generation",
            self.lag_label,
        );
        self.fail_unrecoverable_gap().await
    }

    /// Tell the client the gap is unrecoverable and end the pump.
    async fn fail_unrecoverable_gap(&self) -> ControlFlow<Option<PumpFault>> {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            self.out_tx.send(Outbound::Frame(FrameKind::Error {
                request_id: None,
                code: ErrorCode::InternalError,
                message: "terminal output gap could not be resynchronized".to_owned(),
            })),
        )
        .await;
        ControlFlow::Break(Some(PumpFault::GenerationLost))
    }
}

/// Why a pump gave up on its generation and asked for a resync.
#[derive(Debug, Clone, Copy)]
enum GapCause {
    /// The pane's broadcast overwrote this many chunks under the pump.
    Dropped(u64),
    /// The chunk in hand was read from the pane this long ago, past
    /// [`pump::STALE_OUTPUT_BUDGET`].
    Stale(std::time::Duration),
    /// The consumer mailbox was full.
    Backpressure,
}

impl std::fmt::Display for GapCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dropped(n) => write!(f, "broadcast dropped {n} chunks"),
            Self::Stale(age) => write!(f, "chunk {}ms old", age.as_millis()),
            Self::Backpressure => write!(f, "consumer mailbox full"),
        }
    }
}

/// The next broadcast event, or `None` once [`OutputPumpContext::cancel`]
/// fires. A ready event wins over cancellation: a reaped pane's fenced
/// consumer is still owed its final screen.
async fn next_wait(
    ctx: &OutputPumpContext,
    generation: &PumpGeneration,
    output_rx: &mut tokio::sync::broadcast::Receiver<PaneOutput>,
) -> Option<pump::PumpWait> {
    let Some(cancel) = &ctx.cancel else {
        return Some(pump::next_event(generation, output_rx).await);
    };
    tokio::select! {
        biased;
        wait = pump::next_event(generation, output_rx) => Some(wait),
        () = cancel.cancelled() => None,
    }
}

/// Drive one client's output subscription for a pane: park on the publication
/// gate, then run [`run_started_output_pump`]. Returns the fault that ended
/// it, `None` if orderly.
async fn run_output_pump(
    ctx: &OutputPumpContext,
    gate_rx: oneshot::Receiver<OutputPumpStart>,
    output_rx: tokio::sync::broadcast::Receiver<PaneOutput>,
) -> Option<PumpFault> {
    let start = gate_rx.await.ok()?;
    run_started_output_pump(ctx, start, output_rx).await
}

/// Replay the backlog behind the published cut, then forward live output,
/// ordered control, and generation replacements until the pane or the client
/// goes away.
pub(crate) async fn run_started_output_pump(
    ctx: &OutputPumpContext,
    start: OutputPumpStart,
    mut output_rx: tokio::sync::broadcast::Receiver<PaneOutput>,
) -> Option<PumpFault> {
    let mut generation = PumpGeneration::opened_at(start.published_cut, ctx.initial_bootstrap_id);
    if let Some(live) = start.live {
        output_rx = live;
    }
    if let Err(fault) = ctx
        .forward_gated_replay(&mut generation, start.replay)
        .await
    {
        return Some(fault);
    }
    ctx.publish_last_seq(&generation);
    // The first live chunk waited behind the attach bootstrap and its replay.
    generation.restart_staleness_clock();
    loop {
        let received = match next_wait(ctx, &generation, &mut output_rx).await? {
            pump::PumpWait::Event(received) => received,
            pump::PumpWait::RetryResync => {
                if let ControlFlow::Break(fault) = ctx.retry_gap_resync(&mut generation).await {
                    return fault;
                }
                continue;
            }
            pump::PumpWait::GapUnrecoverable => {
                if let ControlFlow::Break(fault) = ctx.abandon_unanswered_gap(&generation).await {
                    return fault;
                }
                continue;
            }
        };
        if let ControlFlow::Break(fault) =
            dispatch_output(ctx, &mut generation, &mut output_rx, received).await
        {
            return fault;
        }
    }
}

/// Handle one broadcast result for this pump's generation.
async fn dispatch_output(
    ctx: &OutputPumpContext,
    generation: &mut PumpGeneration,
    output_rx: &mut tokio::sync::broadcast::Receiver<PaneOutput>,
    received: Result<PaneOutput, tokio::sync::broadcast::error::RecvError>,
) -> ControlFlow<Option<PumpFault>> {
    match received {
        Ok(PaneOutput::Live { seq, bytes, at }) => {
            let age = generation.chunk_age(at);
            if ctx.stale_skip && pump::is_stale(age) && generation.forwards(seq) {
                // Skip a slow consumer to a fresh checkpoint rather than
                // replay a backlog that only grows.
                ctx.request_gap_resync(generation, GapCause::Stale(age))
                    .await
            } else {
                ctx.forward_live(generation, seq, &bytes).await
            }
        }
        Ok(PaneOutput::Control { owner, frame }) => {
            ctx.forward_control(generation, owner, frame).await
        }
        Ok(PaneOutput::Resync {
            cols,
            rows,
            bytes,
            reason,
            audience,
            base_seq,
        }) => {
            ctx.apply_resync(
                generation,
                output_rx,
                &audience,
                &PaneResync {
                    cols,
                    rows,
                    bytes,
                    reason,
                    base_seq,
                },
            )
            .await
        }
        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
            ctx.request_gap_resync(generation, GapCause::Dropped(n))
                .await
        }
        Err(tokio::sync::broadcast::error::RecvError::Closed) => ControlFlow::Break(None),
    }
}

/// Release a shared pane's consumer after its pump failed. The pane itself
/// belongs to others: an unrecoverable generation detaches only this client
/// and closes its connection.
pub(crate) fn release_after_pump_fault(
    fault: PumpFault,
    state: &SharedState,
    client_id: ClientId,
    connection_token: &CancellationToken,
) {
    match fault {
        PumpFault::OutboundClosed | PumpFault::ReplayAbandoned | PumpFault::PaneGone => {}
        PumpFault::TombstoneNotQueued | PumpFault::GenerationLost => {
            warn!(?client_id, ?fault, "attach pump failed; closing the client");
            crate::runtime::client::detach_and_release_consumer_state(state, client_id);
            connection_token.cancel();
        }
        PumpFault::PublicationNotActivated => {
            warn!(
                ?client_id,
                "attach pump publication not activated; closing the client"
            );
            connection_token.cancel();
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "single rollback boundary deliberately receives every staged and committed resource so cancellation, producer detach, pump abortion, and the fatal sentinel remain strictly ordered"
)]
async fn fail_aggregate_attach_prepublication(
    state: &SharedState,
    client_id: ClientId,
    attach_id: u32,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    connection_token: &CancellationToken,
    staged_handles: &[ResourceHandle],
    staged_pumps: &mut JoinSet<()>,
    committed_pumps: &mut JoinSet<()>,
    reason: &str,
) {
    staged_pumps.abort_all();
    while staged_pumps.join_next().await.is_some() {}
    super::client::abort_output_pumps(committed_pumps, client_id, "failed ATTACH").await;

    let wire_client_id = super::wire_client(client_id);
    let producer_deadline = std::time::Duration::from_secs(1);
    for handle in staged_handles {
        // Native history cuts are a Terminal facet lease.
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if let Ok(terminal) = handle.terminal() {
            let _ = tokio::time::timeout(
                producer_deadline,
                terminal
                    .native_release
                    .send(crate::terminal_actor::NativeReleaseRequest { owner: client_id.0 }),
            )
            .await;
        }
        let (reply, done) = oneshot::channel();
        if matches!(
            tokio::time::timeout(
                producer_deadline,
                handle.consumer_detach.send(ConsumerDetachRequest {
                    client_id: wire_client_id,
                    reply,
                }),
            )
            .await,
            Ok(Ok(()))
        ) {
            let _ = tokio::time::timeout(producer_deadline, done).await;
        }
    }
    crate::runtime::client::detach_and_release_consumer_state(state, client_id);

    // The writer closes right after this terminal ERROR, discarding anything
    // a surviving producer races in behind it.
    if !matches!(
        tokio::time::timeout(
            producer_deadline,
            out_tx.send(Outbound::TerminalError {
                request_id: None,
                code: ErrorCode::CodecUnavailable,
                message: format!("ATTACH {attach_id} failed before publication: {reason}"),
            }),
        )
        .await,
        Ok(Ok(()))
    ) {
        warn!(client = ?client_id, attach_id, "failed to enqueue terminal ATTACH error");
    }
    connection_token.cancel();
}

/// What `handle_attach` needs after leaving the state lock: the snapshot, the
/// initial client id, the panes to bootstrap, and snapshot participants with
/// no actor (published as `RESOURCE_CLOSED` before `ATTACH_READY`).
pub(crate) type AttachPrepared = (
    phux_protocol::wire::info::SessionSnapshot,
    phux_protocol::ids::ClientId,
    Vec<AttachSnapshotPane>,
    Vec<phux_protocol::ids::ResourceId>,
);

/// Resolve `target` to the session the attach joins: the one the dispatch
/// guard authorized, pinned by id before the first await, or the one a
/// `CreateIfMissing` creates. No name is re-resolved after an await, so a
/// concurrent rename cannot redirect the attach (workload-auth §5).
pub(crate) async fn resolve_attach_target(
    state: &SharedState,
    target: AttachTarget,
    pinned: Option<phux_core::ids::SessionId>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) -> Option<phux_core::ids::SessionId> {
    if pinned.is_some() {
        return pinned;
    }
    let refusal = match target {
        AttachTarget::ByName(name) => format!("session {name:?} not found"),
        AttachTarget::ById(id) => format!("session id {} not found", id.get()),
        AttachTarget::Last => "AttachTarget::Last has no live session to resolve".to_owned(),
        AttachTarget::CreateIfMissing { name, command, cwd } => {
            return create_attach_session(
                state,
                name,
                command,
                cwd,
                out_tx,
                root_token,
                default_colors,
            )
            .await;
        }
        _ => "unknown AttachTarget variant".to_owned(),
    };
    send_error(out_tx, ErrorCode::SessionNotFound, &refusal).await;
    None
}

/// `CreateIfMissing` whose session did not exist: create it, then pin the
/// id it was created under.
async fn create_attach_session(
    state: &SharedState,
    name: String,
    command: Option<Vec<String>>,
    cwd: Option<String>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) -> Option<phux_core::ids::SessionId> {
    let name = resolve_create_if_missing(
        state,
        name,
        command,
        cwd,
        out_tx,
        root_token,
        default_colors,
    )
    .await?;
    let created = state.with(|s| s.find_session_by_name(&name));
    if created.is_none() {
        send_error(
            out_tx,
            ErrorCode::SessionNotFound,
            &format!("session {name:?} not found"),
        )
        .await;
    }
    created
}

/// Handle [`AttachTarget::CreateIfMissing`] (SPEC §13): return `name` when
/// the session exists, otherwise seed it (PTY-backed when the server runs
/// with PTYs) and return `name` for the normal attach path.
///
/// With PTYs, a server-wide seed command wins over the wire `command`, and
/// the wire `cwd` applies only when it names an existing directory (a stale
/// client path falls back rather than failing the attach). Spawn failure is
/// reported as `SessionNotFound`: the session is not available to attach.
pub(crate) async fn resolve_create_if_missing(
    state: &SharedState,
    name: String,
    command: Option<Vec<String>>,
    cwd: Option<String>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) -> Option<String> {
    if state.with(|s| s.session_by_name(&name).is_some()) {
        debug!(session = %name, "CreateIfMissing: session already exists, attaching");
        return Some(name);
    }

    let (with_pty, override_cmd, scrollback, term) = state.with(|s| {
        (
            s.attach_create_seeds_pty(),
            s.attach_create_seed_command(),
            s.scrollback_limits(),
            s.term().to_owned(),
        )
    });

    let seed_result = if with_pty {
        let mut seed_cmd = override_cmd.unwrap_or_else(|| spawn_argv_builder(state, command));
        crate::terminal_actor::apply_spawn_cwd(&mut seed_cmd, cwd.as_deref(), &name);
        crate::terminal_actor::apply_term(&mut seed_cmd, &term);
        seed_session_with_pty_and_colors(
            state,
            &name,
            seed_cmd,
            scrollback,
            root_token,
            default_colors,
        )
    } else {
        // No child to exec a wire `command` on.
        seed_session_with_actor(state, &name, scrollback, root_token)
    };

    if let Err(err) = seed_result {
        warn!(
            session = %name,
            error = %err,
            "CreateIfMissing: failed to spawn pane actor for newly-created session",
        );
        send_error(
            out_tx,
            ErrorCode::SessionNotFound,
            &format!("CreateIfMissing: failed to create session {name:?}: {err}"),
        )
        .await;
        return None;
    }

    debug!(
        session = %name,
        pty = with_pty,
        "CreateIfMissing: created session and seeded pane"
    );
    Some(name)
}

/// A new pane's working directory from `defaults.cwd-inheritance` when the
/// spawn left `cwd` unset; `None` inherits the server's CWD.
pub(crate) async fn resolve_inherited_cwd(
    state: &SharedState,
    client_id: ClientId,
) -> Option<String> {
    match state.with(crate::state::ServerState::cwd_inheritance) {
        phux_config::CwdInheritance::InheritFocused => focused_pane_cwd(state, client_id).await,
        phux_config::CwdInheritance::Home => std::env::var("HOME").ok().filter(|h| !h.is_empty()),
        phux_config::CwdInheritance::SessionRoot => session_root_cwd(state, client_id).await,
        phux_config::CwdInheritance::LastCwdPerWindow => last_window_cwd(state, client_id).await,
    }
}

/// The live PTY CWD of the spawning client's focused pane.
async fn focused_pane_cwd(state: &SharedState, client_id: ClientId) -> Option<String> {
    let handle = state.with(|s| {
        let session = s.attached().get(&client_id)?.session;
        let focused = s.active_pane_of_session(session)?;
        s.resource_handle(focused).cloned()
    })?;
    query_pane_cwd(handle).await
}

/// The session's creation directory: the seed pane's CWD, frozen on first
/// observation so a later `cd` does not move it.
async fn session_root_cwd(state: &SharedState, client_id: ClientId) -> Option<String> {
    let (session, handle) = state.with(|s| {
        let session = s.attached().get(&client_id)?.session;
        if let Some(root) = s.session_root(session) {
            return Some((session, FrozenOrQuery::Frozen(path_to_string(root)?)));
        }
        let seed = s.seed_pane_of_session(session)?;
        let handle = s.resource_handle(seed).cloned()?;
        Some((session, FrozenOrQuery::Query(handle)))
    })?;
    match handle {
        FrozenOrQuery::Frozen(root) => Some(root),
        FrozenOrQuery::Query(handle) => {
            let resolved = query_pane_cwd(handle).await?;
            // A racing spawn may have frozen a root first; keep that one.
            let frozen = state.with_mut(|s| {
                path_to_string(s.record_session_root(session, std::path::PathBuf::from(&resolved)))
            });
            frozen.or(Some(resolved))
        }
    }
}

/// The spawning client's active-window CWD: live from its active pane, else
/// the last value recorded for the window.
async fn last_window_cwd(state: &SharedState, client_id: ClientId) -> Option<String> {
    let (window, handle) = state.with(|s| {
        let session = s.attached().get(&client_id)?.session;
        let window = s.active_window_of_session(session)?;
        let handle = s
            .active_pane_of_session(session)
            .and_then(|p| s.resource_handle(p).cloned());
        Some((window, handle))
    })?;
    let resolved = match handle {
        Some(handle) => query_pane_cwd(handle).await,
        None => None,
    };
    if let Some(cwd) = resolved {
        state.with_mut(|s| {
            s.record_window_last_cwd(window, std::path::PathBuf::from(&cwd));
        });
        return Some(cwd);
    }
    state.with(|s| s.window_last_cwd(window).and_then(|p| path_to_string(p)))
}

/// A frozen session root, or the handle to query for one off-lock.
enum FrozenOrQuery {
    Frozen(String),
    Query(ResourceHandle),
}

/// `path` as UTF-8; a non-UTF-8 directory yields no override.
fn path_to_string(path: &std::path::Path) -> Option<String> {
    path.to_str().map(ToOwned::to_owned)
}

/// Ask `handle`'s Terminal engine for its PTY child's live CWD. `None` for a
/// non-Terminal, a gone actor, or an unsupported query.
async fn query_pane_cwd(handle: ResourceHandle) -> Option<String> {
    let terminal = handle.terminal().ok()?;
    let (reply, rx) = tokio::sync::oneshot::channel();
    terminal.pwd.send(PwdRequest { reply }).await.ok()?;
    rx.await.ok().flatten()
}

/// Refresh every pane's registry `cwd` from its PTY child before the
/// `ATTACHED` snapshot, since the spawn-time stamp goes stale on `cd`.
/// Best-effort and concurrent; replies that miss the deadline keep their
/// stamped value, so a wedged actor cannot stall `ATTACHED`.
pub(crate) async fn refresh_registry_cwds(state: &SharedState) {
    const CWD_REFRESH_DEADLINE: std::time::Duration = std::time::Duration::from_millis(250);

    let handles: Vec<(ResourceId, ResourceHandle)> =
        state.with(crate::state::ServerState::all_resource_handles);
    if handles.is_empty() {
        return;
    }
    let mut queries: FuturesUnordered<_> = handles
        .into_iter()
        .map(|(id, handle)| async move { (id, query_pane_cwd(handle).await) })
        .collect();
    let mut resolved: Vec<(ResourceId, std::path::PathBuf)> = Vec::new();
    let drain = async {
        while let Some((id, cwd)) = queries.next().await {
            if let Some(cwd) = cwd {
                resolved.push((id, std::path::PathBuf::from(cwd)));
            }
        }
    };
    if tokio::time::timeout(CWD_REFRESH_DEADLINE, drain)
        .await
        .is_err()
    {
        debug!("attach cwd refresh hit deadline; using stamped values for stragglers");
    }
    if resolved.is_empty() {
        return;
    }
    state.with_mut(|s| {
        for (id, cwd) in resolved {
            if let Some(desc) = s.registry_mut().terminal_mut(id) {
                desc.cwd = cwd;
            }
        }
    });
}

/// The decoded `SPAWN_RESOURCE` payload, minus `request_id`.
#[derive(Debug)]
pub(crate) struct SpawnRequest {
    /// Group under which to spawn (v0.1 servers expose `GroupId(1)`).
    pub(crate) group: GroupId,
    /// Command + argv, or `None` for the server's default shell.
    pub(crate) command: Option<Vec<String>>,
    /// Working directory, or `None` for the server's default policy.
    pub(crate) cwd: Option<String>,
    /// Environment pairs, `None` = inherit the server's environment.
    pub(crate) env: Option<Vec<(String, String)>>,
    /// First-class `TERM` override.
    pub(crate) term: Option<String>,
    /// Satellite host to route the spawn to, `None` = local.
    pub(crate) satellite: Option<phux_protocol::ids::SatelliteHost>,
    /// Existing Terminal on the spawn's host whose exact window owns the new pane.
    pub(crate) owner_terminal: Option<phux_protocol::ids::ResourceId>,
    /// Opaque native agent-session provenance to install before publication.
    pub(crate) agent_session: Option<Vec<u8>>,
    /// `(cols, rows)` for the pane's grid and PTY, `None` for the default.
    pub(crate) initial_size: Option<(u16, u16)>,
    /// Kind, parent, and provenance fields; `None` is a plain Terminal.
    pub(crate) resource: Option<Box<phux_protocol::wire::frame::SpawnResource>>,
}

/// Only an owner explicitly addressed to this satellite can cross its link.
/// Provenance remains an independent local-only restriction.
fn satellite_spawn_owner(
    host: &phux_protocol::ids::SatelliteHost,
    owner: Option<phux_protocol::ids::ResourceId>,
    has_agent_session: bool,
) -> Result<Option<u32>, SpawnError> {
    if has_agent_session {
        return Err(SpawnError::SpawnFailed(
            "agent-session provenance is local-only".to_owned(),
        ));
    }
    match owner {
        None => Ok(None),
        Some(phux_protocol::ids::ResourceId::Satellite {
            host: owner_host,
            id,
        }) if owner_host == *host => Ok(Some(id)),
        Some(_) => Err(SpawnError::SpawnFailed(
            "owner terminal must belong to the requested satellite host".to_owned(),
        )),
    }
}

/// Relay one satellite-addressed spawn over the hub link (L1 §9.1); no route
/// is the typed configuration refusal.
async fn relay_spawn_to_satellite(
    state: &SharedState,
    host: &phux_protocol::ids::SatelliteHost,
    spawn: SatelliteSpawn,
) -> SpawnResult {
    let Some(relay) = state.with(|s| s.hub_relay(host)) else {
        debug!(
            satellite = %host,
            "SPAWN_RESOURCE: no route to satellite (non-hub server, or host not in the registry)",
        );
        return SpawnResult::Err(SpawnError::UnsupportedSatelliteRoute);
    };
    relay.spawn(spawn).await
}

/// ADR-0109: record which hub consumer spawned a satellite resource (the
/// satellite sees one link identity), before any consumer can learn the id.
fn record_satellite_spawn(state: &SharedState, client_id: ClientId, result: &SpawnResult) {
    if let Some(phux_protocol::ids::ResourceId::Satellite { host, id }) = result.spawned_id() {
        let instance = result.instance();
        state.with_mut(|s| s.hub_record_satellite_spawn(host.clone(), *id, instance, client_id));
    }
}

/// Relay a satellite-targeted spawn (or reply with its validation refusal)
/// and reply with the re-tagged result.
pub(crate) async fn dispatch_satellite_spawn(
    state: &SharedState,
    client_id: ClientId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    request_id: u32,
    host: &phux_protocol::ids::SatelliteHost,
    spawn: Result<SatelliteSpawn, SpawnError>,
) {
    let result = match spawn {
        Ok(spawn) => relay_spawn_to_satellite(state, host, spawn).await,
        Err(error) => SpawnResult::Err(error),
    };
    record_satellite_spawn(state, client_id, &result);
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::ResourceSpawned {
            request_id,
            result,
        }))
        .await;
}

/// Handle `MOVE_RESOURCE` (ADR-0056): atomically re-parent `terminal` into
/// the window owning `owner_terminal`, reaping a source window the move
/// emptied. The `ResourceId` is stable, so subscriptions survive; layout is
/// the caller's concern. Local-only.
pub(crate) async fn handle_move_terminal(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    terminal: phux_protocol::ids::ResourceId,
    owner_terminal: phux_protocol::ids::ResourceId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) {
    debug!(
        ?client_id,
        request_id,
        terminal = ?terminal,
        owner_terminal = ?owner_terminal,
        "MOVE_RESOURCE",
    );

    let (result, clients_to_detach) =
        match state.with_mut(|s| move_local_terminal(s, &terminal, &owner_terminal)) {
            Ok(clients) => (MoveResult::Ok(terminal), clients),
            Err(error) => (MoveResult::Err(error), Vec::new()),
        };

    let _ = out_tx
        .send(Outbound::Frame(FrameKind::ResourceMoved {
            request_id,
            result,
        }))
        .await;

    // Clients attached to a reaped session get DETACHED, each in its own task
    // so a wedged mailbox cannot block this command. Headless resource
    // subscriptions keep streaming the stable id.
    for (detached_client, tx) in clients_to_detach {
        let detached_state = state.clone();
        tokio::task::spawn_local(async move {
            let _ = tx
                .send(Outbound::Frame(FrameKind::Detached {
                    reason: Some(DetachReason::SessionKilled),
                    message: "the session this attach was rooted in was reaped".to_owned(),
                }))
                .await;
            super::client::detach_and_release_consumer_state(&detached_state, detached_client);
        });
    }
}

/// Re-parent `terminal` into `owner_terminal`'s window under one lock, reaping
/// an emptied source window. Returns the clients attached to a session the
/// move reaped.
fn move_local_terminal(
    s: &mut crate::state::ServerState,
    terminal: &phux_protocol::ids::ResourceId,
    owner_terminal: &phux_protocol::ids::ResourceId,
) -> Result<Vec<(ClientId, tokio::sync::mpsc::Sender<Outbound>)>, MoveError> {
    let failed = |message: &str| MoveError::MoveFailed(message.to_owned());
    if !terminal.is_local() || !owner_terminal.is_local() {
        return Err(MoveError::UnsupportedSatelliteRoute);
    }
    let moved = s
        .terminal_from_wire(terminal)
        .ok_or_else(|| failed("terminal was not found on this server"))?;
    let owner = s
        .terminal_from_wire(owner_terminal)
        .ok_or_else(|| failed("owner terminal was not found on this server"))?;
    let dest_window = s
        .registry()
        .resource(owner)
        .and_then(|t| t.window)
        .ok_or_else(|| failed("owner terminal has no window on this server"))?;
    let source_window = s.registry().resource(moved).and_then(|t| t.window);
    let source_session = source_window
        .and_then(|window| s.registry().window(window))
        .map(|window| window.session);
    s.registry_mut()
        .move_terminal(moved, dest_window)
        .map_err(|err| MoveError::MoveFailed(err.to_string()))?;
    if let Some(source_window) = source_window {
        s.reap_window_if_empty(source_window);
    }
    Ok(source_session
        .filter(|session| s.registry().session(*session).is_none())
        .map_or_else(Vec::new, |session| s.attached_clients_in_session(session)))
}

/// Handle `SPAWN_RESOURCE` (SPEC §10.1): spawn a PTY-backed Terminal into
/// the spawning client's current session (or the most recently active one
/// for a detached spawner), auto-subscribe the spawner, and publish its first
/// generation. Refusals ride the typed `RESOURCE_SPAWNED` reply. Satellite
/// spawns are relayed over the hub link and never touch local dispatch;
/// other kinds are dispatched by kind (ADR-0102).
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "linear orchestration, one named helper per stage"
)]
pub(crate) async fn handle_spawn_terminal(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    request: SpawnRequest,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    bootstrap_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
    root_token: &CancellationToken,
    connection_token: &CancellationToken,
    output_pumps: &mut JoinSet<()>,
    defer_subscription: bool,
) {
    let Some(profile) = bootstrap_stream_profile(bootstrap_profile) else {
        let _ = out_tx
            .send(Outbound::Frame(FrameKind::Error {
                request_id: Some(request_id),
                code: ErrorCode::CodecUnavailable,
                message: "SPAWN_RESOURCE selected an unsupported bootstrap profile".to_owned(),
            }))
            .await;
        return;
    };
    let initial_size = usable_initial_size(request.initial_size);
    log_spawn_request(client_id, request_id, &request, initial_size);
    let SpawnRequest {
        group,
        command,
        cwd,
        env,
        term,
        satellite,
        owner_terminal,
        agent_session,
        resource,
        ..
    } = request;

    // An unrecognised kind is refused, never spawned as a Terminal.
    let kind = resource
        .as_ref()
        .map_or(phux_protocol::ids::ResourceKind::Terminal, |r| r.kind);
    let bind_instance = resource.as_ref().is_some_and(|r| r.bind_instance);
    let attribution = super::commands::SpawnAttribution {
        actor: Some(client_id),
        operation_id: resource.as_ref().and_then(|r| r.idempotency_key),
        retain_secs: state.with(|s| {
            s.retain_policy()
                .resolve(resource.as_ref().and_then(|r| r.retain_secs))
        }),
    };
    match crate::resource::core_kind(kind) {
        Some(crate::resource::ResourceKind::Terminal) => {}
        Some(crate::resource::ResourceKind::AgentSession) => {
            let resource = resource.unwrap_or_default();
            crate::runtime::resource_commands::spawn_agent_session(
                state,
                client_id,
                request_id,
                &resource,
                satellite.as_ref(),
                out_tx,
                root_token,
                bootstrap_limits,
                connection_token,
            )
            .await;
            return;
        }
        _ => {
            warn!(
                ?client_id,
                request_id,
                ?kind,
                "SPAWN_RESOURCE: unknown kind"
            );
            refuse_spawn(out_tx, request_id, SpawnError::UnsupportedKind).await;
            return;
        }
    }
    // A Terminal takes no parent (ADR-0104 §5).
    if resource.as_ref().is_some_and(|r| r.parent.is_some()) {
        refuse_spawn(out_tx, request_id, SpawnError::ParentKindMismatch).await;
        return;
    }

    // The satellite validates the rest and its errors relay back verbatim.
    if let Some(host) = satellite {
        let spawn = satellite_spawn_owner(&host, owner_terminal, agent_session.is_some()).map(
            |owner_terminal| SatelliteSpawn {
                group,
                command,
                cwd,
                env,
                term,
                owner_terminal,
                initial_size,
                resource: forwarded_resource(
                    bind_instance,
                    attribution.operation_id,
                    resource.as_ref().and_then(|r| r.retain_secs),
                ),
            },
        );
        dispatch_satellite_spawn(state, client_id, out_tx, request_id, &host, spawn).await;
        return;
    }

    if let Err(refusal) = validate_local_spawn(agent_session.as_deref(), group) {
        refuse_spawn(out_tx, request_id, refusal).await;
        return;
    }

    let builder = build_spawn_command(state, client_id, command, cwd, env, term.as_deref()).await;

    let ownership = match resolve_spawn_ownership(state, client_id, owner_terminal) {
        Ok(ownership) => ownership,
        Err(refusal) => {
            refuse_spawn(out_tx, request_id, refusal).await;
            return;
        }
    };

    let (scrollback, default_colors) = state.with(|s| {
        (
            s.scrollback_limits(),
            s.attached()
                .get(&client_id)
                .and_then(|client| client.client_caps.default_colors),
        )
    });
    let core_terminal_id = match spawn_pane_or_refusal(
        state,
        client_id,
        request_id,
        &ownership,
        root_token,
        PaneSpawnPlan {
            builder,
            scrollback,
            default_colors,
            agent_session,
            initial_size,
            attribution,
        },
    ) {
        Ok(id) => id,
        Err(refusal) => {
            refuse_spawn(out_tx, request_id, refusal).await;
            return;
        }
    };

    let Some((wire_terminal_id, handle, client_caps)) =
        subscribe_spawning_client(state, client_id, core_terminal_id)
    else {
        refuse_vanished_pane_handle(state, out_tx, client_id, request_id, core_terminal_id).await;
        return;
    };
    let terminal = match handle.terminal() {
        Ok(terminal) => terminal.clone(),
        Err(error) => {
            warn!(?client_id, request_id, ?core_terminal_id, %error, "SPAWN_RESOURCE");
            refuse_vanished_pane_handle(state, out_tx, client_id, request_id, core_terminal_id)
                .await;
            return;
        }
    };

    // ADR-0126: bind the key to the registered pane before anything awaits,
    // so a repeat never observes the pane without its binding.
    state.with(|s| {
        super::idempotent_create::bind_spawned(s, attribution.operation_id, &wire_terminal_id);
    });
    SpawnPublication {
        state,
        out_tx,
        request_id,
        client_id,
        core_terminal_id,
        wire_terminal_id,
        handle,
        terminal,
        client_caps,
        stream_id: stream_id_from(u64::from(request_id)),
        profile,
        limits: bootstrap_limits,
        bind_instance,
        idempotency_key: attribution.operation_id,
    }
    .publish(output_pumps, connection_token, defer_subscription)
    .await;
}

/// The resource record a satellite Terminal spawn forwards (bind request,
/// idempotency key, `retain_secs`), when the consumer sent any; the
/// satellite applies them itself.
fn forwarded_resource(
    bind_instance: bool,
    idempotency_key: Option<phux_protocol::ids::IdempotencyKey>,
    retain_secs: Option<u32>,
) -> Option<Box<phux_protocol::wire::frame::SpawnResource>> {
    (bind_instance || idempotency_key.is_some() || retain_secs.is_some()).then(|| {
        Box::new(
            phux_protocol::wire::frame::SpawnResource::default()
                .with_bind_instance(bind_instance)
                .with_idempotency_key(idempotency_key)
                .with_retain_secs(retain_secs),
        )
    })
}

/// The success reply for a spawn, bound to `instance` when requested
/// (ADR-0109).
pub(crate) const fn spawned_result(
    id: phux_protocol::ids::ResourceId,
    instance: Option<phux_protocol::ids::ServerInstance>,
) -> SpawnResult {
    match instance {
        Some(instance) => SpawnResult::OkBound { id, instance },
        None => SpawnResult::Ok(id),
    }
}

fn log_spawn_request(
    client_id: ClientId,
    request_id: u32,
    request: &SpawnRequest,
    initial_size: Option<(u16, u16)>,
) {
    debug!(
        ?client_id,
        request_id,
        group = ?request.group,
        command = ?request.command,
        cwd = ?request.cwd,
        env_count = request.env.as_ref().map_or(0, Vec::len),
        satellite = ?request.satellite,
        owner_terminal = ?request.owner_terminal,
        initial_size = ?initial_size,
        "SPAWN_RESOURCE",
    );
}

/// What one `SPAWN_RESOURCE` asks the PTY layer to build.
struct PaneSpawnPlan {
    /// The child to exec, fully configured from the wire frame.
    builder: portable_pty::CommandBuilder,
    /// Scrollback rows the new pane retains.
    scrollback: phux_config::ScrollbackLimits,
    /// Host palette the pane starts with, when the spawner advertised one.
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    /// Opaque native agent-session provenance to install before publication.
    agent_session: Option<Vec<u8>>,
    /// `(cols, rows)` to build the pane's grid and PTY at.
    initial_size: Option<(u16, u16)>,
    /// Who asked, for the pane's `pane_spawned` stamp (ADR-0123).
    attribution: super::commands::SpawnAttribution,
}

/// Spawn the PTY-backed pane into the resolved owner's window, mapping both
/// failure shapes onto the typed wire refusal they are logged with.
fn spawn_pane_or_refusal(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    ownership: &SpawnOwnership,
    root_token: &CancellationToken,
    plan: PaneSpawnPlan,
) -> Result<ResourceId, SpawnError> {
    match spawn_pane_with_pty_and_colors(
        state,
        ownership,
        plan.builder,
        plan.scrollback,
        root_token,
        plan.default_colors,
        plan.agent_session,
        plan.initial_size,
        plan.attribution,
    ) {
        Ok(Some(id)) => Ok(id),
        Ok(None) => {
            warn!(
                ?client_id,
                request_id, "SPAWN_RESOURCE: selected owner has no window to host the pane",
            );
            Err(SpawnError::SpawnFailed(
                "selected owner has no window to host the pane".to_owned(),
            ))
        }
        Err(err) => {
            warn!(
                ?client_id,
                request_id,
                error = %err,
                "SPAWN_RESOURCE: failed to spawn pane actor",
            );
            Err(SpawnError::SpawnFailed(format!("{err}")))
        }
    }
}

/// The pane spawned but its handle vanished: reap it and refuse the spawn so
/// the client is not left waiting.
async fn refuse_vanished_pane_handle(
    state: &SharedState,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    client_id: ClientId,
    request_id: u32,
    core_terminal_id: ResourceId,
) {
    warn!(
        ?client_id,
        request_id,
        ?core_terminal_id,
        "SPAWN_RESOURCE: spawn succeeded but TerminalHandle vanished",
    );
    state.with_mut(|s| {
        let _ = super::client::reap_pane_journaling_close(s, core_terminal_id);
    });
    refuse_spawn(
        out_tx,
        request_id,
        SpawnError::SpawnFailed(
            "internal state inconsistency: handle missing after spawn".to_owned(),
        ),
    )
    .await;
}

/// Reply to a `SPAWN_RESOURCE` with a typed refusal.
pub(crate) async fn refuse_spawn(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    request_id: u32,
    error: SpawnError,
) {
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::ResourceSpawned {
            request_id,
            result: SpawnResult::Err(error),
        }))
        .await;
}

/// A zero axis means "geometry unknown" (SPEC §10.5): use the default.
fn usable_initial_size(initial_size: Option<(u16, u16)>) -> Option<(u16, u16)> {
    initial_size.filter(|&(cols, rows)| cols > 0 && rows > 0)
}

/// Stateless validation of a local spawn: the provenance bound, and the
/// single default Group ([`crate::state::DEFAULT_GROUP_ID`]).
fn validate_local_spawn(agent_session: Option<&[u8]>, group: GroupId) -> Result<(), SpawnError> {
    if agent_session
        .is_some_and(|value| value.is_empty() || value.len() > MAX_AGENT_SESSION_RECORD_BYTES)
    {
        return Err(SpawnError::SpawnFailed(
            "agent-session provenance must contain 1..=4096 bytes".to_owned(),
        ));
    }
    if group != crate::state::DEFAULT_GROUP_ID {
        return Err(SpawnError::GroupNotFound);
    }
    Ok(())
}

/// Build the child's argv; no command runs the default shell.
fn spawn_argv_builder(
    state: &SharedState,
    command: Option<Vec<String>>,
) -> portable_pty::CommandBuilder {
    super::commands::argv_command(command).unwrap_or_else(|| {
        let (shell, login_shell) = state.with(|s| (s.shell().to_owned(), s.login_shell()));
        crate::terminal_actor::default_shell_command(&shell, login_shell)
    })
}

/// Build the `CommandBuilder` a `SPAWN_RESOURCE` execs. `TERM` layers, last
/// wins: server `defaults.term`, the spawn's `term`, then an `env` entry. An
/// explicit `cwd` wins over `defaults.cwd-inheritance`; `env` is additive.
async fn build_spawn_command(
    state: &SharedState,
    client_id: ClientId,
    command: Option<Vec<String>>,
    cwd: Option<String>,
    env: Option<Vec<(String, String)>>,
    term: Option<&str>,
) -> portable_pty::CommandBuilder {
    let mut builder = spawn_argv_builder(state, command);
    let default_term = state.with(|s| s.term().to_owned());
    crate::terminal_actor::apply_term(&mut builder, &default_term);
    if let Some(t) = term {
        crate::terminal_actor::apply_term(&mut builder, t);
    }
    if let Some(path) = cwd {
        builder.cwd(path);
    } else if let Some(path) = resolve_inherited_cwd(state, client_id).await {
        builder.cwd(path);
    }
    if let Some(pairs) = env {
        for (k, v) in pairs {
            builder.env(k, v);
        }
    }
    builder
}

/// Resolve which Terminal's window or session hosts the new pane: the named
/// owner, else the spawner's attached session, else the most recently active
/// one. A server with no session refuses rather than orphan the PTY.
fn resolve_spawn_ownership(
    state: &SharedState,
    client_id: ClientId,
    owner_terminal: Option<phux_protocol::ids::ResourceId>,
) -> Result<SpawnOwnership, SpawnError> {
    if let Some(owner) = owner_terminal {
        if !matches!(owner, phux_protocol::ids::ResourceId::Local { .. })
            || state.with(|s| s.terminal_from_wire(&owner).is_none())
        {
            return Err(SpawnError::SpawnFailed(
                "owner terminal was not found on this server".to_owned(),
            ));
        }
        return Ok(SpawnOwnership::Terminal(owner));
    }
    let session = state.with(|s| {
        s.attached()
            .get(&client_id)
            .map(|c| c.session)
            .or_else(|| s.most_recently_touched_session())
            .or_else(|| s.registry().sessions().next().map(|(id, _)| id))
    });
    let Some(session) = session else {
        return Err(SpawnError::SpawnFailed(
            "server has no session to host the spawned pane".to_owned(),
        ));
    };
    Ok(SpawnOwnership::Session(session))
}

/// Auto-subscribe the spawning client to the new pane (or input to it would
/// be refused) and clone out its wire id, handle, and capabilities.
fn subscribe_spawning_client(
    state: &SharedState,
    client_id: ClientId,
    core_terminal_id: ResourceId,
) -> Option<(
    phux_protocol::ids::ResourceId,
    ResourceHandle,
    ClientCapabilities,
)> {
    state.with_mut(|s| {
        let wire_terminal_id = s.intern_terminal_wire(core_terminal_id);
        // ADR-0109: provenance before any subscription.
        s.record_spawn(core_terminal_id, client_id);
        let client_caps = s
            .attached()
            .get(&client_id)
            .map(|c| c.client_caps)
            .unwrap_or_default();
        // A detached spawner has no `attached` slot to subscribe from.
        if s.attached().contains_key(&client_id) {
            s.subscribe_terminal(client_id, core_terminal_id, None);
            s.mark_if_viewer_session(client_id, &wire_terminal_id);
        }
        s.resource_handle(core_terminal_id)
            .cloned()
            .map(|h| (wire_terminal_id, h, client_caps))
    })
}

/// Spawn the `SPAWN_RESOURCE` output pump and hand back its publication gate.
/// This pump owns its pane, so a lost generation reaps it.
fn spawn_terminal_output_pump(
    ctx: OutputPumpContext,
    output_rx: tokio::sync::broadcast::Receiver<PaneOutput>,
    state: &SharedState,
    connection_token: &CancellationToken,
    core_terminal_id: ResourceId,
    output_pumps: &mut JoinSet<()>,
) -> oneshot::Sender<OutputPumpStart> {
    let pump_state = state.clone();
    let pump_connection_token = connection_token.clone();
    let (gate_tx, gate_rx) = oneshot::channel::<OutputPumpStart>();
    pump::spawn_tracked(
        state,
        ctx.client_id,
        core_terminal_id,
        Some(output_pumps),
        async move {
            let Some(fault) = run_output_pump(&ctx, gate_rx, output_rx).await else {
                return;
            };
            match fault {
                PumpFault::OutboundClosed
                | PumpFault::TombstoneNotQueued
                | PumpFault::ReplayAbandoned
                | PumpFault::PaneGone => {}
                PumpFault::GenerationLost => {
                    let reaped = pump_state.with_mut(|s| {
                        super::client::reap_pane_journaling_close(s, core_terminal_id)
                    });
                    // A pane that already closed ended with RESOURCE_CLOSED;
                    // losing the generation on the way out is no reason to
                    // drop the client.
                    if reaped {
                        warn!(
                            ?core_terminal_id,
                            "spawn pump lost its generation; reaped the pane, closing the client"
                        );
                        pump_connection_token.cancel();
                    }
                }
                PumpFault::PublicationNotActivated => {
                    warn!(
                        ?core_terminal_id,
                        "spawn pump publication not activated; closing the client"
                    );
                    pump_connection_token.cancel();
                }
            }
        },
    );
    gate_tx
}

/// The freshly spawned pane and everything its publication needs.
struct SpawnPublication<'a> {
    state: &'a SharedState,
    out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    request_id: u32,
    client_id: ClientId,
    core_terminal_id: ResourceId,
    wire_terminal_id: phux_protocol::ids::ResourceId,
    handle: ResourceHandle,
    terminal: crate::terminal_actor::TerminalHandle,
    client_caps: ClientCapabilities,
    stream_id: StreamId,
    profile: BootstrapStreamProfile,
    limits: BootstrapLimits,
    /// Answer with the instance token (ADR-0109).
    bind_instance: bool,
    /// Unbound again if the pane is reaped (ADR-0126).
    idempotency_key: Option<phux_protocol::ids::IdempotencyKey>,
}

impl SpawnPublication<'_> {
    /// Drop the pane nobody can reach: the client never received a usable
    /// generation for it.
    fn reap(&self) {
        self.state.with_mut(|s| {
            let _ = super::client::reap_pane_journaling_close(s, self.core_terminal_id);
            super::idempotent_create::unbind_spawned(
                s,
                self.idempotency_key,
                &self.wire_terminal_id,
            );
        });
    }

    /// Queue the successful `RESOURCE_SPAWNED` reply. `false` once the
    /// client's outbound mailbox has closed.
    async fn queue_spawned_ok(&self) -> bool {
        let instance = self
            .bind_instance
            .then(|| self.state.with(|s| s.idspace.instance()));
        self.out_tx
            .send(Outbound::Frame(FrameKind::ResourceSpawned {
                request_id: self.request_id,
                result: spawned_result(self.wire_terminal_id.clone(), instance),
            }))
            .await
            .is_ok()
    }

    /// Capture the pane's first native checkpoint. `None` once the actor is
    /// gone or refuses the capture.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn capture_native_checkpoint(
        &self,
    ) -> Option<crate::terminal_actor::NativeBootstrapReply> {
        let captured = request_native_checkpoint(&self.terminal, |reply| {
            crate::terminal_actor::NativeBootstrapRequest {
                owner: self.client_id.0,
                terminal_id: self.wire_terminal_id.clone(),
                stream_id: self.stream_id,
                bootstrap_id: initial_bootstrap_id(),
                limits: self.limits,
                max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
                max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
                reply,
            }
        })
        .await;
        if let Err(NativeRequestFailure::Refused(error)) = &captured {
            let core_terminal_id = self.core_terminal_id;
            warn!(?core_terminal_id, %error, "native spawn preflight failed");
        }
        captured.ok()
    }

    /// Subscribe the output pump (before the reply, so early PTY output is
    /// buffered), then publish the first generation. Under QUIC multi-stream
    /// (`defer_subscription`) only the reply goes out; the pump and bootstrap
    /// start at the client's `STREAM_BIND`.
    async fn publish(
        self,
        output_pumps: &mut JoinSet<()>,
        connection_token: &CancellationToken,
        defer_subscription: bool,
    ) {
        if defer_subscription {
            if !self.queue_spawned_ok().await {
                self.reap();
            }
            return;
        }
        let output_rx = self.handle.output.subscribe();
        let gate_tx = spawn_terminal_output_pump(
            OutputPumpContext {
                out_tx: self.out_tx.clone(),
                resize: self.terminal.resize.clone(),
                wire_terminal_id: self.wire_terminal_id.clone(),
                stream_id: self.stream_id,
                initial_bootstrap_id: initial_bootstrap_id(),
                client_id: self.client_id,
                client_caps: self.client_caps,
                profile: self.profile,
                limits: self.limits,
                lag_label: "SPAWN_RESOURCE output pump",
                stale_skip: true,
                cancel: None,
                last_seq: None,
                #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                terminal: self.terminal.clone(),
            },
            output_rx,
            self.state,
            connection_token,
            self.core_terminal_id,
            output_pumps,
        );
        self.publish_first_generation(gate_tx).await;
    }

    /// Capture the pane's first synthesized snapshot and the actor cut it was
    /// taken at. `None` once the actor is gone or refuses.
    async fn capture_snapshot(&self) -> Option<(crate::grid::SnapshotBytes, u64)> {
        let (snapshot_tx, snapshot_rx) = oneshot::channel();
        self.terminal
            .snapshot
            .send(SnapshotRequest {
                scrollback: None,
                max_bytes: usize::MAX,
                max_frames: usize::MAX,
                chunk_bytes: 1,
                reply: snapshot_tx,
            })
            .await
            .ok()?;
        snapshot_rx.await.ok()?.ok()
    }

    /// Capture and frame the first generation for the negotiated profile.
    async fn stage_first_generation(&self) -> Result<FirstGeneration, &'static str> {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if publishes_native_checkpoints(self.profile) {
            let reply = self
                .capture_native_checkpoint()
                .await
                .ok_or("native checkpoint preflight failed")?;
            return Ok(FirstGeneration {
                cut: reply.base_seq,
                cursor: Some(reply.publication_cursor),
                frames: reply.frames,
            });
        }
        let (snapshot, cut) = self
            .capture_snapshot()
            .await
            .ok_or("snapshot preflight failed")?;
        let replay = downsample_for_caps(&bytes::Bytes::from(snapshot.bytes), self.client_caps);
        let frames = synthesized_bootstrap_frames(
            self.wire_terminal_id.clone(),
            self.stream_id,
            initial_bootstrap_id(),
            self.profile,
            self.limits,
            snapshot.cols,
            snapshot.rows,
            cut,
            [replay],
        )
        .map_err(|()| "bootstrap limits rejected snapshot")?;
        Ok(FirstGeneration {
            frames,
            cut,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            cursor: None,
        })
    }

    /// Reply, queue the first generation, activate a native publication, then
    /// release the parked output pump. Any failure reaps the pane.
    async fn publish_first_generation(&self, gate_tx: oneshot::Sender<OutputPumpStart>) {
        let generation = match self.stage_first_generation().await {
            Ok(generation) => generation,
            Err(reason) => {
                self.reap();
                refuse_spawn(
                    self.out_tx,
                    self.request_id,
                    SpawnError::SpawnFailed(reason.to_owned()),
                )
                .await;
                return;
            }
        };
        if !self.queue_spawned_ok().await {
            self.reap();
            return;
        }
        for frame in generation.frames {
            if self.out_tx.send(Outbound::Frame(frame)).await.is_err() {
                self.reap();
                return;
            }
        }
        #[cfg_attr(
            not(all(feature = "native-engine", not(target_arch = "wasm32"))),
            allow(unused_mut)
        )]
        let mut start = OutputPumpStart {
            published_cut: generation.cut,
            replay: Vec::new(),
            live: None,
        };
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if let Some(cursor) = generation.cursor {
            let Ok(publication) = activate_native_publication(
                &self.terminal,
                self.client_id.0,
                self.wire_terminal_id.clone(),
                self.stream_id,
                initial_bootstrap_id(),
                cursor,
            )
            .await
            else {
                self.reap();
                return;
            };
            start.replay = publication.replay;
            start.live = Some(publication.live);
        }
        let _ = gate_tx.send(start);
    }
}

/// A spawn's first generation, staged before anything is published.
struct FirstGeneration {
    frames: Vec<FrameKind>,
    cut: u64,
    /// The native publication to activate once the frames are queued.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    cursor: Option<crate::native_state::OpaqueHistoryCursor>,
}

/// Is this ATTACH a replacement for the same client's existing attachment to
/// the same session?
fn is_same_session_reattach(
    state: &SharedState,
    client_id: ClientId,
    session: phux_core::ids::SessionId,
) -> bool {
    state.with(|server| {
        server
            .attached()
            .get(&client_id)
            .is_some_and(|attached| attached.session == session)
    })
}

/// Apply a session `ATTACH`'s declared role (ADR-0127) to every Terminal it
/// returned in one critical section, then announce what changed.
async fn apply_session_role(
    state: &SharedState,
    client_id: ClientId,
    panes: &[crate::state::AttachSnapshotPane],
    role: phux_protocol::wire::frame::RolePolicy,
    was_subscribed: bool,
) {
    let changes: Vec<_> = state.with_mut(|s| {
        s.set_attached_viewer(client_id, role.is_viewer());
        panes
            .iter()
            .map(|pane| {
                let effects = s.apply_attach_role(
                    client_id,
                    &pane.wire_terminal_id,
                    Some(pane.terminal_id),
                    role,
                    was_subscribed,
                );
                (effects, s.input_lease_holder(pane.terminal_id))
            })
            .collect()
    });
    for (pane, (effects, holder)) in panes.iter().zip(changes) {
        if !effects.is_empty() {
            super::commands::announce_role_effects_on(&pane.handle, client_id, effects, holder)
                .await;
        }
    }
}

/// Run [`prepare_attach`], translating each refusal into an `ERROR` frame.
async fn prepare_attach_or_refuse(
    state: &SharedState,
    client_id: ClientId,
    session: phux_core::ids::SessionId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    client_caps: ClientCapabilities,
    negotiated_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
) -> Option<AttachPrepared> {
    use crate::state::AttachError;
    let error = match prepare_attach(
        state,
        client_id,
        session,
        out_tx,
        client_caps,
        negotiated_profile,
        bootstrap_limits,
    ) {
        Ok(prepared) => return Some(prepared),
        Err(error) => error,
    };
    let (code, message) = match error {
        AttachError::UnknownSession(name) => (
            ErrorCode::SessionNotFound,
            format!("session {name:?} not found"),
        ),
        AttachError::AlreadyAttached(_) => (
            ErrorCode::AlreadyAttached,
            "client is already attached".to_owned(),
        ),
        AttachError::ResourceLimit => (
            ErrorCode::CodecUnavailable,
            "session exceeds bounded aggregate attach limits".to_owned(),
        ),
    };
    send_error(out_tx, code, &message).await;
    None
}

/// Stop a state-sync consumer's prior actor-side emitters before the new
/// ATTACHED, so an old delta cannot land between it and the new bootstrap.
async fn detach_prior_state_sync_consumers(
    panes: &[AttachSnapshotPane],
    wire_client_id: phux_protocol::ids::ClientId,
    client_caps: ClientCapabilities,
) {
    if !matches!(
        client_caps.output_mode,
        phux_protocol::caps::OutputMode::StateSync
    ) {
        return;
    }
    for pane in panes {
        let (reply, done) = oneshot::channel();
        if pane
            .handle
            .consumer_detach
            .send(ConsumerDetachRequest {
                client_id: wire_client_id,
                reply,
            })
            .await
            .is_ok()
        {
            let _ = done.await;
        }
    }
}

/// The most recent attach that advertises a palette sets the panes' default
/// colors; each is acknowledged before snapshotting so later OSC 10/11
/// queries see it.
async fn apply_client_default_colors(
    panes: &[AttachSnapshotPane],
    colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) {
    let Some(colors) = colors else {
        return;
    };
    for pane in panes {
        let Ok(terminal) = pane.handle.terminal() else {
            continue;
        };
        let (reply, done) = oneshot::channel();
        if terminal
            .set_default_colors
            .send(SetDefaultColorsRequest { colors, reply })
            .await
            .is_ok()
        {
            let _ = done.await;
        }
    }
}

/// The staging state one ATTACH accumulates, one pane at a time: each
/// capture is charged before the next actor gets its remaining ceiling, so
/// concurrent allocations never exceed the connection-wide cap.
struct AttachStaging {
    budget: BootstrapStagingBudget,
    /// Bootstrap and closure frames staged for atomic publication.
    frames: Vec<FrameKind>,
    /// Handles the rollback boundary detaches.
    handles: Vec<ResourceHandle>,
    /// Per-pane publication gates.
    gates: Vec<SnapshotGate>,
    pumps: JoinSet<()>,
}

impl Default for AttachStaging {
    fn default() -> Self {
        Self {
            budget: BootstrapStagingBudget::with_limits(
                MAX_STAGED_BOOTSTRAP_BYTES,
                MAX_STAGED_BOOTSTRAP_FRAMES,
            ),
            frames: Vec::new(),
            handles: Vec::new(),
            gates: Vec::new(),
            pumps: JoinSet::new(),
        }
    }
}

impl AttachStaging {
    /// Append authoritative closures under the same frame ceiling as bootstraps.
    fn append_closures(
        &mut self,
        terminal_ids: Vec<phux_protocol::ids::ResourceId>,
    ) -> Result<(), ()> {
        let mut frames = Vec::new();
        frames.try_reserve(terminal_ids.len()).map_err(|_| ())?;
        frames.extend(
            terminal_ids
                .into_iter()
                // Their exit watcher already consumed the close reason.
                .map(|terminal_id| FrameKind::ResourceClosed {
                    terminal_id,
                    exit_status: None,
                    reason: phux_protocol::wire::frame::CloseReason::Unknown, signal: None,
                }),
        );
        self.budget
            .append_accounted(&mut self.frames, &mut frames, 0)
    }
}

/// Outcome of the ADR-0018 per-consumer state-sync registration for one pane.
#[derive(Debug, Default)]
struct ConsumerRegistration {
    /// The actor accepted the registration.
    registered: bool,
    /// The actor's tick is this consumer's sole live emitter: no pump.
    tick_managed: bool,
    /// Atomic synthesized bootstrap captured in the same actor turn.
    state_sync_bootstrap: Option<crate::terminal_actor::StateSyncBootstrap>,
}

/// The negotiated shape shared by every pane capture in one ATTACH.
struct PaneCaptureContext<'a> {
    state: &'a SharedState,
    out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    /// Cancelled by a pump whose generation became unrecoverable.
    connection_token: &'a CancellationToken,
    /// Released once the aggregate publication is on the wire.
    live_gate_rx: tokio::sync::watch::Receiver<bool>,
    client_id: ClientId,
    wire_client_id: phux_protocol::ids::ClientId,
    client_caps: ClientCapabilities,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    profile: BootstrapStreamProfile,
    limits: BootstrapLimits,
    /// The ATTACH's scrollback request, capped in lines.
    scrollback: Option<u32>,
    chunk_bytes: usize,
}

impl PaneCaptureContext<'_> {
    /// Did this client negotiate state-sync output?
    const fn wants_state_sync(&self) -> bool {
        matches!(
            self.client_caps.output_mode,
            phux_protocol::caps::OutputMode::StateSync
        )
    }

    /// Register the per-consumer state-sync entry (ADR-0018) before the
    /// snapshot, so its cache is primed against the same state. When the
    /// actor tick-manages the consumer, its tick is the sole emitter and the
    /// broadcast pump must be suppressed (two `seq` streams on one mailbox
    /// violate SPEC §12.2). Any failure falls back to the broadcast path.
    async fn register_consumer(
        &self,
        handle: &ResourceHandle,
        wire_terminal_id: &phux_protocol::ids::ResourceId,
        terminal_id: ResourceId,
        bootstrap_max_bytes: usize,
        bootstrap_max_frames: usize,
    ) -> ConsumerRegistration {
        let Some(wire_id) = wire_terminal_id.local_id() else {
            return ConsumerRegistration::default();
        };
        let (attach_reply_tx, attach_reply_rx) = oneshot::channel();
        if handle
            .consumer_attach
            .send(ConsumerAttachRequest {
                client_id: self.wire_client_id,
                outbound: self.out_tx.clone(),
                wire_terminal_id: wire_id,
                stream_id: self.stream_id,
                bootstrap_id: self.bootstrap_id,
                wants_state_sync: self.wants_state_sync(),
                state_sync_scrollback: self.scrollback,
                bootstrap_max_bytes,
                bootstrap_max_frames,
                bootstrap_chunk_bytes: self.chunk_bytes,
                // A direct consumer's transport is reliable and ordered.
                live_gate: self.live_gate_rx.clone(),
                loss_tolerant: false,
                reply: attach_reply_tx,
            })
            .await
            .is_err()
        {
            warn!(
                ?terminal_id,
                "per-consumer state-sync register: actor mailbox closed",
            );
            return ConsumerRegistration::default();
        }
        match attach_reply_rx.await {
            Ok(Ok(outcome)) => {
                trace!(
                    ?terminal_id,
                    tick_managed = outcome.tick_managed,
                    "per-consumer state-sync entry registered",
                );
                ConsumerRegistration {
                    registered: true,
                    tick_managed: outcome.tick_managed,
                    state_sync_bootstrap: outcome.state_sync_bootstrap,
                }
            }
            Ok(Err(err)) => {
                warn!(
                    ?terminal_id,
                    error = %err,
                    "per-consumer state-sync register failed; broadcast path still serves this pane",
                );
                ConsumerRegistration::default()
            }
            Err(_) => {
                warn!(
                    ?terminal_id,
                    "per-consumer state-sync register: actor dropped reply",
                );
                ConsumerRegistration::default()
            }
        }
    }

    /// Stage this pane's output pump behind its publication gate. It
    /// subscribes before the snapshot is requested, so nothing is missed, and
    /// forwards nothing until the bootstrap is queued. A fatal fault releases
    /// only this client's consumer state: the pane is shared.
    fn spawn_pane_pump(
        &self,
        staging: &mut AttachStaging,
        terminal_id: ResourceId,
        wire_terminal_id: &phux_protocol::ids::ResourceId,
        handle: &ResourceHandle,
        terminal: &crate::terminal_actor::TerminalHandle,
    ) {
        let output_rx = handle.output.subscribe();
        let ctx = OutputPumpContext {
            out_tx: self.out_tx.clone(),
            resize: terminal.resize.clone(),
            wire_terminal_id: wire_terminal_id.clone(),
            stream_id: self.stream_id,
            initial_bootstrap_id: self.bootstrap_id,
            client_id: self.client_id,
            client_caps: self.client_caps,
            profile: self.profile,
            limits: self.limits,
            lag_label: "ResourceOutput pump",
            stale_skip: true,
            cancel: None,
            last_seq: None,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            terminal: terminal.clone(),
        };
        let pump_state = self.state.clone();
        let pump_connection_token = self.connection_token.clone();
        let client_id = self.client_id;
        let (gate_tx, gate_rx) = oneshot::channel::<OutputPumpStart>();
        staging.gates.push(SnapshotGate {
            terminal_id,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            wire_terminal_id: wire_terminal_id.clone(),
            terminal: terminal.clone(),
            gate: gate_tx,
            cut: None,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_cursor: None,
        });
        pump::spawn_tracked(
            self.state,
            client_id,
            terminal_id,
            Some(&mut staging.pumps),
            async move {
                let Some(fault) = run_output_pump(&ctx, gate_rx, output_rx).await else {
                    return;
                };
                release_after_pump_fault(fault, &pump_state, client_id, &pump_connection_token);
            },
        );
    }

    /// Adapt one pane's synthesized snapshot to the client's capabilities,
    /// frame it, and charge the retained result to the aggregate budget.
    ///
    /// `label` names the capture in the rollback reason the client is told.
    fn stage_synthesized_frames(
        &self,
        staging: &mut AttachStaging,
        wire_terminal_id: phux_protocol::ids::ResourceId,
        snapshot: crate::grid::SnapshotBytes,
        base_seq: u64,
        label: &str,
    ) -> Result<(), String> {
        let cols = snapshot.cols;
        let rows = snapshot.rows;
        let Ok(adapted) =
            adapt_bootstrap_snapshot(snapshot, self.client_caps, staging.budget.remaining_bytes())
        else {
            return Err(format!("{label} adaptation exceeded source budget"));
        };
        debug_assert!(adapted.peak_bytes <= staging.budget.remaining_bytes());
        let Ok(mut frames) = synthesized_bootstrap_frames(
            wire_terminal_id,
            self.stream_id,
            self.bootstrap_id,
            self.profile,
            self.limits,
            cols,
            rows,
            base_seq,
            adapted.payloads,
        ) else {
            return Err(format!("{label} exceeded negotiated bounds"));
        };
        let AttachStaging {
            budget,
            frames: staged,
            ..
        } = staging;
        if budget
            .append_accounted(staged, &mut frames, adapted.retained_bytes)
            .is_err()
        {
            return Err("aggregate bootstrap staging budget exceeded".to_owned());
        }
        Ok(())
    }

    /// Capture one pane's native checkpoint against the remaining aggregate
    /// budget and record the cut its pump must resume from.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn stage_native_bootstrap(
        &self,
        staging: &mut AttachStaging,
        terminal_id: ResourceId,
        wire_terminal_id: &phux_protocol::ids::ResourceId,
        terminal: &crate::terminal_actor::TerminalHandle,
    ) -> Result<(), String> {
        let captured = request_native_checkpoint(terminal, |reply| {
            crate::terminal_actor::NativeBootstrapRequest {
                owner: self.client_id.0,
                terminal_id: wire_terminal_id.clone(),
                stream_id: self.stream_id,
                bootstrap_id: self.bootstrap_id,
                limits: self.limits,
                max_bytes: staging.budget.remaining_bytes(),
                max_frames: staging.budget.remaining_frames(),
                reply,
            }
        })
        .await;
        let mut reply = match captured {
            Ok(reply) => reply,
            Err(NativeRequestFailure::Refused(error)) => {
                warn!(?terminal_id, %error, "native checkpoint failed before attach publication");
                return Err("native checkpoint capture failed".to_owned());
            }
            Err(NativeRequestFailure::Unsent) => {
                warn!(?terminal_id, "pane actor dropped before native bootstrap");
                return Err("pane actor dropped native bootstrap request".to_owned());
            }
            Err(NativeRequestFailure::Dropped) => {
                warn!(?terminal_id, "pane actor dropped native checkpoint reply");
                return Err("pane actor dropped native checkpoint reply".to_owned());
            }
        };
        let cut = reply.base_seq;
        let publication_cursor = reply.publication_cursor;
        let AttachStaging {
            budget,
            frames,
            gates,
            ..
        } = staging;
        if budget
            .append_accounted(frames, &mut reply.frames, reply.retained_bytes)
            .is_err()
        {
            return Err("aggregate bootstrap staging budget exceeded".to_owned());
        }
        if let Some(gate) = gates
            .iter_mut()
            .find(|gate| gate.terminal_id == terminal_id)
        {
            gate.cut = Some(cut);
            gate.native_cursor = Some(publication_cursor);
        }
        Ok(())
    }

    /// Ask one pane's actor for a bounded synthesized snapshot and the actor
    /// cut it was taken at.
    async fn request_pane_snapshot(
        &self,
        terminal: &crate::terminal_actor::TerminalHandle,
        terminal_id: ResourceId,
        max_bytes: usize,
        max_frames: usize,
    ) -> Result<(crate::grid::SnapshotBytes, u64), String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if terminal
            .snapshot
            .send(SnapshotRequest {
                scrollback: self.scrollback,
                max_bytes,
                max_frames,
                chunk_bytes: self.chunk_bytes,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            warn!(
                ?terminal_id,
                "pane actor dropped before synthesized bootstrap"
            );
            return Err("pane actor dropped synthesized bootstrap request".to_owned());
        }
        match reply_rx.await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(error)) => {
                warn!(?terminal_id, %error, "bounded snapshot synthesis failed");
                Err("synthesized bootstrap source limit exceeded".to_owned())
            }
            Err(_) => {
                warn!(
                    ?terminal_id,
                    "pane actor dropped synthesized snapshot reply"
                );
                Err("pane actor dropped synthesized snapshot reply".to_owned())
            }
        }
    }

    /// Capture one pane's bounded bootstrap, staging its pump and charging its
    /// retained result to the aggregate budget before the next actor is asked
    /// for its own source.
    async fn capture_pane(
        &self,
        staging: &mut AttachStaging,
        pane: AttachSnapshotPane,
    ) -> Result<(), String> {
        let synthesized_source_max =
            bootstrap_source_ceiling(staging.budget.remaining_bytes(), self.client_caps);
        let terminal_id = pane.terminal_id;
        let handle = pane.handle;
        let terminal = handle
            .terminal()
            .map_err(|error| error.to_string())?
            .clone();
        staging.handles.push(handle.clone());
        let wire_terminal_id = pane.wire_terminal_id;
        let registration = self
            .register_consumer(
                &handle,
                &wire_terminal_id,
                terminal_id,
                synthesized_source_max,
                staging.budget.remaining_frames(),
            )
            .await;
        if self.wants_state_sync() && !registration.registered {
            warn!(
                ?terminal_id,
                "state-sync registration failed before aggregate attach publication"
            );
            return Err("state-sync consumer registration failed".to_owned());
        }
        // A tick-managed consumer's deltas come from the actor, in its own
        // sequence space: a broadcast pump beside it would double-emit.
        if !registration.tick_managed {
            self.spawn_pane_pump(staging, terminal_id, &wire_terminal_id, &handle, &terminal);
        }
        if let Some(state_sync) = registration.state_sync_bootstrap {
            return self.stage_synthesized_frames(
                staging,
                wire_terminal_id,
                state_sync.snapshot,
                state_sync.base_seq,
                "state-sync bootstrap",
            );
        }
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if publishes_native_checkpoints(self.profile) {
            return self
                .stage_native_bootstrap(staging, terminal_id, &wire_terminal_id, &terminal)
                .await;
        }
        let (snapshot, cut) = self
            .request_pane_snapshot(
                &terminal,
                terminal_id,
                synthesized_source_max,
                staging.budget.remaining_frames(),
            )
            .await?;
        self.stage_synthesized_frames(
            staging,
            wire_terminal_id,
            snapshot,
            cut,
            "synthesized bootstrap",
        )?;
        if let Some(gate) = staging
            .gates
            .iter_mut()
            .find(|gate| gate.terminal_id == terminal_id)
        {
            gate.cut = Some(cut);
        }
        Ok(())
    }

    /// Capture every bootstrap-capable pane in turn, then stage authoritative
    /// closures for snapshot participants that had no actor handle. The first
    /// failure is the rollback reason.
    async fn capture_panes(
        &self,
        staging: &mut AttachStaging,
        panes: Vec<AttachSnapshotPane>,
        closed_before_ready: Vec<phux_protocol::ids::ResourceId>,
    ) -> Result<(), String> {
        for pane in panes {
            self.capture_pane(staging, pane).await?;
        }
        staging
            .append_closures(closed_before_ready)
            .map_err(|()| "aggregate bootstrap staging budget exceeded".to_owned())
    }
}

/// The atomic ATTACH publication, in the one order the handshake permits.
struct AttachPublication<'a> {
    state: &'a SharedState,
    out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    /// Cancelled when a published generation cannot be activated.
    connection_token: &'a CancellationToken,
    client_id: ClientId,
    attach_id: u32,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
}

impl AttachPublication<'_> {
    /// Queue one publication frame, releasing this client's consumer state
    /// when the outbound mailbox has closed.
    async fn queue(&self, frame: FrameKind) -> bool {
        if self.out_tx.send(Outbound::Frame(frame)).await.is_ok() {
            return true;
        }
        crate::runtime::client::detach_and_release_consumer_state(self.state, self.client_id);
        false
    }

    /// Queue `ATTACHED`, every staged pane bootstrap or authoritative closure,
    /// then `ATTACH_READY`. `false` once the client's mailbox has closed and
    /// the attach is abandoned.
    async fn publish(
        &self,
        snapshot: phux_protocol::wire::info::SessionSnapshot,
        initial_client_id: phux_protocol::ids::ClientId,
        frames: Vec<FrameKind>,
        session_name: &str,
    ) -> bool {
        if !self
            .queue(FrameKind::Attached {
                attach_id: self.attach_id,
                snapshot,
                initial_client_id,
            })
            .await
        {
            return false;
        }
        crate::hooks::fire_hook(
            self.state,
            crate::hooks::HookEvent::client_attached(self.client_id, session_name),
        );
        for frame in frames {
            if !self.queue(frame).await {
                return false;
            }
        }
        self.queue(FrameKind::AttachReady {
            attach_id: self.attach_id,
        })
        .await
    }

    /// Release every parked output pump now that the publication is on the
    /// wire, activating native publications for their replay and post-cut
    /// receiver.
    async fn release_gates(&self, gates: Vec<SnapshotGate>) {
        for gate in gates {
            let Some(cut) = gate.cut else {
                continue;
            };
            let mut start = OutputPumpStart {
                published_cut: cut,
                replay: Vec::new(),
                live: None,
            };
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            if let Some(cursor) = gate.native_cursor {
                let publication = match activate_native_publication(
                    &gate.terminal,
                    self.client_id.0,
                    gate.wire_terminal_id,
                    self.stream_id,
                    self.bootstrap_id,
                    cursor,
                )
                .await
                {
                    Ok(publication) => publication,
                    // The pane exited after its capture: dropping the gate
                    // ends its pump quietly, and `RESOURCE_CLOSED` tells the
                    // client. The rest of the attach is unaffected.
                    Err(failure) if failure.actor_gone() => continue,
                    Err(_) => {
                        crate::runtime::client::detach_and_release_consumer_state(
                            self.state,
                            self.client_id,
                        );
                        self.connection_token.cancel();
                        return;
                    }
                };
                start.replay = publication.replay;
                start.live = Some(publication.live);
            }
            let _ = gate.gate.send(start);
        }
    }
}

/// Handle `ATTACH`: resolve the target, prepare the snapshot, capture every
/// pane's bounded bootstrap, and publish `ATTACHED`, the bootstraps, and
/// `ATTACH_READY` atomically. Any failure sends `ERROR`; nothing is
/// partially attached.
#[allow(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    reason = "linear orchestration over the decomposed ATTACH payload; the rollback macro must return from this frame"
)]
#[tracing::instrument(
    level = "info",
    name = "handle_attach",
    skip_all,
    fields(?client_id, target = ?target, cols = viewport.cols, rows = viewport.rows),
)]
pub(crate) async fn handle_attach(
    state: &SharedState,
    client_id: ClientId,
    attach_id: u32,
    target: AttachTarget,
    viewport: phux_protocol::wire::frame::ViewportInfo,
    request_scrollback: bool,
    scrollback_limit_lines: u32,
    // ADR-0127: the declared intent for every Terminal this attach returns.
    role_policy: Option<phux_protocol::wire::frame::RolePolicy>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    client_caps: ClientCapabilities,
    negotiated_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
    root_token: &CancellationToken,
    output_pumps: &mut JoinSet<()>,
    connection_token: &CancellationToken,
    // QUIC multi-stream (proto.md §4.2): publish the snapshot only; per-pane
    // registration, pumps, and bootstraps start at `STREAM_BIND`.
    defer_subscription: bool,
) {
    // L1 §8.1: a viewer cannot take over; refused before anything changes.
    let role = role_policy.unwrap_or_default();
    if !role.is_valid() {
        send_error(
            out_tx,
            ErrorCode::MalformedMessage,
            "ATTACH role_policy { VIEWER, DELIBERATE } is invalid: a viewer cannot take over",
        )
        .await;
        return;
    }
    // Pin the session the dispatch guard authorized (nothing has awaited
    // since); from here the attach follows the id, never the name.
    let pinned = state.with(|s| crate::policy::resolve_attach_session(s, &target));
    let Some(stream_profile) = bootstrap_stream_profile(negotiated_profile) else {
        send_error(
            out_tx,
            ErrorCode::CodecUnavailable,
            "ATTACH selected an unsupported bootstrap profile",
        )
        .await;
        return;
    };
    // `scrollback_limit_lines == 0` means all retained history.
    let scrollback_req: Option<u32> = request_scrollback.then_some(scrollback_limit_lines);

    let Some(session) = resolve_attach_target(
        state,
        target,
        pinned,
        out_tx,
        root_token,
        client_caps.default_colors,
    )
    .await
    else {
        return;
    };

    refresh_registry_cwds(state).await;

    let same_session_reattach = is_same_session_reattach(state, client_id, session);

    let Some((snapshot, initial_client_id, panes_to_snapshot, closed_before_ready)) =
        prepare_attach_or_refuse(
            state,
            client_id,
            session,
            out_tx,
            client_caps,
            negotiated_profile,
            bootstrap_limits,
        )
        .await
    else {
        return;
    };
    let wire_client_id = super::wire_client(client_id);
    // For publication only: routing already followed the id.
    let session_name = state
        .with(|s| {
            s.registry()
                .session(session)
                .map(|found| found.name.clone())
        })
        .unwrap_or_default();
    if same_session_reattach {
        detach_prior_state_sync_consumers(&panes_to_snapshot, wire_client_id, client_caps).await;
    }

    apply_client_default_colors(&panes_to_snapshot, client_caps.default_colors).await;

    apply_attach_viewport(state, client_id, &panes_to_snapshot, viewport);

    if defer_subscription {
        let publication = AttachPublication {
            state,
            out_tx,
            connection_token,
            client_id,
            attach_id,
            stream_id: stream_id_from(u64::from(attach_id)),
            bootstrap_id: initial_bootstrap_id(),
        };
        // `STREAM_BIND` carries no role, so the session's role applies here.
        if publication
            .publish(snapshot, initial_client_id, Vec::new(), &session_name)
            .await
        {
            apply_session_role(
                state,
                client_id,
                &panes_to_snapshot,
                role,
                same_session_reattach,
            )
            .await;
        }
        return;
    }

    let stream_id = stream_id_from(u64::from(attach_id));
    let bootstrap_id = initial_bootstrap_id();
    let mut staging = AttachStaging::default();
    macro_rules! fail_prepublication {
        ($reason:expr) => {{
            fail_aggregate_attach_prepublication(
                state,
                client_id,
                attach_id,
                out_tx,
                connection_token,
                &staging.handles,
                &mut staging.pumps,
                output_pumps,
                $reason,
            )
            .await;
            return;
        }};
    }
    if staging
        .handles
        .try_reserve(panes_to_snapshot.len())
        .is_err()
    {
        fail_prepublication!("host allocation failed");
    }

    let (live_gate_tx, live_gate_rx) = tokio::sync::watch::channel(false);
    let Ok(aggregate_chunk_bytes) = usize::try_from(bootstrap_limits.max_chunk_bytes()) else {
        fail_prepublication!("bootstrap chunk bound cannot fit host");
    };
    let capture = PaneCaptureContext {
        state,
        out_tx,
        connection_token,
        live_gate_rx,
        client_id,
        wire_client_id,
        client_caps,
        stream_id,
        bootstrap_id,
        profile: stream_profile,
        limits: bootstrap_limits,
        scrollback: scrollback_req,
        chunk_bytes: aggregate_chunk_bytes,
    };
    // ADR-0127: roles apply only once the attach has published.
    let role_panes = panes_to_snapshot.clone();
    let capture_started = std::time::Instant::now();
    let captured = capture
        .capture_panes(&mut staging, panes_to_snapshot, closed_before_ready)
        .await;
    crate::perf::ATTACH_CAPTURE_WALL.record_elapsed(capture_started);
    if let Err(reason) = captured {
        fail_prepublication!(reason.as_str());
    }

    // Only now, with every bootstrap staged, retire the prior generation.
    if same_session_reattach {
        super::client::abort_output_pumps(output_pumps, client_id, "replacement ATTACH").await;
    }
    let mut committed_output_pumps = staging.pumps;
    output_pumps
        .spawn_local(async move { while committed_output_pumps.join_next().await.is_some() {} });

    let publication = AttachPublication {
        state,
        out_tx,
        connection_token,
        client_id,
        attach_id,
        stream_id,
        bootstrap_id,
    };
    let publication_started = std::time::Instant::now();
    let published = publication
        .publish(snapshot, initial_client_id, staging.frames, &session_name)
        .await;
    crate::perf::ATTACH_PUBLISH_WALL.record_elapsed(publication_started);
    if !published {
        return;
    }
    apply_session_role(state, client_id, &role_panes, role, same_session_reattach).await;
    let _ = live_gate_tx.send(true);
    publication.release_gates(staging.gates).await;
}

/// Record the ATTACH viewport and resize every pane to the window-size
/// policy across its subscribers, so full-screen programs fill the client's
/// terminal. Zero dimensions are a no-op (SPEC §10.5); accepted geometry is
/// coalesced until the actor can apply it before capturing the bootstrap.
pub(crate) fn apply_attach_viewport(
    state: &SharedState,
    client_id: ClientId,
    panes_to_snapshot: &[AttachSnapshotPane],
    viewport: phux_protocol::wire::frame::ViewportInfo,
) {
    let cols = viewport.cols;
    let rows = viewport.rows;
    if cols == 0 || rows == 0 {
        return;
    }
    state.with_mut(|s| {
        s.set_client_viewport(client_id, viewport);
        for pane in panes_to_snapshot {
            if s.retained_exit(pane.terminal_id).is_some() {
                continue;
            }
            let Some((cols, rows)) = s.resolve_terminal_geometry(pane.terminal_id, Some(viewport))
            else {
                continue;
            };
            // No resync: the attach bootstrap is authoritative, and a resync
            // would race ahead of it.
            let Ok(terminal) = pane.handle.terminal() else {
                continue;
            };
            match terminal.resize.try_send(ResizeRequest {
                cols,
                rows,
                cell_px: s.resolve_terminal_cell_px(pane.terminal_id),
                resync_clients: false,
                resync_only: false,
                resync_for: None,
            }) {
                Ok(()) => {
                    if let Some(pane_entry) = s.registry_mut().terminal_mut(pane.terminal_id) {
                        pane_entry.dims = (cols, rows);
                    }
                }
                Err(error) => {
                    debug!(
                        terminal_id = ?pane.terminal_id, %error,
                        "ATTACH viewport apply: pane actor gone; dropping resize",
                    );
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn final_screen_frames(terminal_id: &phux_protocol::ids::ResourceId) -> Vec<FrameKind> {
        synthesized_bootstrap_frames(
            terminal_id.clone(),
            phux_protocol::ids::StreamId::new(1).expect("stream id"),
            phux_protocol::ids::BootstrapId::new(2).expect("bootstrap id"),
            BootstrapStreamProfile::SynthesizedVtRaw,
            BootstrapLimits::default(),
            80,
            24,
            9,
            [bytes::Bytes::from_static(b"FINAL_SCREEN")],
        )
        .expect("bootstrap frames")
    }

    /// phux-fpgl.28: a lagged mailbox is full. A gap resync must not park on
    /// it, and the exit snapshot's chunk — the final screen — must be queued
    /// ahead of `RESOURCE_CLOSED`, not split off after `BOOTSTRAP_BEGIN`.
    #[tokio::test(flavor = "current_thread")]
    async fn exit_resync_on_a_full_mailbox_precedes_close() {
        use phux_protocol::wire::frame::CloseReason;

        let (tx, mut rx) =
            tokio::sync::mpsc::channel::<Outbound>(crate::mailbox::DEFAULT_CLIENT_MAILBOX);
        for nonce in 0..u64::try_from(tx.max_capacity()).expect("mailbox depth fits u64") {
            tx.try_send(Outbound::Frame(FrameKind::Pong { nonce }))
                .expect("fill the lagged mailbox");
        }
        let terminal_id = phux_protocol::ids::ResourceId::local(1);
        let frames = final_screen_frames(&terminal_id);
        assert!(frames.len() > 1 && frames.len() <= tx.max_capacity());

        let deferred = queue_resync_bootstrap(
            &tx,
            crate::terminal_actor::ResyncReason::OutboundGap,
            frames.clone(),
            true,
        )
        .await;
        assert_eq!((deferred, tx.capacity()), (SnapshotQueue::Deferred, 0));

        // Park the exit reservation on this task before close asks for a
        // slot. `yield_now` does not order two spawned waiters, and the
        // first one in the semaphore queue takes every freed slot.
        let exit =
            queue_resync_bootstrap(&tx, crate::terminal_actor::ResyncReason::Exit, frames, true);
        tokio::pin!(exit);
        std::future::poll_fn(|cx| match std::future::Future::poll(exit.as_mut(), cx) {
            std::task::Poll::Pending => std::task::Poll::Ready(()),
            std::task::Poll::Ready(queue) => {
                panic!("exit bootstrap finished on a full mailbox: {queue:?}")
            }
        })
        .await;
        let close = tx.send(Outbound::Frame(FrameKind::ResourceClosed {
            terminal_id,
            exit_status: Some(0),
            reason: CloseReason::Exited,
            signal: None,
        }));
        tokio::pin!(close);
        tokio::select! {
            biased;
            queue = &mut exit => panic!("exit bootstrap finished before any drain: {queue:?}"),
            result = &mut close => {
                panic!("close took a slot ahead of the exit bootstrap: {result:?}")
            }
            () = std::future::ready(()) => {}
        }
        assert_eq!(tx.capacity(), 0);

        let mut saw_chunk = false;
        let mut saw_ready = false;
        let mut exit_queued = false;
        let mut close_queued = false;
        loop {
            tokio::select! {
                biased;
                queue = &mut exit, if !exit_queued => {
                    assert_eq!(queue, SnapshotQueue::Queued);
                    exit_queued = true;
                }
                result = &mut close, if !close_queued => {
                    result.expect("queue RESOURCE_CLOSED");
                    close_queued = true;
                }
                message = rx.recv() => {
                    match message {
                        Some(Outbound::Frame(FrameKind::BootstrapChunk { payload, .. })) => {
                            assert_eq!(payload.as_ref(), b"FINAL_SCREEN");
                            saw_chunk = true;
                        }
                        Some(Outbound::Frame(FrameKind::BootstrapReady { .. })) => {
                            assert!(saw_chunk, "READY arrived before the final chunk");
                            saw_ready = true;
                        }
                        Some(Outbound::Frame(FrameKind::ResourceClosed { .. })) => break,
                        Some(_) => {}
                        None => panic!("mailbox closed before RESOURCE_CLOSED"),
                    }
                }
            }
        }
        assert!(
            saw_chunk && saw_ready && exit_queued && close_queued,
            "RESOURCE_CLOSED split the final screen off the bootstrap"
        );
    }

    fn tiny_staged_pane(pane: u32) -> Vec<FrameKind> {
        let terminal_id = phux_protocol::ids::ResourceId::local(pane + 1);
        let stream_id = StreamId::new(1).expect("stream id");
        let bootstrap_id = BootstrapId::new(1).expect("bootstrap id");
        vec![
            FrameKind::BootstrapBegin {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 80,
                rows: 24,
                base_seq: 0,
            },
            FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: bytes::Bytes::from_static(b"pane"),
            },
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
        ]
    }

    #[test]
    fn aggregate_staging_budget_rejects_many_panes_without_large_allocations() {
        let mut budget = BootstrapStagingBudget::with_limits(8 * 4, 8 * 3);
        let mut staged = Vec::new();

        for pane in 0..16 {
            let mut frames = tiny_staged_pane(pane);
            let result = budget.append_accounted(&mut staged, &mut frames, 4);
            if pane < 8 {
                assert!(result.is_ok(), "pane {pane} fits the aggregate budget");
                assert!(frames.is_empty(), "accepted frames move into staging");
            } else {
                assert!(result.is_err(), "pane {pane} exceeds the aggregate budget");
                assert_eq!(frames.len(), 3, "rejected frames are not appended");
            }
        }

        assert_eq!(staged.len(), 8 * 3);
        assert_eq!(budget.staged_bytes, 8 * 4);
        assert_eq!(budget.staged_frames, 8 * 3);
    }

    #[test]
    fn bootstrap_adaptation_peak_includes_sources_scratch_and_outputs() {
        let mut scrollback = Vec::new();
        scrollback
            .try_reserve_exact(512)
            .expect("scrollback reserve");
        scrollback.resize(512, b's');
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(1_024).expect("snapshot reserve");
        bytes.resize(1_024, b'x');
        let source_capacity = scrollback.capacity() + bytes.capacity();
        let peak_budget = source_capacity.checked_mul(2).expect("peak budget");
        let caps = ClientCapabilities::default()
            .with_color_support(phux_protocol::caps::ColorSupport::Indexed256);
        assert!(!crate::downsample::caps_pass_through(caps));

        let adapted = adapt_bootstrap_snapshot(
            crate::grid::SnapshotBytes {
                cols: 80,
                rows: 24,
                bytes,
                scrollback,
            },
            caps,
            peak_budget,
        )
        .expect("bounded capability adaptation");

        assert_eq!(
            adapted
                .payloads
                .iter()
                .map(bytes::Bytes::len)
                .sum::<usize>(),
            source_capacity,
        );
        assert_eq!(adapted.retained_bytes, source_capacity);
        assert!(adapted.peak_bytes <= peak_budget);
    }

    #[test]
    fn native_live_bytes_ignore_restrictive_presentation_caps() {
        let caps = ClientCapabilities::default()
            .with_color_support(phux_protocol::caps::ColorSupport::Mono)
            .with_hyperlinks(false)
            .with_image_protocols(phux_protocol::caps::ImageProtocolSet::new());
        let bytes = bytes::Bytes::from_static(
            b"\x1b[38;2;1;2;3m\x1b]8;;https://example.invalid\x1b\\exact\xff",
        );
        let profile = phux_protocol::caps::BootstrapStreamProfile::NativeState {
            codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1,
        };

        let adapted = live_bytes_for_profile(&bytes, caps, profile);

        assert_eq!(adapted, bytes);
        assert_eq!(
            adapted.as_ptr(),
            bytes.as_ptr(),
            "native path keeps the shared bytes"
        );
    }

    #[test]
    fn aggregate_staging_charges_tiny_rewrites_by_retained_capacity() {
        fn kitty_snapshot() -> crate::grid::SnapshotBytes {
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(64 * 1024).expect("kitty reserve");
            bytes.extend_from_slice(b"\x1b_Gf=100,a=T;");
            bytes.resize((64 * 1024) - 2, b'A');
            bytes.extend_from_slice(b"\x1b\\");
            crate::grid::SnapshotBytes {
                cols: 80,
                rows: 24,
                bytes,
                scrollback: Vec::new(),
            }
        }
        let caps = ClientCapabilities::default()
            .with_color_support(phux_protocol::caps::ColorSupport::Indexed256)
            .with_image_protocols(phux_protocol::caps::ImageProtocolSet::new());
        let sample =
            adapt_bootstrap_snapshot(kitty_snapshot(), caps, 2 * 64 * 1024).expect("rewrite");
        let retained_per_pane = sample.retained_bytes;
        let wire_per_pane = sample.payloads.iter().map(bytes::Bytes::len).sum::<usize>();
        assert!(
            retained_per_pane > wire_per_pane,
            "dropped Kitty payload retains rewrite allocation capacity"
        );
        drop(sample);

        let mut budget = BootstrapStagingBudget::with_limits(retained_per_pane * 3, usize::MAX);
        let mut staged = Vec::new();
        for pane in 0..4_u32 {
            let adapted = adapt_bootstrap_snapshot(
                kitty_snapshot(),
                caps,
                retained_per_pane.checked_mul(2).expect("peak budget"),
            )
            .expect("bounded pane rewrite");
            let retained_bytes = adapted.retained_bytes;
            let mut frames = synthesized_bootstrap_frames(
                phux_protocol::ids::ResourceId::local(pane + 1),
                StreamId::new(u64::from(pane) + 1).expect("stream id"),
                BootstrapId::new(u64::from(pane) + 1).expect("bootstrap id"),
                BootstrapStreamProfile::SynthesizedVtRaw,
                BootstrapLimits::new(
                    phux_protocol::MAX_BOOTSTRAP_CHUNK_BYTES,
                    phux_protocol::DEFAULT_HISTORY_PAGE_BYTES,
                )
                .expect("limits"),
                80,
                24,
                0,
                adapted.payloads,
            )
            .expect("bootstrap frames");
            let result = budget.append_accounted(&mut staged, &mut frames, retained_bytes);
            assert_eq!(result.is_ok(), pane < 3);
        }
        assert_eq!(budget.staged_bytes, retained_per_pane * 3);
    }

    #[test]
    fn synthesized_bootstrap_is_built_completely_before_publication() {
        let terminal_id = phux_protocol::ids::ResourceId::local(7);
        let stream_id = StreamId::new(3).expect("stream id");
        let bootstrap_id = BootstrapId::new(5).expect("bootstrap id");
        let limits = BootstrapLimits::new(3, phux_protocol::DEFAULT_HISTORY_PAGE_BYTES)
            .expect("bounded test limits");
        let frames = synthesized_bootstrap_frames(
            terminal_id.clone(),
            stream_id,
            bootstrap_id,
            BootstrapStreamProfile::SynthesizedVtRaw,
            limits,
            80,
            24,
            11,
            [bytes::Bytes::from_static(b"abcdefg")],
        )
        .expect("build complete bootstrap");

        assert!(matches!(
            frames.first(),
            Some(FrameKind::BootstrapBegin {
                terminal_id: id,
                stream_id: stream,
                bootstrap_id: bootstrap,
                base_seq: 11,
                ..
            }) if id == &terminal_id && *stream == stream_id && *bootstrap == bootstrap_id
        ));
        let chunks: Vec<_> = frames
            .iter()
            .filter_map(|frame| match frame {
                FrameKind::BootstrapChunk {
                    chunk_seq, payload, ..
                } => Some((*chunk_seq, payload.as_ref())),
                _ => None,
            })
            .collect();
        assert_eq!(
            chunks,
            vec![
                (0, b"abc".as_slice()),
                (1, b"def".as_slice()),
                (2, b"g".as_slice())
            ]
        );
        assert!(matches!(
            frames.last(),
            Some(FrameKind::BootstrapReady {
                terminal_id: id,
                stream_id: stream,
                bootstrap_id: bootstrap,
                history_cursor: None,
            }) if id == &terminal_id && *stream == stream_id && *bootstrap == bootstrap_id
        ));
    }

    #[test]
    fn prepare_attach_reports_snapshot_seed_without_an_actor_as_closed() {
        let state = SharedState::new();
        let (_catalog_session, _catalog_window, _catalog_pane) =
            state.with_mut(|server| server.seed_session("catalog"));
        let (_working_session, working_window, _seed) =
            state.with_mut(|server| server.seed_session("working"));
        let (horizontal, vertical) = state.with_mut(|server| {
            let horizontal = server
                .registry_mut()
                .new_terminal(working_window)
                .expect("horizontal pane");
            let vertical = server
                .registry_mut()
                .new_terminal(working_window)
                .expect("vertical pane");
            (horizontal, vertical)
        });
        let mut actors = Vec::new();
        for terminal in <[_; 2]>::from((horizontal, vertical)) {
            let token = CancellationToken::new();
            let bundle = crate::terminal_actor::TerminalActor::build_with_token(
                80,
                24,
                None,
                phux_config::ScrollbackLimits::default(),
                token.clone(),
            )
            .expect("test terminal actor");
            state.with_mut(|server| {
                server.register_resource_handle(terminal, bundle.handle.clone(), token);
            });
            actors.push(bundle.actor);
        }
        let client_id = state.with_mut(crate::state::ServerState::new_client_id);
        let (out_tx, _out_rx) = tokio::sync::mpsc::channel(crate::state::DEFAULT_CLIENT_MAILBOX);

        let working = state
            .with(|s| s.find_session_by_name("working"))
            .expect("the seeded session");
        let (snapshot, _initial_client_id, bootstrapped, closed) = prepare_attach(
            &state,
            client_id,
            working,
            &out_tx,
            ClientCapabilities::default(),
            BootstrapProfile::SynthesizedVtRaw,
            BootstrapLimits::default(),
        )
        .expect("prepare attach");

        let bootstrapped: std::collections::HashSet<_> = bootstrapped
            .iter()
            .map(|pane| pane.wire_terminal_id.clone())
            .collect();
        let seed = snapshot
            .resources
            .iter()
            .find(|pane| pane.id == snapshot.focused_resource)
            .expect("seed remains in ATTACHED catalog")
            .id
            .clone();
        assert_eq!(snapshot.sessions.len(), 2, "whole session catalog survives");
        assert_eq!(
            snapshot.resources.len(),
            4,
            "ATTACHED keeps every catalog pane"
        );
        assert_eq!(bootstrapped.len(), 2);
        assert!(!bootstrapped.contains(&seed));
        assert_eq!(closed, vec![seed]);
        drop(actors);
    }

    #[test]
    fn prepare_attach_rejects_pane_source_count_before_registration() {
        let state = SharedState::new();
        let (_session, window, _pane) = state.with_mut(|server| server.seed_session("bounded"));
        state.with_mut(|server| {
            for _ in 0..MAX_AGGREGATE_BOOTSTRAP_PANES {
                server
                    .registry_mut()
                    .new_terminal(window)
                    .expect("bounded test pane");
            }
        });
        let client_id = state.with_mut(crate::state::ServerState::new_client_id);
        let (out_tx, _out_rx) = tokio::sync::mpsc::channel(crate::state::DEFAULT_CLIENT_MAILBOX);
        let bounded = state
            .with(|s| s.find_session_by_name("bounded"))
            .expect("the seeded session");
        assert!(matches!(
            prepare_attach(
                &state,
                client_id,
                bounded,
                &out_tx,
                ClientCapabilities::default(),
                BootstrapProfile::SynthesizedVtRaw,
                BootstrapLimits::default(),
            ),
            Err(crate::state::AttachError::ResourceLimit)
        ));
        assert!(!state.with(|server| server.attached().contains_key(&client_id)));
    }

    /// The stream every pump in the two-pump tests publishes on.
    fn two_pump_stream() -> StreamId {
        StreamId::new(1).expect("stream id")
    }

    /// The generation each test pump opens with.
    fn two_pump_initial_generation() -> BootstrapId {
        BootstrapId::new(1).expect("bootstrap id")
    }

    /// A running ATTACH output pump on a shared pane broadcast, with its
    /// consumer's mailbox exposed.
    struct TwoPumpConsumer {
        out_rx: tokio::sync::mpsc::Receiver<Outbound>,
        task: tokio::task::JoinHandle<Option<PumpFault>>,
    }

    /// Start one synthesized-profile ATTACH pump for `client` whose published
    /// bootstrap covers everything up to `published_cut`.
    fn spawn_two_pump_consumer(
        client: u64,
        published_cut: u64,
        mailbox: usize,
        output: &tokio::sync::broadcast::Sender<PaneOutput>,
        resize: &ResizeSender,
    ) -> TwoPumpConsumer {
        let (out_tx, out_rx) = tokio::sync::mpsc::channel(mailbox);
        let ctx = OutputPumpContext {
            out_tx,
            resize: resize.clone(),
            wire_terminal_id: phux_protocol::ids::ResourceId::local(1),
            stream_id: two_pump_stream(),
            initial_bootstrap_id: two_pump_initial_generation(),
            client_id: ClientId(client),
            client_caps: ClientCapabilities::default(),
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            limits: BootstrapLimits::default(),
            lag_label: "two-pump test pump",
            stale_skip: true,
            cancel: None,
            last_seq: None,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            terminal: native_attach_handle()
                .0
                .terminal()
                .expect("terminal facet")
                .clone(),
        };
        let (gate_tx, gate_rx) = oneshot::channel();
        gate_tx
            .send(OutputPumpStart {
                published_cut,
                replay: Vec::new(),
                live: None,
            })
            .unwrap_or_else(|_| panic!("gate receiver alive"));
        let live = output.subscribe();
        let task =
            tokio::task::spawn_local(async move { run_output_pump(&ctx, gate_rx, live).await });
        TwoPumpConsumer { out_rx, task }
    }

    /// What one consumer's mirror saw, reduced to what these tests judge.
    #[derive(Debug, PartialEq, Eq)]
    enum Seen {
        Output {
            generation: BootstrapId,
            seq: u64,
        },
        Begin {
            generation: BootstrapId,
            base_seq: u64,
        },
        Chunk,
        Ready {
            generation: BootstrapId,
        },
        Tombstone,
        Other(String),
    }

    impl Seen {
        fn of(outbound: Outbound) -> Self {
            let Outbound::Frame(frame) = outbound else {
                return Self::Other(format!("{outbound:?}"));
            };
            match frame {
                FrameKind::ResourceOutput {
                    bootstrap_id, seq, ..
                } => Self::Output {
                    generation: bootstrap_id,
                    seq,
                },
                FrameKind::BootstrapBegin {
                    bootstrap_id,
                    base_seq,
                    ..
                } => Self::Begin {
                    generation: bootstrap_id,
                    base_seq,
                },
                FrameKind::BootstrapChunk { .. } => Self::Chunk,
                FrameKind::BootstrapReady { bootstrap_id, .. } => Self::Ready {
                    generation: bootstrap_id,
                },
                FrameKind::BootstrapTombstone { .. } => Self::Tombstone,
                other => Self::Other(format!("{other:?}")),
            }
        }
    }

    /// Generous: only a broken pump ever hits it.
    const TWO_PUMP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

    /// Drain `consumer` until it has seen `count` frames.
    async fn frames_seen(consumer: &mut TwoPumpConsumer, count: usize) -> Vec<Seen> {
        let mut seen = Vec::with_capacity(count);
        while seen.len() < count {
            let outbound = tokio::time::timeout(TWO_PUMP_DEADLINE, consumer.out_rx.recv())
                .await
                .unwrap_or_else(|_| panic!("pump went quiet after {seen:?}"))
                .expect("pump mailbox closed");
            seen.push(Seen::of(outbound));
        }
        seen
    }

    /// Let every pump task on the `LocalSet` run until it parks again.
    async fn let_pumps_run() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    /// The consumer has nothing further queued: no late tombstone or
    /// republish is sitting in its mailbox.
    async fn assert_quiet(consumer: &mut TwoPumpConsumer, who: &str) {
        let_pumps_run().await;
        if let Ok(outbound) = consumer.out_rx.try_recv() {
            panic!("{who} saw an unexpected frame: {:?}", Seen::of(outbound));
        }
    }

    /// Wait for the gap resync request the pump behind `laggard` sends; on
    /// timeout, report what the laggard was sent and whether its pump died.
    async fn resync_request_from(
        resize_rx: &mut crate::terminal_actor::ResizeReceiver,
        laggard: &mut TwoPumpConsumer,
    ) -> ResizeRequest {
        let_pumps_run().await;
        if let Ok(received) = tokio::time::timeout(TWO_PUMP_DEADLINE, resize_rx.recv()).await {
            return received.expect("resize mailbox closed");
        }
        let mut sent = Vec::new();
        while let Ok(outbound) = laggard.out_rx.try_recv() {
            sent.push(Seen::of(outbound));
        }
        panic!(
            "no resync request reached the actor; the laggard was sent {sent:?} \
             (pump task finished: {})",
            laggard.task.is_finished(),
        );
    }

    /// Answer a gap resync the way the actor does: one broadcast `Resync`,
    /// addressed to exactly the pump the request named.
    fn answer_gap_resync(
        output: &tokio::sync::broadcast::Sender<PaneOutput>,
        request: &ResizeRequest,
        base_seq: u64,
    ) {
        let audience = request.resync_for.map_or(ResyncAudience::Everyone, |pump| {
            ResyncAudience::Only(vec![pump].into())
        });
        output
            .send(PaneOutput::Resync {
                cols: 80,
                rows: 24,
                reason: crate::terminal_actor::ResyncReason::OutboundGap,
                audience,
                base_seq,
                bytes: bytes::Bytes::from_static(b"snapshot"),
            })
            .expect("pumps subscribed");
    }

    fn live(seq: u64, at: std::time::Instant) -> PaneOutput {
        PaneOutput::Live {
            seq,
            bytes: bytes::Bytes::from_static(b"x"),
            at,
        }
    }

    /// One consumer going stale re-bootstraps that consumer and nobody else
    /// on the pane; a later reflow is still owed to both.
    #[tokio::test(flavor = "current_thread")]
    #[allow(
        clippy::too_many_lines,
        reason = "one end-to-end scenario: stale, targeted recovery, then a reflow"
    )]
    async fn a_stale_consumer_resyncs_without_republishing_its_neighbour() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (output, _seed) = tokio::sync::broadcast::channel::<PaneOutput>(64);
                let (resize_tx, mut resize_rx) = ResizeSender::channel(8);
                let mut fresh = spawn_two_pump_consumer(1, 2, 64, &output, &resize_tx);
                let mut stale = spawn_two_pump_consumer(2, 1, 64, &output, &resize_tx);
                let initial = two_pump_initial_generation();
                let replacement = next_bootstrap_id(initial);

                // Staleness counts from the later of the read and the
                // generation's publication, so open the pumps and outlive the
                // budget before sending a backdated chunk.
                let_pumps_run().await;
                tokio::time::sleep(
                    crate::runtime::pump::STALE_OUTPUT_BUDGET
                        + std::time::Duration::from_millis(50),
                )
                .await;
                let read_a_second_ago = std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(1))
                    .expect("monotonic clock has run for a second");
                output
                    .send(live(2, read_a_second_ago))
                    .expect("pumps subscribed");

                let request = resync_request_from(&mut resize_rx, &mut stale).await;
                assert!(request.resync_only && request.resync_clients);
                assert_eq!(
                    request.resync_for,
                    Some(ResyncTarget {
                        owner: 2,
                        stream_id: two_pump_stream(),
                        bootstrap_id: initial,
                    }),
                    "the resync names the stale pump, not the pane",
                );

                let now = std::time::Instant::now();
                output.send(live(3, now)).expect("pumps subscribed");
                answer_gap_resync(&output, &request, 3);
                output.send(live(4, now)).expect("pumps subscribed");

                assert_eq!(
                    frames_seen(&mut fresh, 2).await,
                    vec![
                        Seen::Output {
                            generation: initial,
                            seq: 3
                        },
                        Seen::Output {
                            generation: initial,
                            seq: 4
                        },
                    ],
                    "the fresh consumer keeps its generation straight through",
                );
                assert_eq!(
                    frames_seen(&mut stale, 5).await,
                    vec![
                        Seen::Tombstone,
                        Seen::Begin {
                            generation: replacement,
                            base_seq: 3
                        },
                        Seen::Chunk,
                        Seen::Ready {
                            generation: replacement
                        },
                        Seen::Output {
                            generation: replacement,
                            seq: 4
                        },
                    ],
                    "the stale consumer converges onto a fresh generation",
                );
                assert_quiet(&mut fresh, "the fresh consumer").await;
                assert_quiet(&mut stale, "the stale consumer").await;
                assert!(
                    resize_rx.try_recv().is_err(),
                    "nobody else asked the actor for a resync",
                );

                // A reflow changes the grid under everyone: still broadcast.
                output
                    .send(PaneOutput::Resync {
                        cols: 100,
                        rows: 30,
                        reason: crate::terminal_actor::ResyncReason::Resize,
                        audience: ResyncAudience::Everyone,
                        base_seq: 4,
                        bytes: bytes::Bytes::from_static(b"reflowed"),
                    })
                    .expect("pumps subscribed");
                assert_eq!(
                    frames_seen(&mut fresh, 2).await,
                    vec![
                        Seen::Tombstone,
                        Seen::Begin {
                            generation: replacement,
                            base_seq: 4
                        }
                    ],
                );
                assert_eq!(
                    frames_seen(&mut stale, 2).await,
                    vec![
                        Seen::Tombstone,
                        Seen::Begin {
                            generation: next_bootstrap_id(replacement),
                            base_seq: 4
                        }
                    ],
                );

                drop(output);
                for consumer in [fresh, stale] {
                    let fault = consumer.task.await.expect("pump task");
                    assert!(fault.is_none(), "pumps stop cleanly when the pane closes");
                }
            })
            .await;
    }

    /// A consumer whose mailbox fills is re-bootstrapped alone, without
    /// waiting for it to drain; its neighbour sees every chunk on its
    /// original generation.
    #[tokio::test(flavor = "current_thread")]
    async fn a_lagged_consumer_resyncs_without_republishing_its_neighbour() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (output, _seed) = tokio::sync::broadcast::channel::<PaneOutput>(4);
                let (resize_tx, mut resize_rx) = ResizeSender::channel(8);
                let mut fresh = spawn_two_pump_consumer(1, 0, 64, &output, &resize_tx);
                let mut lagging = spawn_two_pump_consumer(2, 0, 1, &output, &resize_tx);
                let initial = two_pump_initial_generation();
                let replacement = next_bootstrap_id(initial);

                // The lagging consumer's one slot takes seq 1; seq 2 is a gap.
                let now = std::time::Instant::now();
                for seq in 1..=9 {
                    output.send(live(seq, now)).expect("pumps subscribed");
                    let_pumps_run().await;
                }
                let request = resync_request_from(&mut resize_rx, &mut lagging).await;
                assert_eq!(
                    request.resync_for,
                    Some(ResyncTarget {
                        owner: 2,
                        stream_id: two_pump_stream(),
                        bootstrap_id: two_pump_initial_generation(),
                    }),
                    "the resync names the lagging pump, not the pane",
                );

                assert_eq!(
                    frames_seen(&mut lagging, 1).await,
                    vec![Seen::Output {
                        generation: initial,
                        seq: 1
                    }],
                );
                answer_gap_resync(&output, &request, 9);
                assert_eq!(
                    frames_seen(&mut lagging, 4).await,
                    vec![
                        Seen::Tombstone,
                        Seen::Begin {
                            generation: replacement,
                            base_seq: 9
                        },
                        Seen::Chunk,
                        Seen::Ready {
                            generation: replacement
                        },
                    ],
                    "the lagging consumer converges onto a fresh generation",
                );

                // Stamped after the republish so it cannot age past the budget.
                output
                    .send(live(10, std::time::Instant::now()))
                    .expect("pumps subscribed");

                let expected_fresh: Vec<_> = (1..=10)
                    .map(|seq| Seen::Output {
                        generation: initial,
                        seq,
                    })
                    .collect();
                assert_eq!(frames_seen(&mut fresh, 10).await, expected_fresh);
                assert_eq!(
                    frames_seen(&mut lagging, 1).await,
                    vec![Seen::Output {
                        generation: replacement,
                        seq: 10
                    }],
                );
                assert_quiet(&mut fresh, "the fresh consumer").await;
                assert_quiet(&mut lagging, "the lagging consumer").await;

                drop(output);
                for consumer in [fresh, lagging] {
                    let fault = consumer.task.await.expect("pump task");
                    assert!(fault.is_none(), "pumps stop cleanly when the pane closes");
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn saturated_resync_mailbox_blocks_until_actor_accepts_request() {
        let (tx, mut rx) = ResizeSender::channel(1);
        tx.send(ResizeRequest {
            cols: 80,
            rows: 24,
            cell_px: None,
            resync_clients: false,
            resync_only: true,
            resync_for: None,
        })
        .await
        .expect("occupy resize mailbox");

        let pump = ResyncTarget {
            owner: 7,
            stream_id: StreamId::new(3).expect("stream id"),
            bootstrap_id: BootstrapId::new(1).expect("bootstrap id"),
        };
        let mut pending = Box::pin(enqueue_output_resync(&tx, pump));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut pending)
                .await
                .is_err(),
            "lagged pump must not resume while the resync mailbox is full"
        );
        assert!(
            rx.recv().await.expect("occupied request").resync_only,
            "first request is the existing mailbox occupant"
        );
        assert!(pending.await, "resync queues once capacity is available");
        let queued = rx.recv().await.expect("queued resync");
        assert!(queued.resync_only && queued.resync_clients);
        assert_eq!(
            queued.resync_for,
            Some(pump),
            "a gap resync names the pump that fell behind, so nobody else republishes",
        );

        drop(rx);
        assert!(
            !enqueue_output_resync(&tx, pump).await,
            "closed actor mailbox fails instead of resuming delta forwarding"
        );
    }

    /// A Terminal handle with every channel closed (no actor behind it).
    fn detached_handle(
        facet: crate::terminal_actor::TerminalHandle,
    ) -> crate::resource::ResourceHandle {
        crate::resource::ResourceHandle {
            kind: crate::resource::ResourceKind::Terminal,
            parent: None,
            output: tokio::sync::broadcast::channel(8).0,
            consumer_attach: tokio::sync::mpsc::channel(1).0,
            consumer_detach: tokio::sync::mpsc::channel(1).0,
            consumer_ack: tokio::sync::mpsc::channel(1).0,
            upgrade: tokio::sync::mpsc::channel(1).0,
            control: tokio::sync::mpsc::channel(1).0,
            facet: crate::resource::ResourceFacetHandle::Terminal(facet),
        }
    }

    /// Drive `handle_attach` for session `name` with fixed defaults.
    #[allow(
        clippy::too_many_arguments,
        reason = "test shorthand for handle_attach"
    )]
    async fn attach(
        state: &SharedState,
        client_id: ClientId,
        attach_id: u32,
        name: &str,
        role: Option<phux_protocol::wire::frame::RolePolicy>,
        out_tx: &tokio::sync::mpsc::Sender<Outbound>,
        profile: BootstrapProfile,
        output_pumps: &mut JoinSet<()>,
        connection_token: &CancellationToken,
        defer_subscription: bool,
    ) {
        handle_attach(
            state,
            client_id,
            attach_id,
            AttachTarget::ByName(name.to_owned()),
            phux_protocol::wire::frame::ViewportInfo::new(80, 24),
            false,
            0,
            role,
            out_tx,
            ClientCapabilities::default(),
            profile,
            BootstrapLimits::default(),
            &CancellationToken::new(),
            output_pumps,
            connection_token,
            defer_subscription,
        )
        .await;
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn native_attach_handle() -> (
        ResourceHandle,
        tokio::sync::mpsc::Receiver<crate::terminal_actor::ConsumerAttachRequest>,
        tokio::sync::mpsc::Receiver<crate::terminal_actor::NativeBootstrapRequest>,
        tokio::sync::mpsc::Receiver<crate::terminal_actor::NativePublicationRequest>,
    ) {
        let (consumer_attach, consumer_attach_rx) = tokio::sync::mpsc::channel(8);
        let (native_bootstrap, native_bootstrap_rx) = tokio::sync::mpsc::channel(8);
        let (native_publication, native_publication_rx) = tokio::sync::mpsc::channel(8);
        let handle = ResourceHandle {
            consumer_attach,
            ..detached_handle(crate::terminal_actor::TerminalHandle {
                native_bootstrap,
                native_publication,
                ..crate::terminal_actor::TerminalHandle::detached_for_test(80, 24)
            })
        };
        (
            handle,
            consumer_attach_rx,
            native_bootstrap_rx,
            native_publication_rx,
        )
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn answer_native_attach(
        consumer_attach_rx: &mut tokio::sync::mpsc::Receiver<
            crate::terminal_actor::ConsumerAttachRequest,
        >,
        native_bootstrap_rx: &mut tokio::sync::mpsc::Receiver<
            crate::terminal_actor::NativeBootstrapRequest,
        >,
        native_publication_rx: &mut tokio::sync::mpsc::Receiver<
            crate::terminal_actor::NativePublicationRequest,
        >,
        succeed: bool,
    ) {
        let registration = consumer_attach_rx
            .recv()
            .await
            .expect("consumer registration");
        registration
            .reply
            .send(Ok(crate::terminal_actor::ConsumerAttachOutcome {
                tick_managed: false,
                state_sync_bootstrap: None,
            }))
            .expect("consumer registration reply");
        let native = native_bootstrap_rx.recv().await.expect("native preflight");
        if !succeed {
            native
                .reply
                .send(Err(crate::native_state::NativeStateError::LimitExceeded))
                .expect("continuation-cap failure reply");
            return;
        }
        let terminal_id = native.terminal_id.clone();
        native
            .reply
            .send(Ok(crate::terminal_actor::NativeBootstrapReply {
                frames: vec![
                    FrameKind::BootstrapBegin {
                        terminal_id: terminal_id.clone(),
                        stream_id: native.stream_id,
                        bootstrap_id: native.bootstrap_id,
                        profile: BootstrapStreamProfile::NativeState {
                            codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1,
                        },
                        cols: 80,
                        rows: 24,
                        base_seq: 0,
                    },
                    FrameKind::BootstrapChunk {
                        terminal_id: terminal_id.clone(),
                        stream_id: native.stream_id,
                        bootstrap_id: native.bootstrap_id,
                        chunk_seq: 0,
                        payload: bytes::Bytes::from_static(b"opaque"),
                    },
                    FrameKind::BootstrapReady {
                        terminal_id,
                        stream_id: native.stream_id,
                        bootstrap_id: native.bootstrap_id,
                        history_cursor: None,
                    },
                ],
                retained_bytes: b"opaque".len(),
                base_seq: 0,
                publication_cursor: [7; 32],
            }))
            .expect("native success reply");
        let publication = native_publication_rx
            .recv()
            .await
            .expect("native publication fence");
        assert_eq!(publication.cursor, [7; 32]);
        publication
            .reply
            .send(Ok(crate::terminal_actor::NativePublicationReply {
                replay: Vec::new(),
                live: tokio::sync::broadcast::channel(1).1,
            }))
            .expect("native publication reply");
    }

    /// ADR-0127 (security review): a QUIC Terminal-stream reset tears the
    /// subscription down through `handle_detach_terminal`, which must keep the
    /// viewer tombstone, or the connection could shed it and then send
    /// subscription-free input.
    #[tokio::test(flavor = "current_thread")]
    async fn a_terminal_stream_reset_keeps_the_viewer_tombstone() {
        let state = crate::state::SharedState::new();
        let (_session, _window, pane) = state.with_mut(|s| s.seed_session("roles"));
        let wire = state.with_mut(|s| s.intern_terminal_wire(pane));
        let client_id = state.with_mut(crate::state::ServerState::new_client_id);
        state.with_mut(|s| {
            let _ = s.apply_attach_role(
                client_id,
                &wire,
                Some(pane),
                phux_protocol::wire::frame::RolePolicy::VIEWER,
                false,
            );
        });
        let _ = crate::runtime::commands::handle_detach_terminal(&state, client_id, &wire).await;
        assert!(
            state.with(|s| s.is_viewer(client_id, &wire)),
            "a stream reset must not shed the viewer tombstone"
        );
    }

    /// ADR-0127: `STREAM_BIND` carries no role, so a deferred (QUIC
    /// multi-stream) session `ATTACH` must apply its declared role itself.
    #[tokio::test(flavor = "current_thread")]
    async fn deferred_session_attach_applies_the_declared_role() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let state = crate::state::SharedState::new();
                let (_session, _window, pane) = state.with_mut(|s| s.seed_session("roles"));
                let handle = detached_handle(
                    crate::terminal_actor::TerminalHandle::detached_for_test(80, 24),
                );
                let wire = state.with_mut(|s| {
                    let _ = s.register_resource_handle(pane, handle, CancellationToken::new());
                    s.intern_terminal_wire(pane)
                });
                let (out_tx, _out_rx) = tokio::sync::mpsc::channel::<crate::state::Outbound>(
                    crate::state::DEFAULT_CLIENT_MAILBOX,
                );
                let client_id = state.with_mut(crate::state::ServerState::new_client_id);
                attach(
                    &state,
                    client_id,
                    1,
                    "roles",
                    Some(phux_protocol::wire::frame::RolePolicy::VIEWER),
                    &out_tx,
                    BootstrapProfile::SynthesizedVtRaw,
                    &mut JoinSet::new(),
                    &CancellationToken::new(),
                    true,
                )
                .await;
                assert!(
                    state.with(|s| s.is_viewer(client_id, &wire)),
                    "a deferred (QUIC multi-stream) session ATTACH must still mark its viewer"
                );
            })
            .await;
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn native_profile() -> BootstrapProfile {
        BootstrapProfile::NativeState {
            codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1,
            features: phux_protocol::caps::EngineFeatureSet::required_native(),
        }
    }

    /// Start a native-profile ATTACH pump on `terminal` whose published
    /// bootstrap covers nothing yet.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn spawn_native_pump(
        terminal: crate::terminal_actor::TerminalHandle,
        output: &tokio::sync::broadcast::Sender<PaneOutput>,
        resize: ResizeSender,
    ) -> TwoPumpConsumer {
        let (out_tx, out_rx) = tokio::sync::mpsc::channel(32);
        let ctx = OutputPumpContext {
            out_tx,
            resize,
            wire_terminal_id: phux_protocol::ids::ResourceId::local(1),
            stream_id: two_pump_stream(),
            initial_bootstrap_id: two_pump_initial_generation(),
            client_id: ClientId(7),
            client_caps: ClientCapabilities::default(),
            profile: BootstrapStreamProfile::NativeState {
                codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1,
            },
            limits: BootstrapLimits::default(),
            lag_label: "native test pump",
            stale_skip: false,
            cancel: None,
            last_seq: None,
            terminal,
        };
        let (gate_tx, gate_rx) = oneshot::channel();
        gate_tx
            .send(OutputPumpStart {
                published_cut: 0,
                replay: Vec::new(),
                live: None,
            })
            .unwrap_or_else(|_| panic!("gate receiver alive"));
        let live = output.subscribe();
        let task =
            tokio::task::spawn_local(async move { run_output_pump(&ctx, gate_rx, live).await });
        TwoPumpConsumer { out_rx, task }
    }

    /// The actor's reply to `capture`: an empty checkpoint at sequence 0.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn empty_native_checkpoint(
        capture: &crate::terminal_actor::NativeBootstrapRequest,
    ) -> crate::terminal_actor::NativeBootstrapReply {
        crate::terminal_actor::NativeBootstrapReply {
            frames: vec![
                FrameKind::BootstrapBegin {
                    terminal_id: capture.terminal_id.clone(),
                    stream_id: capture.stream_id,
                    bootstrap_id: capture.bootstrap_id,
                    profile: BootstrapStreamProfile::NativeState {
                        codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1,
                    },
                    cols: 80,
                    rows: 24,
                    base_seq: 0,
                },
                FrameKind::BootstrapReady {
                    terminal_id: capture.terminal_id.clone(),
                    stream_id: capture.stream_id,
                    bootstrap_id: capture.bootstrap_id,
                    history_cursor: None,
                },
            ],
            retained_bytes: 0,
            base_seq: 0,
            publication_cursor: [9; 32],
        }
    }

    /// A native capture the actor invalidates mid-flight (the last shell's
    /// in-place respawn, a reflow) is superseded, not refused: the pump asks
    /// for the resync and republishes, and the client is never told its
    /// generation was lost or disconnected.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    #[tokio::test(flavor = "current_thread")]
    async fn an_invalidated_native_capture_waits_for_the_resync_instead_of_failing() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (handle, _consumer_attach_rx, mut bootstrap_rx, mut publication_rx) =
                    native_attach_handle();
                let (output, _keepalive) = tokio::sync::broadcast::channel(16);
                let (resize, mut resize_rx) = ResizeSender::channel(4);
                let terminal = handle.terminal().expect("terminal facet").clone();
                let mut consumer = spawn_native_pump(terminal, &output, resize);
                let resync = |reason| PaneOutput::Resync {
                    cols: 80,
                    rows: 24,
                    reason,
                    audience: ResyncAudience::Everyone,
                    base_seq: 0,
                    bytes: bytes::Bytes::new(),
                };

                output
                    .send(resync(crate::terminal_actor::ResyncReason::Exit))
                    .expect("pump subscribed");
                let invalidated = bootstrap_rx.recv().await.expect("exit capture request");
                invalidated
                    .reply
                    .send(Err(crate::native_state::NativeStateError::Resize))
                    .expect("pump awaits its capture");
                let request = resync_request_from(&mut resize_rx, &mut consumer).await;
                assert_eq!(
                    request.resync_for.map(|pump| pump.bootstrap_id),
                    Some(two_pump_initial_generation()),
                    "the pump asks for its own replacement generation"
                );
                assert!(
                    !consumer.task.is_finished(),
                    "the pump must outlive the invalidation"
                );

                output
                    .send(resync(crate::terminal_actor::ResyncReason::Resize))
                    .expect("pump subscribed");
                let capture = bootstrap_rx
                    .recv()
                    .await
                    .expect("replacement capture request");
                let replacement = capture.bootstrap_id;
                let reply = empty_native_checkpoint(&capture);
                capture
                    .reply
                    .send(Ok(reply))
                    .expect("pump awaits its capture");
                let publication = publication_rx.recv().await.expect("publication request");
                publication
                    .reply
                    .send(Ok(crate::terminal_actor::NativePublicationReply {
                        replay: Vec::new(),
                        live: output.subscribe(),
                    }))
                    .expect("pump awaits its publication");

                assert_eq!(
                    frames_seen(&mut consumer, 3).await,
                    vec![
                        Seen::Tombstone,
                        Seen::Begin {
                            generation: replacement,
                            base_seq: 0,
                        },
                        Seen::Ready {
                            generation: replacement,
                        },
                    ],
                    "one tombstone, then the replacement; no error frame"
                );
                assert_eq!(
                    replacement,
                    next_bootstrap_id(two_pump_initial_generation()),
                    "the replacement follows the tombstoned generation"
                );
                assert_quiet(&mut consumer, "the resynced pump").await;
                assert!(!consumer.task.is_finished(), "the client stays attached");
                consumer.task.abort();
            })
            .await;
    }

    /// A pane that exits while its pump is blocked publishing a replacement
    /// generation to a slow consumer ends that pump as `PaneGone`, not as a
    /// connection fault: the client learns of the exit from
    /// `RESOURCE_CLOSED` and keeps its connection. Hosted CI saw the old
    /// mapping disconnect `a_consumer_that_falls_behind...` mid-flood.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    #[tokio::test(flavor = "current_thread")]
    async fn a_pane_gone_before_its_replacement_publication_is_not_a_connection_fault() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (handle, _consumer_attach_rx, mut bootstrap_rx, mut publication_rx) =
                    native_attach_handle();
                let (output, _keepalive) = tokio::sync::broadcast::channel(16);
                let (resize, _resize_rx) = ResizeSender::channel(4);
                let terminal = handle.terminal().expect("terminal facet").clone();
                let mut consumer = spawn_native_pump(terminal, &output, resize);

                output
                    .send(PaneOutput::Resync {
                        cols: 80,
                        rows: 24,
                        reason: crate::terminal_actor::ResyncReason::Resize,
                        audience: ResyncAudience::Everyone,
                        base_seq: 0,
                        bytes: bytes::Bytes::new(),
                    })
                    .expect("pump subscribed");
                let capture = bootstrap_rx.recv().await.expect("replacement capture");
                let reply = empty_native_checkpoint(&capture);
                capture.reply.send(Ok(reply)).expect("pump awaits capture");
                assert_eq!(frames_seen(&mut consumer, 3).await.len(), 3);
                // The actor exits with the request in its mailbox.
                drop(publication_rx.recv().await.expect("publication request"));

                let fault = consumer.task.await.expect("pump task");
                assert!(
                    matches!(fault, Some(PumpFault::PaneGone)),
                    "a gone pane must not close the client: {fault:?}"
                );
            })
            .await;
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    #[tokio::test(flavor = "current_thread")]
    async fn fresh_native_capacity_failure_sends_error_then_closes_without_publication() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let state = SharedState::new();
                let (_session, _window, terminal) =
                    state.with_mut(|s| s.seed_session("fresh-failure"));
                let (
                    handle,
                    mut consumer_attach_rx,
                    mut native_bootstrap_rx,
                    mut native_publication_rx,
                ) = native_attach_handle();
                state.with_mut(|s| {
                    let _ = s.register_resource_handle(terminal, handle, CancellationToken::new());
                });
                let client_id = state.with_mut(crate::state::ServerState::new_client_id);
                let (out_tx, mut out_rx) =
                    tokio::sync::mpsc::channel(crate::state::DEFAULT_CLIENT_MAILBOX);
                let connection_token = CancellationToken::new();
                let mut output_pumps = JoinSet::new();
                tokio::join!(
                    attach(
                        &state,
                        client_id,
                        41,
                        "fresh-failure",
                        None,
                        &out_tx,
                        native_profile(),
                        &mut output_pumps,
                        &connection_token,
                        false,
                    ),
                    answer_native_attach(
                        &mut consumer_attach_rx,
                        &mut native_bootstrap_rx,
                        &mut native_publication_rx,
                        false,
                    )
                );

                assert!(matches!(
                    out_rx.recv().await,
                    Some(Outbound::TerminalError {
                        code: ErrorCode::CodecUnavailable,
                        ..
                    })
                ));
                assert!(out_rx.try_recv().is_err(), "no ATTACHED or BEGIN may leak");
                assert!(connection_token.is_cancelled());
                assert!(state.with(|s| !s.attached().contains_key(&client_id)));
                drop(out_tx);
                assert!(
                    out_rx.recv().await.is_none(),
                    "fatal fresh attach must reach EOF"
                );
            })
            .await;
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    #[tokio::test(flavor = "current_thread")]
    async fn replacement_native_capacity_failure_closes_but_preserves_terminal_state() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let state = SharedState::new();
                let (_session, _window, terminal) =
                    state.with_mut(|s| s.seed_session("replacement-failure"));
                let (
                    handle,
                    mut consumer_attach_rx,
                    mut native_bootstrap_rx,
                    mut native_publication_rx,
                ) = native_attach_handle();
                state.with_mut(|s| {
                    let _ = s.register_resource_handle(terminal, handle, CancellationToken::new());
                });
                let client_id = state.with_mut(crate::state::ServerState::new_client_id);
                let (out_tx, mut out_rx) =
                    tokio::sync::mpsc::channel(crate::state::DEFAULT_CLIENT_MAILBOX);
                let connection_token = CancellationToken::new();
                let mut output_pumps = JoinSet::new();

                for (attach_id, succeed) in [(51, true), (52, false)] {
                    tokio::join!(
                        attach(
                            &state,
                            client_id,
                            attach_id,
                            "replacement-failure",
                            None,
                            &out_tx,
                            native_profile(),
                            &mut output_pumps,
                            &connection_token,
                            false,
                        ),
                        answer_native_attach(
                            &mut consumer_attach_rx,
                            &mut native_bootstrap_rx,
                            &mut native_publication_rx,
                            succeed,
                        )
                    );
                    if succeed {
                        for _ in 0..5 {
                            out_rx.recv().await.expect("initial attach publication");
                        }
                        assert!(state.with(|s| s.attached().contains_key(&client_id)));
                    }
                }
                assert!(matches!(
                    out_rx.recv().await,
                    Some(Outbound::TerminalError {
                        code: ErrorCode::CodecUnavailable,
                        ..
                    })
                ));
                assert!(connection_token.is_cancelled());
                assert!(state.with(|s| !s.attached().contains_key(&client_id)));
                assert!(
                    state.with(|s| s.registry().terminal(terminal).is_some()),
                    "failed replacement must not reap canonical terminal state"
                );
                output_pumps.abort_all();
                while output_pumps.join_next().await.is_some() {}
                drop(out_tx);
                assert!(
                    out_rx.recv().await.is_none(),
                    "fatal replacement must close cleanly after ERROR"
                );
            })
            .await;
    }
}
