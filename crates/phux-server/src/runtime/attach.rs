//! Submodule for runtime internals.

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use phux_core::TerminalId;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ClientCapabilities,
};
use phux_protocol::ids::{BootstrapId, GroupId, StreamId};
use phux_protocol::wire::frame::{
    AgentEvent, AttachTarget, DetachReason, ErrorCode, FrameKind, MAX_AGENT_SESSION_RECORD_BYTES,
    MoveError, MoveResult, SpawnError, SpawnResult,
};
use std::ops::ControlFlow;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use tracing::{debug, trace, warn};

use super::{
    SpawnOwnership, broadcast_event, prepare_attach, seed_session_with_actor,
    seed_session_with_pty_and_colors, send_error, spawn_pane_with_pty_and_colors,
};
use crate::runtime::pump::{self, PumpGeneration};
use crate::state::{AttachSnapshotPane, ClientId, Outbound, SharedState};
use crate::terminal_actor::{
    ConsumerAttachRequest, ConsumerDetachRequest, PaneOutput, PwdRequest, ResizeRequest,
    SetDefaultColorsRequest, SnapshotRequest,
};

/// Adapt a broadcast byte chunk to a client's capabilities for the wire:
/// a capable client gets the refcounted bytes verbatim (no copy); an
/// incapable one gets an SGR-downsampled rewrite. Shared by both output
/// pumps (the attach pump and the `SPAWN_TERMINAL` pump).
pub(crate) fn downsample_for_caps(
    bytes: &bytes::Bytes,
    caps: phux_protocol::ClientCapabilities,
) -> bytes::Bytes {
    if crate::downsample::caps_pass_through(caps) {
        bytes.clone()
    } else {
        crate::downsample::rewrite_bytes_with_caps(bytes, caps).into()
    }
}

fn bootstrap_source_ceiling(
    remaining_bytes: usize,
    caps: phux_protocol::ClientCapabilities,
) -> usize {
    if crate::downsample::caps_pass_through(caps) {
        remaining_bytes
    } else {
        // During adaptation the source and one equally bounded output Vec are
        // simultaneously live. The rewriter has no other payload-sized heap
        // scratch, so half the connection budget is the exact source ceiling.
        remaining_bytes / 2
    }
}

#[derive(Debug)]
struct AdaptedBootstrap {
    payloads: Vec<bytes::Bytes>,
    retained_bytes: usize,
    peak_bytes: usize,
}

