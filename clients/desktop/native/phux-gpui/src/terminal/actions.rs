//! One-shot platform requests addressed to a bound terminal: Ghostty's
//! clipboard actions, and handing a written file to the clipboard or its
//! default app. They use the same GPUI clipboard and paste path as Command-C
//! and Command-V, minus its keyboard focus check: the find bar over the
//! terminal may hold the keyboard.
//!
//! A request is delivered on the call that carries it and is not retained, so
//! a later render cannot repeat it and an unbound surface cannot store one for
//! a future bind.

use crate::input::{InputNotice, LinkAdmit, TerminalInput, admit_path};
use gpui::{self, ClipboardItem};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostAction {
    /// Copy this view's selection, as Command-C.
    Copy,
    /// Paste the clipboard's text, as Command-V.
    Paste,
    /// Send a paste the person confirmed after it was held.
    PasteConfirmed(crate::input::HeldPaste),
    /// Put `text` on the clipboard.
    CopyText(String),
    /// Open this path with its default application.
    Open(String),
}

/// Failures are as quiet as Command-C with nothing selected: the terminal's
/// own state already shows why (no selection, not ready). Copy and Paste are
/// addressed to this terminal, so they do not need its keyboard focus.
/// `Some` when the shell should show a refusal. The action is not kept.
pub(super) fn run(
    action: HostAction,
    input: &gpui::Entity<TerminalInput>,
    cx: &mut gpui::App,
) -> Option<InputNotice> {
    match action {
        HostAction::Copy => {
            input.update(cx, |state, cx| {
                let _ = state.copy_requested(cx);
            });
            None
        }
        HostAction::Paste => input.update(cx, |state, cx| state.paste_command(cx)),
        HostAction::PasteConfirmed(paste) => {
            input.update(cx, |state, _| state.paste_confirmed(&paste.text));
            None
        }
        HostAction::CopyText(text) => {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            None
        }
        HostAction::Open(path) => match admit_path(path.as_ref()) {
            LinkAdmit::Open => {
                cx.open_with_system(path.as_ref());
                None
            }
            LinkAdmit::Refuse { title, body } => Some(InputNotice { title, body }),
        },
    }
}
