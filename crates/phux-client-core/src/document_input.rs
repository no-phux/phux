//! Input to a document pane (`SEND_DOCUMENT_INPUT`).
//!
//! Which documents take it (`docs/resource-kinds.md#authority`), where a
//! pointer lands in a contain-fit frame, and which refusals turn a pane
//! view-only. Native clients share it; each maps its own toolkit's keys.
//! Frame sides are bounded far below `f32`'s exact integers (decoders cap
//! a side at 8192), so the pixel casts here lose nothing.
#![allow(clippy::cast_precision_loss)]

use std::collections::HashMap;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ControlClass, DocumentInput, ErrorCode,
};
use phux_protocol::wire::info::SessionSnapshot;

/// Frame pixels per view point when a `frame`-pixel image is drawn
/// contain-fit (aspect kept, centered, scaled up or down) in a
/// `view`-point area; `None` for an empty frame or area.
#[must_use]
pub fn frame_scale(view: [f32; 2], frame: [u32; 2]) -> Option<f32> {
    let fit = (view[0] / frame[0] as f32).min(view[1] / frame[1] as f32);
    (fit.is_finite() && fit > 0.0).then(|| 1.0 / fit)
}

/// The frame pixel under `point` in a contain-fit image.
///
/// The `frame`-pixel image is drawn in the area `view` (`[x, y, width,
/// height]`, the same coordinates as `point`). `None` in the letterbox
/// bars, outside the area, or for an empty frame or area.
#[must_use]
pub fn frame_point(view: [f32; 4], frame: [u32; 2], point: [f32; 2]) -> Option<[u32; 2]> {
    let scale = frame_scale([view[2], view[3]], frame)?;
    let drawn = [frame[0] as f32 / scale, frame[1] as f32 / scale];
    let x = (point[0] - view[0] - (view[2] - drawn[0]) / 2.0) * scale;
    let y = (point[1] - view[1] - (view[3] - drawn[1]) / 2.0) * scale;
    let inside = |at: f32, side: u32| (0.0..side as f32).contains(&at);
    // Truncation is the floor here: both are non-negative and below the side.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (inside(x, frame[0]) && inside(y, frame[1])).then_some([x as u32, y as u32])
}

/// A wheel turn as [`DocumentInput::Scroll`].
///
/// `content` is how far the
/// toolkit would move the content, in view points, positive right and down
/// (the fingers' direction under natural scrolling, as GPUI and Rune report
/// it). The intent's delta is the opposite, in frame pixels: positive `dy`
/// scrolls the page down, revealing what is below. `None` when it rounds to
/// nothing or the frame or area is empty.
#[must_use]
pub fn scroll(view: [f32; 2], frame: [u32; 2], content: [f32; 2]) -> Option<DocumentInput> {
    let scale = frame_scale(view, frame)?;
    #[allow(clippy::cast_possible_truncation)]
    let pixels = |delta: f32| (-delta * scale).round() as i32;
    let (dx, dy) = (pixels(content[0]), pixels(content[1]));
    (dx != 0 || dy != 0).then_some(DocumentInput::Scroll { dx, dy })
}

/// The host clipboard text a reply to [`DocumentInput::CopyClipboard`]
/// carries; `None` for any other reply.
#[must_use]
pub fn copied_text(result: &CommandResult) -> Option<String> {
    match result {
        CommandResult::OkWith(CommandValue::Bytes(bytes)) => {
            Some(String::from_utf8_lossy(bytes).into_owned())
        }
        _ => None,
    }
}

/// Why a document stopped taking this client's input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The document.
    pub resource: ResourceId,
    /// A short reason, for the pane and a notification.
    pub reason: &'static str,
}

/// Which documents take input on one connection, and the sends in flight.
#[derive(Debug, Default)]
pub struct DocumentInputs {
    controls: HashMap<ResourceId, ControlClass>,
    refused: HashMap<ResourceId, &'static str>,
    pending: HashMap<u32, ResourceId>,
}