fn adapt_bootstrap_snapshot(
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

pub(crate) const fn initial_bootstrap_id() -> BootstrapId {
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

struct OutputPumpStart {
    published_cut: u64,
    replay: Vec<(u64, bytes::Bytes)>,
    live: Option<tokio::sync::broadcast::Receiver<PaneOutput>>,
}

struct SnapshotGate {
    terminal_id: TerminalId,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    wire_terminal_id: phux_protocol::ids::TerminalId,
    handle: crate::terminal_actor::TerminalHandle,
    gate: oneshot::Sender<OutputPumpStart>,
    cut: Option<u64>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    native_cursor: Option<crate::native_state::OpaqueHistoryCursor>,
}

/// Connection-wide retention ceiling for an aggregate ATTACH preflight.
///
/// A session can contain many panes, but the server must hold every pane's
/// complete bootstrap until the atomic publication cut. Keep that aggregate no
/// larger than one maximally bounded native prefix rather than multiplying the
/// per-pane allowance by the pane count.
const MAX_STAGED_BOOTSTRAP_BYTES: usize = 64 * 1024 * 1024;
const MAX_STAGED_BOOTSTRAP_FRAMES: usize = 4_096 + 2;

/// Maximum pane sources admitted to one aggregate bootstrap.
/// Every supported profile consumes at least `BEGIN`, one opaque `CHUNK`, and
/// `READY`, so a larger source set cannot fit the connection-wide frame budget.
/// The preflight runs before the session snapshot and pane-handle vectors are
/// allocated.
pub(crate) const MAX_AGGREGATE_BOOTSTRAP_PANES: usize = MAX_STAGED_BOOTSTRAP_FRAMES / 3;

#[derive(Debug)]
struct BootstrapStagingBudget {
    max_bytes: usize,
    max_frames: usize,
    staged_bytes: usize,
    staged_frames: usize,
}

impl BootstrapStagingBudget {
    const fn new() -> Self {
        Self::with_limits(MAX_STAGED_BOOTSTRAP_BYTES, MAX_STAGED_BOOTSTRAP_FRAMES)
    }

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

    #[cfg(test)]
    fn append(
        &mut self,
        staged: &mut Vec<FrameKind>,
        incoming: &mut Vec<FrameKind>,
    ) -> Result<(), ()> {
        let incoming_bytes = incoming
            .iter()
            .try_fold(0_usize, |total, frame| {
                total.checked_add(match frame {
                    FrameKind::BootstrapChunk { payload, .. } => payload.len(),
                    FrameKind::BootstrapReady { history_cursor, .. } => {
                        history_cursor.as_ref().map_or(0, bytes::Bytes::len)
                    }
                    _ => 0,
                })
            })
            .ok_or(())?;
        self.append_accounted(staged, incoming, incoming_bytes)
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
    terminal_id: phux_protocol::ids::TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    cursor: crate::native_state::OpaqueHistoryCursor,
) -> Result<crate::terminal_actor::NativePublicationReply, ()> {
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
        .map_err(|_| ())?;
    publication.await.map_err(|_| ())?.map_err(|_| ())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn synthesized_bootstrap_frames(
    terminal_id: phux_protocol::ids::TerminalId,
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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn send_synthesized_bootstrap(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    terminal_id: phux_protocol::ids::TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    profile: BootstrapStreamProfile,
    limits: BootstrapLimits,
    cols: u16,
    rows: u16,
    base_seq: u64,
    payloads: impl IntoIterator<Item = bytes::Bytes>,
) -> Result<(), ()> {
    for frame in synthesized_bootstrap_frames(
        terminal_id,
        stream_id,
        bootstrap_id,
        profile,
        limits,
        cols,
        rows,
        base_seq,
        payloads,
    )? {
        out_tx.send(Outbound::Frame(frame)).await.map_err(|_| ())?;
    }
    Ok(())
}

/// Queue the mandatory in-band resync after a broadcast gap.
///
/// The output pump awaits mailbox capacity and therefore cannot consume or
/// forward a later delta until the actor has accepted the resync request.
/// A closed or persistently full actor mailbox fails boundedly.
pub(crate) async fn enqueue_output_resync(
    resize: &tokio::sync::mpsc::Sender<ResizeRequest>,
) -> bool {
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            resize.send(ResizeRequest {
                cols: 0,
                rows: 0,
                cell_px: None,
                resync_clients: true,
                resync_only: true,
            }),
        )
        .await,
        Ok(Ok(()))
    )
}

/// Why an output pump stopped serving its client.
///
/// The pump only reports the fault; the caller decides what to release,
/// because a `SPAWN_TERMINAL` pump owns the pane it feeds while an ATTACH
/// pump shares its panes with the rest of the session.
#[derive(Debug, Clone, Copy)]
enum PumpFault {
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
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
const fn tombstone_reason_for(
    reason: crate::terminal_actor::ResyncReason,
) -> phux_protocol::wire::frame::TombstoneReason {
    match reason {
        crate::terminal_actor::ResyncReason::Resize => {
            phux_protocol::wire::frame::TombstoneReason::Resize
        }
        crate::terminal_actor::ResyncReason::OutboundGap => {
            phux_protocol::wire::frame::TombstoneReason::OutboundGap
        }
    }
}

/// Everything one output pump needs for the life of its subscription: the
/// client it serves, the pane it reads, and the negotiated bootstrap shape.
struct OutputPumpContext {
    /// This client's outbound mailbox.
    out_tx: tokio::sync::mpsc::Sender<Outbound>,
    /// phux-y8v6: lets a lagged pump ask the actor to broadcast an in-band
    /// resync (a full grid snapshot on the same ordered channel) so a
    /// consumer that dropped bytes reconverges.
    resize: tokio::sync::mpsc::Sender<ResizeRequest>,
    /// Wire identity of the pane being pumped.
    wire_terminal_id: phux_protocol::ids::TerminalId,
    /// Stream this pump publishes on.
    stream_id: StreamId,
    /// Generation the first published bootstrap carries.
    initial_bootstrap_id: BootstrapId,
    /// Owner of the ordered control frames this pump must honour.
    client_id: ClientId,
    /// Negotiated capabilities every payload is adapted to.
    client_caps: ClientCapabilities,
    /// Negotiated bootstrap stream profile.
    profile: BootstrapStreamProfile,
    /// Negotiated bootstrap bounds.
    limits: BootstrapLimits,
    /// How this pump names itself in the broadcast-lag warning.
    lag_label: &'static str,
    /// Actor handle used for native checkpoint capture and publication.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    handle: crate::terminal_actor::TerminalHandle,
}

impl OutputPumpContext {
    /// Is this pump publishing native libghostty checkpoints?
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    const fn publishes_native_checkpoints(&self) -> bool {
        matches!(
            self.profile,
            BootstrapStreamProfile::NativeState {
                codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2
            }
        )
    }

    /// Frame one output chunk for this client, adapted to its capabilities.
    fn output_frame(
        &self,
        generation: &PumpGeneration,
        seq: u64,
        bytes: &bytes::Bytes,
    ) -> FrameKind {
        FrameKind::TerminalOutput {
            terminal_id: self.wire_terminal_id.clone(),
            stream_id: self.stream_id,
            bootstrap_id: generation.bootstrap_id(),
            seq,
            bytes: downsample_for_caps(bytes, self.client_caps),
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
    /// published bootstrap already covered.
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
        if self.out_tx.send(Outbound::Frame(frame)).await.is_err() {
            return ControlFlow::Break(Some(PumpFault::OutboundClosed));
        }
        crate::perf::PUMP_FRAMES.incr();
        crate::perf::PUMP_BYTES.add_len(bytes.len());
        crate::perf::PUMP_FRAME_BYTES.record_len(bytes.len());
        generation.note_forwarded(seq);
        ControlFlow::Continue(())
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

    /// Ask the actor for the replacement native checkpoint.
    ///
    /// A closed mailbox, a refused capture, and a dropped reply all lose the
    /// generation; only the refusal is worth telling the client about, since
    /// the other two mean the actor is already gone.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn capture_native_checkpoint(
        &self,
        bootstrap_id: BootstrapId,
    ) -> Result<crate::terminal_actor::NativeBootstrapReply, PumpFault> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .handle
            .native_bootstrap
            .send(crate::terminal_actor::NativeBootstrapRequest {
                owner: self.client_id.0,
                terminal_id: self.wire_terminal_id.clone(),
                stream_id: self.stream_id,
                bootstrap_id,
                limits: self.limits,
                max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
                max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            return Err(PumpFault::GenerationLost);
        }
        let Ok(Ok(reply)) = reply_rx.await else {
            let _ = self
                .out_tx
                .send(Outbound::Frame(FrameKind::Error {
                    request_id: None,
                    code: ErrorCode::CodecUnavailable,
                    message: "native checkpoint resync failed".to_owned(),
                }))
                .await;
            return Err(PumpFault::GenerationLost);
        };
        Ok(reply)
    }

    /// Tombstone the live generation, publish its native replacement, and
    /// adopt the post-cut receiver the actor fenced it behind.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn republish_native_generation(
        &self,
        generation: &mut PumpGeneration,
        output_rx: &mut tokio::sync::broadcast::Receiver<PaneOutput>,
        prior_bootstrap_id: BootstrapId,
        reason: crate::terminal_actor::ResyncReason,
    ) -> Result<(), PumpFault> {
        if generation.is_active()
            && self
                .out_tx
                .send(Outbound::Frame(FrameKind::BootstrapTombstone {
                    terminal_id: self.wire_terminal_id.clone(),
                    stream_id: self.stream_id,
                    bootstrap_id: prior_bootstrap_id,
                    reason: tombstone_reason_for(reason),
                    last_valid_seq: generation.last_forwarded_seq(),
                }))
                .await
                .is_err()
        {
            return Err(PumpFault::TombstoneNotQueued);
        }
        let reply = self
            .capture_native_checkpoint(generation.bootstrap_id())
            .await?;
        let (cut, cursor) = publish_native_bootstrap(&self.out_tx, reply)
            .await
            .map_err(|()| PumpFault::GenerationLost)?;
        let publication = activate_native_publication(
            &self.handle,
            self.client_id.0,
            self.wire_terminal_id.clone(),
            self.stream_id,
            generation.bootstrap_id(),
            cursor,
        )
        .await
        .map_err(|()| PumpFault::PublicationNotActivated)?;
        // Unfenced here, before the replay, so the replay's own frames pass
        // the same `forwards` gate every other live delta does.
        generation.republished_at(cut);
        *output_rx = publication.live;
        // Through the same gate as any other live delta, not around it. A
        // replay entry at or behind the new cut is already inside the
        // checkpoint just published, and re-sending it under the replacement
        // `bootstrap_id` is a `DuplicateSequence` to the client kernel — which
        // detaches on it.
        for (seq, bytes) in publication.replay {
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

    /// Publish the synthesized-VT replacement generation for a resync.
    async fn republish_synthesized_generation(
        &self,
        generation: &mut PumpGeneration,
        resync: &PaneResync,
    ) -> ControlFlow<Option<PumpFault>> {
        let payload = downsample_for_caps(&resync.bytes, self.client_caps);
        if send_synthesized_bootstrap(
            &self.out_tx,
            self.wire_terminal_id.clone(),
            self.stream_id,
            generation.bootstrap_id(),
            self.profile,
            self.limits,
            resync.cols,
            resync.rows,
            resync.base_seq,
            [payload],
        )
        .await
        .is_err()
        {
            return ControlFlow::Break(Some(PumpFault::OutboundClosed));
        }
        generation.republished_at(resync.base_seq);
        ControlFlow::Continue(())
    }

    /// Replace the published generation after the actor resynchronized the
    /// pane.
    ///
    /// Resync is a control event, not replayable live data: even an unchanged
    /// cut (for example a resize directly after READY) invalidates and
    /// replaces the generation.
    async fn republish_generation(
        &self,
        generation: &mut PumpGeneration,
        output_rx: &mut tokio::sync::broadcast::Receiver<PaneOutput>,
        resync: &PaneResync,
    ) -> ControlFlow<Option<PumpFault>> {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let prior_bootstrap_id = generation.bootstrap_id();
        generation.set_bootstrap_id(next_bootstrap_id(generation.bootstrap_id()));
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if self.publishes_native_checkpoints() {
            return match self
                .republish_native_generation(
                    generation,
                    output_rx,
                    prior_bootstrap_id,
                    resync.reason,
                )
                .await
            {
                Ok(()) => ControlFlow::Continue(()),
                Err(fault) => ControlFlow::Break(Some(fault)),
            };
        }
        self.republish_synthesized_generation(generation, resync)
            .await
    }

    /// A dropped broadcast window leaves the client's mirror stale: fence the
    /// generation, ask the actor for an in-band resync, and lose the
    /// generation if it cannot deliver one.
    ///
    /// The fence is set *before* the request and is what makes the resync
    /// land — see [`PumpGeneration::forwards`]. It is set even when the
    /// request itself fails, so the frames between here and the pump's exit
    /// are never the gapped ones that would detach the client.
    async fn request_gap_resync(
        &self,
        generation: &mut PumpGeneration,
        dropped: u64,
    ) -> ControlFlow<Option<PumpFault>> {
        crate::perf::PUMP_LAGGED.incr();
        if generation.fence_for_gap() {
            debug!(
                terminal_id = ?self.wire_terminal_id,
                dropped,
                "{} lagged again while a resync was already in flight; re-requesting",
                self.lag_label,
            );
        } else {
            warn!(
                terminal_id = ?self.wire_terminal_id,
                dropped,
                "{} lagged; requesting in-band resync",
                self.lag_label,
            );
        }
        generation.note_resync_requested();
        crate::perf::PUMP_GAP_RESYNC.incr();
        if enqueue_output_resync(&self.resize).await {
            return ControlFlow::Continue(());
        }
        self.fail_unrecoverable_gap().await
    }

    /// The resync asked for at the last gap has not arrived within
    /// [`pump::GAP_RESYNC_RETRY`]: ask again.
    ///
    /// A pump waiting on a resync forwards nothing, so a request the actor
    /// accepted but never answered (its snapshot synthesis failed, say) would
    /// otherwise leave the client on a screen that can never change — silence
    /// being the one failure the old behaviour did not have. Re-asking costs
    /// one coalesced grid synthesis per retry and makes convergence
    /// unconditional rather than conditional on the actor's first answer.
    async fn retry_gap_resync(
        &self,
        generation: &mut PumpGeneration,
    ) -> ControlFlow<Option<PumpFault>> {
        // DEBUG, not WARN: the first gap already warned, and a retry loop that
        // warns every time turns one wedged actor into a log flood.
        debug!(
            terminal_id = ?self.wire_terminal_id,
            attempt = generation.gap_attempts(),
            "{} is still waiting on its in-band resync; re-requesting",
            self.lag_label,
        );
        generation.note_resync_requested();
        if enqueue_output_resync(&self.resize).await {
            return ControlFlow::Continue(());
        }
        self.fail_unrecoverable_gap().await
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

/// What one turn of [`next_pump_event`] produced.
enum PumpStep {
    /// Dispatch this broadcast result.
    Event(Result<PaneOutput, tokio::sync::broadcast::error::RecvError>),
    /// The turn was spent re-asking for a stalled resync; take another.
    Again,
    /// The pump cannot continue.
    Stop(Option<PumpFault>),
}

/// The next broadcast event for this pump, re-asking for a resync that a
/// fenced pump has waited [`pump::GAP_RESYNC_RETRY`] without seeing.
async fn next_pump_event(
    ctx: &OutputPumpContext,
    generation: &mut PumpGeneration,
    output_rx: &mut tokio::sync::broadcast::Receiver<PaneOutput>,
) -> PumpStep {
    let outcome = match pump::next_event(generation, output_rx).await {
        pump::PumpWait::Event(received) => return PumpStep::Event(received),
        pump::PumpWait::RetryResync => ctx.retry_gap_resync(generation).await,
        pump::PumpWait::GapUnrecoverable => ctx.abandon_unanswered_gap(generation).await,
    };
    match outcome {
        ControlFlow::Continue(()) => PumpStep::Again,
        ControlFlow::Break(fault) => PumpStep::Stop(fault),
    }
}

/// Drive one client's output subscription for a pane: park on the publication
/// gate, replay the backlog behind the published cut, then forward live
/// output, ordered control, and generation replacements until the pane or the
/// client goes away.
///
/// Returns the fault that ended the pump, or `None` for an orderly stop. The
/// caller owns the cleanup, because a `SPAWN_TERMINAL` pump owns the pane it
/// feeds while an ATTACH pump shares its panes with the rest of the session.
async fn run_output_pump(
    ctx: &OutputPumpContext,
    gate_rx: oneshot::Receiver<OutputPumpStart>,
    mut output_rx: tokio::sync::broadcast::Receiver<PaneOutput>,
) -> Option<PumpFault> {
    let Ok(start) = gate_rx.await else {
        return None;
    };
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
    loop {
        let received = match next_pump_event(ctx, &mut generation, &mut output_rx).await {
            PumpStep::Event(received) => received,
            PumpStep::Again => continue,
            PumpStep::Stop(fault) => return fault,
        };
        let step = match received {
            Ok(PaneOutput::Live { seq, bytes }) => {
                ctx.forward_live(&mut generation, seq, &bytes).await
            }
            Ok(PaneOutput::Control { owner, frame }) => {
                ctx.forward_control(&mut generation, owner, frame).await
            }
            Ok(PaneOutput::Resync {
                cols,
                rows,
                bytes,
                reason,
                base_seq,
            }) => {
                ctx.republish_generation(
                    &mut generation,
                    &mut output_rx,
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
                ctx.request_gap_resync(&mut generation, n).await
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => ControlFlow::Break(None),
        };
        if let ControlFlow::Break(fault) = step {
            return fault;
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
    staged_handles: &[crate::terminal_actor::TerminalHandle],
    staged_pumps: &mut JoinSet<()>,
    committed_pumps: &mut JoinSet<()>,
    reason: &str,
) {
    staged_pumps.abort_all();
    while staged_pumps.join_next().await.is_some() {}
    super::client::abort_output_pumps(committed_pumps, client_id, "failed ATTACH").await;

    let wire_client_id =
        phux_protocol::ids::ClientId::new(u32::try_from(client_id.0).unwrap_or(u32::MAX));
    let producer_deadline = std::time::Duration::from_secs(1);
    for handle in staged_handles {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        {
            let _ = tokio::time::timeout(
                producer_deadline,
                handle
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

    // Queue one ordered terminal sentinel after rollback. Even if an old
    // state-sync producer survives its bounded detach acknowledgement and
    // races another frame, the writer closes immediately after this ERROR and
    // discards everything behind it.
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

/// Tuple bundling everything `handle_attach` needs after it is done
/// touching [`ServerState`]. Cloned out of the critical section so the
/// remaining awaits do not hold the state lock. The final vector names
/// snapshot participants with no actor handle; publication resolves those
/// with `TERMINAL_CLOSED` before `ATTACH_READY`.
pub(crate) type AttachPrepared = (
    phux_protocol::wire::info::SessionSnapshot,
    phux_protocol::ids::ClientId,
    Vec<AttachSnapshotPane>,
    Vec<phux_protocol::ids::TerminalId>,
);

/// Resolve `Last` without conflating an untouched server with stale touch
/// history. A configured seed is a fallback only in the former case.
fn resolve_last_session_name(state: &crate::state::ServerState) -> Option<String> {
    match state.most_recently_touched_session() {
        Some(sid) => state
            .registry()
            .session(sid)
            .map(|session| session.name.clone()),
        None if state.has_session_touch_history() => None,
        None => state
            .pre_seeded_session()
            .and_then(|name| state.session_by_name(name))
            .map(|session| session.name.clone()),
    }
}

/// Resolve `target` to a live session name.
///
/// `Last` preserves touch-order authority once any activity exists. Before
/// the first touch it falls back to the server's configured pre-seeded
/// session, if that session is still live; neither path creates a session.
pub(crate) async fn resolve_attach_target(
    state: &SharedState,
    target: AttachTarget,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) -> Option<String> {
    match target {
        AttachTarget::ByName(name) => Some(name),
        AttachTarget::ById(id) => {
            let resolved = state
                .with(|s| s.idspace.resolve_session(id))
                .and_then(|sid| {
                    state.with(|s| s.registry().session(sid).map(|sess| sess.name.clone()))
                });
            if resolved.is_none() {
                send_error(
                    out_tx,
                    ErrorCode::SessionNotFound,
                    &format!("session id {} not found", id.get()),
                )
                .await;
            }
            resolved
        }
        AttachTarget::Last => {
            // A real touch remains authoritative, including the existing
            // stale-touch failure behavior when that session has since died.
            // Only a server with no touch history may select its configured
            // pre-seeded session. That identity comes from ServerConfig and
            // is mirrored before seeding, so native clients can send `Last`
            // without loading or reproducing the server's config template.
            let resolved = state.with(resolve_last_session_name);
            if resolved.is_none() {
                send_error(
                    out_tx,
                    ErrorCode::SessionNotFound,
                    "AttachTarget::Last has no live session to resolve",
                )
                .await;
            }
            resolved
        }
        AttachTarget::CreateIfMissing { name, command, cwd } => {
            resolve_create_if_missing(
                state,
                name,
                command,
                cwd,
                out_tx,
                root_token,
                default_colors,
            )
            .await
        }
        _ => {
            send_error(
                out_tx,
                ErrorCode::SessionNotFound,
                "unknown AttachTarget variant",
            )
            .await;
            None
        }
    }
}

/// Handle [`AttachTarget::CreateIfMissing`] (phux-k61.3, SPEC §13).
///
/// Behavior:
///
/// * If a session with `name` already exists in the registry, return
///   its name unchanged — the caller's `prepare_attach` then runs the
///   normal `ByName` attach path against it. No duplicate session is
///   created.
/// * Otherwise, seed a fresh `(session, window, pane)` triple, spawn
///   the seed pane's actor in the mode the server was configured
///   with (PTY-backed via [`seed_session_with_pty`] when
///   [`crate::state::ServerState::attach_create_seeds_pty`] is `true`,
///   or no-PTY via [`seed_session_with_actor`] otherwise), and return
///   the name so the caller proceeds with the normal attach path.
///
/// `command` from the wire frame is honored only when the PTY mode is
/// on AND no explicit
/// [`crate::state::ServerState::attach_create_seed_command`] preempts
/// it: an explicit per-server seed command always wins (it's how the
/// `phux server` binary pins the default-shell command for the user).
/// `cwd` from the wire frame (phux-3mtf) seeds the PTY child's working
/// directory when it names an existing directory on the server host; a
/// missing or non-directory path falls back to the pre-existing
/// behavior (the builder's cwd stays unset, so the spawn lands where a
/// `cwd: None` spawn would) rather than failing the attach — the
/// client's idea of a path may be stale or belong to another host. A
/// cwd already set on the server-wide override command is never
/// clobbered. The no-PTY path ignores both, matching the existing
/// `seed_session_with_actor` shape.
///
/// On terminal-actor spawn failure (e.g. PTY allocation fails on a
/// host with no remaining ptys), emits a `SessionNotFound` error
/// frame (mirroring how the pre-seed path logs-and-continues at
/// startup) and returns `None` so the attach fails atomically. We
/// reuse `SessionNotFound` rather than introducing a new error code:
/// the user-visible effect is "the requested session is not available
/// to attach to", which is what `SessionNotFound` already means on
/// the wire. A richer error code (e.g. `SessionCreateFailed`) is a
/// SPEC-level follow-up.
pub(crate) async fn resolve_create_if_missing(
    state: &SharedState,
    name: String,
    command: Option<Vec<String>>,
    cwd: Option<String>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) -> Option<String> {
    // Fast path: a session with this name already exists. Fall through
    // to the normal `ByName(name)` attach by returning `name` as-is.
    // The lookup is read-only so we hold only an immutable borrow.
    if state.with(|s| s.session_by_name(&name).is_some()) {
        debug!(session = %name, "CreateIfMissing: session already exists, attaching");
        return Some(name);
    }

    // Slow path: create the session + seed pane. Snapshot the server's
    // configured PTY mode and (optional) override command before
    // releasing the state borrow.
    let (with_pty, override_cmd, scrollback, term, shell, login_shell) = state.with(|s| {
        (
            s.attach_create_seeds_pty(),
            s.attach_create_seed_command(),
            s.scrollback_limits(),
            s.term().to_owned(),
            s.shell().to_owned(),
            s.login_shell(),
        )
    });

    let seed_result = if with_pty {
        // Resolve the command. Precedence:
        //   1. The server-wide override stashed via
        //      `set_attach_create_pty(_, Some(cmd))`. Set explicitly by
        //      the runtime (or by tests that want a deterministic
        //      child like `cat`).
        //   2. The wire-level `command` from the CreateIfMissing
        //      variant. This is the per-attach command knob clients
        //      use to spawn (e.g.) `phux new -- vim foo.txt`.
        //   3. `default_shell_command` over the resolved default shell
        //      (`defaults.shell` → `$SHELL` → `/bin/sh`, phux-i0e8.4.1)
        //      — same fallback the pre-seed path uses.
        let mut seed_cmd = override_cmd.unwrap_or_else(|| match command {
            Some(argv) if !argv.is_empty() => {
                let mut head = argv.into_iter();
                // Safe: argv is non-empty here.
                let program = head.next().unwrap_or_default();
                let mut builder = portable_pty::CommandBuilder::new(program);
                for arg in head {
                    builder.arg(arg);
                }
                builder
            }
            _ => crate::terminal_actor::default_shell_command(&shell, login_shell),
        });
        // phux-3mtf / phux-0v1l: honor the wire `cwd` through the shared
        // validate-and-fall-back helper, uniform with the
        // `SESSION_CREATE_KEY` create-without-attach path. The wire cwd is
        // applied only over a cwd-less builder (a server-wide override's cwd
        // wins wholesale), honored only when it names an existing, enterable
        // directory, and dropped with a warn otherwise — never failing the
        // attach. The stamp in `seed_session_with_pty_and_colors` reads the
        // builder's cwd back (`spawn_cwd_of`), so the honored value also
        // lands on the pane's registry descriptor for the ATTACHED snapshot.
        crate::terminal_actor::apply_spawn_cwd(&mut seed_cmd, cwd.as_deref(), &name);
        // Apply the server-wide `defaults.term` (phux-ign); this overrides
        // whatever baseline the builder carried.
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
        // No-PTY path: the wire `command` is meaningless without a
        // child to exec it on. We still create the session+pane so
        // the snapshot path has a target — this is the shape every
        // existing `spawn_server` test uses.
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

/// Resolve a freshly-spawned pane's working directory from
/// `defaults.cwd-inheritance` (phux-cs6) when the `SPAWN_TERMINAL` wire
/// frame left `cwd` unset.
///
/// Returns the directory to seed the new pane's `CommandBuilder.cwd`
/// with, or `None` to inherit the server process's CWD (no override) —
/// the same effect the wire-`cwd = None` path had before this policy
/// existed.
///
/// Policy mapping:
/// * [`InheritFocused`](phux_config::CwdInheritance::InheritFocused) —
///   look up the spawning client's focused pane and ask its actor for
///   the live PTY CWD (a kernel query on the PTY child, see
///   [`crate::cwd_query`]). `None` when the client is not attached, has
///   no focused pane, the pane has no live handle, or the query is
///   unsupported/denied — each falls through to no override.
/// * [`Home`](phux_config::CwdInheritance::Home) — `$HOME`, or `None`
///   when unset.
/// * [`SessionRoot`](phux_config::CwdInheritance::SessionRoot) — the
///   session's creation directory: the live CWD of the session's seed
///   (oldest) pane, captured once and frozen in
///   [`crate::state::ServerState::record_session_root`] so a later `cd`
///   in the seed pane does not move the root. `None` when the client is
///   not attached, the session has no live seed pane, or the query is
///   unsupported/denied (with no previously frozen value to fall back on).
/// * [`LastCwdPerWindow`](phux_config::CwdInheritance::LastCwdPerWindow) —
///   the most-recent CWD observed in the spawning client's active window.
///   Resolved from the active pane's live CWD, recorded into
///   [`crate::state::ServerState::record_window_last_cwd`], and reused as
///   the fallback when a subsequent live query fails. `None` when there is
///   no active window and nothing was ever recorded.
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
///
/// Find the spawning client's focused pane's actor handle in a
/// single critical section, then query it off-lock (the actor
/// runs on the same `LocalSet`; `with` must not be held across
/// the await).
async fn focused_pane_cwd(state: &SharedState, client_id: ClientId) -> Option<String> {
    let handle = state.with(|s| {
        let session = s.attached().get(&client_id)?.session;
        let focused = s.active_pane_of_session(session)?;
        s.terminal_handle(focused).cloned()
    })?;
    query_pane_cwd(handle).await
}

/// The session's creation directory.
///
/// The session root is the seed pane's directory at session
/// creation, frozen on first observation. Query the seed pane
/// live; if a root was already frozen, reuse it (and the live
/// query is redundant). The freeze happens in `with_mut` after
/// the off-lock query so a concurrent spawn cannot move it.
async fn session_root_cwd(state: &SharedState, client_id: ClientId) -> Option<String> {
    let (session, handle) = state.with(|s| {
        let session = s.attached().get(&client_id)?.session;
        if let Some(root) = s.session_root(session) {
            // Already frozen — return it without a live query.
            return Some((session, FrozenOrQuery::Frozen(path_to_string(root)?)));
        }
        let seed = s.seed_pane_of_session(session)?;
        let handle = s.terminal_handle(seed).cloned()?;
        Some((session, FrozenOrQuery::Query(handle)))
    })?;
    match handle {
        FrozenOrQuery::Frozen(root) => Some(root),
        FrozenOrQuery::Query(handle) => {
            let resolved = query_pane_cwd(handle).await?;
            // Freeze the first observed root; reuse any value a
            // racing spawn already inserted.
            let frozen = state.with_mut(|s| {
                path_to_string(s.record_session_root(session, std::path::PathBuf::from(&resolved)))
            });
            frozen.or(Some(resolved))
        }
    }
}

/// The most-recent CWD observed in the spawning client's active window.
///
/// Resolve the active window and its active pane's handle. If the
/// window has no live active pane, fall back to the last value we
/// recorded for that window.
async fn last_window_cwd(state: &SharedState, client_id: ClientId) -> Option<String> {
    let (window, handle) = state.with(|s| {
        let session = s.attached().get(&client_id)?.session;
        let window = s.active_window_of_session(session)?;
        let handle = s
            .active_pane_of_session(session)
            .and_then(|p| s.terminal_handle(p).cloned());
        Some((window, handle))
    })?;
    let resolved = match handle {
        Some(handle) => query_pane_cwd(handle).await,
        None => None,
    };
    if let Some(cwd) = resolved {
        // Record the freshly observed CWD and seed the new pane with
        // it.
        state.with_mut(|s| {
            s.record_window_last_cwd(window, std::path::PathBuf::from(&cwd));
        });
        return Some(cwd);
    }
    // Live query unavailable — reuse the most recent recorded value
    // for this window, if any.
    state.with(|s| s.window_last_cwd(window).and_then(|p| path_to_string(p)))
}

/// Either a directory already frozen as a session root or the actor handle
/// to query for it. Lets `resolve_inherited_cwd` decide whether a live PTY
/// query is needed inside a single `with` critical section without holding
/// the lock across the `await`.
pub(crate) enum FrozenOrQuery {
    Frozen(String),
    Query(crate::terminal_actor::TerminalHandle),
}

/// Render `path` as a UTF-8 string, or `None` if it is not valid UTF-8 — the
/// wire `cwd` and `CommandBuilder.cwd` plumbing are string-based, so a
/// non-UTF-8 directory simply yields no override.
pub(crate) fn path_to_string(path: &std::path::Path) -> Option<String> {
    path.to_str().map(ToOwned::to_owned)
}

/// Ask `handle`'s actor for its live PTY child CWD (a kernel query, see
/// [`crate::cwd_query`]). `None` when the actor has gone away or the query
/// is unsupported/denied. The handle must be cloned out of state before the
/// call: `with` must not be held across the `await`.
pub(crate) async fn query_pane_cwd(
    handle: crate::terminal_actor::TerminalHandle,
) -> Option<String> {
    let (reply, rx) = tokio::sync::oneshot::channel();
    handle.pwd.send(PwdRequest { reply }).await.ok()?;
    rx.await.ok().flatten()
}

/// Refresh every live pane's registry `cwd` from its PTY child's kernel
/// CWD (phux-p4vp).
///
/// `TerminalDescriptor.cwd` is stamped once at spawn time (see
/// `stamp_spawn_cwd` in `runtime::commands`) and would otherwise go stale
/// as soon as the shell `cd`s. `handle_attach` calls this right before
/// `prepare_attach` builds the `ATTACHED` snapshot, so
/// `SessionSnapshot.panes[].cwd` reflects each pane's *current* directory
/// — the TUI sidebar derives its per-window VCS branch line from it.
///
/// Best-effort per pane: a dead child, an unsupported platform, or a
/// vanished actor leaves that pane's stamped value untouched. Queries fan
/// out concurrently (same `FuturesUnordered` rationale as the snapshot
/// fan-out below: attach latency scales with the MAX pane reply time, not
/// the SUM) and the whole drain is capped by [`CWD_REFRESH_DEADLINE`]:
/// an actor that never services its `pwd` mailbox (wedged, or a
/// synthetic test handle) must not stall the `ATTACHED` frame. Panes
/// whose replies miss the deadline keep their stamped spawn-time value;
/// replies that landed before it still apply. Handles are cloned out of
/// state first — `with` must not be held across an await.
pub(crate) async fn refresh_registry_cwds(state: &SharedState) {
    /// Upper bound on the attach-time kernel-cwd fan-out. Real actors
    /// answer a `PwdRequest` in well under a millisecond (one kernel
    /// call, no PTY I/O), so this only ever fires for a wedged or
    /// mock actor — where waiting longer buys nothing and every 100ms
    /// visibly delays the attacher's first paint.
    const CWD_REFRESH_DEADLINE: std::time::Duration = std::time::Duration::from_millis(250);

    let handles: Vec<(TerminalId, crate::terminal_actor::TerminalHandle)> =
        state.with(crate::state::ServerState::all_terminal_handles);
    if handles.is_empty() {
        return;
    }
    let mut queries: FuturesUnordered<_> = handles
        .into_iter()
        .map(|(id, handle)| async move { (id, query_pane_cwd(handle).await) })
        .collect();
    let mut resolved: Vec<(TerminalId, std::path::PathBuf)> = Vec::new();
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

/// The decoded `SPAWN_TERMINAL` payload, bundled 1:1 with the wire frame
/// (minus `request_id`, threaded separately like every reply-correlated
/// handler). Keeps [`handle_spawn_terminal`]'s signature stable as the
/// frame grows additive fields (`term` — phux-ign, `satellite` —
/// phux-v45.6).
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
    /// First-class `TERM` override (phux-ign).
    pub(crate) term: Option<String>,
    /// Satellite host to route the spawn to (phux-v45.6), `None` = local.
    pub(crate) satellite: Option<phux_protocol::ids::SatelliteHost>,
    /// Existing local Terminal whose exact window must own the new pane.
    pub(crate) owner_terminal: Option<phux_protocol::ids::TerminalId>,
    /// Opaque native agent-session provenance to install before publication.
    pub(crate) agent_session: Option<Vec<u8>>,
    /// `(cols, rows)` to build the pane's grid and PTY at (phux-a5xj),
    /// `None` for the server's default.
    pub(crate) initial_size: Option<(u16, u16)>,
}

/// The parts of a `SPAWN_TERMINAL` payload a satellite can serve. Everything
/// else in the frame is local-only.
#[derive(Debug)]
struct SatelliteSpawn {
    /// Group under which to spawn on the satellite.
    group: GroupId,
    /// Command + argv, or `None` for the satellite's default shell.
    command: Option<Vec<String>>,
    /// Working directory, or `None` for the satellite's default policy.
    cwd: Option<String>,
    /// Environment pairs, `None` = inherit the satellite's environment.
    env: Option<Vec<(String, String)>>,
    /// First-class `TERM` override (phux-ign).
    term: Option<String>,
}

/// Owner-terminal targeting and agent-session provenance are local-only: a
/// satellite can resolve neither.
const fn targets_local_only(request: &SpawnRequest) -> bool {
    request.owner_terminal.is_some() || request.agent_session.is_some()
}

/// Relay one satellite-addressed spawn over the owning hub link
/// (phux-v45.6, L1 §3.1 / §9.1) and return the re-tagged result. A
/// missing route — non-hub server, or `host` absent from the hub's
/// registry — is the typed configuration refusal; an unreachable
/// satellite fails fast inside [`crate::hub::relay::RelayHandle::spawn`].
async fn relay_spawn_to_satellite(
    state: &SharedState,
    host: &phux_protocol::ids::SatelliteHost,
    spawn: SatelliteSpawn,
) -> SpawnResult {
    let Some(relay) = state.with(|s| s.hub_relay(host)) else {
        debug!(
            satellite = %host,
            "SPAWN_TERMINAL: no route to satellite (non-hub server, or host not in the registry)",
        );
        return SpawnResult::Err(SpawnError::UnsupportedSatelliteRoute);
    };
    relay
        .spawn(spawn.group, spawn.command, spawn.cwd, spawn.env, spawn.term)
        .await
}

/// Relay a satellite-targeted spawn and reply with its re-tagged result.
///
/// A payload carrying owner-terminal targeting or agent-session provenance is
/// refused rather than silently stripped, because the satellite cannot honour
/// either.
async fn dispatch_satellite_spawn(
    state: &SharedState,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    request_id: u32,
    host: &phux_protocol::ids::SatelliteHost,
    spawn: SatelliteSpawn,
    local_only_targeting: bool,
) {
    let result = if local_only_targeting {
        SpawnResult::Err(SpawnError::SpawnFailed(
            "owner-terminal targeting and agent-session provenance are local-only".to_owned(),
        ))
    } else {
        relay_spawn_to_satellite(state, host, spawn).await
    };
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::TerminalSpawned {
            request_id,
            result,
        }))
        .await;
}

/// Handle `MOVE_TERMINAL` (ADR-0056, L1 §10.1).
///
/// Re-parents `terminal` into the window that currently owns
/// `owner_terminal`, atomically under the state lock: resolve both
/// Terminals, move the registry entry, and reap the source window if the
/// move emptied it — either the whole re-parent lands or none of it does.
/// The pane's process, PTY, scrollback, metadata, and agent record are
/// untouched; its `TerminalId` is stable across the move, so subscriptions
/// and outstanding waits survive. Layout is deliberately NOT written here:
/// geometry is the caller's L3 concern (the ADR-0019 seam), exactly as
/// with spawn placement.
///
/// Local-only: a satellite-tagged id on either end is the typed
/// [`MoveError::UnsupportedSatelliteRoute`], matching spawn's refusal.
pub(crate) async fn handle_move_terminal(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    terminal: phux_protocol::ids::TerminalId,
    owner_terminal: phux_protocol::ids::TerminalId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) {
    debug!(
        ?client_id,
        request_id,
        terminal = ?terminal,
        owner_terminal = ?owner_terminal,
        "MOVE_TERMINAL",
    );

    let (result, clients_to_detach) =
        if !matches!(terminal, phux_protocol::ids::TerminalId::Local { .. })
            || !matches!(owner_terminal, phux_protocol::ids::TerminalId::Local { .. })
        {
            (
                MoveResult::Err(MoveError::UnsupportedSatelliteRoute),
                Vec::new(),
            )
        } else {
            state.with_mut(|s| {
                let Some(moved) = s.terminal_from_wire(&terminal) else {
                    return (
                        MoveResult::Err(MoveError::MoveFailed(
                            "terminal was not found on this server".to_owned(),
                        )),
                        Vec::new(),
                    );
                };
                let Some(owner) = s.terminal_from_wire(&owner_terminal) else {
                    return (
                        MoveResult::Err(MoveError::MoveFailed(
                            "owner terminal was not found on this server".to_owned(),
                        )),
                        Vec::new(),
                    );
                };
                let Some(dest_window) = s.registry().terminal(owner).map(|t| t.window) else {
                    return (
                        MoveResult::Err(MoveError::MoveFailed(
                            "owner terminal has no window on this server".to_owned(),
                        )),
                        Vec::new(),
                    );
                };
                let source_window = s.registry().terminal(moved).map(|t| t.window);
                let source_session = source_window
                    .and_then(|window| s.registry().window(window))
                    .map(|window| window.session);
                match s.registry_mut().move_terminal(moved, dest_window) {
                    Ok(()) => {
                        // A move that emptied its source window leaves it for
                        // the same cascade pane death uses (ADR-0056: "the
                        // server already reaps by its existing rules").
                        if let Some(source_window) = source_window {
                            s.reap_window_if_empty(source_window);
                        }
                        let clients = source_session
                            .filter(|session| s.registry().session(*session).is_none())
                            .map_or_else(Vec::new, |session| {
                                s.attached_clients_in_session(session)
                            });
                        (MoveResult::Ok(terminal), clients)
                    }
                    Err(err) => (
                        MoveResult::Err(MoveError::MoveFailed(err.to_string())),
                        Vec::new(),
                    ),
                }
            })
        };

    let _ = out_tx
        .send(Outbound::Frame(FrameKind::TerminalMoved {
            request_id,
            result,
        }))
        .await;

    // A session-scoped ATTACH cannot remain coherent after its session was
    // reaped. Reply to the move first, then queue DETACHED for only those
    // attached TUIs. Each delivery waits in its own task so a wedged client's
    // full mailbox cannot block this command or the mover's follow-up requests.
    // Headless ATTACH_TERMINAL subscriptions are not session-attached and keep
    // streaming the stable TerminalId as ADR-0056 requires.
    for (detached_client, tx) in clients_to_detach {
        let detached_state = state.clone();
        tokio::task::spawn_local(async move {
            let _ = tx
                .send(Outbound::Frame(FrameKind::Detached {
                    // The group this attach was rooted in is gone — the
                    // `SESSION_KILLED` case in proto.md §7.2, under its legacy
                    // wire name (ADR-0030).
                    reason: Some(DetachReason::SessionKilled),
                    message: "the session this attach was rooted in was reaped".to_owned(),
                }))
                .await;
            super::client::detach_and_release_consumer_state(&detached_state, detached_client);
        });
    }
}

/// Handle `SPAWN_TERMINAL` (phux-4li.11, SPEC §7.2 / §10.1).
///
/// v0.1 servers expose a single default Group at
/// [`crate::state::DEFAULT_GROUP_ID`] (= `GroupId(1)`). Any
/// other id is rejected with [`SpawnError::GroupNotFound`] inside
/// the [`SpawnResult::Err`] arm of the reply frame — separate from
/// the catch-all `Error` channel so command-correlated failures stay
/// typed end-to-end (the same precedent the metadata reply path uses).
///
/// On success the spawn reuses the same PTY primitive
/// [`seed_session_with_pty`] that
/// [`resolve_create_if_missing`] threads through. We always go PTY-
/// backed: a `SPAWN_TERMINAL` with no PTY would be functionally
/// indistinguishable from "nothing happened," and the wire frame
/// commits to a runnable Terminal (the `command = None` ↔ "use the
/// server's default shell" contract from
/// `FrameKind::SpawnTerminal`'s doc).
///
/// `command`/`cwd`/`env` from the wire frame populate the
/// `portable_pty::CommandBuilder`:
///   * `command = None`  → fall back to
///     [`crate::terminal_actor::default_shell_command`] over the
///     resolved default shell (`defaults.shell` → `$SHELL` → `/bin/sh`;
///     same as `AttachTarget::CreateIfMissing.command = None`).
///   * `cwd = Some(p)`    → `builder.cwd(p)`.
///   * `env = Some(v)`    → each `(k, v)` set via `builder.env(k, v)`,
///     additive over the parent environment. `env = Some(vec![])` is
///     distinct from `None` per the wire schema but has no observable
///     effect on the resulting child today (we don't `env_clear`).
///
/// The spawning client is auto-subscribed to the new pane and gets an
/// output-pump task fanning the actor's broadcast into its outbound
/// mailbox — the same machinery `handle_attach` uses for the session's
/// initial panes. Without that, an `INPUT_KEY` to the freshly-spawned
/// id would be rejected at [`crate::runtime::commands::handle_terminal_input`]'s
/// subscription
/// gate and the user would see nothing.
///
/// The pane joins the spawning client's CURRENT session's window
/// (phux-i9zl): a TUI split keeps the session intact so `phux ls` shows one
/// session and a reattach resolves every split pane. The session is
/// resolved from the client's attachment; a `SPAWN_TERMINAL` from a
/// non-attached client (the headless `phux spawn` CLI, or a hub's relayed
/// spawn arriving over the link — phux-v45.6) falls back to the server's
/// most recently active session, and is refused only when the server has
/// no session at all to host the pane.
///
/// A `satellite: Some(host)` spawn never touches local dispatch: on a hub
/// it is relayed over `host`'s link and the reply carries the new
/// Terminal re-tagged `Satellite { host, id }`; a non-hub server (or a
/// hub without `host` in its registry) refuses with the typed
/// [`SpawnError::UnsupportedSatelliteRoute`], and an unreachable
/// satellite fails fast with [`SpawnError::SatelliteUnreachable`]
/// (L1 §3.1 / §9.1).
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "linear orchestration: route satellite spawns → validate group → build CommandBuilder from wire frame → resolve hosting session → spawn PTY-backed pane into its window → auto-subscribe spawning client + spawn output pump → reply on the wire. Every stage now lives in its own named helper, so what is left is the fixed argument list handle_client dispatches into plus one call per stage; the explicit context arguments preserve cancellation and output-pump ownership, and rebundling them would just move the arity to the call site."
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
) {
    let Some(profile) = bootstrap_stream_profile(bootstrap_profile) else {
        let _ = out_tx
            .send(Outbound::Frame(FrameKind::Error {
                request_id: Some(request_id),
                code: ErrorCode::CodecUnavailable,
                message: "SPAWN_TERMINAL selected an unsupported bootstrap profile".to_owned(),
            }))
            .await;
        return;
    };
    let local_only_targeting = targets_local_only(&request);
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
        ..
    } = request;

    // Satellite-targeted spawn (phux-v45.6, L1 §3.1 / §9.1): relay over
    // the owning hub link; the group and PTY details are validated on the
    // satellite, whose errors relay back verbatim. Never falls through to
    // local dispatch.
    if let Some(host) = satellite {
        dispatch_satellite_spawn(
            state,
            out_tx,
            request_id,
            &host,
            SatelliteSpawn {
                group,
                command,
                cwd,
                env,
                term,
            },
            local_only_targeting,
        )
        .await;
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

    SpawnPublication {
        state,
        out_tx,
        request_id,
        client_id,
        core_terminal_id,
        wire_terminal_id,
        handle,
        client_caps,
        stream_id: stream_id_from(u64::from(request_id)),
        profile,
        limits: bootstrap_limits,
    }
    .publish(output_pumps, connection_token)
    .await;
}

/// Record the decoded `SPAWN_TERMINAL` payload at the handler's entry point.
/// `initial_size` is the geometry hint after the zero-axis drop, not the raw
/// wire value.
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
        "SPAWN_TERMINAL",
    );
}

/// What one `SPAWN_TERMINAL` asks the PTY layer to build.
struct PaneSpawnPlan {
    /// The child to exec, fully configured from the wire frame.
    builder: portable_pty::CommandBuilder,
    /// Scrollback rows the new pane retains.
    scrollback: phux_config::ScrollbackLimits,
    /// Host palette the pane starts with, when the spawner advertised one.
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    /// Opaque native agent-session provenance to install before publication.
    agent_session: Option<Vec<u8>>,
    /// `(cols, rows)` to build the pane's grid and PTY at (phux-a5xj).
    initial_size: Option<(u16, u16)>,
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
) -> Result<TerminalId, SpawnError> {
    match spawn_pane_with_pty_and_colors(
        state,
        ownership,
        plan.builder,
        plan.scrollback,
        root_token,
        plan.default_colors,
        plan.agent_session,
        plan.initial_size,
    ) {
        Ok(Some(id)) => Ok(id),
        Ok(None) => {
            warn!(
                ?client_id,
                request_id, "SPAWN_TERMINAL: selected owner has no window to host the pane",
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
                "SPAWN_TERMINAL: failed to spawn pane actor",
            );
            Err(SpawnError::SpawnFailed(format!("{err}")))
        }
    }
}

/// Defensive: the pane spawn succeeded but its handle somehow vanished before
/// we could clone it. Reap the unreachable pane and treat it as a spawn
/// failure on the wire so the client doesn't hang on a reply that will never
/// arrive.
async fn refuse_vanished_pane_handle(
    state: &SharedState,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    client_id: ClientId,
    request_id: u32,
    core_terminal_id: TerminalId,
) {
    warn!(
        ?client_id,
        request_id,
        ?core_terminal_id,
        "SPAWN_TERMINAL: spawn succeeded but TerminalHandle vanished",
    );
    state.with_mut(|s| {
        s.reap_terminal(core_terminal_id);
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

/// Reply to a `SPAWN_TERMINAL` with a typed refusal. Command-correlated
/// failures stay on the reply frame rather than the catch-all `Error` channel.
async fn refuse_spawn(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    request_id: u32,
    error: SpawnError,
) {
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::TerminalSpawned {
            request_id,
            result: SpawnResult::Err(error),
        }))
        .await;
}

/// phux-a5xj: a zero on either axis is "I do not know my geometry", not a
/// zero-cell grid — libghostty has no such thing. Drop it and take the
/// server default, matching SPEC §10.5's zero-viewport no-op rule.
fn usable_initial_size(initial_size: Option<(u16, u16)>) -> Option<(u16, u16)> {
    initial_size.filter(|&(cols, rows)| cols > 0 && rows > 0)
}

/// Validate the parts of a local `SPAWN_TERMINAL` payload that need no state:
/// the agent-session provenance bound, and the v0.1 single-Group rule.
///
/// v0.1 servers expose a single default Group at
/// [`crate::state::DEFAULT_GROUP_ID`]; any other id is
/// [`SpawnError::GroupNotFound`].
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

/// Build the child's argv.
///
/// `command = None` mirrors `AttachTarget::CreateIfMissing.command = None`:
/// fall back to the resolved default shell (`defaults.shell` → `$SHELL` →
/// `/bin/sh`, phux-i0e8.4.1).
fn spawn_argv_builder(
    state: &SharedState,
    command: Option<Vec<String>>,
) -> portable_pty::CommandBuilder {
    match command {
        Some(argv) if !argv.is_empty() => {
            let mut head = argv.into_iter();
            let program = head.next().unwrap_or_default();
            let mut builder = portable_pty::CommandBuilder::new(program);
            for arg in head {
                builder.arg(arg);
            }
            builder
        }
        _ => {
            let (shell, login_shell) = state.with(|s| (s.shell().to_owned(), s.login_shell()));
            crate::terminal_actor::default_shell_command(&shell, login_shell)
        }
    }
}

/// Build the `CommandBuilder` a `SPAWN_TERMINAL` execs: argv, `TERM`, working
/// directory, and environment, each in the precedence order it defines.
///
/// TERM precedence (phux-ign): each later tier overrides the prior via
/// `CommandBuilder::env`, which overwrites. So the order is:
///   1. compiled-in `DEFAULT_TERM` (from `default_shell_command`)
///   2. server `defaults.term` (here)
///   3. per-spawn first-class `SPAWN_TERMINAL.term` field (below)
///   4. per-spawn `SPAWN_TERMINAL.env` entry for `TERM` (wire `env`
///      loop, which runs last) — authoritative for the Terminal.
///
/// Working directory precedence (phux-cs6): an explicit wire `cwd`
/// always wins; otherwise fall back to `defaults.cwd-inheritance`. The
/// inherit-focused policy reads the spawning client's focused pane's
/// live PTY CWD via a kernel query, so `C-a |` from a pane cd'd to
/// /tmp opens the new pane in /tmp.
///
/// `env = Some(v)` sets each `(k, v)` additively over the parent environment.
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

/// Resolve which existing Terminal or session must host the new pane.
///
/// phux-i9zl: a split spawns into the spawning client's CURRENT session's
/// window, not a fresh `spawn-N` wrapper session. Resolve that session
/// from the client's attachment (the same `s.attached()` lookup the cwd
/// inheritance uses). A non-attached spawner — the headless
/// `phux spawn` CLI, or a hub's relayed spawn arriving over the link
/// (phux-v45.6; the hub's link consumer never attaches) — falls back to
/// the server's most recently active session (the same focus heuristic
/// `GET_STATE` snapshots use). Only a server with no session at all
/// refuses, rather than orphan a PTY nothing can list.
fn resolve_spawn_ownership(
    state: &SharedState,
    client_id: ClientId,
    owner_terminal: Option<phux_protocol::ids::TerminalId>,
) -> Result<SpawnOwnership, SpawnError> {
    if let Some(owner) = owner_terminal {
        if !matches!(owner, phux_protocol::ids::TerminalId::Local { .. })
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

/// Auto-subscribe the spawning client to the new pane and clone out its wire
/// id, actor handle, and negotiated capabilities.
///
/// Without subscription the `INPUT_*` dispatch path's
/// `subscribers_for_terminal(...).contains(&client_id)` gate would reject
/// every keystroke the spawning client sends to the new id.
///
/// The subscribe-and-handle lookup happens in a single `with_mut`
/// critical section so the wire-id allocation and the subscriber
/// append observe the same registry state.
fn subscribe_spawning_client(
    state: &SharedState,
    client_id: ClientId,
    core_terminal_id: TerminalId,
) -> Option<(
    phux_protocol::ids::TerminalId,
    crate::terminal_actor::TerminalHandle,
    ClientCapabilities,
)> {
    state.with_mut(|s| {
        let wire_terminal_id = s.intern_terminal_wire(core_terminal_id);
        let client_caps = s
            .attached()
            .get(&client_id)
            .map(|c| c.client_caps)
            .unwrap_or_default();
        // Only auto-subscribe if the client is currently attached —
        // a bare `SPAWN_TERMINAL` from a non-attached client is legal
        // wire-wise (the frame doesn't require ATTACH first) but the
        // subscription would have no `attached` slot to live in.
        if s.attached().contains_key(&client_id) {
            // `None` mailbox: the `attached` entry just checked already
            // carries this client's sender, so terminal-scoped fanout
            // resolves it without a second copy (phux-w7z2.56).
            s.subscribe_terminal(client_id, core_terminal_id, None);
        }
        s.terminal_handle(core_terminal_id)
            .cloned()
            .map(|h| (wire_terminal_id, h, client_caps))
    })
}

/// Spawn the `SPAWN_TERMINAL` output pump and hand back its publication gate.
///
/// A `SPAWN_TERMINAL` pump owns the pane it feeds: nothing else is subscribed
/// yet, so a lost generation reaps the terminal rather than leaving an
/// unreadable PTY behind. A tombstone that could not be queued, a closed
/// mailbox, and an abandoned replay only end the pump — the pane is untouched.
fn spawn_terminal_output_pump(
    ctx: OutputPumpContext,
    output_rx: tokio::sync::broadcast::Receiver<PaneOutput>,
    state: &SharedState,
    connection_token: &CancellationToken,
    core_terminal_id: TerminalId,
    output_pumps: &mut JoinSet<()>,
) -> oneshot::Sender<OutputPumpStart> {
    let pump_state = state.clone();
    let pump_connection_token = connection_token.clone();
    let (gate_tx, gate_rx) = oneshot::channel::<OutputPumpStart>();
    output_pumps.spawn_local(async move {
        let Some(fault) = run_output_pump(&ctx, gate_rx, output_rx).await else {
            return;
        };
        match fault {
            PumpFault::OutboundClosed
            | PumpFault::TombstoneNotQueued
            | PumpFault::ReplayAbandoned => {}
            PumpFault::GenerationLost => {
                pump_state.with_mut(|s| {
                    s.reap_terminal(core_terminal_id);
                });
                pump_connection_token.cancel();
            }
            PumpFault::PublicationNotActivated => pump_connection_token.cancel(),
        }
    });
    gate_tx
}

/// The freshly spawned pane and everything its publication stage needs: the
/// wire identity to announce, the actor to capture from, and the reply
/// correlation the client is waiting on.
struct SpawnPublication<'a> {
    /// Server state, for reaping a pane the client can never read.
    state: &'a SharedState,
    /// The spawning client's outbound mailbox.
    out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    /// Correlates every reply frame with the client's request.
    request_id: u32,
    /// The spawning client.
    client_id: ClientId,
    /// Server-local id of the pane just spawned.
    core_terminal_id: TerminalId,
    /// Wire id announced to the client.
    wire_terminal_id: phux_protocol::ids::TerminalId,
    /// Actor handle for the capture.
    handle: crate::terminal_actor::TerminalHandle,
    /// Negotiated capabilities the payload is adapted to.
    client_caps: ClientCapabilities,
    /// Stream the pane's generation publishes on.
    stream_id: StreamId,
    /// Negotiated bootstrap stream profile.
    profile: BootstrapStreamProfile,
    /// Negotiated bootstrap bounds.
    limits: BootstrapLimits,
}

impl SpawnPublication<'_> {
    /// Is this spawn publishing native libghostty checkpoints?
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    const fn publishes_native_checkpoints(&self) -> bool {
        matches!(
            self.profile,
            BootstrapStreamProfile::NativeState {
                codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2
            }
        )
    }

    /// Drop the pane nobody can reach: the client never received a usable
    /// generation for it.
    fn reap(&self) {
        self.state.with_mut(|s| {
            s.reap_terminal(self.core_terminal_id);
        });
    }

    /// Queue the successful `TERMINAL_SPAWNED` reply. `false` once the
    /// client's outbound mailbox has closed.
    async fn queue_spawned_ok(&self) -> bool {
        self.out_tx
            .send(Outbound::Frame(FrameKind::TerminalSpawned {
                request_id: self.request_id,
                result: SpawnResult::Ok(self.wire_terminal_id.clone()),
            }))
            .await
            .is_ok()
    }

    /// phux-y2t: fan a `pane_spawned` agent event to event-stream
    /// subscribers (SPEC §7.5). The new pane's wire id rides the
    /// `EVENT` envelope; server-wide subscribers and any per-pane
    /// subscribers for this id receive it.
    fn announce_pane_spawned(&self) {
        broadcast_event(
            self.state,
            Some(&self.wire_terminal_id),
            &AgentEvent::PaneSpawned,
        );
    }

    /// Capture the pane's first native checkpoint. `None` once the actor is
    /// gone or refuses the capture.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn capture_native_checkpoint(
        &self,
    ) -> Option<crate::terminal_actor::NativeBootstrapReply> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let sent = self
            .handle
            .native_bootstrap
            .send(crate::terminal_actor::NativeBootstrapRequest {
                owner: self.client_id.0,
                terminal_id: self.wire_terminal_id.clone(),
                stream_id: self.stream_id,
                bootstrap_id: initial_bootstrap_id(),
                limits: self.limits,
                max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
                max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
                reply: reply_tx,
            })
            .await
            .is_ok();
        if !sent {
            return None;
        }
        match reply_rx.await {
            Ok(Ok(reply)) => Some(reply),
            Ok(Err(error)) => {
                let core_terminal_id = self.core_terminal_id;
                warn!(?core_terminal_id, %error, "native spawn preflight failed");
                None
            }
            Err(_) => None,
        }
    }

    /// Spawn the pane's output pump, then publish its first generation for
    /// whichever bootstrap profile was negotiated.
    ///
    /// `profile` was validated before spawning the pane, so an unknown
    /// future profile can never publish a partial bootstrap generation.
    /// Spawn the output pump BEFORE replying with `TerminalSpawned`
    /// so any bytes the freshly-spawned PTY emits in the gap between
    /// exec and the client's first read are queued on the broadcast
    /// channel (broadcasts buffer per subscriber). Mirrors the
    /// subscribe-before-snapshot ordering in `handle_attach`.
    async fn publish(self, output_pumps: &mut JoinSet<()>, connection_token: &CancellationToken) {
        let output_rx = self.handle.output.subscribe();
        let gate_tx = spawn_terminal_output_pump(
            OutputPumpContext {
                out_tx: self.out_tx.clone(),
                resize: self.handle.resize.clone(),
                wire_terminal_id: self.wire_terminal_id.clone(),
                stream_id: self.stream_id,
                initial_bootstrap_id: initial_bootstrap_id(),
                client_id: self.client_id,
                client_caps: self.client_caps,
                profile: self.profile,
                limits: self.limits,
                lag_label: "SPAWN_TERMINAL output pump",
                #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                handle: self.handle.clone(),
            },
            output_rx,
            self.state,
            connection_token,
            self.core_terminal_id,
            output_pumps,
        );
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if self.publishes_native_checkpoints() {
            self.publish_native_spawn(gate_tx).await;
            return;
        }
        self.publish_synthesized_spawn(gate_tx).await;
    }

    /// Publish the native checkpoint generation: reply, queue the staged
    /// frames, activate the publication, then release the parked output pump.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn publish_native_spawn(&self, gate_tx: oneshot::Sender<OutputPumpStart>) {
        let Some(reply) = self.capture_native_checkpoint().await else {
            self.reap();
            refuse_spawn(
                self.out_tx,
                self.request_id,
                SpawnError::SpawnFailed("native checkpoint preflight failed".to_owned()),
            )
            .await;
            return;
        };
        let cut = reply.base_seq;
        let cursor = reply.publication_cursor;
        if !self.queue_spawned_ok().await {
            self.reap();
            return;
        }
        for frame in reply.frames {
            if self.out_tx.send(Outbound::Frame(frame)).await.is_err() {
                self.reap();
                return;
            }
        }
        let Ok(publication) = activate_native_publication(
            &self.handle,
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
        let _ = gate_tx.send(OutputPumpStart {
            published_cut: cut,
            replay: publication.replay,
            live: Some(publication.live),
        });
        self.announce_pane_spawned();
    }

    /// Capture the pane's first synthesized snapshot and the actor cut it was
    /// taken at. `None` once the actor is gone or refuses.
    async fn capture_snapshot(&self) -> Option<(crate::grid::SnapshotBytes, u64)> {
        let (snapshot_tx, snapshot_rx) = oneshot::channel();
        if self
            .handle
            .snapshot
            .send(SnapshotRequest {
                scrollback: None,
                max_bytes: usize::MAX,
                max_frames: usize::MAX,
                chunk_bytes: 1,
                reply: snapshot_tx,
            })
            .await
            .is_err()
        {
            return None;
        }
        snapshot_rx.await.ok()?.ok()
    }

    /// Publish the synthesized-VT generation: reply, queue BEGIN/CHUNK/READY,
    /// then release the parked output pump.
    async fn publish_synthesized_spawn(&self, gate_tx: oneshot::Sender<OutputPumpStart>) {
        let Some((snapshot, cut)) = self.capture_snapshot().await else {
            self.reap();
            refuse_spawn(
                self.out_tx,
                self.request_id,
                SpawnError::SpawnFailed("snapshot preflight failed".to_owned()),
            )
            .await;
            return;
        };
        let replay = downsample_for_caps(&bytes::Bytes::from(snapshot.bytes), self.client_caps);
        let Ok(frames) = synthesized_bootstrap_frames(
            self.wire_terminal_id.clone(),
            self.stream_id,
            initial_bootstrap_id(),
            self.profile,
            self.limits,
            snapshot.cols,
            snapshot.rows,
            cut,
            [replay],
        ) else {
            self.reap();
            refuse_spawn(
                self.out_tx,
                self.request_id,
                SpawnError::SpawnFailed("bootstrap limits rejected snapshot".to_owned()),
            )
            .await;
            return;
        };
        if !self.queue_spawned_ok().await {
            self.reap();
            return;
        }
        for frame in frames {
            if self.out_tx.send(Outbound::Frame(frame)).await.is_err() {
                self.reap();
                return;
            }
        }
        let _ = gate_tx.send(OutputPumpStart {
            published_cut: cut,
            replay: Vec::new(),
            live: None,
        });
        self.announce_pane_spawned();
    }
}

/// Is this ATTACH a replacement for the same client's existing attachment to
/// the same session?
fn is_same_session_reattach(state: &SharedState, client_id: ClientId, session_name: &str) -> bool {
    state.with(|server| {
        let target = server.find_session_by_name(session_name);
        matches!(
            (server.attached().get(&client_id), target),
            (Some(attached), Some(target)) if attached.session == target
        )
    })
}

/// Run [`prepare_attach`] and translate each refusal into the `ERROR` frame the
/// client sees. `None` once the attach has been refused.
async fn prepare_attach_or_refuse(
    state: &SharedState,
    client_id: ClientId,
    session_name: &str,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    client_caps: ClientCapabilities,
    negotiated_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
) -> Option<AttachPrepared> {
    match prepare_attach(
        state,
        client_id,
        session_name,
        out_tx,
        client_caps,
        negotiated_profile,
        bootstrap_limits,
    ) {
        Ok(prepared) => Some(prepared),
        Err(crate::state::AttachError::UnknownSession(name)) => {
            send_error(
                out_tx,
                ErrorCode::SessionNotFound,
                &format!("session {name:?} not found"),
            )
            .await;
            None
        }
        Err(crate::state::AttachError::AlreadyAttached(_)) => {
            send_error(
                out_tx,
                ErrorCode::AlreadyAttached,
                "client is already attached",
            )
            .await;
            None
        }
        Err(crate::state::AttachError::ResourceLimit) => {
            send_error(
                out_tx,
                ErrorCode::CodecUnavailable,
                "session exceeds bounded aggregate attach limits",
            )
            .await;
            None
        }
    }
}

/// Stop the prior generation's actor-side tick emitters before the new
/// ATTACHED frame is visible. Raw pumps were aborted by the caller; without
/// this matching teardown, a state-sync delta from the old stream
/// could interleave between ATTACHED and the replacement bootstrap.
///
/// Only a state-sync consumer has actor-side emitters to stop.
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

/// Terminal defaults are shared pane state. The most recently attached
/// interactive client that advertises a palette wins; palette-less agent
/// and legacy attaches leave the last known values untouched. Await each
/// acknowledgement before snapshotting so OSC 10/11 queries parsed after
/// ATTACHED observe the selected host palette.
async fn apply_client_default_colors(
    panes: &[AttachSnapshotPane],
    colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) {
    let Some(colors) = colors else {
        return;
    };
    for pane in panes {
        let (reply, done) = oneshot::channel();
        if pane
            .handle
            .set_default_colors
            .send(SetDefaultColorsRequest { colors, reply })
            .await
            .is_ok()
        {
            let _ = done.await;
        }
    }
}

/// The aggregate staging state one ATTACH accumulates across its panes.
///
/// Captures are deliberately awaited one pane at a time: the retained result
/// from earlier panes is charged here before the next actor receives its
/// remaining source-allocation ceiling, so no set of concurrent actor
/// allocations can exceed the connection-wide cap.
struct AttachStaging {
    /// Connection-wide byte and frame ceiling for the whole publication.
    budget: BootstrapStagingBudget,
    /// Bootstrap and authoritative-closure frames staged for atomic publication.
    frames: Vec<FrameKind>,
    /// Actor handles staged so the rollback boundary can detach each producer.
    handles: Vec<crate::terminal_actor::TerminalHandle>,
    /// phux-7w1j: per-pane "snapshot has been sent" gates.
    gates: Vec<SnapshotGate>,
    /// A failed replacement is connection-fatal: preserving an older producer
    /// would allow output to overtake the terminal ERROR.
    pumps: JoinSet<()>,
}

impl Default for AttachStaging {
    fn default() -> Self {
        Self {
            budget: BootstrapStagingBudget::new(),
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
        terminal_ids: Vec<phux_protocol::ids::TerminalId>,
    ) -> Result<(), ()> {
        let mut frames = Vec::new();
        frames.try_reserve(terminal_ids.len()).map_err(|_| ())?;
        frames.extend(
            terminal_ids
                .into_iter()
                .map(|terminal_id| FrameKind::TerminalClosed {
                    terminal_id,
                    exit_status: None,
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
    /// phux-3uv: the actor's tick is this consumer's sole live emitter, so the
    /// broadcast pump must be suppressed for this pane.
    tick_managed: bool,
    /// Atomic synthesized bootstrap captured in the same actor turn.
    state_sync_bootstrap: Option<crate::terminal_actor::StateSyncBootstrap>,
}

/// The negotiated shape of one ATTACH's per-pane capture: everything that is
/// identical for every pane in the session.
struct PaneCaptureContext<'a> {
    /// Server state, for the pumps' fatal-fault cleanup.
    state: &'a SharedState,
    /// The attaching client's outbound mailbox.
    out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    /// Cancelled by a pump whose generation became unrecoverable.
    connection_token: &'a CancellationToken,
    /// Released once the aggregate publication is on the wire.
    live_gate_rx: tokio::sync::watch::Receiver<bool>,
    /// The attaching client.
    client_id: ClientId,
    /// Wire form of [`Self::client_id`], as the actors key consumers.
    wire_client_id: phux_protocol::ids::ClientId,
    /// Negotiated capabilities every payload is adapted to.
    client_caps: ClientCapabilities,
    /// Stream every pane's generation publishes on.
    stream_id: StreamId,
    /// Generation the aggregate publication carries.
    bootstrap_id: BootstrapId,
    /// Negotiated bootstrap stream profile.
    profile: BootstrapStreamProfile,
    /// Negotiated bootstrap bounds.
    limits: BootstrapLimits,
    /// phux-9q5f: the ATTACH's scrollback request, capped in lines.
    scrollback: Option<u32>,
    /// Per-chunk ceiling shared by every pane's capture.
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

    /// Is this attach publishing native libghostty checkpoints?
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    const fn publishes_native_checkpoints(&self) -> bool {
        matches!(
            self.profile,
            BootstrapStreamProfile::NativeState {
                codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2
            }
        )
    }

    /// ADR-0018 / phux-0q8: register the per-consumer state-sync entry
    /// so the actor allocates and primes a per-consumer `RenderState`
    /// cache for this client/pane, keyed by `wire_client_id`. We do
    /// this BEFORE emitting the snapshot so the per-consumer cache is
    /// primed against the same canonical state the snapshot installs
    /// on the client mirror (see `register_consumer`'s doc).
    ///
    /// phux-3uv: the register reply reports whether the actor is
    /// tick-managing this consumer (`consumer_tick_emits == true`). If
    /// so, the actor's `tick_emit` is the sole emitter and we MUST
    /// suppress the broadcast pump — otherwise two independent
    /// `seq` streams land on one consumer mailbox (double-paint, SPEC
    /// §12.2 monotonic-per-consumer violation). If not tick-managed
    /// (gate off, or register failed / actor gone / no local id), the
    /// broadcast pump stays the live emitter and the per-consumer
    /// entry just drives the dormant `FRAME_ACK` eviction loop.
    ///
    /// Awaited (not fire-and-forget) so the cache is primed before the
    /// pump starts streaming deltas; a dropped reply or actor-gone is
    /// logged and we fall back to the broadcast path.
    async fn register_consumer(
        &self,
        handle: &crate::terminal_actor::TerminalHandle,
        wire_terminal_id: &phux_protocol::ids::TerminalId,
        terminal_id: TerminalId,
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
                // phux-fseo: honor the consumer's negotiated output mode.
                // StateSync ⇒ the actor's tick is this consumer's emitter
                // and the broadcast pump is suppressed for it; Raw
                // (the human-TUI default) keeps the pump.
                wants_state_sync: self.wants_state_sync(),
                state_sync_scrollback: self.scrollback,
                bootstrap_max_bytes,
                bootstrap_max_frames,
                bootstrap_chunk_bytes: self.chunk_bytes,
                // phux-v45.8: a directly-attached consumer rides a reliable,
                // ordered transport (UDS / SSH stdio / WebSocket / QUIC
                // stream), so the emit-once model is correct and cheapest —
                // no loss-tolerant re-diff needed. Activation for a
                // forwarded (hub->satellite->consumer) leg, where the hub's
                // fan-out can drop whole frames, is the deferred follow-up
                // (the satellite cannot see the downstream drop from the
                // link's reliable transport); the advance-on-ack mechanism
                // it flips on is fully implemented here (ADR-0042).
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

    /// Stage this pane's output pump behind its publication gate.
    ///
    /// Subscribe to live PTY output BEFORE requesting the snapshot.
    /// Subscribing first means anything the `TerminalActor` broadcasts
    /// after this point lands in our receiver; we then ask for a
    /// snapshot so the client has a complete starting picture, and
    /// any subsequent `TerminalOutput` we forward is "post-snapshot
    /// delta" rather than racing against it.
    ///
    /// phux-7w1j: the pump parks on the gate registered here and must not
    /// FORWARD a `TerminalOutput` frame until the pane's bootstrap has been
    /// written to `out_tx` — else a PTY-active pane races output ahead of its
    /// snapshot and the client sees frame 2 = OUTPUT instead of SNAPSHOT.
    ///
    /// An ATTACH pump shares its panes with the rest of the session, so a
    /// fatal fault releases only this client's consumer state.
    fn spawn_pane_pump(
        &self,
        staging: &mut AttachStaging,
        terminal_id: TerminalId,
        wire_terminal_id: &phux_protocol::ids::TerminalId,
        handle: &crate::terminal_actor::TerminalHandle,
    ) {
        let output_rx = handle.output.subscribe();
        let ctx = OutputPumpContext {
            out_tx: self.out_tx.clone(),
            resize: handle.resize.clone(),
            wire_terminal_id: wire_terminal_id.clone(),
            stream_id: self.stream_id,
            initial_bootstrap_id: self.bootstrap_id,
            client_id: self.client_id,
            client_caps: self.client_caps,
            profile: self.profile,
            limits: self.limits,
            lag_label: "TerminalOutput pump",
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            handle: handle.clone(),
        };
        let pump_state = self.state.clone();
        let pump_connection_token = self.connection_token.clone();
        let client_id = self.client_id;
        let (gate_tx, gate_rx) = oneshot::channel::<OutputPumpStart>();
        staging.gates.push(SnapshotGate {
            terminal_id,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            wire_terminal_id: wire_terminal_id.clone(),
            handle: handle.clone(),
            gate: gate_tx,
            cut: None,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_cursor: None,
        });
        staging.pumps.spawn_local(async move {
            let Some(fault) = run_output_pump(&ctx, gate_rx, output_rx).await else {
                return;
            };
            match fault {
                PumpFault::OutboundClosed | PumpFault::ReplayAbandoned => {}
                PumpFault::TombstoneNotQueued | PumpFault::GenerationLost => {
                    crate::runtime::client::detach_and_release_consumer_state(
                        &pump_state,
                        client_id,
                    );
                    pump_connection_token.cancel();
                }
                PumpFault::PublicationNotActivated => pump_connection_token.cancel(),
            }
        });
    }

    /// Adapt one pane's synthesized snapshot to the client's capabilities,
    /// frame it, and charge the retained result to the aggregate budget.
    ///
    /// `label` names the capture in the rollback reason the client is told.
    fn stage_synthesized_frames(
        &self,
        staging: &mut AttachStaging,
        wire_terminal_id: phux_protocol::ids::TerminalId,
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
        terminal_id: TerminalId,
        wire_terminal_id: &phux_protocol::ids::TerminalId,
        handle: &crate::terminal_actor::TerminalHandle,
    ) -> Result<(), String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if handle
            .native_bootstrap
            .send(crate::terminal_actor::NativeBootstrapRequest {
                owner: self.client_id.0,
                terminal_id: wire_terminal_id.clone(),
                stream_id: self.stream_id,
                bootstrap_id: self.bootstrap_id,
                limits: self.limits,
                max_bytes: staging.budget.remaining_bytes(),
                max_frames: staging.budget.remaining_frames(),
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            warn!(?terminal_id, "pane actor dropped before native bootstrap");
            return Err("pane actor dropped native bootstrap request".to_owned());
        }
        let mut reply = match reply_rx.await {
            Ok(Ok(reply)) => reply,
            Ok(Err(error)) => {
                warn!(?terminal_id, %error, "native checkpoint failed before attach publication");
                return Err("native checkpoint capture failed".to_owned());
            }
            Err(_) => {
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
        handle: &crate::terminal_actor::TerminalHandle,
        terminal_id: TerminalId,
        max_bytes: usize,
        max_frames: usize,
    ) -> Result<(crate::grid::SnapshotBytes, u64), String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if handle
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
        // A tick-managed (state-sync) consumer gets NO broadcast output pump:
        // the actor's 33 Hz tick emits its deltas directly, in that consumer's
        // own per-consumer sequence space, with its own resync. That is what
        // keeps the pump's gap fence (`PumpGeneration::forwards`) and the
        // state-sync path disjoint rather than merely non-interfering — there
        // is no pump here to fence, and no broadcast sequence for a fence to
        // hold back.
        //
        // The invariant is enforced in two places and they must agree: here,
        // for ATTACH, and in `commands.rs` for the `SPAWN_TERMINAL` path,
        // which additionally drops its `pump_done_guard` because there is no
        // pump task for a replacement to wait on. What covers it today is
        // indirect — `statesync_convergence` would diverge if a pump were also
        // feeding raw broadcast bytes into a state-sync consumer's stream —
        // and no test pins the two call sites against each other directly.
        // (A citation here previously named a test called
        // `state_sync_consumer_gets_no_broadcast_pump`; no such test has ever
        // existed. Writing the real two-path version is its own piece of work
        // and its own bead, not a comment.)
        if !registration.tick_managed {
            self.spawn_pane_pump(staging, terminal_id, &wire_terminal_id, &handle);
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
        if self.publishes_native_checkpoints() {
            return self
                .stage_native_bootstrap(staging, terminal_id, &wire_terminal_id, &handle)
                .await;
        }
        let (snapshot, cut) = self
            .request_pane_snapshot(
                &handle,
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
        closed_before_ready: Vec<phux_protocol::ids::TerminalId>,
    ) -> Result<(), String> {
        for pane in panes {
            self.capture_pane(staging, pane).await?;
        }
        staging
            .append_closures(closed_before_ready)
            .map_err(|()| "aggregate bootstrap staging budget exceeded".to_owned())
    }
}

/// The atomic ATTACH publication: the frames the client sees, in the one order
/// the handshake permits.
struct AttachPublication<'a> {
    /// Server state, for releasing consumer state on a closed mailbox.
    state: &'a SharedState,
    /// The attaching client's outbound mailbox.
    out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    /// Cancelled when a published generation can no longer be activated.
    connection_token: &'a CancellationToken,
    /// The attaching client.
    client_id: ClientId,
    /// Correlates `ATTACHED` and `ATTACH_READY` with the client's request.
    attach_id: u32,
    /// Stream every pane's generation publishes on.
    stream_id: StreamId,
    /// Generation this publication carries.
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
    /// wire.
    ///
    /// A native generation activates its publication here: the actor hands back
    /// the replay backlog and the post-cut receiver the pump adopts. A pane
    /// whose capture produced no cut has no pump to release.
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
                let Ok(publication) = activate_native_publication(
                    &gate.handle,
                    self.client_id.0,
                    gate.wire_terminal_id,
                    self.stream_id,
                    self.bootstrap_id,
                    cursor,
                )
                .await
                else {
                    crate::runtime::client::detach_and_release_consumer_state(
                        self.state,
                        self.client_id,
                    );
                    self.connection_token.cancel();
                    return;
                };
                start.replay = publication.replay;
                start.live = Some(publication.live);
            }
            let _ = gate.gate.send(start);
        }
    }
}

/// Handle `TERMINAL_RESIZE` (phux-4li.11, SPEC §7.2 / §10.2).
///
/// Look up the target Terminal by its wire id, then `try_send` the new
/// `(cols, rows)` into the actor's resize mailbox. The actor's existing
/// `handle_resize` (built for `VIEWPORT_RESIZE` in phux-byc.5) drives
/// both `libghostty_vt::Terminal::resize` and the PTY
/// `ioctl(TIOCSWINSZ)` from one place — we reuse it verbatim so the
/// per-Terminal resize and the per-Viewport resize stay in lockstep.
///
/// Silent on every "not found" path per the wire frame's
/// no-reply-by-design contract. The frame label distinguishes this
/// path from `VIEWPORT_RESIZE` in logs.
///
/// `client_id` is unused today (the wire frame is unauthenticated;
/// SATELLITE-routed ids are rejected before we get here). It's wired
/// through anyway so future per-client validation (e.g. checking that
/// the client is subscribed to the pane) doesn't require widening the
/// helper signature.
/// Resolve `target`, call [`prepare_attach`], and queue the
/// `ATTACHED` + per-pane `TERMINAL_SNAPSHOT` frames on `out_tx`.
///
/// On any failure path, emits an `ERROR` frame and returns. We never
/// partially-attach: either every frame queues or none does.
#[allow(
    clippy::too_many_lines,
    reason = "linear attach orchestration: resolve target -> prepare -> stage per-pane output pumps -> capture each bounded source against the remaining aggregate budget -> publish atomically. Every stage now lives in its own named helper; what is left is the fixed argument list, the rollback macro that must `return` from this frame, and one call per stage."
)]
#[allow(
    clippy::too_many_arguments,
    reason = "the ATTACH branch in handle_client pre-decomposes the FrameKind::Attach payload (target/viewport/request_scrollback/scrollback_limit_lines) and threads the negotiated ColorSupport alongside the SharedState + client_id + out_tx; rebundling into a struct would just move the arity from the call site to a builder"
)]
// Lifecycle span (info): one ATTACH per client. Its CLOSE duration is the
// attach-handshake timing (bounded per-pane capture is the slow part); the
// fields correlate it to a client + target + requested dims. `skip_all` keeps the
// large arg list (state handle, channels, token) out of the span.
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
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    client_caps: ClientCapabilities,
    negotiated_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
    root_token: &CancellationToken,
    output_pumps: &mut JoinSet<()>,
    connection_token: &CancellationToken,
) {
    let Some(stream_profile) = bootstrap_stream_profile(negotiated_profile) else {
        send_error(
            out_tx,
            ErrorCode::CodecUnavailable,
            "ATTACH selected an unsupported bootstrap profile",
        )
        .await;
        return;
    };
    // phux-9q5f: honor the ATTACH scrollback request. `request_scrollback`
    // gates the feature; `scrollback_limit_lines` caps it (0 ⇒ all retained
    // history, the SCROLLBACK_ALL sentinel). The per-pane SnapshotRequest
    // carries this so the actor primes TERMINAL_SNAPSHOT.scrollback_bytes.
    let scrollback_req: Option<u32> = request_scrollback.then_some(scrollback_limit_lines);

    let Some(session_name) = resolve_attach_target(
        state,
        target,
        out_tx,
        root_token,
        client_caps.default_colors,
    )
    .await
    else {
        return;
    };

    // phux-p4vp: fold each live pane's kernel CWD into its registry
    // descriptor before the snapshot is built, so ATTACHED carries a
    // current `cwd` per pane (the sidebar's VCS branch line depends on it).
    refresh_registry_cwds(state).await;

    let same_session_reattach = is_same_session_reattach(state, client_id, &session_name);

    let Some((snapshot, initial_client_id, panes_to_snapshot, closed_before_ready)) =
        prepare_attach_or_refuse(
            state,
            client_id,
            &session_name,
            out_tx,
            client_caps,
            negotiated_profile,
            bootstrap_limits,
        )
        .await
    else {
        return;
    };
    let wire_client_id =
        phux_protocol::ids::ClientId::new(u32::try_from(client_id.0).unwrap_or(u32::MAX));
    if same_session_reattach {
        detach_prior_state_sync_consumers(&panes_to_snapshot, wire_client_id, client_caps).await;
    }

    apply_client_default_colors(&panes_to_snapshot, client_caps.default_colors).await;

    // phux-2lj: apply the client's ATTACH viewport to every pane so
    // freshly-spawned PTYs (currently built at hardcoded 80x24, see
    // `seed_session_with_pty`) are resized to match the attaching
    // client's host terminal. Without this, e.g. `vim` running in a
    // 120x48 host terminal only fills the top 24 rows of the screen
    // until SIGWINCH or an explicit VIEWPORT_RESIZE drives a resize.
    //
    // SPEC §10.5: ATTACH.viewport is the outer client viewport. Single-
    // pane: the server applies it directly as the PTY's winsize (matches
    // the existing `handle_viewport_resize` convention; the off-by-one
    // for a host-side status bar is the client's concern via the
    // post-attach `TERMINAL_RESIZE` reflow path used by multi-pane).
    apply_attach_viewport(state, client_id, &panes_to_snapshot, viewport);

    // Capture sources one pane at a time. Each completed result is charged to
    // the aggregate staging budget before the next actor receives its remaining
    // byte/frame ceiling, so no set of concurrent actor allocations can exceed
    // the connection-wide cap.
    let stream_id = stream_id_from(u64::from(attach_id));
    let bootstrap_id = initial_bootstrap_id();
    // `stream_profile` was validated before resolving or mutating the attach
    // target, so no ATTACHED/BOOTSTRAP_BEGIN can precede this preflight.
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
    if let Err(reason) = capture
        .capture_panes(&mut staging, panes_to_snapshot, closed_before_ready)
        .await
    {
        fail_prepublication!(reason.as_str());
    }

    // Commit the replacement only after every pane has produced a complete,
    // bounded bootstrap. Until this point the prior generation's pumps remain
    // live and every new pump is parked on its unpublished gate.
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
    if !publication
        .publish(snapshot, initial_client_id, staging.frames, &session_name)
        .await
    {
        return;
    }
    let _ = live_gate_tx.send(true);
    publication.release_gates(staging.gates).await;
}

/// phux-2lj: Apply the ATTACH viewport to every pane in the freshly-
/// attached session.
///
/// Panes are spawned at a hardcoded 80x24 default ([`seed_session_with_pty`]
/// / [`seed_session_with_actor`]) because the session may exist before any
/// client attaches (e.g. `phux-server` pre-seeding). On the first attach
/// we have to size the PTY to match the client's outer viewport, otherwise
/// full-screen TUIs (vim, htop) think they're running in 24 rows and
/// render into a fraction of the visible area. This mirrors what
/// [`crate::runtime::commands::handle_viewport_resize`] does for a live
/// `VIEWPORT_RESIZE` frame.
///
/// The resize is fire-and-forget on the per-actor mpsc channel — same
/// primitive `handle_viewport_resize` and `handle_terminal_resize` use.
/// We `try_send` rather than `.await` so we can stay in a sync helper
/// (no impact on `handle_attach`'s lock ordering) and because the
/// resize channel is sized at `DEFAULT_INPUT_MAILBOX = 64`, which is
/// well above the worst-case number of panes per attach (1 today; would
/// stay << 64 even with multi-window sessions).
///
/// The `pane.dims` update is wrapped in `with_mut` once so the registry
/// stays consistent with what future `TERMINAL_SNAPSHOT` payloads will
/// report; the resize sends are emitted while holding the same lock,
/// matching `handle_viewport_resize`'s pattern (the actor's mailbox is
/// independent of the state lock).
pub(crate) fn apply_attach_viewport(
    state: &SharedState,
    client_id: ClientId,
    panes_to_snapshot: &[AttachSnapshotPane],
    viewport: phux_protocol::wire::frame::ViewportInfo,
) {
    let cols = viewport.cols;
    let rows = viewport.rows;
    if cols == 0 || rows == 0 {
        // SPEC §10.5: zero-dimension viewports are treated as no-ops
        // rather than kernel errors. Skip the resize entirely.
        return;
    }
    state.with_mut(|s| {
        // phux-nk07: this client now contributes its viewport to every pane
        // it just subscribed to; each pane's geometry is the window-size
        // policy applied across all subscribers (so a second, smaller client
        // attaching under `smallest` shrinks the grid rather than the
        // last-writer winning). `Manual` (or no usable viewport) skips the
        // resize, leaving the pane at its current size.
        s.set_client_viewport(client_id, viewport);
        for pane in panes_to_snapshot {
            let Some((cols, rows)) =
                s.resolve_terminal_geometry(pane.terminal_id, Some(viewport))
            else {
                continue;
            };
            if let Some(pane_entry) = s.registry_mut().terminal_mut(pane.terminal_id) {
                pane_entry.dims = (cols, rows);
            }
            // ATTACH-time resize: do NOT resync — the attach handshake
            // already sends an authoritative TERMINAL_SNAPSHOT, and a
            // resync broadcast here would race ahead of it (phux-8v1).
            // Pixel geometry rides along (most recent usable subscriber
            // report — normally the viewport recorded above).
            match pane.handle.resize.try_send(ResizeRequest {
                cols,
                rows,
                cell_px: s.resolve_terminal_cell_px(pane.terminal_id),
                resync_clients: false,
                resync_only: false,
            }) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    warn!(
                        terminal_id = ?pane.terminal_id,
                        cols,
                        rows,
                        "ATTACH viewport apply: pane resize mailbox full; dropping (next VIEWPORT_RESIZE will retry)",
                    );
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    debug!(
                        terminal_id = ?pane.terminal_id,
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

    fn tiny_staged_pane(pane: u32) -> Vec<FrameKind> {
        let terminal_id = phux_protocol::ids::TerminalId::local(pane + 1);
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
            let result = budget.append(&mut staged, &mut frames);
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
    fn aggregate_staging_charges_many_tiny_native_records_by_capacity() {
        const RECORDS: usize = 64;
        const RECORD_CAPACITY: usize = 1_024;
        let retained_per_pane = RECORDS * RECORD_CAPACITY;
        let mut budget = BootstrapStagingBudget::with_limits(retained_per_pane * 3, usize::MAX);
        let mut staged = Vec::new();

        for pane in 0..4_u32 {
            let terminal_id = phux_protocol::ids::TerminalId::local(pane + 1);
            let stream_id = StreamId::new(u64::from(pane) + 1).expect("stream id");
            let bootstrap_id = BootstrapId::new(u64::from(pane) + 1).expect("bootstrap id");
            let mut frames = Vec::new();
            frames.push(FrameKind::BootstrapBegin {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::NativeState {
                    codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2,
                },
                cols: 80,
                rows: 24,
                base_seq: 0,
            });
            let mut retained_bytes = 0_usize;
            for chunk_seq in 0..RECORDS {
                let mut record = Vec::with_capacity(RECORD_CAPACITY);
                record.push(b'x');
                retained_bytes += record.capacity();
                frames.push(FrameKind::BootstrapChunk {
                    terminal_id: terminal_id.clone(),
                    stream_id,
                    bootstrap_id,
                    chunk_seq: u32::try_from(chunk_seq).expect("chunk sequence"),
                    payload: bytes::Bytes::from(record),
                });
            }
            frames.push(FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: None,
            });
            let wire_bytes = frames
                .iter()
                .map(|frame| match frame {
                    FrameKind::BootstrapChunk { payload, .. } => payload.len(),
                    _ => 0,
                })
                .sum::<usize>();
            assert_eq!(retained_bytes, retained_per_pane);
            assert!(retained_bytes > wire_bytes);

            let result = budget.append_accounted(&mut staged, &mut frames, retained_bytes);
            assert_eq!(result.is_ok(), pane < 3);
        }
        assert_eq!(budget.staged_bytes, retained_per_pane * 3);
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
                phux_protocol::ids::TerminalId::local(pane + 1),
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
        let terminal_id = phux_protocol::ids::TerminalId::local(7);
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
                server.register_terminal_handle(terminal, bundle.handle.clone(), token);
            });
            actors.push(bundle.actor);
        }
        let client_id = state.with_mut(crate::state::ServerState::new_client_id);
        let (out_tx, _out_rx) = tokio::sync::mpsc::channel(crate::state::DEFAULT_CLIENT_MAILBOX);

        let (snapshot, _initial_client_id, bootstrapped, closed) = prepare_attach(
            &state,
            client_id,
            "working",
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
            .panes
            .iter()
            .find(|pane| pane.id == snapshot.focused_pane)
            .expect("seed remains in ATTACHED catalog")
            .id
            .clone();
        assert_eq!(snapshot.sessions.len(), 2, "whole session catalog survives");
        assert_eq!(snapshot.panes.len(), 4, "ATTACHED keeps every catalog pane");
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
        assert!(matches!(
            prepare_attach(
                &state,
                client_id,
                "bounded",
                &out_tx,
                ClientCapabilities::default(),
                BootstrapProfile::SynthesizedVtRaw,
                BootstrapLimits::default(),
            ),
            Err(crate::state::AttachError::ResourceLimit)
        ));
        assert!(!state.with(|server| server.attached().contains_key(&client_id)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn saturated_resync_mailbox_blocks_until_actor_accepts_request() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        tx.send(ResizeRequest {
            cols: 80,
            rows: 24,
            cell_px: None,
            resync_clients: false,
            resync_only: false,
        })
        .await
        .expect("occupy resize mailbox");

        let mut pending = Box::pin(enqueue_output_resync(&tx));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut pending)
                .await
                .is_err(),
            "lagged pump must not resume while the resync mailbox is full"
        );
        assert!(
            !rx.recv().await.expect("occupied request").resync_only,
            "first request is the existing mailbox occupant"
        );
        assert!(pending.await, "resync queues once capacity is available");
        let queued = rx.recv().await.expect("queued resync");
        assert!(queued.resync_only && queued.resync_clients);

        drop(rx);
        assert!(
            !enqueue_output_resync(&tx).await,
            "closed actor mailbox fails instead of resuming delta forwarding"
        );
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn native_attach_handle() -> (
        crate::terminal_actor::TerminalHandle,
        tokio::sync::mpsc::Receiver<crate::terminal_actor::ConsumerAttachRequest>,
        tokio::sync::mpsc::Receiver<crate::terminal_actor::NativeBootstrapRequest>,
        tokio::sync::mpsc::Receiver<crate::terminal_actor::NativePublicationRequest>,
    ) {
        use tokio::sync::{broadcast, mpsc, watch};

        let (output, _seed) = broadcast::channel(8);
        let (consumer_attach, consumer_attach_rx) = mpsc::channel(8);
        let (native_bootstrap, native_bootstrap_rx) = mpsc::channel(8);
        let (native_publication, native_publication_rx) = mpsc::channel(8);
        (
            crate::terminal_actor::TerminalHandle {
                input: mpsc::channel(8).0,
                encoded_input: mpsc::channel(8).0,
                input_snapshot: watch::channel(crate::input::InputEncoderSnapshot::default()).1,
                snapshot: mpsc::channel(8).0,
                native_bootstrap,
                native_publication,
                native_history: mpsc::channel(8).0,
                native_release: mpsc::channel(8).0,
                set_default_colors: mpsc::channel(8).0,
                screen: mpsc::channel(8).0,
                upgrade: mpsc::channel(8).0,
                pwd: mpsc::channel(8).0,
                output,
                resize: mpsc::channel(8).0,
                consumer_attach,
                consumer_detach: mpsc::channel(8).0,
                consumer_ack: mpsc::channel(8).0,
                subscribe_to_events: mpsc::channel(8).0,
                unsubscribe_from_events: mpsc::channel(8).0,
                control: mpsc::channel(8).0,
                cols: 80,
                rows: 24,
            },
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
                            codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2,
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

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn native_profile() -> BootstrapProfile {
        BootstrapProfile::NativeState {
            codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2,
            features: phux_protocol::caps::EngineFeatureSet::required_native(),
        }
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
                    let _ = s.register_terminal_handle(terminal, handle, CancellationToken::new());
                });
                let client_id = state.with_mut(crate::state::ServerState::new_client_id);
                let (out_tx, mut out_rx) =
                    tokio::sync::mpsc::channel(crate::state::DEFAULT_CLIENT_MAILBOX);
                let root_token = CancellationToken::new();
                let connection_token = CancellationToken::new();
                let mut output_pumps = JoinSet::new();

                let attach = handle_attach(
                    &state,
                    client_id,
                    41,
                    AttachTarget::ByName("fresh-failure".to_owned()),
                    phux_protocol::wire::frame::ViewportInfo::new(80, 24),
                    false,
                    0,
                    &out_tx,
                    ClientCapabilities::default(),
                    native_profile(),
                    BootstrapLimits::default(),
                    &root_token,
                    &mut output_pumps,
                    &connection_token,
                );
                let actor = answer_native_attach(
                    &mut consumer_attach_rx,
                    &mut native_bootstrap_rx,
                    &mut native_publication_rx,
                    false,
                );
                tokio::join!(attach, actor);

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
                    let _ = s.register_terminal_handle(terminal, handle, CancellationToken::new());
                });
                let client_id = state.with_mut(crate::state::ServerState::new_client_id);
                let (out_tx, mut out_rx) =
                    tokio::sync::mpsc::channel(crate::state::DEFAULT_CLIENT_MAILBOX);
                let root_token = CancellationToken::new();
                let connection_token = CancellationToken::new();
                let mut output_pumps = JoinSet::new();

                let first = handle_attach(
                    &state,
                    client_id,
                    51,
                    AttachTarget::ByName("replacement-failure".to_owned()),
                    phux_protocol::wire::frame::ViewportInfo::new(80, 24),
                    false,
                    0,
                    &out_tx,
                    ClientCapabilities::default(),
                    native_profile(),
                    BootstrapLimits::default(),
                    &root_token,
                    &mut output_pumps,
                    &connection_token,
                );
                tokio::join!(
                    first,
                    answer_native_attach(
                        &mut consumer_attach_rx,
                        &mut native_bootstrap_rx,
                        &mut native_publication_rx,
                        true,
                    )
                );
                for _ in 0..5 {
                    out_rx.recv().await.expect("initial attach publication");
                }
                assert!(state.with(|s| s.attached().contains_key(&client_id)));

                let replacement = handle_attach(
                    &state,
                    client_id,
                    52,
                    AttachTarget::ByName("replacement-failure".to_owned()),
                    phux_protocol::wire::frame::ViewportInfo::new(80, 24),
                    false,
                    0,
                    &out_tx,
                    ClientCapabilities::default(),
                    native_profile(),
                    BootstrapLimits::default(),
                    &root_token,
                    &mut output_pumps,
                    &connection_token,
                );
                tokio::join!(
                    replacement,
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
