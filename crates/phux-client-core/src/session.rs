//! Synchronous, transport-neutral session state machine.

use std::collections::{HashMap, HashSet};

#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::{AgentEvent, CloseReason, ControlAction, TombstoneReason};
use phux_protocol::{
    BootstrapId, BootstrapProfile, BootstrapStreamProfile, ResourceId, ResourceKind, StreamId,
};

use agent_stream::{AgentLog, AgentPublication, AgentRecordError, AgentSessionState, AgentStream};

use crate::engine::{
    BootstrapProgress, CanonicalGeometry, DocumentPoint, DocumentSpace, EngineAdapter,
    EngineDamage, EngineDocumentAdapter, EngineDocumentSelection, EngineEffect, EngineEffectBuffer,
    EngineHistoryProjection, EngineJob, EnginePresentationAdapter, EngineProjectionOrigin,
    EngineSearchMatch, EngineStatus,
};
use crate::history::{
    DocumentAnchorId, HistoryCache, HistoryCacheConfig, HistoryCacheError, HistoryCursor,
    HistoryLoadState, HistoryPageCheck, HistoryStatus, ViewportAnchor,
};

/// Hard connection-wide ceiling for bootstrap bytes accepted before READY.
const MAX_BOOTSTRAP_STAGING_BYTES: usize = 64 * 1024 * 1024;
/// Hard connection-wide ceiling for bootstrap chunks accepted before READY.
const MAX_BOOTSTRAP_STAGING_CHUNKS: usize = 4_096;
/// A peer must complete one bootstrap generation within this interval.
const MAX_BOOTSTRAP_STAGING_LIFETIME_MS: u64 = 30_000;

mod attach;
mod bootstrap;
mod document;
mod effects;
mod error;
mod history_pages;
mod kernel_input;
mod lifecycle;
mod output;
mod queries;
mod replicas;
mod update;

pub use effects::{
    EffectBuffer, KernelDamage, KernelDamageKind, KernelEffect, KernelJob, KernelSend,
    KernelStatus, TombstoneRecord,
};
pub use error::{HistoryRejectionReason, HistoryUnavailableReason, KernelError};
pub use kernel_input::{
    AgentSessionDeclaration, InputBlockReason, InputEligibility, KernelAction, KernelInput,
};
pub use replicas::{
    ClosedReplica, LoadedHistorySearch, PublishedReplica, ReplicaKey, StagingReplica,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GenerationId {
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
}

struct Replica<R> {
    key: ReplicaKey,
    geometry: CanonicalGeometry,
    last_seq: u64,
    next_seq: Option<u64>,
    engine: R,
    history: HistoryCache,
}

struct Staging<R> {
    key: ReplicaKey,
    geometry: CanonicalGeometry,
    base_seq: u64,
    next_chunk_seq: Option<u32>,
    engine_ready: bool,
    protocol_ready: bool,
    history_cursor: Option<HistoryCursor>,
    engine: R,
    pending_effects: Vec<EngineEffect>,
    accepted_bytes: usize,
    accepted_chunks: usize,
    started_at_ms: u64,
}

const fn staging_expired<R>(staging: &Staging<R>, now_ms: u64) -> bool {
    now_ms.saturating_sub(staging.started_at_ms) > MAX_BOOTSTRAP_STAGING_LIFETIME_MS
}

fn retained_engine_effect_bytes(capacity: usize, effects: &[EngineEffect]) -> usize {
    let allocation = capacity.saturating_mul(std::mem::size_of::<EngineEffect>());
    effects.iter().fold(allocation, |total, effect| {
        let owned = match effect {
            EngineEffect::Status(EngineStatus::Title(title)) => title.capacity(),
            EngineEffect::Status(EngineStatus::ClipboardWrite(text)) => text.0.capacity(),
            EngineEffect::Damage(_)
            | EngineEffect::Status(EngineStatus::Bell)
            | EngineEffect::Job(_) => 0,
        };
        total.saturating_add(owned)
    })
}

struct TerminalState<R> {
    published: Option<Replica<R>>,
    staging: Option<Staging<R>>,
    retired: HashMap<GenerationId, TombstoneRecord>,
}

impl<R> Default for TerminalState<R> {
    fn default() -> Self {
        Self {
            published: None,
            staging: None,
            retired: HashMap::new(),
        }
    }
}

/// Why one history page did not reach the cache. `error` is `None` when
/// the page was dropped without a protocol violation.
struct HistoryPageRejection<E> {
    /// What the frontend is told about progressive history.
    reason: HistoryUnavailableReason,
    /// The error the caller returns, when the page was a violation.
    error: Option<KernelError<E>>,
}

impl<E> HistoryPageRejection<E> {
    /// The page violated the protocol or could not be decoded.
    const fn codec_failure(error: KernelError<E>) -> Self {
        Self {
            reason: HistoryUnavailableReason::CodecFailure,
            error: Some(error),
        }
    }
}

/// One inbound progressive-history page, as it arrives on the wire.
struct HistoryPageInput<'a> {
    /// Target terminal.
    terminal_id: &'a ResourceId,
    /// Published replica generation the page belongs to.
    generation: GenerationId,
    /// Non-zero cursor-local page sequence.
    page_seq: u64,
    /// Declared decoded row contribution.
    rows: u32,
    /// Borrowed opaque selected-codec history bytes.
    payload: &'a [u8],
    /// Opaque cursor consumed by this response.
    cursor: &'a [u8],
    /// Opaque cursor for the next older page, if any.
    next_cursor: Option<&'a [u8]>,
}

