//! Synchronous, transport-neutral session state machine.

use std::collections::{HashMap, HashSet};

use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::TombstoneReason;
use phux_protocol::{BootstrapId, BootstrapProfile, BootstrapStreamProfile, StreamId, TerminalId};

use crate::engine::{
    BootstrapProgress, CanonicalGeometry, DocumentPoint, DocumentSpace, EngineAdapter,
    EngineDamage, EngineDocumentAdapter, EngineDocumentSelection, EngineEffect, EngineEffectBuffer,
    EngineHistoryProjection, EngineJob, EngineProjectionOrigin, EngineSearchMatch, EngineSend,
    EngineStatus,
};
use crate::history::{
    DocumentAnchorId, HistoryCache, HistoryCacheConfig, HistoryCacheError, HistoryCursor,
    HistoryLoadState, HistoryPageCheck, HistoryStatus, ViewportAnchor,
};

/// Why one history cursor became unavailable without retiring live state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryUnavailableReason {
    /// Cursor checkpoint no longer matches current engine history.
    Stale,
    /// Cursor boundary was pruned.
    Pruned,
    /// Terminal reset invalidated the captured history.
    Reset,
    /// Resize or reflow invalidated the captured history.
    Resize,
    /// Server lease expired.
    Expired,
    /// Server released the lease.
    Released,
    /// Server history retention limit removed the boundary.
    Limit,
    /// The selected codec could not import the page.
    CodecFailure,
}

/// Why a bounded request was not consumed or invalidated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryRejectionReason {
    /// Either requested budget was zero.
    ZeroLimit,
    /// The next indivisible engine unit exceeds a requested budget.
    TooSmall,
    /// The engine is temporarily serving another import/export transaction.
    Busy,
}
/// Exact identity of one terminal replica generation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReplicaKey {
    /// Protocol terminal identifier.
    pub terminal_id: TerminalId,
    /// Connection-scoped logical subscription.
    pub stream_id: StreamId,
    /// Replaceable replica generation.
    pub bootstrap_id: BootstrapId,
    /// Explicit profile repeated by `BOOTSTRAP_BEGIN`.
    pub profile: BootstrapStreamProfile,
}

/// A normalized borrowed input to [`SessionKernel::update`].
#[derive(Debug)]
pub enum KernelInput<'a> {
    /// Start one aggregate attach and its first-damage barrier.
    AttachStarted {
        /// Client-chosen attach correlation identifier.
        attach_id: u32,
        /// Complete ordered terminal inventory for this attach.
        terminals: &'a [TerminalId],
    },
    /// Release the aggregate first-damage barrier.
    AttachReady {
        /// Correlation identifier from `ATTACHED`.
        attach_id: u32,
    },
    /// Begin staging a replacement replica.
    BootstrapBegin {
        /// Target terminal.
        terminal_id: &'a TerminalId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replacement generation.
        bootstrap_id: BootstrapId,
        /// Explicit stream-local selected profile.
        profile: BootstrapStreamProfile,
        /// Authoritative live geometry.
        geometry: CanonicalGeometry,
        /// Actor cut sequence.
        base_seq: u64,
    },
    /// Apply one borrowed bootstrap fragment.
    BootstrapChunk {
        /// Target terminal.
        terminal_id: &'a TerminalId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replica generation.
        bootstrap_id: BootstrapId,
        /// Zero-based contiguous chunk sequence.
        chunk_seq: u32,
        /// Borrowed opaque engine or synthesized-VT bytes.
        payload: &'a [u8],
    },
    /// Mark the protocol half of the dual READY fence.
    BootstrapReady {
        /// Target terminal.
        terminal_id: &'a TerminalId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replica generation.
        bootstrap_id: BootstrapId,
        /// Opaque newest-page cursor, if the READY cut retained history.
        history_cursor: Option<&'a [u8]>,
    },
    /// Apply one borrowed post-publication native history page.
    HistoryPage {
        /// Target terminal.
        terminal_id: &'a TerminalId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Published replica generation.
        bootstrap_id: BootstrapId,
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
    },
    /// Invalidate only one progressive-history cursor.
    HistoryTombstone {
        /// Target terminal.
        terminal_id: &'a TerminalId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Published replica generation.
        bootstrap_id: BootstrapId,
        /// Exact outstanding cursor being invalidated.
        cursor: &'a [u8],
        /// Engine/server invalidation cause.
        reason: HistoryUnavailableReason,
    },
    /// Reject one request without advancing or invalidating its cursor.
    HistoryRejected {
        /// Target terminal.
        terminal_id: &'a TerminalId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Published replica generation.
        bootstrap_id: BootstrapId,
        /// Exact outstanding cursor not consumed.
        cursor: &'a [u8],
        /// Non-advancing rejection class.
        reason: HistoryRejectionReason,
        /// Minimum byte budget required, if known.
        required_bytes: u32,
        /// Minimum row budget required, if known.
        required_rows: u32,
    },
    /// Apply one borrowed live output fragment.
    TerminalOutput {
        /// Target terminal.
        terminal_id: &'a TerminalId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Published replica generation.
        bootstrap_id: BootstrapId,
        /// Exact live sequence.
        seq: u64,
        /// Borrowed raw or profile-selected live bytes.
        payload: &'a [u8],
    },
    /// Permanently retire one replica generation.
    Tombstone {
        /// Target terminal.
        terminal_id: &'a TerminalId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Invalidated generation.
        bootstrap_id: BootstrapId,
        /// Protocol tombstone reason.
        reason: TombstoneReason,
        /// Highest live sequence known valid for the generation.
        last_valid_seq: u64,
    },
    /// Resolve an attach participant by terminal closure.
    TerminalClosed {
        /// Closed terminal.
        terminal_id: &'a TerminalId,
    },
    /// Apply one explicit-terminal user action.
    Action(KernelAction<'a>),
}

/// A normalized borrowed user action.
#[derive(Debug)]
pub enum KernelAction<'a> {
    /// Send one structured input atom to an explicitly eligible terminal.
    Input {
        /// Target terminal selected by the frontend.
        terminal_id: &'a TerminalId,
        /// Borrowed protocol input atom.
        event: &'a InputEvent,
    },
}

/// Why a terminal cannot currently receive user input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputBlockReason {
    /// The terminal is not part of the current kernel state.
    UnknownTerminal,
    /// No dual-READY replica is published yet.
    AwaitingReplica,
    /// The aggregate attach barrier has not been released.
    AwaitingAttachReady,
    /// The last published view is frozen pending a replacement bootstrap.
    FrozenReplica,
    /// The terminal was permanently closed.
    Closed,
}

/// Explicit per-terminal input eligibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEligibility {
    /// Input targets this published generation.
    Eligible {
        /// Published logical subscription.
        stream_id: StreamId,
        /// Published replica generation.
        bootstrap_id: BootstrapId,
    },
    /// Input is currently rejected for the stated reason.
    Ineligible(InputBlockReason),
}

/// A typed transport send requested by the kernel.
#[derive(Debug, Clone, PartialEq)]
pub enum KernelSend {
    /// Send one structured protocol input atom.
    Input {
        /// Explicit target terminal.
        terminal_id: TerminalId,
        /// Owned atom ready for transport framing outside the kernel.
        event: InputEvent,
    },
    /// Write a terminal-engine response to the owning PTY.
    PtyWrite {
        /// Terminal whose engine generated the response.
        terminal_id: TerminalId,
        /// One response payload; batching and encoding remain outside.
        bytes: Vec<u8>,
    },
    /// Acknowledge one successfully applied `StateSync` live frame.
    FrameAck {
        /// Terminal whose reference advanced.
        terminal_id: TerminalId,
        /// Logical `StateSync` subscription.
        stream_id: StreamId,
        /// Published replica generation.
        bootstrap_id: BootstrapId,
        /// Highest contiguous sequence applied.
        seq: u64,
    },
    /// Request one bounded newest-to-oldest history page.
    HistoryRequest {
        /// Exact published generation.
        key: ReplicaKey,
        /// Opaque engine cursor to consume.
        cursor: Vec<u8>,
        /// Negotiated response byte bound.
        max_bytes: u32,
        /// Negotiated response row bound.
        max_rows: u32,
    },
}

