//! Input intents for a document (`SEND_DOCUMENT_INPUT`): a viewer sends one,
//! and the server relays it to the connection producing the document, which
//! receives it as a `document_input` event alongside its publishes.

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{AgentEvent, Command, CommandResult, ErrorCode, FrameKind};
pub use phux_protocol::wire::frame::{DocumentInput, DocumentKey};

use super::{TreeError, map_refusal, require_support};
use crate::attach::AttachError;
use crate::attach::connection::Connection;

const REQUEST_INPUT: u32 = 6;

/// One intent a viewer sent to a document this connection produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    /// The document it names (a page's tree or surface).
    pub resource: ResourceId,
    /// What the viewer asked for.
    pub input: DocumentInput,
}

/// The intent `frame` carries, when it is a relayed `document_input`.
#[must_use]
pub fn intent(frame: &FrameKind) -> Option<Intent> {
    match frame {
        FrameKind::Event {
            terminal: Some(resource),
            event: AgentEvent::DocumentInput { intent },
            ..
        } => Some(Intent {
            resource: resource.clone(),
            input: intent.clone(),
        }),
        _ => None,
    }
}

/// Send `input` to the producer of `resource`.
///
/// # Errors
///
/// [`TreeError::NoProducer`] when nothing produces the document now, the
/// server's other refusals, or a connection failure.
pub async fn send_input(
    conn: &mut Connection,
    resource: &ResourceId,
    input: DocumentInput,
) -> Result<(), TreeError> {
    require_support(conn)?;
    let (result, _) = conn
        .request(
            REQUEST_INPUT,
            Command::SendDocumentInput {
                terminal_id: resource.clone(),
                intent: input,
                control: None,
            },
        )
        .await?
        .into_parts();
    match result {
        CommandResult::Ok | CommandResult::OkWith(_) => Ok(()),
        CommandResult::Error {
            code: ErrorCode::NotProducer,
            message,
        } => Err(TreeError::NoProducer(message)),
        CommandResult::Error { code, message } => Err(map_refusal(resource, code, message)),
        other => Err(AttachError::Protocol(format!(
            "SEND_DOCUMENT_INPUT answered with an unrecognised result: {other:?}"
        ))
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_relayed_document_input_is_an_intent() {
        let resource = ResourceId::local(9);
        let input = DocumentInput::Type { text: "hi".into() };
        let relayed = FrameKind::Event {
            terminal: Some(resource.clone()),
            event: AgentEvent::DocumentInput {
                intent: input.clone(),
            },
            stamp: None,
        };
        assert_eq!(intent(&relayed), Some(Intent { resource, input }));
        let other = FrameKind::Event {
            terminal: None,
            event: AgentEvent::Bell,
            stamp: None,
        };
        assert_eq!(intent(&other), None);
    }
}