struct AttachParticipant {
    terminal_id: ResourceId,
    resolved: bool,
    pending_removal: bool,
}

/// Authoritative identity and cursor of a committed `AgentSession` generation.
///
/// Staged records never change this value. It survives same-incarnation
/// transport loss and is forgotten on detach, resource closure, or replacement
/// of the server incarnation. A publication's `stream` field is this watermark
/// after the commit, including an empty resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentStreamInfo {
    /// The committed logical stream.
    pub stream_id: StreamId,
    /// The committed bootstrap generation.
    pub bootstrap_id: BootstrapId,
    /// Last fully validated and committed record sequence.
    pub last_seq: u64,
}

/// Borrowed view of one `AgentSession` resource for frontend projection.
#[derive(Debug, Clone, Copy)]
pub struct AgentSessionView<'a> {
    /// The `AgentSession` resource.
    pub terminal_id: &'a ResourceId,
    /// Its Terminal-kind parent, when declared.
    pub parent: Option<&'a ResourceId>,
    /// Identity and lifecycle folded from the declaration and every record.
    pub state: &'a AgentSessionState,
    /// The retained record log, oldest first.
    pub log: &'a AgentLog,
    /// Whether a stream generation is published (records may still arrive
    /// before the first one is).
    pub published: bool,
}

struct AttachState {
    attach_id: u32,
    released: bool,
    terminals: Vec<AttachParticipant>,
}

