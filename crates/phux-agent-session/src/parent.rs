//! The mailbox of a resource that hosts an agent.
//!
//! A kind that can run an agent adds [`AgentParent`] to its handle as a
//! companion, and the host reaches it with
//! [`ResourceHandle::companion`](phux_resource::ResourceHandle::companion)
//! without naming that kind's engine. A resource without one hosts no agent.

use phux_protocol::wire::frame::ReportedAgentState;
use tokio::sync::{mpsc, oneshot};

use crate::session::AppendRequest;

/// Depth of an [`AgentParent`] mailbox.
pub const AGENT_PARENT_MAILBOX: usize = 64;

/// Whether the parent accepted a report; `Err` carries why not.
pub type ReportReply = oneshot::Sender<Result<(), String>>;

/// A request to the resource an agent runs in.
#[derive(Debug)]
pub enum AgentParentRequest {
    /// Something else wrote `phux.agent/v1` (ADR-0046 §E): clear the
    /// detector's edge filter so it republishes once. No-op without a
    /// detector.
    RecordInvalidated,
    /// Bind the producer channel of an `AgentSession` child just spawned
    /// here (ADR-0103 §6). No reply.
    BindSession {
        /// The child engine's append channel.
        append: mpsc::Sender<AppendRequest>,
    },
    /// Feed an `AgentSession` child's state into the detector at the
    /// `Stream` rank (ADR-0103 §5); `None` is the `session_end` retraction.
    ReportStreamState {
        /// The derived state, or `None` to withdraw the stream's claim.
        state: Option<ReportedAgentState>,
        /// Whether the detector accepted the evidence.
        reply: ReportReply,
    },
    /// Feed lifecycle-hook evidence into the detector.
    ReportState {
        /// Hook-reported state.
        state: ReportedAgentState,
        /// Whether the detector accepted the evidence.
        reply: ReportReply,
    },
    /// Append hook-reported state as a synthesized record on the live
    /// `AgentSession` child (ADR-0103 §6); used only when one exists.
    SynthesizeStateRecord {
        /// Hook-reported state to append.
        state: ReportedAgentState,
        /// Whether the record was accepted.
        reply: ReportReply,
    },
}

/// The sender half of a resource's agent mailbox: a handle companion.
#[derive(Debug, Clone)]
pub struct AgentParent {
    /// Requests to the resource's engine.
    pub mailbox: mpsc::Sender<AgentParentRequest>,
}