impl DocumentInputs {
    /// Fold a topology snapshot's control classes in. A document with none
    /// (`control: null`) takes no input from a pane.
    pub fn note_snapshot(&mut self, snapshot: &SessionSnapshot) {
        for resource in &snapshot.resources {
            match resource.control {
                Some(class) => self.controls.insert(resource.id.clone(), class),
                None => self.controls.remove(&resource.id),
            };
        }
    }

    /// Forget everything: a new connection's handles mean nothing here.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The document's control class, when it has one.
    #[must_use]
    pub fn control(&self, resource: &ResourceId) -> Option<ControlClass> {
        self.controls.get(resource).copied()
    }

    /// Why the document refused this client, once it has.
    #[must_use]
    pub fn refused(&self, resource: &ResourceId) -> Option<&'static str> {
        self.refused.get(resource).copied()
    }

    /// Whether a pane showing the document should take pointer and keys.
    #[must_use]
    pub fn accepts(&self, resource: &ResourceId) -> bool {
        self.controls.contains_key(resource) && !self.refused.contains_key(resource)
    }

    /// The command for `intent`; pass the request id it is queued under
    /// to [`Self::sent`].
    #[must_use]
    pub fn command(resource: &ResourceId, intent: DocumentInput) -> Command {
        Command::SendDocumentInput {
            terminal_id: resource.clone(),
            intent,
            control: None,
        }
    }

    /// Remember that `request_id` carried input to `resource`.
    pub fn sent(&mut self, request_id: u32, resource: ResourceId) {
        self.pending.insert(request_id, resource);
    }

    /// The host clipboard text `result` carries, when it answers a copy
    /// this connection sent. Read it before [`Self::answer`] settles the
    /// request.
    #[must_use]
    pub fn copied(&self, request_id: u32, result: &CommandResult) -> Option<String> {
        self.pending
            .contains_key(&request_id)
            .then(|| copied_text(result))
            .flatten()
    }

    /// Settle a reply. A missing grant or producer makes the document
    /// view-only and is returned the first time only; any other error is
    /// transient and returned as `Err` for a log line. `Ok(None)` for a
    /// success or a reply that is not ours.
    ///
    /// # Errors
    ///
    /// The server's message for a refusal that does not end input.
    pub fn answer(
        &mut self,
        request_id: u32,
        result: &CommandResult,
    ) -> Result<Option<Refusal>, String> {
        let Some(resource) = self.pending.remove(&request_id) else {
            return Ok(None);
        };
        let CommandResult::Error { code, message } = result else {
            return Ok(None);
        };
        let reason = match code {
            ErrorCode::PermissionDenied => "no control grant for this document",
            ErrorCode::NotProducer => "its producer is not connected",
            _ => return Err(format!("{code:?}: {message}")),
        };
        if self.refused.contains_key(&resource) {
            return Ok(None);
        }
        self.refused.insert(resource.clone(), reason);
        Ok(Some(Refusal { resource, reason }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::info::ResourceInfo;

    #[test]
    fn a_wide_frame_in_a_tall_pane_is_barred_above_and_below() {
        // 200x100 in 100x200 at (10, 20): scale 2, drawn 100x50 from y = 95.
        let view = [10.0, 20.0, 100.0, 200.0];
        assert_eq!(frame_point(view, [200, 100], [10.0, 95.0]), Some([0, 0]));
        assert_eq!(
            frame_point(view, [200, 100], [60.0, 120.0]),
            Some([100, 50])
        );
        assert_eq!(
            frame_point(view, [200, 100], [109.9, 144.9]),
            Some([199, 99])
        );
        assert_eq!(frame_point(view, [200, 100], [60.0, 94.0]), None, "top bar");
        assert_eq!(
            frame_point(view, [200, 100], [60.0, 146.0]),
            None,
            "bottom bar"
        );
    }

    #[test]
    fn a_tall_frame_in_a_wide_pane_is_barred_left_and_right_and_scales_up() {
        // 10x20 in 400x100: scale 0.2, drawn 50x100 from x = 175.
        let view = [0.0, 0.0, 400.0, 100.0];
        assert_eq!(frame_point(view, [10, 20], [200.0, 50.0]), Some([5, 10]));
        assert_eq!(frame_point(view, [10, 20], [174.0, 50.0]), None, "left bar");
        assert_eq!(
            frame_point(view, [10, 20], [226.0, 50.0]),
            None,
            "right bar"
        );
        assert_eq!(frame_point(view, [10, 20], [200.0, -1.0]), None, "above");
        assert_eq!(
            frame_point(view, [0, 20], [200.0, 50.0]),
            None,
            "empty frame"
        );
        assert_eq!(
            frame_point([0.0, 0.0, 0.0, 100.0], [10, 20], [0.0, 0.0]),
            None
        );
    }

    #[test]
    fn a_wheel_scrolls_the_page_against_the_content_in_frame_pixels() {
        // 200x100 in 100x50: two frame pixels per point.
        let scroll = |content| scroll([100.0, 50.0], [200, 100], content);
        assert_eq!(
            scroll([0.0, -10.0]),
            Some(DocumentInput::Scroll { dx: 0, dy: 20 })
        );
        assert_eq!(
            scroll([3.0, 0.0]),
            Some(DocumentInput::Scroll { dx: -6, dy: 0 })
        );
        assert_eq!(scroll([0.1, 0.2]), None, "rounds to nothing");
    }

    #[test]
    fn controls_come_from_snapshots_and_a_hard_refusal_is_said_once() {
        let page = ResourceId::local(9);
        let plain = ResourceId::local(8);
        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(0))
                .with_resources(vec![
                    ResourceInfo::new(page.clone(), WindowId::new(1), 0, 0)
                        .with_control(Some(ControlClass::Page)),
                    ResourceInfo::new(plain.clone(), WindowId::new(1), 0, 0),
                ]);
        let mut inputs = DocumentInputs::default();
        inputs.note_snapshot(&snapshot);
        assert!(inputs.accepts(&page) && !inputs.accepts(&plain));
        assert_eq!(inputs.control(&page), Some(ControlClass::Page));
        let error = |code| CommandResult::Error {
            code,
            message: "no".into(),
        };
        inputs.sent(1, page.clone());
        inputs.sent(2, page.clone());
        inputs.sent(3, page.clone());
        inputs.sent(4, page.clone());
        assert_eq!(inputs.answer(1, &CommandResult::Ok), Ok(None));
        assert!(inputs.answer(2, &error(ErrorCode::Overflow)).is_err());
        assert!(inputs.accepts(&page), "a transient error keeps input");
        assert_eq!(
            inputs.answer(3, &error(ErrorCode::PermissionDenied)),
            Ok(Some(Refusal {
                resource: page.clone(),
                reason: "no control grant for this document"
            }))
        );
        assert_eq!(inputs.answer(4, &error(ErrorCode::NotProducer)), Ok(None));
        assert_eq!(inputs.answer(5, &error(ErrorCode::NotProducer)), Ok(None));
        assert!(!inputs.accepts(&page));
        assert!(inputs.refused(&page).is_some());
        inputs.clear();
        assert!(!inputs.accepts(&page) && inputs.refused(&page).is_none());
    }

    #[test]
    fn only_a_pending_copy_reply_carries_clipboard_text() {
        let mut inputs = DocumentInputs::default();
        let text = CommandResult::OkWith(CommandValue::Bytes(b"host".to_vec()));
        assert_eq!(inputs.copied(5, &text), None, "not a send of ours");
        inputs.sent(5, ResourceId::local(1));
        assert_eq!(inputs.copied(5, &CommandResult::Ok), None);
        assert_eq!(inputs.copied(5, &text).as_deref(), Some("host"));
        assert_eq!(inputs.answer(5, &text), Ok(None));
        assert_eq!(inputs.copied(5, &text), None, "settled");
    }
}
