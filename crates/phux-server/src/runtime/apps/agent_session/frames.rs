//! Client frames about an `AgentSession`. It owns none: its stream is
//! raw-profile with no history beyond its bootstrap (ADR-0103 §4) and it takes
//! no input (L1 §1.1). Each refusal keeps the connection.

use phux_protocol::wire::frame::{ErrorCode, FrameKind};

use crate::resource::{ResourceKind, WrongResourceKind};
use crate::runtime::apps::ResourceFrame;
use crate::runtime::connections::dispatch::FrameFlow;
use crate::state::Outbound;

/// Refuse one client frame about an `AgentSession`. A refusal never waits on
/// the mailbox: a full one drops it, so a flood cannot stall this read loop.
pub(super) fn serve(request: &ResourceFrame<'_>, frame: &FrameKind) -> FrameFlow {
    let (code, message) = match frame {
        FrameKind::FrameAck { .. } => (
            ErrorCode::MalformedMessage,
            "FRAME_ACK is not valid on an agent-session stream".to_owned(),
        ),
        FrameKind::HistoryRequest { .. } => (
            ErrorCode::WrongResourceKind,
            "an agent-session stream retains no history beyond its bootstrap".to_owned(),
        ),
        // The host already checked input subscription and revocation.
        // RESIZE_TERMINAL needs no subscription.
        _ => wrong_kind(),
    };
    let _ = request
        .plumbing
        .out_tx
        .try_send(Outbound::Frame(FrameKind::Error {
            request_id: None,
            code,
            message,
        }));
    FrameFlow::Continue(())
}

/// The refusal of a Terminal-facet frame.
fn wrong_kind() -> (ErrorCode, String) {
    let error = WrongResourceKind {
        required: ResourceKind::Terminal,
        actual: ResourceKind::AgentSession,
    };
    (ErrorCode::WrongResourceKind, error.to_string())
}