/// Frontend-neutral synchronous client session kernel.
pub struct SessionKernel<E: EngineAdapter> {
    adapter: E,
    selected_profile: BootstrapProfile,
    terminals: HashMap<ResourceId, TerminalState<E::Replica>>,
    /// Final published generations awaiting optional frontend transfer.
    closed_replicas: HashMap<ResourceId, ClosedReplica<E>>,
    /// Terminals whose next close should retain the published generation.
    retain_replica_on_close: HashSet<ResourceId>,
    /// Kind per resource; survives closure, released with the resource.
    kinds: HashMap<ResourceId, ResourceKind>,
    /// `AgentSession` resources: record streams without replicas.
    agents: HashMap<ResourceId, AgentStream>,
    /// Last controller views for closed native sources, readable but unavailable.
    closed_native_views: HashMap<ResourceId, phux_protocol::native_agent::NativeView>,
    closed: HashSet<ResourceId>,
    /// Terminals whose [`KernelStatus::Exited`] was already emitted: a retained
    /// resource reports its exit via `TerminalControl` and again at purge.
    exit_reported: HashSet<ResourceId>,
    attach: Option<AttachState>,
    engine_effects: EngineEffectBuffer,
    history_config: HistoryCacheConfig,
    /// Per-terminal echo arming for `crate::perf::ECHO_RTT`.
    perf_echo: crate::perf::EchoProbe,
    /// Caller-supplied monotonic time, selected by [`Self::update_at`].
    injected_now_ms: Option<u64>,
    /// Native default clock. Web hosts inject their own monotonic time because
    /// `std::time::Instant::now` panics on `wasm32-unknown-unknown`.
    #[cfg(not(target_arch = "wasm32"))]
    monotonic_origin: Instant,
}
impl<E: EngineAdapter> std::fmt::Debug for SessionKernel<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionKernel")
            .field("selected_profile", &self.selected_profile)
            .field("terminal_count", &self.terminals.len())
            .field("closed_replica_count", &self.closed_replicas.len())
            .field(
                "retain_replica_on_close_count",
                &self.retain_replica_on_close.len(),
            )
            .field("agent_session_count", &self.agents.len())
            .field("closed_terminal_count", &self.closed.len())
            .field("exit_reported_count", &self.exit_reported.len())
            .field(
                "active_attach_id",
                &self.attach.as_ref().map(|attach| attach.attach_id),
            )
            .finish_non_exhaustive()
    }
}

impl<E: EngineAdapter> SessionKernel<E> {
    /// Construct a kernel bound to the exact profile selected by `HELLO_OK`.
    #[must_use]
    pub fn new(adapter: E, selected_profile: BootstrapProfile) -> Self {
        Self::with_history_config(adapter, selected_profile, HistoryCacheConfig::default())
    }

    /// Construct a kernel with explicit client-local history bounds.
    #[must_use]
    pub fn with_history_config(
        adapter: E,
        selected_profile: BootstrapProfile,
        history_config: HistoryCacheConfig,
    ) -> Self {
        let history_config = history_config.normalized();
        Self {
            adapter,
            selected_profile,
            terminals: HashMap::new(),
            closed_replicas: HashMap::new(),
            retain_replica_on_close: HashSet::new(),
            kinds: HashMap::new(),
            agents: HashMap::new(),
            closed_native_views: HashMap::new(),
            closed: HashSet::new(),
            exit_reported: HashSet::new(),
            attach: None,
            engine_effects: EngineEffectBuffer::new(),
            history_config,
            perf_echo: crate::perf::EchoProbe::default(),
            injected_now_ms: None,
            #[cfg(not(target_arch = "wasm32"))]
            monotonic_origin: Instant::now(),
        }
    }

    /// Apply one input at a caller-supplied monotonic millisecond timestamp.
    ///
    /// This is the portable clock seam for browser and deterministic hosts.
    /// Once selected, callers must keep supplying timestamps; values that move
    /// backward are clamped to the last observed value. Native hosts may keep
    /// using [`Self::update`], which uses a process-local monotonic clock.
    pub fn update_at(
        &mut self,
        now_ms: u64,
        input: KernelInput<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.injected_now_ms = Some(
            self.injected_now_ms
                .map_or(now_ms, |previous| previous.max(now_ms)),
        );
        self.update(input, effects)
    }

    /// Retire every incomplete bootstrap older than the hard staging lifetime.
    ///
    /// A synchronous kernel cannot observe a silent peer by itself. Drivers
    /// should call this from their existing connection timer using the same
    /// monotonic millisecond source passed to [`Self::update_at`].
    pub fn expire_bootstrap_staging(&mut self, now_ms: u64, effects: &mut EffectBuffer) -> usize {
        self.injected_now_ms = Some(
            self.injected_now_ms
                .map_or(now_ms, |previous| previous.max(now_ms)),
        );
        effects.clear();
        let now_ms = self.monotonic_now_ms();
        let expired: Vec<_> = self
            .terminals
            .iter()
            .filter_map(|(terminal_id, state)| {
                state.staging.as_ref().and_then(|staging| {
                    staging_expired(staging, now_ms)
                        .then(|| (terminal_id.clone(), generation_of(&staging.key)))
                })
            })
            .chain(self.agents.iter().filter_map(|(terminal_id, stream)| {
                stream
                    .expired_staging(now_ms)
                    .map(|generation| (terminal_id.clone(), generation))
            }))
            .collect();
        for (terminal_id, generation) in &expired {
            self.retire_staging_limit(terminal_id, *generation, effects);
        }
        expired.len()
    }

