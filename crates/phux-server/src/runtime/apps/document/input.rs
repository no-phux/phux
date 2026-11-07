//! `SEND_DOCUMENT_INPUT`: relay a viewer's typed intent to the connection
//! producing a document. The server reads none of it; the producer maps it
//! to its source (`docs/resource-kinds.md#producers`).

use phux_protocol::wire::frame::{AgentEvent, CommandResult, DocumentInput, ErrorCode, FrameKind};
use phux_tree::DocumentResource as _;

use super::DocumentApi;
use crate::mailbox::Outbound;
use crate::state::SharedState;

/// Queue `intent` on the producer's connection as an unjournaled
/// `document_input` event. The producer is the connection that spawned the
/// document; once it is gone the intent is refused, never held.
pub(super) fn relay<A: DocumentApi>(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    intent: DocumentInput,
) -> CommandResult {
    let found = state.with(|s| {
        let core = s.resource_from_wire(terminal_id)?;
        let handle = s.resource_handle(core)?.clone();
        let producer = s.resource_producer(core);
        Some((handle, producer))
    });
    let Some((handle, producer)) = found else {
        return CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: format!("no such resource: {terminal_id}"),
        };
    };
    if let Err(error) = handle.document::<A>() {
        return crate::runtime::commands::wrong_resource_kind(error);
    }
    let not_connected = || CommandResult::Error {
        code: ErrorCode::NotProducer,
        message: format!("the {}'s producer is not connected", A::LABEL),
    };
    let Some(producer) = producer else {
        return not_connected();
    };
    let event = FrameKind::Event {
        terminal: Some(terminal_id.clone()),
        event: AgentEvent::DocumentInput { intent },
        stamp: None,
    };
    match producer.try_send(Outbound::Frame(event)) {
        Ok(()) => CommandResult::Ok,
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => CommandResult::Error {
            code: ErrorCode::Overflow,
            message: format!("the {}'s producer is not keeping up", A::LABEL),
        },
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => not_connected(),
    }
}
