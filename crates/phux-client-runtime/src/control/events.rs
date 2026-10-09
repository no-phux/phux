//! The owned events a consumer drains from the control plane.

use phux_client_core::history::HistoryStatus;
use phux_client_core::session::agent_stream::AgentEventRecord;
use phux_client_core::session::{HistoryUnavailableReason, KernelSend};
use phux_protocol::ClientId;
use phux_protocol::ResourceId;
use phux_protocol::wire::frame::{
    CloseReason, CommandResult, ControlAction, DetachReason, ErrorCode, FrameKind, TombstoneReason,
};
use phux_protocol::wire::info::SessionSnapshot;

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
    /// Decoded records appended to an `AgentSession` stream, in order.
    AgentRecords {
        /// The `AgentSession` resource.
        terminal_id: ResourceId,
        /// The records.
        records: Vec<AgentEventRecord>,
        /// `true` when `records` is a generation's whole retained stream,
        /// delivered once when it publishes (after a subscription, a
        /// reconnect, or a resync), and replaces every record held for the
        /// resource. `false` for one live output frame appended to it.
        retained: bool,
        /// The resource's parent and provider identity.
        session: AgentSessionInfo,
    },
    /// An `AgentSession` stream the runtime subscribed
    /// (`ControlOptions::subscribe_agent_sessions`) ended: the server
    /// closed the resource, a topology read no longer lists it, its pane
    /// was detached, or the server restarted as a new incarnation. Exactly
    /// once per subscription; no records follow, and a later server close
    /// of the same id never surfaces as [`Event::TerminalClosed`].
    AgentSessionClosed {
        /// The `AgentSession` resource.
        terminal_id: ResourceId,
        /// The resource's parent and provider identity.
        session: AgentSessionInfo,
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
    /// A transport opened: connection `connection_epoch` begins. Every later
    /// event belongs to it, every earlier one to a previous connection, even
    /// when the new connection reuses the old session and terminal ids.
    /// Lossless, so a reconnect is visible after a queue overflow.
    ConnectionOpened {
        /// The new [`Observation::connection_epoch`].
        connection_epoch: u64,
    },
}

/// The drained events plus the state they lead to, sampled in one step.
///
/// A polling binding renders from the ordered events since the last drain
/// and, from the same instant, the status, the failure message and the
/// topology.
///
/// Sampling these separately (`take_events`, then `status`, then
/// `topology`) can straddle a reconnect that completed between the calls;
/// one observation cannot.
#[derive(Debug, Clone)]
pub struct Observation {
    /// The connection incarnation the sample was taken on: bumped by every
    /// transport the session opens, and never reused. Two observations with
    /// the same epoch saw no reconnect between them.
    pub connection_epoch: u64,
    /// Every event since the last drain, in order. An
    /// [`Event::ConnectionOpened`] inside marks where a reconnect happened.
    pub events: Vec<Event>,
    /// The queue cap dropped droppable events since the last drain
    /// ([`crate::control::EVENT_QUEUE_CAP`]). Lossless events and the
    /// latest `ConnectionOpened` survive; a `TopologyChanged` follows the
    /// loss, and agent declarations and asked flags are re-read, so levels
    /// recover through later events rather than this batch.
    pub events_dropped: bool,
    /// Where the session is, after the last event.
    pub status: Status,
    /// The last failure message, after the last event.
    pub last_error: Option<String>,
    /// The session graph, after the last event.
    pub topology: Option<super::Topology>,
}

impl Event {
    /// Whether the queue cap may drop this event. Correlated replies and
    /// receipts resolve one user action exactly once and are never dropped.
    #[must_use]
    pub const fn is_lossless(&self) -> bool {
        matches!(
            self,
            Self::TerminalSpawned { .. }
                | Self::TerminalAttached { .. }
                | Self::TerminalDetached { .. }
                | Self::TerminalKilled { .. }
                | Self::TerminalsClosed { .. }
                | Self::InputDelivery { .. }
                | Self::CommandResult { .. }
                | Self::InputHolderChanged { .. }
                | Self::AgentRecords { .. }
                | Self::AgentSessionClosed { .. }
                | Self::ConnectionOpened { .. }
        )
    }
}
