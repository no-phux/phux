//! Bootstrap adaptation, staging budgets, and native checkpoint publication.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;

/// Adapt a chunk to a client's capabilities without copying when neither
/// the capabilities nor the bytes require rewriting.
pub(crate) fn downsample_for_caps(
    presentation: &phux_terminal_api::Presentation,
    bytes: &bytes::Bytes,
    caps: phux_protocol::ClientCapabilities,
) -> bytes::Bytes {
    (presentation.adapt_bytes)(bytes, caps)
}

/// Preserve native live bytes exactly; only synthesized profiles may adapt
/// presentation capabilities.
pub(crate) fn live_bytes_for_profile(
    presentation: &phux_terminal_api::Presentation,
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
        downsample_for_caps(presentation, bytes, caps)
    }
}

pub(crate) fn bootstrap_source_ceiling(
    presentation: &phux_terminal_api::Presentation,
    remaining_bytes: usize,
    caps: phux_protocol::ClientCapabilities,
) -> usize {
    (presentation.source_ceiling)(remaining_bytes, caps)
}

pub(crate) use phux_terminal_api::AdaptedBootstrap;

pub(crate) fn adapt_bootstrap_snapshot(
    presentation: &phux_terminal_api::Presentation,
    snapshot: phux_terminal_api::grid::SnapshotBytes,
    caps: phux_protocol::ClientCapabilities,
    peak_budget: usize,
) -> Result<AdaptedBootstrap, ()> {
    (presentation.adapt_snapshot)(snapshot, caps, peak_budget)
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

pub(in crate::runtime) use crate::runtime::pump::initial_bootstrap_id;

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
    pub(crate) live: Option<tokio::sync::broadcast::Receiver<ResourceOutput>>,
}

pub(super) struct SnapshotGate {
    pub(super) terminal_id: ResourceId,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) wire_terminal_id: phux_protocol::ids::ResourceId,
    /// Terminal facet, for the native publication fence.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) terminal: phux_terminal_api::actor::TerminalHandle,
    pub(super) gate: oneshot::Sender<OutputPumpStart>,
    pub(super) cut: Option<u64>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) native_cursor: Option<phux_terminal_api::native_state::OpaqueHistoryCursor>,
}

// Compatibility re-exports for Terminal bootstrap helpers and their tests.
#[cfg(test)]
pub(crate) use crate::runtime::session_attach::staging::MAX_AGGREGATE_BOOTSTRAP_PANES;
pub(crate) use crate::runtime::session_attach::staging::{
    MAX_STAGED_BOOTSTRAP_BYTES, MAX_STAGED_BOOTSTRAP_FRAMES,
};

pub(crate) use crate::runtime::session_attach::staging::BootstrapStagingBudget;

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
    Refused(phux_terminal_api::native_state::NativeStateError),
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
    terminal: &phux_terminal_api::actor::TerminalHandle,
    make: impl FnOnce(
        oneshot::Sender<
            Result<
                phux_terminal_api::actor::NativeBootstrapReply,
                phux_terminal_api::native_state::NativeStateError,
            >,
        >,
    ) -> phux_terminal_api::actor::NativeBootstrapRequest,
) -> Result<phux_terminal_api::actor::NativeBootstrapReply, NativeRequestFailure> {
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
    reply: phux_terminal_api::actor::NativeBootstrapReply,
) -> Result<(u64, phux_terminal_api::native_state::OpaqueHistoryCursor), ()> {
    let cut = reply.base_seq;
    let cursor = reply.publication_cursor;
    for frame in reply.frames {
        out_tx.send(Outbound::Frame(frame)).await.map_err(|_| ())?;
    }
    Ok((cut, cursor))
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(crate) async fn activate_native_publication(
    handle: &phux_terminal_api::actor::TerminalHandle,
    owner: u64,
    terminal_id: phux_protocol::ids::ResourceId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    cursor: phux_terminal_api::native_state::OpaqueHistoryCursor,
) -> Result<phux_terminal_api::actor::NativePublicationReply, NativeRequestFailure> {
    let (reply, publication) = oneshot::channel();
    handle
        .native_publication
        .send(phux_terminal_api::actor::NativePublicationRequest {
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
