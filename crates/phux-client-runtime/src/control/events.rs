//! The owned events a consumer drains from the control plane.

use std::sync::Arc;

use phux_client_core::engine::ClipboardText;
use phux_client_core::history::HistoryStatus;
use phux_client_core::org_ops::OrgOutcome;
use phux_client_core::session::AgentStreamInfo;
pub use phux_client_core::session::agent_stream::{AgentPublication, AgentRecordMode};
use phux_client_core::session::{HistoryUnavailableReason, KernelSend};
use phux_protocol::ClientId;
use phux_protocol::ResourceId;
use phux_protocol::ids::{ApprovalId, OperationId};
use phux_protocol::wire::frame::{
    CloseReason, CommandResult, ControlAction, DetachReason, ErrorCode, FrameKind, TombstoneReason,
};
use phux_protocol::wire::info::SessionSnapshot;

use crate::approvals::{Decision, DecisionOutcome};

mod observation;

pub use observation::Observation;

/// Where the session is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// No connection has been opened yet.
    Idle,
    /// Dialing, or waiting out the reconnect ladder.
    Connecting,
    /// `HELLO_OK` accepted; no attach has completed on this connection.
    Negotiated,
    /// The attach barrier released; frames are flowing.
    Attached,
    /// The consumer ended the session, or the server detached it at the
    /// consumer's request. Terminal.
    Closed,
    /// A refusal no retry can satisfy, or the initial ladder ran out.
    /// Terminal; the message is in `last_error`.
    Failed,
}

impl Status {
    /// Whether no further transition can happen.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Closed | Self::Failed)
    }
}

/// How one acknowledged input operation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// The server acknowledged the write.
    Delivered,
    /// Nothing was written; retyping is safe.
    Refused,
    /// Some, all, or none of the bytes may have landed; read the pane
    /// before retyping.
    Unknown,
}

/// The catalog's facts about an `AgentSession` resource (ADR-0103).
///
/// The Terminal it belongs to and its provider identity, as listed when the
/// runtime subscribed it. Every field is `None` for a stream a binding
/// subscribed itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentSessionInfo {
    /// The Terminal the session runs in.
    pub parent: Option<ResourceId>,
    /// The provider, for example `claude`.
    pub provider: Option<String>,
    /// The provider's own opaque session id.
    pub native_id: Option<String>,
}

