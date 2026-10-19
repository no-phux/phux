//! Copy, paste, and the paste protection that holds a risky paste for the
//! shell to confirm.

use super::*;

/// Clipboard text held back until the person confirms the paste.
/// `Debug` shows its size, never its contents.
#[derive(Clone, PartialEq, Eq)]
pub struct HeldPaste {
    /// The text the person pasted.
    pub text: String,
}

impl HeldPaste {
    /// How many lines the paste would type.
    #[must_use]
    pub fn lines(&self) -> usize {
        self.text.lines().count().max(1)
    }
}

impl std::fmt::Debug for HeldPaste {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "HeldPaste({} bytes)", self.text.len())
    }
}

/// The bracketed-paste terminator. Pasted, it could end the bracket early and
/// type the rest as keys.
const BRACKET_END: &str = "\x1b[201~";

/// Ghostty's `clipboard-paste-protection` with its default
/// `clipboard-paste-bracketed-safe`, the rule the TUI applies too: a paste
/// the program receives as a paste (DEC 2004 bracketed paste on, as zsh,
/// fish, bash 5.1+, vim and coding agents set it) goes straight through. One
/// that would arrive as typed keys asks first when it carries a line break,
/// since each break is an Enter. Text holding the bracket terminator always
/// asks.
#[must_use]
pub fn paste_needs_confirmation(bracketed: bool, text: &str) -> bool {
    text.contains(BRACKET_END) || (!bracketed && text.contains(['\n', '\r']))
}

/// Largest selection the clipboard takes, in UTF-8 bytes. A larger one is
/// refused whole, never truncated.
const COPY_LIMIT: usize = 1024 * 1024;

/// The notice for a copy the clipboard refused because it was too large.
#[must_use]
pub fn copy_too_large() -> InputNotice {
    InputNotice {
        title: "Selection not copied",
        body: format!(
            "It is over {} MiB. Open Scrollback as a File takes all of it.",
            COPY_LIMIT / (1024 * 1024)
        ),
    }
}

