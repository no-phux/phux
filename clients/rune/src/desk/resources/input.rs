//! Pointer, wheel and keys into a surface whose producer takes input
//! (`SEND_DOCUMENT_INPUT`). A document with no control class, or one that
//! refused this window, stays a read-only mirror and captures nothing.
//! Cmd+V pastes this machine's clipboard into it, and Cmd+C brings the
//! host's clipboard back.

use phux_client_core::document_input::{DocumentInputs, frame_point, scroll};
use phux_client_runtime::control::Status;
use phux_protocol::ResourceId;
use phux_protocol::wire::frame::{CommandResult, DocumentInput, DocumentKey, MAX_CLIPBOARD_TEXT};
use rune::{Key, Mods, Tx};

use super::{Body, Desk};
use crate::desk::NoticeKind;

/// What a key does to a document, or `None` when it is not the pane's: Cmd
/// and Ctrl chords stay the app's, and keys outside the document's small
/// named set are dropped rather than guessed at.
pub fn document_typed(key: &Key, mods: Mods) -> Option<DocumentInput> {
    if mods.cmd || mods.ctrl || mods.logo {
        return None;
    }
    let named = match key {
        Key::Text(text) => {
            return (!text.is_empty() && !text.chars().any(char::is_control))
                .then(|| DocumentInput::Type { text: text.clone() });
        }
        Key::Enter => DocumentKey::Enter,
        Key::Tab => DocumentKey::Tab,
        Key::Backspace => DocumentKey::Backspace,
        Key::Escape => DocumentKey::Escape,
        Key::Up => DocumentKey::ArrowUp,
        Key::Down => DocumentKey::ArrowDown,
        Key::Left => DocumentKey::ArrowLeft,
        Key::Right => DocumentKey::ArrowRight,
        _ => return None,
    };
    // Shift-Tab and the like have no named form; a plain key would lie.
    (!mods.shift && !mods.alt).then_some(DocumentInput::Key { key: named })
}

impl Desk {
    /// The frame size of a surface that takes pointer and keys now.
    pub fn document_input_frame(&self, id: &ResourceId) -> Option<[u32; 2]> {
        if !self.resources.input.accepts(id) {
            return None;
        }
        match &self.resources.entries.get(id)?.body {
            Body::Surface(image) if image.error().is_none() => image.size(),
            _ => None,
        }
    }

