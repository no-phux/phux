//! History availability reasons and the kernel's protocol/adapter error.

use super::{
    AgentRecordError, BootstrapId, BootstrapProfile, BootstrapProgress, BootstrapStreamProfile,
    HistoryCacheError, InputBlockReason, ResourceId, ResourceKind, StreamId,
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
    DuplicateAttachResource(ResourceId),
    /// An event named a permanently closed terminal.
    #[error("terminal {0} is closed")]
    ClosedTerminal(ResourceId),
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
    UnknownTerminal(ResourceId),
    /// The exact stream/bootstrap generation is not current.
    #[error("generation ({stream_id}, {bootstrap_id}) is not current for {terminal_id}")]
    GenerationMismatch {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Received logical subscription.
        stream_id: StreamId,
        /// Received generation.
        bootstrap_id: BootstrapId,
    },
    /// The exact stream/bootstrap generation was permanently retired.
    #[error("generation ({stream_id}, {bootstrap_id}) is retired for {terminal_id}")]
    RetiredGeneration {
        /// Target terminal.
        terminal_id: ResourceId,
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
        terminal_id: ResourceId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replica generation.
        bootstrap_id: BootstrapId,
    },
    /// A publication fence was reached without its staging replica.
    #[error("terminal {0} has no staging replica to publish")]
    MissingStaging(ResourceId),
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
    /// Aggregate pre-READY staging exceeded its byte, chunk, or lifetime cap.
    #[error("bootstrap staging limit exceeded")]
    BootstrapStagingLimitExceeded,
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
    /// A live `AgentSession` frame did not contain any records.
    #[error("empty live agent record batch")]
    EmptyAgentBatch,
    /// A resumed agent bootstrap has no committed generation to anchor to.
    #[error("agent resume has no committed generation for {0}")]
    AgentResumeUnavailable(ResourceId),
    /// The frontend attempted input while its target was ineligible.
    #[error("input is not eligible for {terminal_id}: {reason:?}")]
    InputIneligible {
        /// Explicit input target.
        terminal_id: ResourceId,
        /// Deterministic gate reason.
        reason: InputBlockReason,
    },
    /// The terminal engine rejected an operation.
    #[error("terminal engine failed: {0}")]
    Engine(#[source] E),
    /// A frame's kind disagreed with the resource's declared kind.
    #[error("resource {terminal_id} is {declared:?}, frame implies {incoming:?}")]
    KindMismatch {
        /// Target resource.
        terminal_id: ResourceId,
        /// Kind the resource was declared or bootstrapped as.
        declared: ResourceKind,
        /// Kind the rejected frame implies.
        incoming: ResourceKind,
    },
    /// A resource kind this build cannot host.
    #[error("resource {terminal_id} has unsupported kind {kind:?}")]
    UnsupportedKind {
        /// Target resource.
        terminal_id: ResourceId,
        /// The declared kind.
        kind: ResourceKind,
    },
    /// An `AgentSession` stream payload was not well-formed JSONL.
    #[error("agent session stream failed: {0}")]
    AgentRecord(#[from] AgentRecordError),
}