/// Frontend-neutral render invalidation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelDamage {
    /// Damaged terminal.
    pub terminal_id: TerminalId,
    /// Kind of terminal damage.
    pub kind: KernelDamageKind,
}

/// Kind of frontend-neutral terminal damage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelDamageKind {
    /// Reproject the complete canonical live grid.
    Full,
    /// Reproject an inclusive canonical row range.
    Rows {
        /// First damaged row.
        first: u16,
        /// Last damaged row.
        last: u16,
    },
    /// Remove the terminal's prior projection.
    Removed,
}

/// Frontend-neutral session or engine status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelStatus {
    /// Status emitted by a terminal engine.
    Engine {
        /// Exact published generation reporting status.
        key: ReplicaKey,
        /// Engine status payload.
        status: EngineStatus,
    },
    /// The named generation was invalidated and requires a fresh bootstrap.
    ResyncRequired {
        /// Terminal requiring replacement.
        terminal_id: TerminalId,
        /// Invalid logical subscription.
        stream_id: StreamId,
        /// Invalid replica generation.
        bootstrap_id: BootstrapId,
        /// Protocol reason recorded in the local tombstone.
        reason: TombstoneReason,
    },
    /// Progressive history loading and cache status.
    History {
        /// Exact published generation.
        key: ReplicaKey,
        /// Frontend presentation state.
        status: HistoryStatus,
    },
    /// One progressive-history boundary became unavailable; live state remains valid.
    HistoryUnavailable {
        /// Exact published generation.
        key: ReplicaKey,
        /// Engine/server reason for ending this cursor chain.
        reason: HistoryUnavailableReason,
    },
}

/// Cooperative work handed to the host executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelJob {
    /// Exact published generation requesting work.
    pub key: ReplicaKey,
    /// Engine-owned-thread work request.
    pub job: EngineJob,
}

/// One declarative effect produced by a synchronous kernel update.
#[derive(Debug, Clone, PartialEq)]
pub enum KernelEffect {
    /// A typed send for the transport or PTY executor.
    Send(KernelSend),
    /// Render damage for frontend projection.
    Damage(KernelDamage),
    /// Status for optional frontend presentation.
    Status(KernelStatus),
    /// Cooperative work for a host scheduler.
    Job(KernelJob),
}

/// Reusable high-water-mark effect queue.
///
/// [`SessionKernel::update`] clears the logical contents before each update but
/// retains the vector allocation. Callers must execute or copy effects before
/// the next update.
#[derive(Debug, Default)]
pub struct EffectBuffer {
    effects: Vec<KernelEffect>,
}

impl EffectBuffer {
    /// Construct an empty allocation-free buffer.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            effects: Vec::new(),
        }
    }

    /// Construct an empty buffer with reusable capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            effects: Vec::with_capacity(capacity),
        }
    }

    /// Number of effects produced by the last update.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.effects.len()
    }

    /// Whether the last update produced no effects.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }

    /// Current reusable allocation capacity.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.effects.capacity()
    }

    /// Effects produced by the last update.
    #[must_use]
    pub fn as_slice(&self) -> &[KernelEffect] {
        &self.effects
    }

    /// Remove all logical effects while retaining capacity.
    pub fn clear(&mut self) {
        self.effects.clear();
    }

    /// Take every pending effect, leaving this buffer logically empty.
    ///
    /// Frontends that process effects outside [`SessionKernel::update`] must
    /// consume rather than repeatedly borrow the previous update's effects.
    pub fn take(&mut self) -> Vec<KernelEffect> {
        std::mem::take(&mut self.effects)
    }

    /// Return an emptied allocation obtained from [`Self::take`] for reuse.
    pub fn restore_allocation(&mut self, mut effects: Vec<KernelEffect>) {
        effects.clear();
        self.effects = effects;
    }

    fn push(&mut self, effect: KernelEffect) {
        self.effects.push(effect);
    }
}

/// Metadata retained for a permanently invalid generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TombstoneRecord {
    /// Why continuity ended.
    pub reason: TombstoneReason,
    /// Highest live sequence known valid.
    pub last_valid_seq: u64,
}

/// Borrowed access to an atomically published engine replica.
pub struct PublishedReplica<'a, E: EngineAdapter> {
    key: &'a ReplicaKey,
    geometry: CanonicalGeometry,
    last_seq: u64,
    engine: &'a E::Replica,
    history: &'a HistoryCache,
}
impl<E: EngineAdapter> std::fmt::Debug for PublishedReplica<'_, E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PublishedReplica")
            .field("key", &self.key)
            .field("geometry", &self.geometry)
            .field("last_seq", &self.last_seq)
            .finish_non_exhaustive()
    }
}

impl<E: EngineAdapter> PublishedReplica<'_, E> {
    /// Exact protocol identity and profile.
    #[must_use]
    pub const fn key(&self) -> &ReplicaKey {
        self.key
    }

    /// Canonical live PTY geometry.
    #[must_use]
    pub const fn geometry(&self) -> CanonicalGeometry {
        self.geometry
    }

    /// Highest contiguous live sequence applied to this replica.
    #[must_use]
    pub const fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// Borrow this client's generation-scoped progressive history.
    #[must_use]
    pub const fn history(&self) -> &HistoryCache {
        self.history
    }

    /// Borrow the adapter-owned live state for frontend projection.
    #[must_use]
    pub const fn engine(&self) -> &E::Replica {
        self.engine
    }
}

/// Borrowed staging state, exposed for diagnostics without publication.
pub struct StagingReplica<'a, E: EngineAdapter> {
    key: &'a ReplicaKey,
    geometry: CanonicalGeometry,
    engine_ready: bool,
    protocol_ready: bool,
    engine: &'a E::Replica,
}
impl<E: EngineAdapter> std::fmt::Debug for StagingReplica<'_, E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StagingReplica")
            .field("key", &self.key)
            .field("geometry", &self.geometry)
            .field("engine_ready", &self.engine_ready)
            .field("protocol_ready", &self.protocol_ready)
            .finish_non_exhaustive()
    }
}

impl<E: EngineAdapter> StagingReplica<'_, E> {
    /// Exact protocol identity and profile.
    #[must_use]
    pub const fn key(&self) -> &ReplicaKey {
        self.key
    }

    /// Canonical live PTY geometry.
    #[must_use]
    pub const fn geometry(&self) -> CanonicalGeometry {
        self.geometry
    }

    /// Whether the engine reported its internal READY boundary.
    #[must_use]
    pub const fn engine_ready(&self) -> bool {
        self.engine_ready
    }

    /// Whether protocol `BOOTSTRAP_READY` arrived.
    #[must_use]
    pub const fn protocol_ready(&self) -> bool {
        self.protocol_ready
    }

    /// Borrow the adapter-owned staging state.
    #[must_use]
    pub const fn engine(&self) -> &E::Replica {
        self.engine
    }
}

