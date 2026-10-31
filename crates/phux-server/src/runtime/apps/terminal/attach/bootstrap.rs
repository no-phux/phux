//! Bootstrap adaptation, staging budgets, and native checkpoint publication.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;

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
    pub(super) retained_bytes: usize,
    pub(super) peak_bytes: usize,
}

pub(crate) fn adapt_bootstrap_snapshot(
    snapshot: phux_terminal::grid::SnapshotBytes,
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

pub(super) const fn initial_bootstrap_id() -> BootstrapId {
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

pub(super) struct SnapshotGate {
    pub(super) terminal_id: ResourceId,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) wire_terminal_id: phux_protocol::ids::ResourceId,
    /// Terminal facet, for the native publication fence.
    pub(super) terminal: phux_terminal::actor::TerminalHandle,
    pub(super) gate: oneshot::Sender<OutputPumpStart>,
    pub(super) cut: Option<u64>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) native_cursor: Option<phux_terminal::native_state::OpaqueHistoryCursor>,
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
pub(super) struct BootstrapStagingBudget {
    pub(super) max_bytes: usize,
    pub(super) max_frames: usize,
    pub(super) staged_bytes: usize,
    pub(super) staged_frames: usize,
}

impl BootstrapStagingBudget {
    pub(super) const fn with_limits(max_bytes: usize, max_frames: usize) -> Self {
        Self {
            max_bytes,
            max_frames,
            staged_bytes: 0,
            staged_frames: 0,
        }
    }

    pub(super) const fn remaining_bytes(&self) -> usize {
        self.max_bytes.saturating_sub(self.staged_bytes)
    }

    pub(super) const fn remaining_frames(&self) -> usize {
        self.max_frames.saturating_sub(self.staged_frames)
    }

    pub(super) fn append_accounted(
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
pub(super) const fn publishes_native_checkpoints(profile: BootstrapStreamProfile) -> bool {
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
    Refused(phux_terminal::native_state::NativeStateError),
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
impl NativeRequestFailure {
    /// The pane's actor is gone: the pane exited, and `RESOURCE_CLOSED` is
    /// its end. No generation of it can be published, and that is no fault
    /// of the connection.
    pub(super) const fn actor_gone(&self) -> bool {
        matches!(self, Self::Unsent | Self::Dropped)
    }
}

/// Send the native checkpoint request `make` builds and await its reply.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(in crate::runtime) async fn request_native_checkpoint(
    terminal: &phux_terminal::actor::TerminalHandle,
    make: impl FnOnce(
        oneshot::Sender<
            Result<
                phux_terminal::actor::NativeBootstrapReply,
                phux_terminal::native_state::NativeStateError,
            >,
        >,
    ) -> phux_terminal::actor::NativeBootstrapRequest,
) -> Result<phux_terminal::actor::NativeBootstrapReply, NativeRequestFailure> {
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
    reply: phux_terminal::actor::NativeBootstrapReply,
) -> Result<(u64, phux_terminal::native_state::OpaqueHistoryCursor), ()> {
    let cut = reply.base_seq;
    let cursor = reply.publication_cursor;
    for frame in reply.frames {
        out_tx.send(Outbound::Frame(frame)).await.map_err(|_| ())?;
    }
    Ok((cut, cursor))
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(crate) async fn activate_native_publication(
    handle: &phux_terminal::actor::TerminalHandle,
    owner: u64,
    terminal_id: phux_protocol::ids::ResourceId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    cursor: phux_terminal::native_state::OpaqueHistoryCursor,
) -> Result<phux_terminal::actor::NativePublicationReply, NativeRequestFailure> {
    let (reply, publication) = oneshot::channel();
    handle
        .native_publication
        .send(phux_terminal::actor::NativePublicationRequest {
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
