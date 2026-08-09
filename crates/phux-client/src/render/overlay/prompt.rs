//! Prompt overlay (phux-ahv.1) — a single-line text input modal.
//!
//! Captures a string from the user, then commits it as a configured
//! action: on Enter the overlay returns [`OverlayCommand::Commit`] with a
//! [`ResolvedAction`] whose `args[arg_key]` is the typed text, which the
//! dispatcher runs through the normal `run_action` path. Esc cancels.
//!
//! It is deliberately generic: the active use is `rename-window`, but any
//! action that wants a single string argument can reuse it by varying
//! `action` + `arg_key`.

use std::collections::BTreeMap;

use phux_config::keybind::ResolvedAction;
use phux_protocol::input::key::{KeyAction, KeyEvent, PhysicalKey};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::widgets::{Modal, centered_panel};
use super::{OverlayCommand, RenderOverlay};
use crate::render::Theme;

/// A single-line text-input modal that commits to an action.
#[derive(Debug, Clone)]
pub struct PromptOverlay {
    /// Modal title (e.g. `"rename window"`).
    title: String,
    /// The action name to run on commit (e.g. `"rename-window"`).
    action: String,
    /// The arg key the typed text is bound to (e.g. `"name"`).
    arg_key: String,
    /// Current input buffer.
    input: String,
    /// Color slots snapshotted from the active [`Theme`] at construction.
    /// Captured (not borrowed) so the overlay stays `'static`.
    theme: Theme,
}

impl PromptOverlay {
    /// Build a prompt that commits the typed text as
    /// `action { arg_key: <text> }`. `initial` pre-fills the input
    /// (cursor lands at the end); `theme` styles the modal chrome.
    #[must_use]
    pub fn new(title: &str, action: &str, arg_key: &str, initial: &str, theme: &Theme) -> Self {
        Self {
            title: title.to_owned(),
            action: action.to_owned(),
            arg_key: arg_key.to_owned(),
            input: initial.to_owned(),
            theme: *theme,
        }
    }

    /// The `rename-window` prompt, pre-filled with the window's current
    /// name and styled with `theme`.
    #[must_use]
    pub fn rename_window(current_name: &str, theme: &Theme) -> Self {
        Self::new(
            "rename window",
            "rename-window",
            "name",
            current_name,
            theme,
        )
    }

    /// The `rename-session` prompt, pre-filled with the current session's
    /// name and styled with `theme`. Committing it renames the session this
    /// client is attached to (via the `RENAME_SESSION` command); the server
    /// is authoritative and the next `ATTACHED` snapshot reconciles the name
    /// across every attached client.
    #[must_use]
    pub fn rename_session(current_name: &str, theme: &Theme) -> Self {
        Self::new(
            "rename session",
            "rename-session",
            "name",
            current_name,
            theme,
        )
    }

    /// The `new-session` prompt: type a name for a brand-new session.
    /// Committing it re-attaches this client to the freshly-created
    /// session (via the same in-process re-attach path as switch-session).
    #[must_use]
    pub fn new_session(theme: &Theme) -> Self {
        Self::new("new session", "new-session", "name", "", theme)
    }

    fn committed_action(&self) -> ResolvedAction {
        let mut args = BTreeMap::new();
        args.insert(
            self.arg_key.clone(),
            toml::Value::String(self.input.clone()),
        );
        ResolvedAction {
            action: self.action.clone(),
            args,
        }
    }

    /// A small centered modal: 60% width (min 20), fixed 3 rows (border
    /// + one input line).
    fn modal_area(outer: Rect) -> Rect {
        // Reuse the shared centering for width/x, then pin height to 3
        // rows (border + one input line) and re-center vertically against
        // that fixed height — the fraction-based height a `Modal`-style
        // box would otherwise get is wrong for a one-line prompt.
        let wide = centered_panel(outer, 6, 20, 3);
        let h = 3.min(outer.height);
        let y = outer.y + (outer.height.saturating_sub(h)) / 2;
        Rect::new(wide.x, y, wide.width, h)
    }
}

