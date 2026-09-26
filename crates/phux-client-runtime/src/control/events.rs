//! The owned events a consumer drains from the control plane.

use phux_client_core::history::HistoryStatus;
use phux_client_core::session::agent_stream::AgentEventRecord;
use phux_client_core::session::{HistoryUnavailableReason, KernelSend};
use phux_protocol::ResourceId;
use phux_protocol::wire::frame::{
    CloseReason, CommandResult, DetachReason, ErrorCode, FrameKind, TombstoneReason,
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
    /// An agent in the terminal is waiting on a human answer.
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
                | Self::AgentRecords { .. }
        )
    }
}