    /// Why a document will not take this window's input, once it said.
    pub fn document_input_refused(&self, id: &ResourceId) -> Option<&'static str> {
        self.resources.input.refused(id)
    }

    fn send_document_input(&mut self, id: &ResourceId, intent: DocumentInput) -> bool {
        self.send_document_request(id, intent).is_some()
    }

    fn send_document_request(&mut self, id: &ResourceId, intent: DocumentInput) -> Option<u32> {
        let client = self.client.clone()?;
        if client.status() != Status::Attached || self.document_input_frame(id).is_none() {
            return None;
        }
        let command = DocumentInputs::command(id, intent);
        let request = client.with_control(|control| control.send_command(command));
        self.resources.input.sent(request, id.clone());
        client.wake();
        Some(request)
    }

    /// The surface on screen, when it takes input now.
    fn shown_input_document(&self) -> Option<ResourceId> {
        let id = self.shown.as_ref()?.resource.clone();
        self.document_input_frame(&id).is_some().then_some(id)
    }

    /// Cmd+V over a surface that takes input: this machine's clipboard
    /// goes to the host's and the host presses its paste chord. False when
    /// no such surface is on screen.
    pub(in crate::desk) fn paste_into_document(&mut self, tx: &mut Tx) -> bool {
        let Some(id) = self.shown_input_document() else {
            return false;
        };
        tx.read_clipboard(move |desk: &mut Self, content, _| {
            let text = content.text();
            if text.is_empty() {
                desk.say(NoticeKind::Error, "Nothing here can be pasted.");
            } else if text.len() > MAX_CLIPBOARD_TEXT {
                desk.say(
                    NoticeKind::Error,
                    "Clipboard too large to paste (over 1 MiB).",
                );
            } else {
                let paste = DocumentInput::Paste {
                    text: text.to_owned(),
                    chord: true,
                };
                desk.send_document_input(&id, paste);
            }
        });
        true
    }

    /// Cmd+C over a surface that takes input: ask its host for its
    /// clipboard; the reply lands on this machine's. False when no such
    /// surface is on screen.
    pub(in crate::desk) fn copy_from_document(&mut self) -> bool {
        let Some(id) = self.shown_input_document() else {
            return false;
        };
        let copy = DocumentInput::CopyClipboard { reply: 0 };
        if let Some(request) = self.send_document_request(&id, copy) {
            self.resources.copying = Some(request);
        }
        true
    }

    /// A copy from a host answered with its text: onto this machine's
    /// clipboard. True when `result` was one.
    pub(in crate::desk) fn host_copied(
        &mut self,
        request: u32,
        result: &CommandResult,
        tx: &mut Tx,
    ) -> bool {
        let Some(text) = self.resources.input.copied(request, result) else {
            return false;
        };
        let _ = self.resources.input.answer(request, result);
        self.resources.copying = None;
        tx.copy(text);
        self.say(NoticeKind::Info, "Copied the host's clipboard.");
        true
    }

    /// A press at `x, y` in an area `w` by `h` where the surface is drawn
    /// contain-fit: click the frame pixel under it, unless it fell in the
    /// letterbox.
    pub fn document_press(&mut self, id: &ResourceId, [x, y, w, h]: [f32; 4]) {
        self.focus_pane(id);
        let Some(frame) = self.document_input_frame(id) else {
            return;
        };
        if let Some([x, y]) = frame_point([0.0, 0.0, w, h], frame, [x, y]) {
            self.send_document_input(id, DocumentInput::Click { x, y });
        }
    }

    /// A wheel turn over the surface, as Rune reports it (content movement
    /// in points), scrolled in frame pixels.
    pub fn document_wheel(&mut self, id: &ResourceId, area: [f32; 2], content: [f32; 2]) -> bool {
        let Some(frame) = self.document_input_frame(id) else {
            return false;
        };
        if let Some(intent) = scroll(area, frame, content) {
            self.send_document_input(id, intent);
        }
        true
    }

    /// A key nothing else took, while a surface that takes input is shown;
    /// true when the document took it.
    pub(in crate::desk) fn document_key(&mut self, event: &rune::KeyEvent) -> bool {
        let Some(id) = self.shown.as_ref().map(|s| s.resource.clone()) else {
            return false;
        };
        if self.document_input_frame(&id).is_none() {
            return false;
        }
        document_typed(&event.key, event.mods)
            .is_some_and(|intent| self.send_document_input(&id, intent))
    }

    /// Settle a reply to a send; true when it was one.
    pub(in crate::desk) fn document_input_result(
        &mut self,
        request: u32,
        result: &CommandResult,
    ) -> bool {
        match self.resources.input.answer(request, result) {
            Ok(None) => false,
            Ok(Some(refusal)) => {
                let text = format!("{} is view only: {}.", refusal.resource, refusal.reason);
                self.say(NoticeKind::Error, text);
                self.generation += 1;
                true
            }
            Err(error) if self.resources.copying == Some(request) => {
                self.resources.copying = None;
                self.say(
                    NoticeKind::Error,
                    format!("Could not copy from the host: {error}"),
                );
                true
            }
            Err(error) => {
                eprintln!("phux-rune: document input not delivered: {error}");
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_keys_type_named_keys_press_and_app_chords_pass() {
        let plain = Mods::default();
        let shift = Mods {
            shift: true,
            ..Mods::default()
        };
        let cmd = Mods {
            cmd: true,
            ..Mods::default()
        };
        let ctrl = Mods {
            ctrl: true,
            ..Mods::default()
        };
        let text = |t: &str| Key::Text(t.into());
        let typed = |t: &str| Some(DocumentInput::Type { text: t.into() });
        let key = |key| Some(DocumentInput::Key { key });
        assert_eq!(document_typed(&text("a"), plain), typed("a"));
        assert_eq!(document_typed(&text("A"), shift), typed("A"));
        assert_eq!(document_typed(&Key::Enter, plain), key(DocumentKey::Enter));
        assert_eq!(document_typed(&Key::Tab, plain), key(DocumentKey::Tab));
        assert_eq!(
            document_typed(&Key::Backspace, plain),
            key(DocumentKey::Backspace)
        );
        assert_eq!(
            document_typed(&Key::Escape, plain),
            key(DocumentKey::Escape)
        );
        assert_eq!(document_typed(&Key::Up, plain), key(DocumentKey::ArrowUp));
        assert_eq!(
            document_typed(&Key::Down, plain),
            key(DocumentKey::ArrowDown)
        );
        assert_eq!(
            document_typed(&Key::Left, plain),
            key(DocumentKey::ArrowLeft)
        );
        assert_eq!(
            document_typed(&Key::Right, plain),
            key(DocumentKey::ArrowRight)
        );
        assert_eq!(document_typed(&Key::Tab, shift), None, "no shift-tab");
        assert_eq!(
            document_typed(&Key::Command('c'), cmd),
            None,
            "app shortcut"
        );
        assert_eq!(document_typed(&text(" "), cmd), None, "Cmd-Space");
        assert_eq!(document_typed(&text("c"), ctrl), None, "control chord");
        assert_eq!(
            document_typed(&Key::Function(5), plain),
            None,
            "unnamed key"
        );
    }
}
