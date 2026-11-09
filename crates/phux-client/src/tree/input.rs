//! Input intents for a document (`SEND_DOCUMENT_INPUT`): a viewer sends one,
//! and the server relays it to the connection producing the document, which
//! receives it as a `document_input` event alongside its publishes.

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{AgentEvent, Command, CommandResult, ErrorCode, FrameKind};
pub use phux_protocol::wire::frame::{DocumentInput, DocumentKey};

use phux_client_core::document_input::copied_text;

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

/// Who is driving `resource`, when `frame` is the `input_activity` the
/// server sends a producer ahead of each intent: the viewer's HELLO name.
#[must_use]
pub fn activity<'a>(frame: &'a FrameKind, resource: &ResourceId) -> Option<&'a str> {
    match frame {
        FrameKind::Event {
            terminal: Some(at),
            event: AgentEvent::InputActivity { origin },
            ..
        } if at == resource => Some(origin),
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
    send(conn, resource, input, &mut Vec::new()).await.map(drop)
}

/// The host clipboard's text, from the producer of `resource`.
///
/// # Errors
///
/// As [`send_input`], and the producer's reason when it could not read
/// the clipboard.
pub async fn copy_clipboard(
    conn: &mut Connection,
    resource: &ResourceId,
) -> Result<String, TreeError> {
    let input = DocumentInput::CopyClipboard { reply: 0 };
    send(conn, resource, input, &mut Vec::new())
        .await?
        .ok_or_else(|| AttachError::Protocol("the copy answered without text".to_owned()).into())
}

/// Answer copy `reply` on `resource`, which this connection produces, with
/// the host clipboard's `text` or why it could not be read. Intents that
/// arrive meanwhile are kept in `intents`.
///
/// # Errors
///
/// The server's refusal (the copy gave up waiting), or a connection failure.
pub async fn answer_copy(
    conn: &mut Connection,
    resource: &ResourceId,
    reply: u32,
    text: Result<String, String>,
    intents: &mut Vec<Intent>,
) -> Result<(), TreeError> {
    let input = DocumentInput::ClipboardText { reply, text };
    send(conn, resource, input, intents).await.map(drop)
}

async fn send(
    conn: &mut Connection,
    resource: &ResourceId,
    input: DocumentInput,
    intents: &mut Vec<Intent>,
) -> Result<Option<String>, TreeError> {
    require_support(conn)?;
    let (result, interleaved) = conn
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
    intents.extend(interleaved.iter().filter_map(intent));
    match result {
        CommandResult::Ok => Ok(None),
        CommandResult::OkWith(_) => Ok(copied_text(&result)),
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