    fn monotonic_now_ms(&self) -> u64 {
        if let Some(now_ms) = self.injected_now_ms {
            return now_ms;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            u64::try_from(self.monotonic_origin.elapsed().as_millis()).unwrap_or(u64::MAX)
        }
        #[cfg(target_arch = "wasm32")]
        {
            0
        }
    }

    /// The immutable profile selected for this connection.
    #[must_use]
    pub const fn selected_profile(&self) -> BootstrapProfile {
        self.selected_profile
    }

    /// Borrow the engine adapter for diagnostics or frontend-owned projection helpers.
    #[must_use]
    pub const fn adapter(&self) -> &E {
        &self.adapter
    }

    /// Mutably borrow the engine adapter outside an update.
    #[must_use]
    pub const fn adapter_mut(&mut self) -> &mut E {
        &mut self.adapter
    }
}

const fn progressive_finish_missing(
    profile: BootstrapStreamProfile,
    progress: BootstrapProgress,
    has_history_cursor: bool,
) -> bool {
    matches!(
        profile,
        BootstrapStreamProfile::NativeState {
            codec: phux_protocol::EngineCodec::LibghosttySnapshotV1,
        }
    ) && matches!(progress, BootstrapProgress::Ready)
        && !has_history_cursor
}

const fn generation_of(key: &ReplicaKey) -> GenerationId {
    GenerationId {
        stream_id: key.stream_id,
        bootstrap_id: key.bootstrap_id,
    }
}

fn mismatch_error<E>(terminal_id: &ResourceId, generation: GenerationId) -> KernelError<E> {
    KernelError::GenerationMismatch {
        terminal_id: terminal_id.clone(),
        stream_id: generation.stream_id,
        bootstrap_id: generation.bootstrap_id,
    }
}

fn retired_error<E>(terminal_id: &ResourceId, generation: GenerationId) -> KernelError<E> {
    KernelError::RetiredGeneration {
        terminal_id: terminal_id.clone(),
        stream_id: generation.stream_id,
        bootstrap_id: generation.bootstrap_id,
    }
}

/// Whether a stream profile names the `AgentSession` record codec. Such a
/// stream is legal under every connection profile: it carries no engine
/// checkpoint and no VT, so profile negotiation does not constrain it.
const fn is_agent_profile(profile: BootstrapStreamProfile) -> bool {
    matches!(
        profile,
        BootstrapStreamProfile::AgentEventsJsonlV1
            | BootstrapStreamProfile::AgentEventsJsonlResumeV1 { .. }
    )
}

const fn profile_matches(selected: BootstrapProfile, incoming: BootstrapStreamProfile) -> bool {
    match (selected, incoming) {
        (
            BootstrapProfile::NativeState {
                codec: selected, ..
            },
            BootstrapStreamProfile::NativeState { codec: incoming },
        ) => selected as u8 == incoming as u8,
        (BootstrapProfile::SynthesizedVtRaw, BootstrapStreamProfile::SynthesizedVtRaw)
        | (
            BootstrapProfile::SynthesizedVtStateSync,
            BootstrapStreamProfile::SynthesizedVtStateSync,
        ) => true,
        _ => false,
    }
}

pub mod agent_stream;
pub mod agent_transcript;

#[cfg(test)]
mod agent_gap_tests;

#[cfg(test)]
mod kernel_rig;

#[cfg(test)]
mod property_tests;

#[cfg(test)]
mod tests;