impl RenderOverlay for PromptOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let modal_area = Self::modal_area(area);
        // Input line + a reverse-video cursor block so the caret is
        // visible without driving the host terminal cursor (the overlay
        // paint hides it).
        let line = Line::from(vec![
            Span::raw(self.input.clone()),
            Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
        ]);
        Modal::new(&self.theme, self.title.clone(), vec![line]).render_into(modal_area, buf);
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        Some(Self::modal_area(area))
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        // Press-only; ignore release/repeat so a held key doesn't double.
        if key.action != KeyAction::Press {
            return OverlayCommand::Stay;
        }
        match key.key {
            PhysicalKey::Escape => OverlayCommand::Dismiss,
            PhysicalKey::Enter => {
                // Empty input cancels rather than committing a blank name.
                if self.input.trim().is_empty() {
                    OverlayCommand::Dismiss
                } else {
                    OverlayCommand::Commit(self.committed_action())
                }
            }
            PhysicalKey::Backspace => {
                self.input.pop();
                OverlayCommand::Stay
            }
            _ => {
                // Append the event's text (the resolved grapheme), if any.
                // Control keys carry no `text`, so they're absorbed.
                if let Some(t) = &key.text
                    && !t.chars().any(char::is_control)
                {
                    self.input.push_str(t);
                }
                OverlayCommand::Stay
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use phux_protocol::input::key::ModSet;

    fn press(key: PhysicalKey, text: Option<&str>) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: text.map(ToOwned::to_owned),
            unshifted_codepoint: None,
        }
    }

    fn typ(p: &mut PromptOverlay, ch: char) -> OverlayCommand {
        // PhysicalKey is irrelevant for text input; the buffer reads `text`.
        p.handle_key(&press(PhysicalKey::A, Some(&ch.to_string())))
    }

    #[test]
    fn typing_then_enter_commits_resolved_action() {
        let mut p = PromptOverlay::rename_window("", &Theme::default());
        for ch in ['b', 'u', 'i', 'l', 'd'] {
            assert_eq!(typ(&mut p, ch), OverlayCommand::Stay);
        }
        let cmd = p.handle_key(&press(PhysicalKey::Enter, None));
        let OverlayCommand::Commit(action) = cmd else {
            panic!("expected Commit, got {cmd:?}");
        };
        assert_eq!(action.action, "rename-window");
        assert_eq!(
            action.args.get("name"),
            Some(&toml::Value::String("build".to_owned()))
        );
    }

    #[test]
    fn backspace_edits_buffer() {
        let mut p = PromptOverlay::rename_window("ab", &Theme::default());
        assert_eq!(
            p.handle_key(&press(PhysicalKey::Backspace, None)),
            OverlayCommand::Stay
        );
        let OverlayCommand::Commit(action) = p.handle_key(&press(PhysicalKey::Enter, None)) else {
            panic!("expected Commit");
        };
        assert_eq!(
            action.args.get("name"),
            Some(&toml::Value::String("a".to_owned()))
        );
    }

    #[test]
    fn escape_cancels() {
        let mut p = PromptOverlay::rename_window("x", &Theme::default());
        assert_eq!(
            p.handle_key(&press(PhysicalKey::Escape, None)),
            OverlayCommand::Dismiss
        );
    }

    #[test]
    fn empty_enter_cancels_rather_than_committing_blank() {
        let mut p = PromptOverlay::rename_window("", &Theme::default());
        assert_eq!(
            p.handle_key(&press(PhysicalKey::Enter, None)),
            OverlayCommand::Dismiss
        );
    }

    #[test]
    fn control_text_is_ignored() {
        let mut p = PromptOverlay::rename_window("", &Theme::default());
        // A control char in `text` must not enter the buffer.
        assert_eq!(typ(&mut p, '\t'), OverlayCommand::Stay);
        assert_eq!(
            p.handle_key(&press(PhysicalKey::Enter, None)),
            OverlayCommand::Dismiss,
            "buffer should still be empty → Enter cancels"
        );
    }
}
