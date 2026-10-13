//! One-shot platform requests addressed to a bound terminal: Ghostty's
//! clipboard actions, and handing a written file to the clipboard or its
//! default app. They use the same GPUI clipboard and paste path as Command-C
//! and Command-V, minus its keyboard focus check: the find bar over the
//! terminal may hold the keyboard.
//!
//! [`accept`] is the once-only gate. A request is delivered on the call that
//! carries it and is not retained, so a later render cannot repeat it and an
//! unbound surface cannot store one for a future bind.

use crate::input::TerminalInput;
use gpui::{self, ClipboardItem};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostAction {
    /// Copy this view's selection, as Command-C.
    Copy,
    /// Paste the clipboard's text, as Command-V.
    Paste,
    /// Put `text` on the clipboard.
    CopyText(String),
    /// Open this path with its default application.
    Open(String),
}

/// `None` when the surface is not the bound entity. The caller must not keep
/// the action for a later attempt.
pub(super) fn accept(bound: bool, action: HostAction) -> Option<HostAction> {
    bound.then_some(action)
}

/// Failures are as quiet as Command-C with nothing selected: the terminal's
/// own state already shows why (no selection, not ready). Copy and Paste are
/// addressed to this terminal, so they do not need its keyboard focus.
pub(super) fn run(action: HostAction, input: &gpui::Entity<TerminalInput>, cx: &mut gpui::App) {
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
    use super::{HostAction, accept};

    #[test]
    fn an_unbound_request_is_dropped_and_not_retained() {
        assert_eq!(accept(false, HostAction::Paste), None);
        assert_eq!(accept(false, HostAction::Copy), None);
        // A later bind does not see the dropped request: accept keeps no slot.
        assert_eq!(accept(true, HostAction::Copy), Some(HostAction::Copy));
    }

    #[test]
    fn each_accepted_request_is_a_separate_delivery() {
        let first = accept(true, HostAction::CopyText("a".into()));
        let second = accept(true, HostAction::Open("/tmp/x.txt".into()));
        assert_eq!(first, Some(HostAction::CopyText("a".into())));
        assert_eq!(second, Some(HostAction::Open("/tmp/x.txt".into())));
        assert_ne!(first, second);
    }
}
