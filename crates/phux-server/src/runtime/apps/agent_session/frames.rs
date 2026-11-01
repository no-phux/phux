//! Client frames about an `AgentSession`. It owns none: its stream is
//! raw-profile with no history beyond its bootstrap (ADR-0103 §4) and it takes
//! no input (L1 §1.1). Each refusal keeps the connection.

use phux_protocol::wire::frame::{ErrorCode, FrameKind};

use crate::resource::{ResourceKind, WrongResourceKind};
use crate::runtime::apps::ResourceFrame;
use crate::runtime::connections::dispatch::FrameFlow;
use crate::runtime::connections::frames::direction_invalid;

/// Refuse one client frame about an `AgentSession`.
pub(super) async fn serve(request: ResourceFrame<'_>, frame: FrameKind) -> FrameFlow {
    let (code, message) = match &frame {
        FrameKind::FrameAck { .. } => (
            ErrorCode::MalformedMessage,
            "FRAME_ACK is not valid on an agent-session stream".to_owned(),
        ),
        FrameKind::HistoryRequest { .. } => (
            ErrorCode::WrongResourceKind,
            "an agent-session stream retains no history beyond its bootstrap".to_owned(),
        ),
        FrameKind::ResizeTerminal { .. } => wrong_kind(),
        FrameKind::InputKey { terminal_id, .. }
        | FrameKind::InputMouse { terminal_id, .. }
        | FrameKind::InputFocus { terminal_id, .. }
        | FrameKind::InputPaste { terminal_id, .. }
        | FrameKind::InputTerminalReply { terminal_id, .. } => {
            // Input from a revoked or unsubscribed connection earns no
            // answer, on any kind.
            let answered = request.state.with(|s| {
                !s.connection_revoked(request.client_id)
                    && s.resource_from_wire(terminal_id).is_some_and(|core| {
                        s.subscribers_for_resource(core)
                            .contains(&request.client_id)
                    })
            });
            if !answered {
                return FrameFlow::Continue(());
            }
            wrong_kind()
        }
        other => return direction_invalid(request.client_id, other),
    };
    crate::runtime::send_error(&request.plumbing.out_tx, code, &message).await;
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
