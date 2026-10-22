//! Normalized kernel inputs, frontend actions, and input eligibility.

#[cfg(doc)]
use super::SessionKernel;
use super::{
    AgentEvent, BootstrapId, BootstrapStreamProfile, CanonicalGeometry, CloseReason,
    HistoryRejectionReason, HistoryUnavailableReason, InputEvent, ResourceId, StreamId,
    TombstoneReason,
};

/// A normalized borrowed input to [`SessionKernel::update`].
#[derive(Debug)]
pub enum KernelInput<'a> {
    /// Start one aggregate attach and its first-damage barrier.
    AttachStarted {
        /// Client-chosen attach correlation identifier.
        attach_id: u32,
        /// Complete ordered terminal inventory for this attach.
        terminals: &'a [ResourceId],
    },
    /// Release the aggregate first-damage barrier.
    AttachReady {
        /// Correlation identifier from `ATTACHED`.
        attach_id: u32,
    },
    /// Begin staging a replacement replica.
    BootstrapBegin {
        /// Target terminal.
        terminal_id: &'a ResourceId,
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
        terminal_id: &'a ResourceId,
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
        terminal_id: &'a ResourceId,
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
        terminal_id: &'a ResourceId,
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
        terminal_id: &'a ResourceId,
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
        terminal_id: &'a ResourceId,
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
    ResourceOutput {
        /// Target terminal.
        terminal_id: &'a ResourceId,
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
        terminal_id: &'a ResourceId,
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
    ResourceClosed {
        /// Closed terminal.
        terminal_id: &'a ResourceId,
        /// Process exit code (`_exit(n)`), or `None` for signals / unknown,
        /// mirroring `RESOURCE_CLOSED.exit_status`.
        exit_status: Option<i32>,
        /// Terminating signal number, or `None` when the process exited
        /// without one or the cause is unknown, mirroring
        /// `RESOURCE_CLOSED.signal` (ADR-0124).
        signal: Option<i32>,
        /// Why the resource closed, mirroring `RESOURCE_CLOSED.reason`.
        reason: CloseReason,
    },
    /// Apply one subscribed [`AgentEvent`] delivered on `EVENT`
    /// (`docs/protocol.md#commands-events-resources`) to the frontend-status effects it can
    /// produce (cwd/command boundaries/process exit). Every event kind this
    /// build does not surface as status — including one this decoder does
    /// not recognise (`AgentEvent::Unknown`) — is a no-op.
    Event {
        /// The Terminal the event concerns.
        terminal_id: &'a ResourceId,
        /// The borrowed event payload.
        event: &'a AgentEvent,
    },
    /// Register one `AgentSession` resource ahead of its record stream.
    AgentSessionDeclared(AgentSessionDeclaration<'a>),
    /// Apply one explicit-terminal user action.
    Action(KernelAction<'a>),
}

/// Declared identity of one `AgentSession` resource, as the attach snapshot
/// or a spawn reply states it before any record arrives.
///
/// An `AgentSession` is never an attach-barrier participant: the barrier gates
/// the first paint, and a record stream paints nothing. Declaring one only
/// fixes its kind and seeds the identity its stream may later refine.
#[derive(Debug, Clone, Copy)]
pub struct AgentSessionDeclaration<'a> {
    /// The resource being declared.
    pub terminal_id: &'a ResourceId,
    /// The Terminal-kind parent the session is bound to.
    pub parent: Option<&'a ResourceId>,
    /// Provider slug from the facet, if any.
    pub provider: Option<&'a str>,
    /// Opaque provider session id from the facet, if any.
    pub native_id: Option<&'a str>,
    /// Server-derived state word from the facet, if any.
    pub state: Option<&'a str>,
}

/// A normalized borrowed user action.
#[derive(Debug)]
pub enum KernelAction<'a> {
    /// Send one structured input atom to an explicitly eligible terminal.
    Input {
        /// Target terminal selected by the frontend.
        terminal_id: &'a ResourceId,
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
    /// The resource is not a Terminal; only Terminal-kind resources accept
    /// input atoms.
    NotATerminal,
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
