//! Drain sampling: the [`Observation`] and the per-event queue and graph predicates.

use super::{Event, Status};

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
    /// The session graph after the last event, when this drain sampled it.
    ///
    /// [`crate::control::ControlPlane::take_observation`] always samples the
    /// graph. [`crate::control::ControlPlane::take_activity`] samples it when
    /// the batch can change the graph, and leaves it empty for terminal
    /// damage, input delivery, agent metadata, and session project tags.
    pub topology: Option<crate::control::Topology>,
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
                | Self::OrganizationOperation { .. }
                | Self::InputDelivery { .. }
                | Self::NativeAgentResult { .. }
                | Self::CommandResult { .. }
                | Self::InputHolderChanged { .. }
                | Self::AgentPublication { .. }
                | Self::AgentSessionClosed { .. }
                | Self::ApprovalsChanged
                | Self::ApprovalDecision { .. }
                | Self::ConnectionOpened { .. }
        )
    }

    /// Whether a drain that contains this event has to sample the session graph.
    ///
    /// Terminal damage, input delivery, agent metadata, and a session project
    /// tag leave the graph a caller already holds unchanged.
    #[must_use]
    pub const fn changes_session_graph(&self) -> bool {
        !matches!(
            self,
            Self::TerminalChanged { .. }
                | Self::ClipboardWrite { .. }
                | Self::InputDelivery { .. }
                | Self::NativeAgentResult { .. }
                | Self::AgentMetadata { .. }
                | Self::SessionProject { .. }
                | Self::ApprovalsChanged
                | Self::ApprovalDecision { .. }
        )
    }
}