/// A state change the control plane surfaces, drained in order through
/// `take_events`. Every event is owned: no field borrows the control plane.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Event {
    /// The session status changed.
    StatusChanged(Status),
    /// The session graph changed (a fresh `ATTACHED`, a `GET_STATE`
    /// refresh, or a close); re-read the topology.
    TopologyChanged,
    /// The organization replica changed (a seed, a journal event, a
    /// refetch); re-read `organization()` and the topology.
    OrganizationChanged,
    /// The organization records could not be read.
    OrganizationUnavailable {
        /// What the server answered.
        message: String,
    },
    /// A submitted organization operation resolved. A resend after a
    /// reconnect reused its id; [`OrgOutcome::Unknown`] means reconcile from
    /// the replica before acting again.
    OrganizationOperation {
        /// The operation.
        operation: OperationId,
        /// How it ended.
        outcome: OrgOutcome,
    },
    /// The complete wire snapshot behind a topology change.
    ///
    /// Runtime-owned extensions use the facets not carried by the public
    /// topology projection (layout metadata, resource kinds, and session
    /// attributes) without decoding the same reply a second time.
    TopologySnapshot {
        /// `Some` for an `ATTACHED` barrier, `None` for a `GET_STATE` read.
        attach_id: Option<u32>,
        /// The owned graph snapshot.
        snapshot: SessionSnapshot,
    },
    /// The attach barrier released for `attach_id`.
    Attached {
        /// The attach correlation.
        attach_id: u32,
    },
    /// The terminal's published frame (or, headless, its retained bytes)
    /// changed since the last drain. One per terminal per drain.
    TerminalChanged {
        /// The terminal.
        terminal_id: ResourceId,
    },
    /// A terminal appeared mid-session; the topology refresh that lists it
    /// follows.
    PaneSpawned {
        /// The new terminal.
        terminal_id: ResourceId,
    },
    /// The server answered one of this client's spawns. Exactly one of
    /// `terminal_id` and `error` is set.
    TerminalSpawned {
        /// The spawn correlation.
        request_id: u32,
        /// The spawned terminal.
        terminal_id: Option<ResourceId>,
        /// Why the spawn failed.
        error: Option<String>,
    },
    /// The server answered a per-terminal attach.
    TerminalAttached {
        /// The command correlation.
        request_id: u32,
        /// The terminal.
        terminal_id: ResourceId,
        /// Why the attach failed, if it did.
        error: Option<String>,
    },
    /// The server answered a per-terminal detach.
    TerminalDetached {
        /// The command correlation.
        request_id: u32,
        /// The terminal.
        terminal_id: ResourceId,
        /// Why the detach failed, if it did.
        error: Option<String>,
    },
    /// The server answered a kill.
    TerminalKilled {
        /// The command correlation.
        request_id: u32,
        /// The terminal.
        terminal_id: ResourceId,
        /// Why the kill failed, if it did.
        error: Option<String>,
    },
    /// The server answered a `CLOSE_TAB_RESOURCES` batch.
    TerminalsClosed {
        /// The command correlation.
        request_id: u32,
        /// Why the close failed, if it did.
        error: Option<String>,
    },
    /// A terminal closed (process exit, signal, kill, or it vanished from a
    /// fresh topology).
    TerminalClosed {
        /// The terminal.
        terminal_id: ResourceId,
        /// The process exit code, or `None` for signals and unknown.
        exit_status: Option<i32>,
        /// The terminating signal, if any.
        signal: Option<i32>,
        /// Why it closed; `Unknown` when inferred rather than announced.
        reason: CloseReason,
    },
    /// The terminal's process exited; the resource may be retained
    /// (ADR-0124). Reported at most once per terminal by the kernel.
    Exited {
        /// The terminal.
        terminal_id: ResourceId,
        /// The process exit code, or `None` for signals and unknown.
        exit_status: Option<i32>,
        /// The terminating signal, if any.
        signal: Option<i32>,
        /// Why it exited.
        reason: CloseReason,
    },
    /// The terminal rang its bell.
    Bell {
        /// The terminal.
        terminal_id: ResourceId,
    },
    /// A program requested a clipboard write from live output (OSC 52).
    /// The host must discard it unless this terminal is focused in the
    /// foreground, and must never retain it for later delivery.
    ClipboardWrite {
        /// The source terminal.
        terminal_id: ResourceId,
        /// Bounded UTF-8 text, redacted by `Debug`.
        text: ClipboardText,
    },
    /// The terminal's title changed; the topology entry is updated too.
    TitleChanged {
        /// The terminal.
        terminal_id: ResourceId,
        /// The new title.
        title: String,
    },
    /// An output burst began.
    OutputStarted {
        /// The terminal.
        terminal_id: ResourceId,
    },
    /// Output settled.
    OutputSettled {
        /// The terminal.
        terminal_id: ResourceId,
    },
    /// A shell command began (OSC 133 C).
    CommandStarted {
        /// The terminal.
        terminal_id: ResourceId,
    },
    /// A shell command finished (OSC 133 D).
    CommandFinished {
        /// The terminal.
        terminal_id: ResourceId,
        /// The exit code the shell reported, if any.
        exit_code: Option<i32>,
    },
    /// The terminal's working directory changed; the topology entry is
    /// updated too.
    CwdChanged {
        /// The terminal.
        terminal_id: ResourceId,
        /// The new working directory.
        cwd: String,
    },
    /// The terminal's input lease changed hands or was restated
    /// (`TerminalControl`, ADR-0033). `holder` is `None` while the wheel is
    /// free; `mine` says this connection holds it.
    InputHolderChanged {
        /// The terminal.
        terminal_id: ResourceId,
        /// The client holding the lease, or `None` when open.
        holder: Option<ClientId>,
        /// Whether `holder` is this connection.
        mine: bool,
        /// What just happened to the lease.
        action: ControlAction,
    },
    /// The current agent declaration, from a fenced read or a live update.
    /// The binding owns interpretation of the record; `None` retracts it.
    AgentMetadata {
        /// The terminal.
        terminal_id: ResourceId,
        /// The `phux.agent/v1` record, or its absence.
        value: Option<Vec<u8>>,
    },
    /// The one stored `phux.session.project/v1` tag.
    ///
    /// `project: None` clears it. The server keeps a single global value, so
    /// a new tag replaces the previous session's tag.
    SessionProject {
        /// Session name the tag belongs to. Empty when the tag was cleared.
        name: String,
        /// The project tag, or `None` when it was removed.
        project: Option<String>,
    },
    /// Whether a question is pending in the terminal now: the server-owned
    /// `phux.agent.asked/v1` flag (L3.md §1.3), from a fenced read or a live
    /// change, re-read after every gap and reconnect.
    ///
    /// `asked: false` retracts every earlier [`Event::AgentAsked`] for the
    /// terminal: no question is pending, so none may be shown or resurrected.
    /// `asked: true` says a question is pending without identifying it: the
    /// latest `AgentAsked` is its best description, and after a gap or a
    /// reconnect that description may predate the pending question.
    AgentAskedState {
        /// The terminal.
        terminal_id: ResourceId,
        /// Whether any question is pending.
        asked: bool,
    },
    /// An agent in the terminal is waiting on a human answer. An announcement
    /// edge; [`Event::AgentAskedState`] is the level that retracts it.
    AgentAsked {
        /// The terminal.
        terminal_id: ResourceId,
        /// The question's id.
        question_id: String,
        /// The question.
        text: String,
        /// Suggested answers.
        suggestions: Vec<String>,
        /// How long it has been waiting, if known.
        waiting_seconds: Option<u64>,
    },
    /// Progressive-history loading status for a terminal.
    History {
        /// The terminal.
        terminal_id: ResourceId,
        /// The cache's presentation state.
        status: HistoryStatus,
    },
    /// One history cursor chain ended; live state stays valid.
    HistoryUnavailable {
        /// The terminal.
        terminal_id: ResourceId,
        /// Why.
        reason: HistoryUnavailableReason,
    },
    /// A replica generation was invalidated; the connection driver
    /// reconnects for fresh snapshots, and a sans-IO consumer must do the
    /// same.
    ResyncRequired {
        /// The terminal.
        terminal_id: ResourceId,
        /// The tombstone reason.
        reason: TombstoneReason,
    },
    /// One committed `AgentSession` publication, in commit order.
    ///
    /// `publication` is an [`Arc`] the runtime retains until an accepting
    /// drain (`take_events`) or, after [`take_events_leased`](super::ControlPlane::take_events_leased),
    /// [`ControlPlane::accept_agent_publication`](super::ControlPlane::accept_agent_publication).
    /// Acceptance is the consumer taking delivery, not the end of a view
    /// borrow. The record vector is not copied. `stream` on the publication
    /// is the commit watermark, including an empty publication.
    AgentPublication {
        /// The committed batch and its watermark.
        publication: Arc<AgentPublication>,
        /// The resource's parent and provider identity.
        session: AgentSessionInfo,
    },
    /// An `AgentSession` stream the runtime subscribed
    /// (`ControlOptions::subscribe_agent_sessions`) ended: the server
    /// closed the resource, a topology read no longer lists it, its pane
    /// was detached, or the server restarted as a new incarnation. Exactly
    /// once per subscription; no records follow, and a later server close
    /// of the same id never surfaces as [`Event::TerminalClosed`].
    ///
    /// `stream` is the last committed watermark, captured before detach or
    /// deletion. `None` when nothing had committed.
    AgentSessionClosed {
        /// The `AgentSession` resource.
        terminal_id: ResourceId,
        /// The resource's parent and provider identity.
        session: AgentSessionInfo,
        /// Authoritative committed stream, if one had published.
        stream: Option<AgentStreamInfo>,
    },
    /// An SDK intent resolved, independently of terminal input delivery.
    /// Queued is submission only; Unknown must never trigger automatic replay.
    NativeAgentResult {
        /// The exact `AgentSession` resource.
        agent_session_id: ResourceId,
        /// The identity returned by `native_agent_command`.
        operation_id: u64,
        /// The factual admission or completion stage.
        disposition: phux_protocol::native_agent::NativeDisposition,
        /// Provider or broker detail, preserved without guessing a stage.
        detail: Option<String>,
    },
    /// One acknowledged input operation resolved. Lossless: never dropped
    /// by the queue cap.
    InputDelivery {
        /// The correlation `apply_*` returned.
        delivery_id: u64,
        /// How it ended.
        outcome: DeliveryOutcome,
        /// The server's error code, when it refused.
        code: Option<u16>,
        /// Diagnostic detail.
        message: String,
    },
    /// One engine send for a manually driven binding to fence and encode.
    KernelSend(KernelSend),
    /// The reply to a command a binding sent through the extension point
    /// (`send_command`). Lossless.
    CommandResult {
        /// The command correlation.
        request_id: u32,
        /// The reply.
        result: CommandResult,
    },
    /// A frame the control plane does not consume, for a binding's own
    /// handlers: metadata changes and values, directory listings, moves,
    /// and every kind a later rung adds.
    Frame(Box<FrameKind>),
    /// The server sent an `ERROR` frame.
    ServerError {
        /// The code.
        code: ErrorCode,
        /// The message.
        message: String,
        /// The request the error answers, if any.
        request_id: Option<u32>,
    },
    /// The server ended the attach.
    Detached {
        /// The stated reason; `None` when unstated.
        reason: Option<DetachReason>,
        /// The message.
        message: String,
    },
    /// A connection ended and the ladder will retry.
    ConnectionLost {
        /// Why, when known.
        message: Option<String>,
    },
    /// The pending set of server-held approvals changed (ADR-0128); re-read
    /// `approvals()`. Only after `watch_approvals`.
    ApprovalsChanged,
    /// One decision `decide_approval` sent resolved. Lossless, and never
    /// retried: a [`DecisionOutcome::Stale`] decision lost a race with
    /// another decider, the expiry, or a withdrawal.
    ApprovalDecision {
        /// The approval decided.
        id: ApprovalId,
        /// What was sent.
        decision: Decision,
        /// How it ended.
        outcome: DecisionOutcome,
    },
    /// A transport opened: connection `connection_epoch` begins. Every later
    /// event belongs to it, every earlier one to a previous connection, even
    /// when the new connection reuses the old session and terminal ids.
    /// Lossless, so a reconnect is visible after a queue overflow.
    ConnectionOpened {
        /// The new [`Observation::connection_epoch`].
        connection_epoch: u64,
    },
}
