//! Client frames about an `AgentSession`. It owns none: its stream is
//! raw-profile with no history beyond its bootstrap (ADR-0103 §4) and it takes
//! no input. Each refusal keeps the connection.

use phux_protocol::wire::frame::{ErrorCode, FrameKind};

use crate::resource::{ResourceKind, WrongResourceKind};

/// Kind-owned validity facts; the host owns delivery and backpressure.
pub(super) fn error(frame: &FrameKind) -> (ErrorCode, String) {
    match frame {
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
    }
}

/// The refusal of a Terminal-facet frame.
fn wrong_kind() -> (ErrorCode, String) {
    let error = WrongResourceKind {
        required: ResourceKind::Terminal,
        actual: ResourceKind::AgentSession,
    };
    (ErrorCode::WrongResourceKind, error.to_string())
}
