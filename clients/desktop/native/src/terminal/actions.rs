//! One-shot requests the shell makes of the focused terminal that need the
//! platform: Ghostty's clipboard actions on any chord, and handing a written
//! file to the clipboard or its default app. They use the same GPUI
//! clipboard and paste path as Command-C and Command-V, minus its keyboard
//! focus check: the find bar over the terminal may hold the keyboard.
//!
//! The shell sends `hostAction = { id, kind, text? }`; a new `id` runs once,
//! on the next render. A request already present when the element is created
//! predates it (a re-created pane must not repeat an old paste) and is dropped.

use crate::input::TerminalInput;
use gpuix_native::native_extensions::gpui::{self, ClipboardItem};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum HostAction {
    /// Copy this view's selection, as Command-C.
    Copy,
    /// Paste the clipboard's text, as Command-V.
    Paste,
    /// Put `text` on the clipboard.
    CopyText(String),
    /// Open this path with its default application.
    Open(String),
}

impl HostAction {
    fn parse(value: &Value) -> Option<(String, Self)> {
        let id = value["id"].as_str()?.to_owned();
        let text = || value["text"].as_str().map(str::to_owned);
        let action = match value["kind"].as_str()? {
            "copy" => Self::Copy,
            "paste" => Self::Paste,
            "copyText" => Self::CopyText(text()?),
            "open" => Self::Open(text()?),
            _ => return None,
        };
        Some((id, action))
    }
}

#[derive(Default)]
pub(super) struct HostActions {
    last: Option<String>,
    pending: Option<HostAction>,
    armed: bool,
}

impl HostActions {
    pub(super) fn offer(&mut self, value: &Value) {
        let Some((id, action)) = HostAction::parse(value) else {
            return;
        };
        if self.last.as_deref() == Some(id.as_str()) {
            return;
        }
        self.last = Some(id);
        if self.armed {
            self.pending = Some(action);
        }
    }

    /// Called once the element has rendered: later requests are its own.
    pub(super) fn arm(&mut self) {
        self.armed = true;
    }

    pub(super) fn take(&mut self) -> Option<HostAction> {
        self.pending.take()
    }
}

/// Failures are as quiet as Command-C with nothing selected: the terminal's
/// own state already shows why (no selection, not ready). Copy and Paste are
/// addressed to this terminal, so they do not need its keyboard focus.
pub(super) fn run(
    action: HostAction,
    input: &gpui::Entity<TerminalInput>,
    cx: &mut gpui::App,
) {
    match action {
        HostAction::Copy => input.update(cx, |state, cx| {
            let _ = state.copy_requested(cx);
        }),
        HostAction::Paste => {
            let text = cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .unwrap_or_default();
            if !text.is_empty() {
                input.update(cx, |state, _| {
                    let _ = state.paste_requested(&text);
                });
            }
        }
        HostAction::CopyText(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
        HostAction::Open(path) => cx.open_with_system(std::path::Path::new(&path)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requests_before_the_first_render_and_repeats_never_run() {
        let mut actions = HostActions::default();
        actions.offer(&json!({"id": "1", "kind": "paste"}));
        actions.arm();
        assert_eq!(actions.take(), None);
        actions.offer(&json!({"id": "1", "kind": "paste"}));
        assert_eq!(actions.take(), None);
        actions.offer(&json!({"id": "2", "kind": "copy"}));
        actions.offer(&Value::Null);
        assert_eq!(actions.take(), Some(HostAction::Copy));
        assert_eq!(actions.take(), None);
    }

    #[test]
    fn malformed_requests_are_ignored() {
        let mut actions = HostActions::default();
        actions.arm();
        for bad in [
            json!({"id": 3, "kind": "copy"}),
            json!({"id": "3", "kind": "cut"}),
            json!({"id": "3", "kind": "open"}),
            json!({"id": "3", "kind": "copyText", "text": 7}),
        ] {
            actions.offer(&bad);
            assert_eq!(actions.take(), None);
        }
        actions.offer(&json!({"id": "4", "kind": "open", "text": "/tmp/x.txt"}));
        assert_eq!(actions.take(), Some(HostAction::Open("/tmp/x.txt".into())));
        actions.offer(&json!({"id": "5", "kind": "copyText", "text": "/tmp/x.txt"}));
        assert_eq!(
            actions.take(),
            Some(HostAction::CopyText("/tmp/x.txt".into()))
        );
    }
}