impl TerminalInput {
    /// The user's own clipboard paste, trusted (the runtime's
    /// `apply_user_paste`). The sole FFI event owner receives InputDelivery
    /// for a returned correlation; `None` is one atomic paste event too large
    /// for acknowledged input, with no receipt, or a paste held for
    /// confirmation ([`InputEvent::ConfirmPaste`]). Neither the native adapter
    /// nor its caller may auto-retry Unknown.
    pub fn paste_text(
        &mut self,
        text: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<Option<u64>, InputError> {
        let focused = self.focused(window, cx);
        if focused && self.hold_paste(text, cx) {
            return Ok(None);
        }
        self.paste(text, focused)
    }

    /// Send a paste the person confirmed after [`InputEvent::ConfirmPaste`].
    /// Like the shell's Paste command it is addressed by placement, and it
    /// is not held a second time.
    pub fn paste_confirmed(&mut self, text: &str) {
        let result = self.paste(text, true).map(drop);
        self.record(result);
    }

    /// Hold a paste [`paste_needs_confirmation`] flags for a terminal that
    /// would accept it, and ask the shell to confirm it. True when held.
    fn hold_paste(&self, text: &str, cx: &mut Context<Self>) -> bool {
        if !text.contains(['\n', '\r', '\x1b']) {
            return false;
        }
        let Ok(client) = self.client() else {
            return false;
        };
        let held = client.with_control(|control| {
            self.validate(control, true).is_ok()
                && paste_needs_confirmation(bracketed_paste(control, &self.terminal), text)
        });
        if held {
            cx.emit(InputEvent::ConfirmPaste(HeldPaste {
                text: text.to_owned(),
            }));
        }
        held
    }

    pub(super) fn paste(&mut self, text: &str, focused: bool) -> Result<Option<u64>, InputError> {
        self.cancel();
        let client = self.client()?;
        let delivery = client.with_control(|control| {
            let admission = if focused {
                self.validate(control, true)
            } else {
                Err(InputError::WrongFocus)
            };
            admission.map(|()| {
                let had_events = control.has_events();
                let delivery = control.apply_user_paste(&self.terminal, text);
                (delivery, !had_events && control.has_events())
            })
        });
        // Refusal only publishes a receipt; it cannot enqueue a paste after a
        // reconnect. Admission and successful enqueue above share one lock.
        let (delivery, resolved) = delivery.unwrap_or_else(|error| {
            (
                Some(client.refuse_acknowledged_input(&format!("native paste refused: {error:?}"))),
                false,
            )
        });
        if resolved {
            client.wake();
        }
        Ok(delivery)
    }

    pub fn copy_selection(&self, window: &Window, cx: &mut App) -> Result<(), InputError> {
        if !self.focused(window, cx) {
            return Err(InputError::WrongFocus);
        }
        self.copy_requested(cx)
    }

    /// The shell's Copy command, addressed to this terminal by placement: its
    /// selection is copied wherever keyboard focus is, as with the find bar
    /// open.
    pub fn copy_requested(&self, cx: &mut App) -> Result<(), InputError> {
        let text = self.selection_text()?;
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        Ok(())
    }

    /// This view's selection as complete UTF-8, or why there is none to copy.
    fn selection_text(&self) -> Result<String, InputError> {
        use phux_client_runtime::engine::BoundedSelectionText;
        let text = self.with_current(false, |control| {
            control
                .engine()
                .ok_or(InputError::NotReady)?
                .selection_text_view_bounded(self.view, COPY_LIMIT)
                .map_err(engine_error)
        })?;
        match text {
            BoundedSelectionText::Text(text) if !text.is_empty() => {
                String::from_utf8(text).map_err(|error| InputError::Engine(error.to_string()))
            }
            BoundedSelectionText::ByteLimitExceeded | BoundedSelectionText::WorkLimitExceeded => {
                Err(InputError::TooLarge)
            }
            other => Err(InputError::Engine(format!("nothing to copy: {other:?}"))),
        }
    }

    /// Linux's primary selection holds the latest mouse selection, so a
    /// middle click pastes it here or in any other app.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    pub(super) fn publish_primary(&self, cx: &mut App) {
        if let Ok(text) = self.selection_text() {
            cx.write_to_primary(ClipboardItem::new_string(text));
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    pub(super) fn publish_primary(&self, _: &mut App) {}

    /// The shell's Paste command, addressed to this terminal by placement.
    /// Keyboard focus may sit in an overlay over it, such as the find bar,
    /// whose own Command-V pastes into the field instead. A refusal is
    /// returned so the shell can show it without a nested event.
    pub fn paste_command(&mut self, cx: &mut Context<Self>) -> Option<InputNotice> {
        let item = cx.read_from_clipboard()?;
        self.paste_item(&item, true, cx)
    }

    pub(super) fn paste_item(
        &mut self,
        item: &ClipboardItem,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> Option<InputNotice> {
        match crate::attach::clipboard_for_host(item, &self.host())? {
            crate::attach::PathAttach::Paste(text) => {
                if focused && self.hold_paste(&text, cx) {
                    return None;
                }
                let result = self.paste(&text, focused);
                self.record(result.map(drop));
                None
            }
            crate::attach::PathAttach::Upload(batch) => {
                let result = if focused {
                    self.with_current(true, |_| Ok(()))
                } else {
                    Err(InputError::WrongFocus)
                };
                let notice = result.as_ref().err().map(|_| InputNotice {
                    title: crate::attach::PASTE_REFUSED,
                    body: "The terminal is not ready for a file paste. Nothing was uploaded."
                        .into(),
                });
                if result.is_ok() {
                    cx.emit(InputEvent::Upload(batch));
                }
                self.record(result);
                notice
            }
            crate::attach::PathAttach::Refuse { title, body } => Some(InputNotice { title, body }),
        }
    }
}

/// Whether the program has DEC 2004 bracketed paste on. Unreadable counts as
/// off, so an unknown mode asks rather than types.
fn bracketed_paste(control: &ControlPlane, terminal: &ResourceId) -> bool {
    control
        .engine()
        .is_some_and(|engine| engine.bracketed_paste(terminal).unwrap_or(false))
}
