//! Pointer, wheel and keys into a document pane that takes input
//! (`SEND_DOCUMENT_INPUT`). A pane whose document has no control class, or
//! that refused this window, stays view-only and captures nothing.

use gpui::{Keystroke, Point, ScrollEvent, px};
use phux_client_core::document_input::{frame_point, scroll};
use phux_protocol::wire::frame::{DocumentInput, DocumentKey};

use super::*;

/// What a key does to a document, or `None` when it is not the pane's:
/// Cmd and Ctrl chords stay the app's (menus, key bindings, the shell
/// chords a terminal would take), and keys outside the document's small
/// named set are dropped rather than guessed at.
pub(crate) fn document_typed(keystroke: &Keystroke) -> Option<DocumentInput> {
    let modifiers = &keystroke.modifiers;
    if modifiers.platform || modifiers.control || modifiers.function {
        return None;
    }
    let key = match keystroke.key.as_str() {
        "enter" => Some(DocumentKey::Enter),
        "tab" => Some(DocumentKey::Tab),
        "backspace" => Some(DocumentKey::Backspace),
        "escape" => Some(DocumentKey::Escape),
        "up" => Some(DocumentKey::ArrowUp),
        "down" => Some(DocumentKey::ArrowDown),
        "left" => Some(DocumentKey::ArrowLeft),
        "right" => Some(DocumentKey::ArrowRight),
        _ => None,
    };
    if let Some(key) = key {
        // Shift-Tab and the like have no named form; a plain key would lie.
        return (!modifiers.shift && !modifiers.alt).then_some(DocumentInput::Key { key });
    }
    let text = keystroke.key_char.as_ref()?;
    (!text.is_empty() && !text.chars().any(char::is_control))
        .then(|| DocumentInput::Type { text: text.clone() })
}

impl Desk {
    /// The focused pane's document, when it takes input now.
    fn focused_input_document(&self) -> Option<ResourceId> {
        let id = self.focused_placement()?.terminal_id.clone()?;
        let link = self.link.as_ref()?;
        link.connection.document_input_frame(&id).map(|_| id)
    }

    /// A press on a document pane: focus it, and click the frame pixel
    /// under the pointer unless it fell in the letterbox. `area` is where
    /// the frame is drawn contain-fit, in window coordinates.
    pub(crate) fn document_press(
        &mut self,
        placement: &str,
        id: &ResourceId,
        area: Bounds<Pixels>,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.active_tab_id() {
            self.focus_placement(&tab, placement, window, cx);
        }
        let Some(link) = self.link.as_mut() else {
            return;
        };
        let Some(frame) = link.connection.document_input_frame(id) else {
            return;
        };
        let view = [
            area.origin.x,
            area.origin.y,
            area.size.width,
            area.size.height,
        ]
        .map(f32::from);
        if let Some([x, y]) = frame_point(view, frame, [f32::from(at.x), f32::from(at.y)]) {
            link.connection
                .send_document_input(id, DocumentInput::Click { x, y });
        }
    }

    /// A wheel or trackpad turn over a document pane, in frame pixels.
    pub(crate) fn document_wheel(
        &mut self,
        id: &ResourceId,
        area: Bounds<Pixels>,
        event: &ScrollEvent,
    ) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        let Some(frame) = link.connection.document_input_frame(id) else {
            return;
        };
        // A notched wheel's line, in points, as a browser scrolls one.
        let delta = event.delta.pixel_delta(px(40.));
        let view = [area.size.width, area.size.height].map(f32::from);
        if let Some(intent) = scroll(view, frame, [delta.x, delta.y].map(f32::from)) {
            link.connection.send_document_input(id, intent);
        }
    }

    /// A key while a document pane that takes input is focused; true when
    /// the document took it.
    pub(super) fn document_key(&mut self, event: &KeyDownEvent) -> bool {
        let Some(id) = self.focused_input_document() else {
            return false;
        };
        let Some(intent) = document_typed(&event.keystroke) else {
            return false;
        };
        self.link
            .as_mut()
            .is_some_and(|link| link.connection.send_document_input(&id, intent))
    }

    /// Say once why a document stopped taking this window's input.
    pub(super) fn note_document_refusals(&mut self) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        for refusal in link.connection.take_document_refusals() {
            let body = format!("{} is view only: {}.", refusal.resource, refusal.reason);
            self.post(Kind::Refusal, "Input not sent", Some(body));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn stroke(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.into(),
            key_char: key_char.map(Into::into),
        }
    }

    #[test]
    fn printable_keys_type_named_keys_press_and_app_chords_pass() {
        let plain = Modifiers::default();
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        let cmd = Modifiers {
            platform: true,
            ..Modifiers::default()
        };
        let ctrl = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        let typed = |text: &str| Some(DocumentInput::Type { text: text.into() });
        let key = |key| Some(DocumentInput::Key { key });
        assert_eq!(document_typed(&stroke("a", Some("a"), plain)), typed("a"));
        assert_eq!(document_typed(&stroke("a", Some("A"), shift)), typed("A"));
        assert_eq!(
            document_typed(&stroke("space", Some(" "), plain)),
            typed(" ")
        );
        assert_eq!(
            document_typed(&stroke("enter", Some("\r"), plain)),
            key(DocumentKey::Enter)
        );
        assert_eq!(
            document_typed(&stroke("tab", Some("\t"), plain)),
            key(DocumentKey::Tab)
        );
        assert_eq!(
            document_typed(&stroke("backspace", None, plain)),
            key(DocumentKey::Backspace)
        );
        assert_eq!(
            document_typed(&stroke("escape", None, plain)),
            key(DocumentKey::Escape)
        );
        assert_eq!(
            document_typed(&stroke("up", None, plain)),
            key(DocumentKey::ArrowUp)
        );
        assert_eq!(
            document_typed(&stroke("down", None, plain)),
            key(DocumentKey::ArrowDown)
        );
        assert_eq!(
            document_typed(&stroke("left", None, plain)),
            key(DocumentKey::ArrowLeft)
        );
        assert_eq!(
            document_typed(&stroke("right", None, plain)),
            key(DocumentKey::ArrowRight)
        );
        assert_eq!(
            document_typed(&stroke("tab", None, shift)),
            None,
            "no shift-tab"
        );
        assert_eq!(
            document_typed(&stroke("c", Some("c"), cmd)),
            None,
            "app shortcut"
        );
        assert_eq!(
            document_typed(&stroke("t", Some("t"), cmd)),
            None,
            "app shortcut"
        );
        assert_eq!(
            document_typed(&stroke("c", Some("c"), ctrl)),
            None,
            "control chord"
        );
        assert_eq!(
            document_typed(&stroke("f5", None, plain)),
            None,
            "unnamed key"
        );
    }
}