/// A deterministic protocol or adapter failure.
#[derive(Debug, thiserror::Error)]
pub enum KernelError<E> {
    /// The stream profile differs from the profile selected by `HELLO_OK`.
    #[error("bootstrap profile {incoming:?} does not match selected profile {selected:?}")]
    ProfileMismatch {
        /// Connection-selected profile.
        selected: BootstrapProfile,
        /// Stream-local profile from `BOOTSTRAP_BEGIN`.
        incoming: BootstrapStreamProfile,
    },
    /// The authoritative live geometry contained an empty axis.
    #[error("invalid canonical geometry {cols}x{rows}")]
    InvalidGeometry {
        /// Received cell width.
        cols: u16,
        /// Received cell height.
        rows: u16,
    },
    /// An attach was restarted before its barrier completed.
    #[error("attach {active_attach_id} is still waiting for ATTACH_READY")]
    AttachInProgress {
        /// Active attach correlation identifier.
        active_attach_id: u32,
    },
    /// The attach inventory named a terminal more than once.
    #[error("attach inventory contains duplicate terminal {0}")]
    DuplicateAttachTerminal(TerminalId),
    /// An event named a permanently closed terminal.
    #[error("terminal {0} is closed")]
    ClosedTerminal(TerminalId),
    /// `ATTACH_READY` did not match the active attach.
    #[error("ATTACH_READY {actual} does not match active attach {expected:?}")]
    AttachIdMismatch {
        /// Active attach identifier, if any.
        expected: Option<u32>,
        /// Received attach identifier.
        actual: u32,
    },
    /// `ATTACH_READY` arrived before every pane was READY or closed.
    #[error("ATTACH_READY arrived with {remaining} unresolved terminals")]
    AttachNotReady {
        /// Number of unresolved attach participants.
        remaining: usize,
    },
    /// No state exists for the target terminal.
    #[error("unknown terminal {0}")]
    UnknownTerminal(TerminalId),
    /// The exact stream/bootstrap generation is not current.
    #[error("generation ({stream_id}, {bootstrap_id}) is not current for {terminal_id}")]
    GenerationMismatch {
        /// Target terminal.
        terminal_id: TerminalId,
        /// Received logical subscription.
        stream_id: StreamId,
        /// Received generation.
        bootstrap_id: BootstrapId,
    },
    /// The exact stream/bootstrap generation was permanently retired.
    #[error("generation ({stream_id}, {bootstrap_id}) is retired for {terminal_id}")]
    RetiredGeneration {
        /// Target terminal.
        terminal_id: TerminalId,
        /// Retired logical subscription.
        stream_id: StreamId,
        /// Retired generation.
        bootstrap_id: BootstrapId,
    },
    /// Native history framing disagreed with the authenticated engine FINISH.
    #[error(
        "native history completion mismatch: engine reported {progress:?}, next cursor present: {has_more}"
    )]
    HistoryCompletionMismatch {
        /// Progress returned by the native engine adapter.
        progress: BootstrapProgress,
        /// Whether the wire page advertised a next cursor.
        has_more: bool,
    },
    /// The authenticated history row contribution disagreed with the engine.
    #[error("history page declared {declared} rows but imported {actual}")]
    HistoryRowCountMismatch {
        /// Rows declared by the wire response.
        declared: u32,
        /// Exact engine total-row delta.
        actual: u64,
    },
    /// Progressive history cache rejected cursor continuity or bounds.
    #[error("progressive history failed: {0}")]
    HistoryCache(#[from] HistoryCacheError),
    /// The same generation began more than once.
    #[error("generation ({stream_id}, {bootstrap_id}) already exists for {terminal_id}")]
    DuplicateGeneration {
        /// Target terminal.
        terminal_id: TerminalId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replica generation.
        bootstrap_id: BootstrapId,
    },
    /// A publication fence was reached without its staging replica.
    #[error("terminal {0} has no staging replica to publish")]
    MissingStaging(TerminalId),
    /// A bootstrap chunk repeated or moved backward.
    #[error("duplicate bootstrap chunk {actual}; expected {expected}")]
    DuplicateChunk {
        /// Next required chunk sequence.
        expected: u32,
        /// Received chunk sequence.
        actual: u32,
    },
    /// A bootstrap chunk skipped forward.
    #[error("bootstrap chunk gap at {actual}; expected {expected}")]
    ChunkGap {
        /// Next required chunk sequence.
        expected: u32,
        /// Received chunk sequence.
        actual: u32,
    },
    /// No chunk sequence remains representable.
    #[error("bootstrap chunk sequence exhausted")]
    ChunkSequenceExhausted,
    /// A chunk arrived after the engine's READY boundary.
    #[error("bootstrap chunk arrived after engine READY")]
    ChunkAfterEngineReady,
    /// Protocol READY was duplicated.
    #[error("duplicate protocol BOOTSTRAP_READY")]
    DuplicateProtocolReady,
    /// Protocol READY arrived but the engine decoder did not become READY.
    #[error("protocol BOOTSTRAP_READY did not reach engine READY")]
    EngineNotReady,
    /// A live sequence repeated or moved backward.
    #[error("duplicate live sequence {actual}; expected {expected}")]
    DuplicateSequence {
        /// Next required live sequence.
        expected: u64,
        /// Received live sequence.
        actual: u64,
    },
    /// A live sequence skipped forward.
    #[error("live sequence gap at {actual}; expected {expected}")]
    SequenceGap {
        /// Next required live sequence.
        expected: u64,
        /// Received live sequence.
        actual: u64,
    },
    /// No live sequence remains representable.
    #[error("live sequence exhausted")]
    SequenceExhausted,
    /// The frontend attempted input while its target was ineligible.
    #[error("input is not eligible for {terminal_id}: {reason:?}")]
    InputIneligible {
        /// Explicit input target.
        terminal_id: TerminalId,
        /// Deterministic gate reason.
        reason: InputBlockReason,
    },
    /// The terminal engine rejected an operation.
    #[error("terminal engine failed: {0}")]
    Engine(#[source] E),
}

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

struct AttachParticipant {
    terminal_id: TerminalId,
    resolved: bool,
    pending_removal: bool,
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
    terminals: HashMap<TerminalId, TerminalState<E::Replica>>,
    closed: HashSet<TerminalId>,
    attach: Option<AttachState>,
    engine_effects: EngineEffectBuffer,
    history_config: HistoryCacheConfig,
}
impl<E: EngineAdapter> std::fmt::Debug for SessionKernel<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionKernel")
            .field("selected_profile", &self.selected_profile)
            .field("terminal_count", &self.terminals.len())
            .field("closed_terminal_count", &self.closed.len())
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
            closed: HashSet::new(),
            attach: None,
            engine_effects: EngineEffectBuffer::new(),
            history_config,
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

    /// Whether the active ATTACH inventory authorizes one open terminal.
    #[must_use]
    pub fn active_attach_contains(&self, terminal_id: &TerminalId) -> bool {
        !self.closed.contains(terminal_id)
            && self.attach.as_ref().is_some_and(|attach| {
                attach
                    .terminals
                    .iter()
                    .any(|participant| &participant.terminal_id == terminal_id)
            })
    }

    /// Release the active ATTACH inventory after the connection is detached.
    pub fn release_active_attach(&mut self) {
        self.attach = None;
    }

    /// Borrow the published replica for one terminal.
    #[must_use]
    pub fn published(&self, terminal_id: &TerminalId) -> Option<PublishedReplica<'_, E>> {
        let replica = self.terminals.get(terminal_id)?.published.as_ref()?;
        Some(PublishedReplica {
            key: &replica.key,
            geometry: replica.geometry,
            last_seq: replica.last_seq,
            engine: &replica.engine,
            history: &replica.history,
        })
    }
    /// Borrow the published engine replica directly for frontend projection.
    #[must_use]
    pub fn published_engine(&self, terminal_id: &TerminalId) -> Option<&E::Replica> {
        Some(&self.terminals.get(terminal_id)?.published.as_ref()?.engine)
    }

    /// Mutably borrow a published engine for frontend-local controlled operations.
    ///
    /// Wire bootstrap, history, and live bytes must still enter through
    /// [`Self::update`]. This is only for adapter-owned viewport controls that
    /// do not alter protocol sequencing.
    #[must_use]
    pub fn published_engine_mut(&mut self, terminal_id: &TerminalId) -> Option<&mut E::Replica> {
        Some(
            &mut self
                .terminals
                .get_mut(terminal_id)?
                .published
                .as_mut()?
                .engine,
        )
    }

    /// Borrow one published generation's progressive history cache.
    #[must_use]
    pub fn history_cache(&self, terminal_id: &TerminalId) -> Option<&HistoryCache> {
        Some(&self.terminals.get(terminal_id)?.published.as_ref()?.history)
    }

    /// Request the next older page when a pinned viewport nears the cache edge.
    ///
    /// Returns `true` only when a request was emitted. Repeated calls are
    /// idempotent while that exact cursor is in flight.
    pub fn prefetch_history(
        &mut self,
        terminal_id: &TerminalId,
        rows_from_oldest: usize,
        effects: &mut EffectBuffer,
    ) -> bool {
        let Some(state) = self.terminals.get_mut(terminal_id) else {
            return false;
        };
        let Some(replica) = state.published.as_mut() else {
            return false;
        };
        if state.retired.contains_key(&generation_of(&replica.key)) {
            return false;
        }
        if !replica.history.should_prefetch(rows_from_oldest) {
            return false;
        }
        let Some(cursor) = replica.history.begin_fetch() else {
            return false;
        };
        effects.push(KernelEffect::Send(KernelSend::HistoryRequest {
            key: replica.key.clone(),
            cursor: cursor.as_bytes().to_vec(),
            max_bytes: self.history_config.request_max_bytes,
            max_rows: self.history_config.request_max_rows,
        }));
        effects.push(KernelEffect::Status(KernelStatus::History {
            key: replica.key.clone(),
            status: replica.history.status(),
        }));
        true
    }

    /// Borrow the unpublished staging replica for one terminal.
    #[must_use]
    pub fn staging(&self, terminal_id: &TerminalId) -> Option<StagingReplica<'_, E>> {
        let staging = self.terminals.get(terminal_id)?.staging.as_ref()?;
        Some(StagingReplica {
            key: &staging.key,
            geometry: staging.geometry,
            engine_ready: staging.engine_ready,
            protocol_ready: staging.protocol_ready,
            engine: &staging.engine,
        })
    }

    /// Return a retained tombstone, if this exact generation is retired.
    #[must_use]
    pub fn tombstone(
        &self,
        terminal_id: &TerminalId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
    ) -> Option<TombstoneRecord> {
        self.terminals
            .get(terminal_id)?
            .retired
            .get(&GenerationId {
                stream_id,
                bootstrap_id,
            })
            .copied()
    }

    /// Compute explicit input eligibility without producing effects.
    #[must_use]
    pub fn input_eligibility(&self, terminal_id: &TerminalId) -> InputEligibility {
        if self.closed.contains(terminal_id) {
            return InputEligibility::Ineligible(InputBlockReason::Closed);
        }
        let Some(state) = self.terminals.get(terminal_id) else {
            return InputEligibility::Ineligible(InputBlockReason::UnknownTerminal);
        };
        let Some(replica) = state.published.as_ref() else {
            return InputEligibility::Ineligible(InputBlockReason::AwaitingReplica);
        };
        if state.retired.contains_key(&generation_of(&replica.key)) {
            return InputEligibility::Ineligible(InputBlockReason::FrozenReplica);
        }
        if self.attach_blocks(terminal_id) {
            return InputEligibility::Ineligible(InputBlockReason::AwaitingAttachReady);
        }
        InputEligibility::Eligible {
            stream_id: replica.key.stream_id,
            bootstrap_id: replica.key.bootstrap_id,
        }
    }

    /// Apply one normalized input and replace `effects` with its declarative result.
    ///
    /// An adapter error retires the possibly mutated generation before
    /// returning and leaves a [`KernelStatus::ResyncRequired`] effect in the
    /// buffer. The host must execute effects even when this method returns an
    /// error.
    #[allow(clippy::too_many_lines)]
    pub fn update(
        &mut self,
        input: KernelInput<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        effects.clear();
        match input {
            KernelInput::AttachStarted {
                attach_id,
                terminals,
            } => self.attach_started(attach_id, terminals),
            KernelInput::AttachReady { attach_id } => self.attach_ready(attach_id, effects),
            KernelInput::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                geometry,
                base_seq,
            } => self.bootstrap_begin(
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                geometry,
                base_seq,
                effects,
            ),
            KernelInput::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } => self.bootstrap_chunk(
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
                effects,
            ),
            KernelInput::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor,
            } => self.bootstrap_ready(
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor,
                effects,
            ),
            KernelInput::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                page_seq,
                rows,
                payload,
                cursor,
                next_cursor,
            } => self.history_page(
                terminal_id,
                stream_id,
                bootstrap_id,
                page_seq,
                rows,
                payload,
                cursor,
                next_cursor,
                effects,
            ),
            KernelInput::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
            } => self.history_tombstone(
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
                effects,
            ),
            KernelInput::HistoryRejected {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
                required_bytes,
                required_rows,
            } => self.history_rejected(
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
                required_bytes,
                required_rows,
                effects,
            ),
            KernelInput::TerminalOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                payload,
            } => self.terminal_output(terminal_id, stream_id, bootstrap_id, seq, payload, effects),
            KernelInput::Tombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            } => {
                self.tombstone_generation(
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    TombstoneRecord {
                        reason,
                        last_valid_seq,
                    },
                );
                Ok(())
            }
            KernelInput::TerminalClosed { terminal_id } => {
                self.terminal_closed(terminal_id, effects);
                Ok(())
            }
            KernelInput::Action(action) => self.action(&action, effects),
        }
    }

    fn attach_started(
        &mut self,
        attach_id: u32,
        terminal_ids: &[TerminalId],
    ) -> Result<(), KernelError<E::Error>> {
        if let Some(attach) = self.attach.as_ref()
            && !attach.released
        {
            return Err(KernelError::AttachInProgress {
                active_attach_id: attach.attach_id,
            });
        }
        for (index, terminal_id) in terminal_ids.iter().enumerate() {
            if self.closed.contains(terminal_id) {
                return Err(KernelError::ClosedTerminal(terminal_id.clone()));
            }
            if terminal_ids[..index].contains(terminal_id) {
                return Err(KernelError::DuplicateAttachTerminal(terminal_id.clone()));
            }
        }

        self.terminals.reserve(terminal_ids.len());
        for terminal_id in terminal_ids {
            self.terminals.entry(terminal_id.clone()).or_default();
        }

        let attach = self.attach.get_or_insert_with(|| AttachState {
            attach_id,
            released: false,
            terminals: Vec::with_capacity(terminal_ids.len()),
        });
        attach.attach_id = attach_id;
        attach.released = false;
        attach.terminals.clear();
        attach.terminals.reserve(terminal_ids.len());
        attach
            .terminals
            .extend(
                terminal_ids
                    .iter()
                    .cloned()
                    .map(|terminal_id| AttachParticipant {
                        terminal_id,
                        resolved: false,
                        pending_removal: false,
                    }),
            );
        Ok(())
    }

    fn attach_ready(
        &mut self,
        attach_id: u32,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        let Some(attach) = self.attach.as_mut() else {
            return Err(KernelError::AttachIdMismatch {
                expected: None,
                actual: attach_id,
            });
        };
        if attach.attach_id != attach_id {
            return Err(KernelError::AttachIdMismatch {
                expected: Some(attach.attach_id),
                actual: attach_id,
            });
        }
        let remaining = attach
            .terminals
            .iter()
            .filter(|participant| !participant.resolved)
            .count();
        if remaining != 0 {
            return Err(KernelError::AttachNotReady { remaining });
        }
        if attach.released {
            return Err(KernelError::AttachIdMismatch {
                expected: None,
                actual: attach_id,
            });
        }

        attach.released = true;
        for participant in &attach.terminals {
            if participant.pending_removal {
                effects.push(KernelEffect::Damage(KernelDamage {
                    terminal_id: participant.terminal_id.clone(),
                    kind: KernelDamageKind::Removed,
                }));
                continue;
            }
            if self
                .terminals
                .get(&participant.terminal_id)
                .and_then(|state| state.published.as_ref())
                .is_some()
            {
                effects.push(KernelEffect::Damage(KernelDamage {
                    terminal_id: participant.terminal_id.clone(),
                    kind: KernelDamageKind::Full,
                }));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn bootstrap_begin(
        &mut self,
        terminal_id: &TerminalId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        profile: BootstrapStreamProfile,
        geometry: CanonicalGeometry,
        base_seq: u64,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        if self.closed.contains(terminal_id) {
            return Err(KernelError::ClosedTerminal(terminal_id.clone()));
        }
        if !profile_matches(self.selected_profile, profile) {
            return Err(KernelError::ProfileMismatch {
                selected: self.selected_profile,
                incoming: profile,
            });
        }
        if geometry.cols == 0 || geometry.rows == 0 {
            return Err(KernelError::InvalidGeometry {
                cols: geometry.cols,
                rows: geometry.rows,
            });
        }

        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let state = self.terminals.entry(terminal_id.clone()).or_default();
        if state.retired.contains_key(&generation) {
            return Err(KernelError::RetiredGeneration {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
            });
        }
        if state
            .published
            .as_ref()
            .is_some_and(|replica| generation_of(&replica.key) == generation)
            || state
                .staging
                .as_ref()
                .is_some_and(|staging| generation_of(&staging.key) == generation)
        {
            return Err(KernelError::DuplicateGeneration {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
            });
        }

        self.engine_effects.clear();
        let engine = match self.adapter.start_replica(profile, geometry) {
            Ok(engine) => engine,
            Err(error) => {
                state.retired.insert(
                    generation,
                    TombstoneRecord {
                        reason: TombstoneReason::CodecFailure,
                        last_valid_seq: base_seq,
                    },
                );
                effects.push(KernelEffect::Status(KernelStatus::ResyncRequired {
                    terminal_id: terminal_id.clone(),
                    stream_id,
                    bootstrap_id,
                    reason: TombstoneReason::CodecFailure,
                }));
                return Err(KernelError::Engine(error));
            }
        };
        if let Some(old_staging) = state.staging.replace(Staging {
            key: ReplicaKey {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                profile,
            },
            geometry,
            base_seq,
            next_chunk_seq: Some(0),
            engine_ready: false,
            protocol_ready: false,
            history_cursor: None,
            engine,
            pending_effects: Vec::new(),
        }) {
            state.retired.insert(
                generation_of(&old_staging.key),
                TombstoneRecord {
                    reason: TombstoneReason::ExplicitReattach,
                    last_valid_seq: old_staging.base_seq,
                },
            );
        }
        Ok(())
    }

    fn bootstrap_chunk(
        &mut self,
        terminal_id: &TerminalId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        chunk_seq: u32,
        payload: &[u8],
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let state = self
            .terminals
            .get_mut(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        if state.retired.contains_key(&generation) {
            return Err(retired_error(terminal_id, generation));
        }
        let staging = state
            .staging
            .as_mut()
            .filter(|staging| generation_of(&staging.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))?;
        if staging.engine_ready {
            return Err(KernelError::ChunkAfterEngineReady);
        }
        let Some(expected) = staging.next_chunk_seq else {
            return Err(KernelError::ChunkSequenceExhausted);
        };
        if chunk_seq < expected {
            return Err(KernelError::DuplicateChunk {
                expected,
                actual: chunk_seq,
            });
        }
        if chunk_seq > expected {
            return Err(KernelError::ChunkGap {
                expected,
                actual: chunk_seq,
            });
        }

        self.engine_effects.clear();
        let progress = match self.adapter.apply_bootstrap_chunk(
            &mut staging.engine,
            payload,
            &mut self.engine_effects,
        ) {
            Ok(progress) => progress,
            Err(error) => {
                let last_valid_seq = staging.base_seq;
                self.engine_effects.clear();
                state.staging = None;
                state.retired.insert(
                    generation,
                    TombstoneRecord {
                        reason: TombstoneReason::CodecFailure,
                        last_valid_seq,
                    },
                );
                effects.push(KernelEffect::Status(KernelStatus::ResyncRequired {
                    terminal_id: terminal_id.clone(),
                    stream_id,
                    bootstrap_id,
                    reason: TombstoneReason::CodecFailure,
                }));
                return Err(KernelError::Engine(error));
            }
        };
        staging.next_chunk_seq = chunk_seq.checked_add(1);
        staging.engine_ready |= progress.is_ready();
        self.buffer_bootstrap_effects(terminal_id, generation);
        Ok(())
    }

    fn bootstrap_ready(
        &mut self,
        terminal_id: &TerminalId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        history_cursor: Option<&[u8]>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let state = self
            .terminals
            .get_mut(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        if state.retired.contains_key(&generation) {
            return Err(retired_error(terminal_id, generation));
        }
        let staging = state
            .staging
            .as_mut()
            .filter(|staging| generation_of(&staging.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))?;
        if staging.protocol_ready {
            return Err(KernelError::DuplicateProtocolReady);
        }
        self.engine_effects.clear();
        let progress = match self
            .adapter
            .finish_bootstrap(&mut staging.engine, &mut self.engine_effects)
        {
            Ok(progress) => progress,
            Err(error) => {
                let last_valid_seq = staging.base_seq;
                self.engine_effects.clear();
                state.staging = None;
                state.retired.insert(
                    generation,
                    TombstoneRecord {
                        reason: TombstoneReason::CodecFailure,
                        last_valid_seq,
                    },
                );
                effects.push(KernelEffect::Status(KernelStatus::ResyncRequired {
                    terminal_id: terminal_id.clone(),
                    stream_id,
                    bootstrap_id,
                    reason: TombstoneReason::CodecFailure,
                }));
                return Err(KernelError::Engine(error));
            }
        };
        staging.history_cursor = history_cursor.map(HistoryCursor::new);
        staging.protocol_ready = true;
        staging.engine_ready |= progress.is_ready();
        let engine_ready = staging.engine_ready;
        if !engine_ready {
            let last_valid_seq = staging.base_seq;
            state.staging = None;
            state.retired.insert(
                generation,
                TombstoneRecord {
                    reason: TombstoneReason::CodecFailure,
                    last_valid_seq,
                },
            );
        }
        if engine_ready {
            self.buffer_bootstrap_effects(terminal_id, generation);
        } else {
            self.engine_effects.clear();
        }
        if !engine_ready {
            effects.push(KernelEffect::Status(KernelStatus::ResyncRequired {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                reason: TombstoneReason::CodecFailure,
            }));
            return Err(KernelError::EngineNotReady);
        }

        self.publish(terminal_id, effects)
    }

    fn publish(
        &mut self,
        terminal_id: &TerminalId,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        let history_config = self.history_config;
        let (replica_key, pending_effects, initial_history_cursor, history_status) = {
            let state = self
                .terminals
                .get_mut(terminal_id)
                .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
            {
                let staging = state
                    .staging
                    .as_mut()
                    .ok_or_else(|| KernelError::MissingStaging(terminal_id.clone()))?;
                debug_assert!(staging.engine_ready && staging.protocol_ready);
                self.adapter
                    .configure_history_budget(
                        &mut staging.engine,
                        history_config.max_bytes,
                        history_config.max_materialized_rows,
                    )
                    .map_err(KernelError::Engine)?;
            }
            let mut staging = state
                .staging
                .take()
                .ok_or_else(|| KernelError::MissingStaging(terminal_id.clone()))?;
            let replica_key = staging.key.clone();
            let pending_effects = std::mem::take(&mut staging.pending_effects);
            let mut history = HistoryCache::new(
                history_config,
                staging.history_cursor.take(),
                staging.geometry.cols,
            );
            let initial_history_cursor = history.begin_fetch();
            let replacement = Replica {
                key: staging.key,
                geometry: staging.geometry,
                last_seq: staging.base_seq,
                next_seq: staging.base_seq.checked_add(1),
                engine: staging.engine,
                history,
            };
            let history_status = replacement.history.status();
            if let Some(old) = state.published.replace(replacement) {
                state
                    .retired
                    .entry(generation_of(&old.key))
                    .or_insert(TombstoneRecord {
                        reason: TombstoneReason::ExplicitReattach,
                        last_valid_seq: old.last_seq,
                    });
            }
            (
                replica_key,
                pending_effects,
                initial_history_cursor,
                history_status,
            )
        };

        self.mark_attach_resolved(terminal_id);
        let damage_allowed = !self.attach_blocks(terminal_id);
        for effect in pending_effects {
            Self::translate_engine_effect(&replica_key, effect, damage_allowed, effects);
        }
        if let Some(cursor) = initial_history_cursor {
            effects.push(KernelEffect::Send(KernelSend::HistoryRequest {
                key: replica_key.clone(),
                cursor: cursor.as_bytes().to_vec(),
                max_bytes: history_config.request_max_bytes,
                max_rows: history_config.request_max_rows,
            }));
            effects.push(KernelEffect::Status(KernelStatus::History {
                key: replica_key.clone(),
                status: history_status,
            }));
        }
        if damage_allowed {
            effects.push(KernelEffect::Damage(KernelDamage {
                terminal_id: terminal_id.clone(),
                kind: KernelDamageKind::Full,
            }));
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn terminal_output(
        &mut self,
        terminal_id: &TerminalId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        seq: u64,
        payload: &[u8],
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let state = self
            .terminals
            .get_mut(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        if state.retired.contains_key(&generation) {
            return Err(retired_error(terminal_id, generation));
        }
        let replica = state
            .published
            .as_mut()
            .filter(|replica| generation_of(&replica.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))?;
        let Some(expected) = replica.next_seq else {
            return Err(KernelError::SequenceExhausted);
        };
        if seq < expected {
            return Err(KernelError::DuplicateSequence {
                expected,
                actual: seq,
            });
        }
        if seq > expected {
            return Err(KernelError::SequenceGap {
                expected,
                actual: seq,
            });
        }

        let pinned_anchor = match replica.history.viewport_anchor() {
            ViewportAnchor::Pinned(anchor) => Some(anchor),
            ViewportAnchor::Tail => None,
        };
        let distance_before = if let Some(anchor) = pinned_anchor {
            self.adapter
                .history_anchor_tail_distance(&replica.engine, anchor)
                .map_err(KernelError::Engine)?
        } else {
            None
        };
        self.engine_effects.clear();
        if let Err(error) =
            self.adapter
                .apply_output(&mut replica.engine, payload, &mut self.engine_effects)
        {
            let last_valid_seq = replica.last_seq;
            self.engine_effects.clear();
            state.published = None;
            state.retired.insert(
                generation,
                TombstoneRecord {
                    reason: TombstoneReason::CodecFailure,
                    last_valid_seq,
                },
            );
            let damage_blocked = self.attach_blocks(terminal_id);
            self.mark_attach_unresolved(terminal_id, damage_blocked);
            effects.push(KernelEffect::Status(KernelStatus::ResyncRequired {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                reason: TombstoneReason::CodecFailure,
            }));
            if !damage_blocked {
                effects.push(KernelEffect::Damage(KernelDamage {
                    terminal_id: terminal_id.clone(),
                    kind: KernelDamageKind::Removed,
                }));
            }
            return Err(KernelError::Engine(error));
        }
        if let Some(anchor) = pinned_anchor {
            match (
                distance_before,
                self.adapter
                    .history_anchor_tail_distance(&replica.engine, anchor),
            ) {
                (Some(before), Ok(Some(after))) => {
                    replica
                        .history
                        .note_live_output(after.saturating_sub(before));
                }
                (None, Ok(Some(_))) | (_, Ok(None)) => {
                    replica.history.mark_pruned();
                    self.adapter.clear_document_state(&mut replica.engine);
                    effects.push(KernelEffect::Status(KernelStatus::HistoryUnavailable {
                        key: replica.key.clone(),
                        reason: HistoryUnavailableReason::Pruned,
                    }));
                    effects.push(KernelEffect::Status(KernelStatus::History {
                        key: replica.key.clone(),
                        status: replica.history.status(),
                    }));
                }
                (_, Err(_)) => {
                    replica.history.tombstone();
                    self.adapter.clear_document_state(&mut replica.engine);
                    effects.push(KernelEffect::Status(KernelStatus::HistoryUnavailable {
                        key: replica.key.clone(),
                        reason: HistoryUnavailableReason::CodecFailure,
                    }));
                    effects.push(KernelEffect::Status(KernelStatus::History {
                        key: replica.key.clone(),
                        status: replica.history.status(),
                    }));
                }
            }
        }
        replica.last_seq = seq;
        replica.next_seq = seq.checked_add(1);
        let replica_key = replica.key.clone();
        let acknowledge = matches!(
            replica_key.profile,
            BootstrapStreamProfile::SynthesizedVtStateSync
        );

        let damage_allowed = !self.attach_blocks(terminal_id);
        let mut captured = std::mem::take(&mut self.engine_effects);
        for effect in captured.drain() {
            Self::translate_engine_effect(&replica_key, effect, damage_allowed, effects);
        }
        self.engine_effects = captured;
        if acknowledge {
            effects.push(KernelEffect::Send(KernelSend::FrameAck {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                seq,
            }));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn history_page(
        &mut self,
        terminal_id: &TerminalId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        page_seq: u64,
        rows: u32,
        payload: &[u8],
        cursor: &[u8],
        next_cursor: Option<&[u8]>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let state = self
            .terminals
            .get_mut(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        if state.retired.contains_key(&generation) {
            return Err(retired_error(terminal_id, generation));
        }
        let replica = state
            .published
            .as_mut()
            .filter(|replica| generation_of(&replica.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))?;
        let cursor = HistoryCursor::new(cursor);
        let next_cursor = next_cursor.map(HistoryCursor::new);
        match replica
            .history
            .check_page(&cursor, page_seq, next_cursor.as_ref(), rows, payload)
        {
            Ok(HistoryPageCheck::Duplicate(_)) => {
                effects.push(KernelEffect::Status(KernelStatus::History {
                    key: replica.key.clone(),
                    status: replica.history.status(),
                }));
                return Ok(());
            }
            Ok(HistoryPageCheck::New) => {}
            Err(error) => {
                // Cursor/sequence failures invalidate only progressive history.
                // The published live replica remains authoritative and usable.
                replica.history.tombstone();
                self.adapter.clear_document_state(&mut replica.engine);
                effects.push(KernelEffect::Status(KernelStatus::HistoryUnavailable {
                    key: replica.key.clone(),
                    reason: HistoryUnavailableReason::CodecFailure,
                }));
                effects.push(KernelEffect::Status(KernelStatus::History {
                    key: replica.key.clone(),
                    status: replica.history.status(),
                }));
                return Err(KernelError::HistoryCache(error));
            }
        }

        self.engine_effects.clear();
        let outcome = match self.adapter.apply_history_page(
            &mut replica.engine,
            payload,
            &mut self.engine_effects,
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                replica.history.tombstone();
                self.adapter.clear_document_state(&mut replica.engine);
                self.engine_effects.clear();
                effects.push(KernelEffect::Status(KernelStatus::HistoryUnavailable {
                    key: replica.key.clone(),
                    reason: HistoryUnavailableReason::CodecFailure,
                }));
                effects.push(KernelEffect::Status(KernelStatus::History {
                    key: replica.key.clone(),
                    status: replica.history.status(),
                }));
                return Err(KernelError::Engine(error));
            }
        };

        let has_more = next_cursor.is_some();
        let native = matches!(
            replica.key.profile,
            BootstrapStreamProfile::NativeState { .. }
        );
        let finished = matches!(outcome.progress, BootstrapProgress::Finished);
        if outcome.retained && native && finished == has_more {
            replica.history.tombstone();
            self.adapter.clear_document_state(&mut replica.engine);
            self.engine_effects.clear();
            effects.push(KernelEffect::Status(KernelStatus::HistoryUnavailable {
                key: replica.key.clone(),
                reason: HistoryUnavailableReason::CodecFailure,
            }));
            effects.push(KernelEffect::Status(KernelStatus::History {
                key: replica.key.clone(),
                status: replica.history.status(),
            }));
            return Err(KernelError::HistoryCompletionMismatch {
                progress: outcome.progress,
                has_more,
            });
        }

        if !outcome.retained {
            replica.history.tombstone();
            self.adapter.clear_document_state(&mut replica.engine);
            self.engine_effects.clear();
            effects.push(KernelEffect::Status(KernelStatus::HistoryUnavailable {
                key: replica.key.clone(),
                reason: HistoryUnavailableReason::Limit,
            }));
            effects.push(KernelEffect::Status(KernelStatus::History {
                key: replica.key.clone(),
                status: replica.history.status(),
            }));
            return Ok(());
        }

        if let Err(error) = replica.history.accept_page(
            &cursor,
            page_seq,
            next_cursor,
            rows,
            rows as usize,
            payload,
        ) {
            replica.history.tombstone();
            self.adapter.clear_document_state(&mut replica.engine);
            self.engine_effects.clear();
            effects.push(KernelEffect::Status(KernelStatus::HistoryUnavailable {
                key: replica.key.clone(),
                reason: HistoryUnavailableReason::CodecFailure,
            }));
            effects.push(KernelEffect::Status(KernelStatus::History {
                key: replica.key.clone(),
                status: replica.history.status(),
            }));
            return Err(KernelError::HistoryCache(error));
        }

        let next_request = replica
            .history
            .should_auto_continue()
            .then(|| replica.history.begin_fetch())
            .flatten();
        let replica_key = replica.key.clone();
        let history_status = replica.history.status();
        let damage_allowed = !self.attach_blocks(terminal_id);
        let mut captured = std::mem::take(&mut self.engine_effects);
        for effect in captured.drain() {
            Self::translate_engine_effect(&replica_key, effect, damage_allowed, effects);
        }
        self.engine_effects = captured;
        effects.push(KernelEffect::Status(KernelStatus::History {
            key: replica_key.clone(),
            status: history_status,
        }));
        if let Some(cursor) = next_request {
            effects.push(KernelEffect::Send(KernelSend::HistoryRequest {
                key: replica_key,
                cursor: cursor.as_bytes().to_vec(),
                max_bytes: self.history_config.request_max_bytes,
                max_rows: self.history_config.request_max_rows,
            }));
        }
        Ok(())
    }

    fn history_tombstone(
        &mut self,
        terminal_id: &TerminalId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        cursor: &[u8],
        reason: HistoryUnavailableReason,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let state = self
            .terminals
            .get_mut(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        if state.retired.contains_key(&generation) {
            return Err(retired_error(terminal_id, generation));
        }
        let replica = state
            .published
            .as_mut()
            .filter(|replica| generation_of(&replica.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))?;
        let cursor = HistoryCursor::new(cursor);
        let state = match reason {
            HistoryUnavailableReason::Stale => HistoryLoadState::Stale,
            HistoryUnavailableReason::Pruned => HistoryLoadState::Pruned,
            HistoryUnavailableReason::Reset
            | HistoryUnavailableReason::Resize
            | HistoryUnavailableReason::Expired
            | HistoryUnavailableReason::Released
            | HistoryUnavailableReason::Limit
            | HistoryUnavailableReason::CodecFailure => HistoryLoadState::Tombstoned,
        };
        if !replica.history.invalidate_cursor(&cursor, state) {
            return Err(KernelError::HistoryCache(HistoryCacheError::Gap));
        }
        self.adapter.clear_document_state(&mut replica.engine);
        effects.push(KernelEffect::Status(KernelStatus::HistoryUnavailable {
            key: replica.key.clone(),
            reason,
        }));
        effects.push(KernelEffect::Status(KernelStatus::History {
            key: replica.key.clone(),
            status: replica.history.status(),
        }));
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn history_rejected(
        &mut self,
        terminal_id: &TerminalId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        cursor: &[u8],
        reason: HistoryRejectionReason,
        required_bytes: u32,
        required_rows: u32,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        self.ensure_open(terminal_id)?;
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let state = self
            .terminals
            .get_mut(terminal_id)
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        if state.retired.contains_key(&generation) {
            return Err(retired_error(terminal_id, generation));
        }
        let replica = state
            .published
            .as_mut()
            .filter(|replica| generation_of(&replica.key) == generation)
            .ok_or_else(|| mismatch_error(terminal_id, generation))?;
        let cursor = HistoryCursor::new(cursor);
        let retry_limits = (reason == HistoryRejectionReason::TooSmall)
            .then(|| replica.history.retry_limits(required_bytes, required_rows))
            .flatten();
        if !replica.history.cancel_fetch(&cursor) {
            return Err(KernelError::HistoryCache(HistoryCacheError::Gap));
        }
        let next_request = retry_limits.and_then(|(max_bytes, max_rows)| {
            replica
                .history
                .begin_fetch_with_limits(max_bytes, max_rows)
                .map(|cursor| (cursor, max_bytes, max_rows))
        });
        if reason == HistoryRejectionReason::TooSmall && next_request.is_none() {
            replica.history.tombstone();
            self.adapter.clear_document_state(&mut replica.engine);
            effects.push(KernelEffect::Status(KernelStatus::HistoryUnavailable {
                key: replica.key.clone(),
                reason: HistoryUnavailableReason::Limit,
            }));
        }
        effects.push(KernelEffect::Status(KernelStatus::History {
            key: replica.key.clone(),
            status: replica.history.status(),
        }));
        if let Some((cursor, max_bytes, max_rows)) = next_request {
            effects.push(KernelEffect::Send(KernelSend::HistoryRequest {
                key: replica.key.clone(),
                cursor: cursor.as_bytes().to_vec(),
                max_bytes,
                max_rows,
            }));
        }
        Ok(())
    }

    fn buffer_bootstrap_effects(&mut self, terminal_id: &TerminalId, generation: GenerationId) {
        let mut captured = std::mem::take(&mut self.engine_effects);
        let staging = self
            .terminals
            .get_mut(terminal_id)
            .and_then(|state| state.staging.as_mut())
            .filter(|staging| generation_of(&staging.key) == generation);
        if let Some(staging) = staging {
            for effect in captured.drain() {
                match effect {
                    // Bootstrap is replay into staging, never live PTY input.
                    // Replies are suppressed and publication supplies damage.
                    EngineEffect::Send(_) | EngineEffect::Damage(_) => {}
                    effect @ (EngineEffect::Status(_) | EngineEffect::Job(_)) => {
                        staging.pending_effects.push(effect);
                    }
                }
            }
        } else {
            captured.clear();
        }
        self.engine_effects = captured;
    }

    fn translate_engine_effect(
        key: &ReplicaKey,
        effect: EngineEffect,
        damage_allowed: bool,
        effects: &mut EffectBuffer,
    ) {
        match effect {
            EngineEffect::Send(EngineSend::PtyWrite(bytes)) => {
                effects.push(KernelEffect::Send(KernelSend::PtyWrite {
                    terminal_id: key.terminal_id.clone(),
                    bytes,
                }));
            }
            EngineEffect::Damage(damage) if damage_allowed => {
                effects.push(KernelEffect::Damage(KernelDamage {
                    terminal_id: key.terminal_id.clone(),
                    kind: match damage {
                        EngineDamage::Full => KernelDamageKind::Full,
                        EngineDamage::Rows { first, last } => {
                            KernelDamageKind::Rows { first, last }
                        }
                    },
                }));
            }
            EngineEffect::Damage(_) => {}
            EngineEffect::Status(status) => {
                effects.push(KernelEffect::Status(KernelStatus::Engine {
                    key: key.clone(),
                    status,
                }));
            }
            EngineEffect::Job(job) => {
                effects.push(KernelEffect::Job(KernelJob {
                    key: key.clone(),
                    job,
                }));
            }
        }
    }

    fn tombstone_generation(
        &mut self,
        terminal_id: &TerminalId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        record: TombstoneRecord,
    ) {
        let generation = GenerationId {
            stream_id,
            bootstrap_id,
        };
        let state = self.terminals.entry(terminal_id.clone()).or_default();
        state.retired.entry(generation).or_insert(record);
        if state
            .staging
            .as_ref()
            .is_some_and(|staging| generation_of(&staging.key) == generation)
        {
            state.staging = None;
        }
        let froze_published = state
            .published
            .as_ref()
            .is_some_and(|replica| generation_of(&replica.key) == generation);
        if froze_published {
            self.mark_attach_unresolved(terminal_id, false);
        }
    }

    fn terminal_closed(&mut self, terminal_id: &TerminalId, effects: &mut EffectBuffer) {
        let damage_blocked = self.attach_blocks(terminal_id);
        let had_published = self
            .terminals
            .remove(terminal_id)
            .is_some_and(|state| state.published.is_some());
        self.closed.insert(terminal_id.clone());
        self.mark_attach_closed(terminal_id, had_published && damage_blocked);
        if had_published && !damage_blocked {
            effects.push(KernelEffect::Damage(KernelDamage {
                terminal_id: terminal_id.clone(),
                kind: KernelDamageKind::Removed,
            }));
        }
    }

    fn action(
        &self,
        action: &KernelAction<'_>,
        effects: &mut EffectBuffer,
    ) -> Result<(), KernelError<E::Error>> {
        match *action {
            KernelAction::Input { terminal_id, event } => {
                let eligibility = self.input_eligibility(terminal_id);
                if let InputEligibility::Ineligible(reason) = eligibility {
                    return Err(KernelError::InputIneligible {
                        terminal_id: terminal_id.clone(),
                        reason,
                    });
                }
                effects.push(KernelEffect::Send(KernelSend::Input {
                    terminal_id: terminal_id.clone(),
                    event: event.clone(),
                }));
                Ok(())
            }
        }
    }

    fn ensure_open(&self, terminal_id: &TerminalId) -> Result<(), KernelError<E::Error>> {
        if self.closed.contains(terminal_id) {
            Err(KernelError::ClosedTerminal(terminal_id.clone()))
        } else {
            Ok(())
        }
    }

    fn attach_blocks(&self, terminal_id: &TerminalId) -> bool {
        self.attach.as_ref().is_some_and(|attach| {
            !attach.released
                && attach
                    .terminals
                    .iter()
                    .any(|participant| &participant.terminal_id == terminal_id)
        })
    }

    fn mark_attach_resolved(&mut self, terminal_id: &TerminalId) {
        if let Some(participant) = self.attach.as_mut().and_then(|attach| {
            attach
                .terminals
                .iter_mut()
                .find(|participant| &participant.terminal_id == terminal_id)
        }) {
            participant.resolved = true;
            participant.pending_removal = false;
        }
    }

    fn mark_attach_unresolved(&mut self, terminal_id: &TerminalId, pending_removal: bool) {
        if let Some(participant) = self.attach.as_mut().and_then(|attach| {
            attach
                .terminals
                .iter_mut()
                .find(|participant| &participant.terminal_id == terminal_id)
        }) {
            participant.resolved = false;
            participant.pending_removal |= pending_removal;
        }
    }

    fn mark_attach_closed(&mut self, terminal_id: &TerminalId, pending_removal: bool) {
        if let Some(participant) = self.attach.as_mut().and_then(|attach| {
            attach
                .terminals
                .iter_mut()
                .find(|participant| &participant.terminal_id == terminal_id)
        }) {
            participant.resolved = true;
            participant.pending_removal |= pending_removal;
        }
    }
}

impl<E: EngineDocumentAdapter> SessionKernel<E> {
    /// Ask the engine to project loaded history at a frontend-only width.
    ///
    /// This never resizes the canonical terminal replica.
    pub fn project_history(
        &mut self,
        terminal_id: &TerminalId,
        width: u16,
        max_rows: usize,
    ) -> Result<EngineHistoryProjection, KernelError<E::Error>> {
        let replica = self
            .terminals
            .get_mut(terminal_id)
            .and_then(|state| state.published.as_mut())
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        replica.history.reproject(width);
        let width = replica.history.projection_width();
        let origin = match replica.history.viewport_anchor() {
            ViewportAnchor::Tail => EngineProjectionOrigin::Tail,
            ViewportAnchor::Pinned(anchor) => {
                let valid = self
                    .adapter
                    .document_anchor_point(&replica.engine, anchor, DocumentSpace::History)
                    .map_err(KernelError::Engine)?
                    .is_some();
                if valid {
                    EngineProjectionOrigin::Anchor(anchor)
                } else {
                    replica.history.mark_pruned();
                    self.adapter.clear_document_state(&mut replica.engine);
                    EngineProjectionOrigin::Tail
                }
            }
        };
        let mut projection = self
            .adapter
            .project_history(&mut replica.engine, width, origin, max_rows)
            .map_err(KernelError::Engine)?;
        projection.has_older |= replica.history.has_continuation();
        Ok(projection)
    }

    /// Create one engine-owned anchor in the published document.
    pub fn track_document_anchor(
        &mut self,
        terminal_id: &TerminalId,
        point: DocumentPoint,
    ) -> Result<DocumentAnchorId, KernelError<E::Error>> {
        let replica = self
            .terminals
            .get_mut(terminal_id)
            .and_then(|state| state.published.as_mut())
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        replica.history.ensure_anchor_capacity(1)?;
        let anchor = self
            .adapter
            .track_document_anchor(&mut replica.engine, point)
            .map_err(KernelError::Engine)?;
        replica
            .history
            .register_anchor_pages(anchor, std::iter::empty())?;
        Ok(anchor)
    }

    /// Resolve an engine-owned anchor after output, reflow, or history import.
    pub fn document_anchor_point(
        &self,
        terminal_id: &TerminalId,
        anchor: DocumentAnchorId,
        space: DocumentSpace,
    ) -> Result<Option<DocumentPoint>, KernelError<E::Error>> {
        let replica = self
            .terminals
            .get(terminal_id)
            .and_then(|state| state.published.as_ref())
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        self.adapter
            .document_anchor_point(&replica.engine, anchor, space)
            .map_err(KernelError::Engine)
    }

    /// Pin the client viewport to a valid engine-owned document anchor.
    pub fn pin_history_viewport(
        &mut self,
        terminal_id: &TerminalId,
        anchor: DocumentAnchorId,
    ) -> Result<(), KernelError<E::Error>> {
        let replica = self
            .terminals
            .get_mut(terminal_id)
            .and_then(|state| state.published.as_mut())
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        if self
            .adapter
            .document_anchor_point(&replica.engine, anchor, DocumentSpace::History)
            .map_err(KernelError::Engine)?
            .is_none()
        {
            return Err(KernelError::HistoryCache(
                HistoryCacheError::AnchorUnavailable,
            ));
        }
        replica.history.pin_viewport(anchor)?;
        Ok(())
    }

    /// Resume following the live tail without resizing canonical state.
    pub fn follow_history_tail(
        &mut self,
        terminal_id: &TerminalId,
    ) -> Result<(), KernelError<E::Error>> {
        let replica = self
            .terminals
            .get_mut(terminal_id)
            .and_then(|state| state.published.as_mut())
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        replica.history.follow_tail();
        Ok(())
    }

    /// Release one engine-owned anchor.
    pub fn release_document_anchor(
        &mut self,
        terminal_id: &TerminalId,
        anchor: DocumentAnchorId,
    ) -> Result<(), KernelError<E::Error>> {
        let replica = self
            .terminals
            .get_mut(terminal_id)
            .and_then(|state| state.published.as_mut())
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        self.adapter
            .release_document_anchor(&mut replica.engine, anchor);
        replica.history.remove_anchor(anchor);
        Ok(())
    }

    /// Search only state already loaded into the engine.
    pub fn search_loaded_history(
        &mut self,
        terminal_id: &TerminalId,
        needle: &str,
        max_matches: usize,
    ) -> Result<Vec<EngineSearchMatch>, KernelError<E::Error>> {
        let replica = self
            .terminals
            .get_mut(terminal_id)
            .and_then(|state| state.published.as_mut())
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        let max_matches = max_matches.min(replica.history.remaining_anchor_capacity() / 2);
        if max_matches == 0 {
            return Ok(Vec::new());
        }
        let matches = self
            .adapter
            .search_loaded(&mut replica.engine, needle, max_matches)
            .map_err(KernelError::Engine)?;
        let registration = matches.iter().try_for_each(|found| {
            replica
                .history
                .register_anchor_pages(found.start, std::iter::empty())?;
            replica
                .history
                .register_anchor_pages(found.end, std::iter::empty())
        });
        if let Err(error) = registration {
            for found in &matches {
                self.adapter
                    .release_document_anchor(&mut replica.engine, found.start);
                self.adapter
                    .release_document_anchor(&mut replica.engine, found.end);
                replica.history.remove_anchor(found.start);
                replica.history.remove_anchor(found.end);
            }
            return Err(KernelError::HistoryCache(error));
        }
        Ok(matches)
    }

    /// Format a selection through the engine's canonical text semantics.
    pub fn format_document_selection(
        &self,
        terminal_id: &TerminalId,
        selection: EngineDocumentSelection,
    ) -> Result<Option<String>, KernelError<E::Error>> {
        let replica = self
            .terminals
            .get(terminal_id)
            .and_then(|state| state.published.as_ref())
            .ok_or_else(|| KernelError::UnknownTerminal(terminal_id.clone()))?;
        self.adapter
            .format_selection(&replica.engine, selection)
            .map_err(KernelError::Engine)
    }
}

const fn generation_of(key: &ReplicaKey) -> GenerationId {
    GenerationId {
        stream_id: key.stream_id,
        bootstrap_id: key.bootstrap_id,
    }
}

fn mismatch_error<E>(terminal_id: &TerminalId, generation: GenerationId) -> KernelError<E> {
    KernelError::GenerationMismatch {
        terminal_id: terminal_id.clone(),
        stream_id: generation.stream_id,
        bootstrap_id: generation.bootstrap_id,
    }
}

fn retired_error<E>(terminal_id: &TerminalId, generation: GenerationId) -> KernelError<E> {
    KernelError::RetiredGeneration {
        terminal_id: terminal_id.clone(),
        stream_id: generation.stream_id,
        bootstrap_id: generation.bootstrap_id,
    }
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

#[cfg(test)]
mod kernel_rig;

#[cfg(test)]
mod property_tests;

#[cfg(test)]
mod tests;
