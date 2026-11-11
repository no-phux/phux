//! Input to a document pane from a phone (`SEND_DOCUMENT_INPUT`).
//!
//! The control class rides on [`WireEvent::DocumentPublished`], so the
//! viewer holding a frame knows whether to offer input without a topology
//! join; the reply comes back as [`WireEvent::DocumentInputResult`]. The
//! letterbox math and the refusal codes are the desktop and Rune panes'
//! (`phux_client_core::document_input`).

use phux_client_core::document_input::{self as core_input, DocumentInputs};
use phux_protocol::wire::frame::{
    CommandResult, ControlClass, DocumentInput, ErrorCode, MAX_CLIPBOARD_TEXT,
    MAX_DOCUMENT_INPUT_TEXT,
};

use super::*;

/// The control classes the session snapshots declared, and the sends
/// awaiting a reply.
#[derive(Default)]
pub(super) struct DocumentInputState {
    inputs: DocumentInputs,
    pending: HashMap<u32, ResourceId>,
}

/// The frame pixel under a touch at (`x`, `y`) in a `view_width` by
/// `view_height` area that draws a `frame_width` by `frame_height` frame
/// contain-fit (aspect kept, centered). `None` in the letterbox bars,
/// outside the area, or for an empty frame or area.
#[uniffi::export]
#[must_use]
pub fn document_frame_point(
    view_width: f32,
    view_height: f32,
    frame_width: u32,
    frame_height: u32,
    x: f32,
    y: f32,
) -> Option<WireFramePoint> {
    let view = [0.0, 0.0, view_width, view_height];
    core_input::frame_point(view, [frame_width, frame_height], [x, y])
        .map(|[x, y]| WireFramePoint { x, y })
}

/// A pan as a [`WireDocumentIntent::Scroll`]. `pan_right`/`pan_down` is
/// how far the content would follow the finger, in view points, positive
/// right and down; the intent scrolls the page the other way, in frame
/// pixels. `None` when it rounds to nothing or the frame or area is empty.
#[uniffi::export]
#[must_use]
pub fn document_scroll(
    view_width: f32,
    view_height: f32,
    frame_width: u32,
    frame_height: u32,
    pan_right: f32,
    pan_down: f32,
) -> Option<WireDocumentIntent> {
    let scroll = core_input::scroll(
        [view_width, view_height],
        [frame_width, frame_height],
        [pan_right, pan_down],
    );
    match scroll? {
        DocumentInput::Scroll { dx, dy } => Some(WireDocumentIntent::Scroll { dx, dy }),
        _ => None,
    }
}

#[uniffi::export]
impl RemoteClient {
    /// Send `intent` to the producer of the document `resource_id`. Returns
    /// the request id its [`WireEvent::DocumentInputResult`] carries, or 0
    /// when nothing was sent: offline, an unknown id, a URL or text over
    /// the wire's 8 KiB, or a paste over 1 MiB.
    pub fn send_document_input(&self, resource_id: String, intent: WireDocumentIntent) -> u32 {
        let intent = DocumentInput::from(intent);
        let too_long = match &intent {
            DocumentInput::Navigate { url: text } | DocumentInput::Type { text } => {
                text.len() > MAX_DOCUMENT_INPUT_TEXT
            }
            DocumentInput::Paste { text, .. } => text.len() > MAX_CLIPBOARD_TEXT,
            _ => false,
        };
        if too_long {
            return 0;
        }
        let (Some(id), Some(client)) = (id::parse(&resource_id), self.runtime_client()) else {
            return 0;
        };
        let request_id = client.send_command(DocumentInputs::command(&id, intent));
        let mut state = self.document_input.lock().unwrap();
        state.pending.insert(request_id, id);
        request_id
    }
}

impl RemoteClient {
    /// Fold control classes in, stamp them on published documents, and
    /// settle input replies; any other event comes back unchanged.
    pub(super) fn project_document(
        &self,
        event: Event,
        projected: &mut Vec<WireEvent>,
    ) -> Option<Event> {
        if let Event::DocumentPublished { resource_id, .. } = &event {
            self.project_published_document(resource_id, projected);
            return None;
        }
        let mut state = self.document_input.lock().unwrap();
        match event {
            Event::TopologySnapshot { snapshot, .. } => state.inputs.note_snapshot(&snapshot),
            Event::CommandResult { request_id, result } => {
                let Some(resource) = state.pending.remove(&request_id) else {
                    return Some(Event::CommandResult { request_id, result });
                };
                let control = state.inputs.control(&resource);
                projected.push(WireEvent::DocumentInputResult {
                    request_id,
                    resource_id: id::encode(&resource),
                    outcome: outcome(&result, control),
                });
            }
            event => return Some(event),
        }
        None
    }

    /// The resource's newest document, read from the runtime's publication
    /// table; nothing once its slot is gone (closed, detached, replaced).
    fn project_published_document(&self, resource_id: &ResourceId, projected: &mut Vec<WireEvent>) {
        let Some(publication) = self
            .runtime_client()
            .and_then(|client| client.acquire_document(resource_id))
        else {
            return;
        };
        let control = self
            .document_input
            .lock()
            .unwrap()
            .inputs
            .control(resource_id);
        projected.push(WireEvent::DocumentPublished {
            resource_id: id::encode(resource_id),
            kind: crate::projection::topology::kind_word(publication.kind),
            seq: publication.seq,
            bytes: publication.bytes.clone(),
            content_type: publication.content_type.clone(),
            control: control.map(|class| class.name().to_owned()),
        });
    }
}

/// A reply as an outcome. A refused document with no control class was
/// never controllable; with one, the refusal is a missing grant.
fn outcome(result: &CommandResult, control: Option<ControlClass>) -> WireDocumentInputOutcome {
    if let Some(text) = phux_client_core::document_input::copied_text(result) {
        return WireDocumentInputOutcome::Copied { text };
    }
    let CommandResult::Error { code, message } = result else {
        return WireDocumentInputOutcome::Sent;
    };
    match code {
        ErrorCode::PermissionDenied if control.is_none() => {
            WireDocumentInputOutcome::NotControllable
        }
        ErrorCode::PermissionDenied => WireDocumentInputOutcome::NoGrant,
        ErrorCode::NotProducer => WireDocumentInputOutcome::NoProducer,
        _ => WireDocumentInputOutcome::Other {
            message: format!("{code:?}: {message}"),
        },
    }
}

#[cfg(test)]
mod tests;
