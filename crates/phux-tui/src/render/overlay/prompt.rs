//! Prompt overlay: a single-line text input committing `action { arg_key:
//! <text> }` through `run_action` on Enter ([`OverlayCommand::Commit`]); Esc
//! cancels.
//!
//! Editing is the shared [`LineEdit`] (the readline keys).

use std::collections::BTreeMap;

use phux_config::keybind::ResolvedAction;
use phux_protocol::input::key::{KeyAction, KeyEvent, PhysicalKey};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

use super::line_edit::LineEdit;
use super::widgets::{Modal, centered_panel};
use super::{OverlayCommand, RenderOverlay};
use crate::render::{ChromeBreakpoints, Theme};

/// A single-line text-input modal that commits to an action.
#[derive(Debug, Clone)]
pub struct PromptOverlay {
    /// Modal title (e.g. `"rename window"`).
    title: String,
    /// The action name to run on commit (e.g. `"rename-window"`).
    action: String,
    /// The arg key the typed text is bound to (e.g. `"name"`).
    arg_key: String,
    /// Current input buffer and cursor.
    input: LineEdit,
    /// Color slots snapshotted from the active [`Theme`] at construction.
    /// Captured (not borrowed) so the overlay stays `'static`.
    theme: Theme,
    /// `[chrome]` thresholds, stamped by `OverlayState::push`.
    breakpoints: ChromeBreakpoints,
}

impl PromptOverlay {
    /// A prompt committing `action { arg_key: <text> }`, pre-filled with
    /// `initial`.
    #[must_use]
    pub fn new(title: &str, action: &str, arg_key: &str, initial: &str, theme: &Theme) -> Self {
        Self {
            title: title.to_owned(),
            action: action.to_owned(),
            arg_key: arg_key.to_owned(),
            input: LineEdit::new(initial),
            theme: *theme,
            breakpoints: ChromeBreakpoints::default(),
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

    /// The `rename-session` prompt, pre-filled with the current name.
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

    /// The `new-session` prompt; committing re-attaches to the new session.
    #[must_use]
    pub fn new_session(theme: &Theme) -> Self {
        Self::new("new session", "new-session", "name", "", theme)
    }

    fn committed_action(&self) -> ResolvedAction {
        let mut args = BTreeMap::new();
        args.insert(
            self.arg_key.clone(),
            toml::Value::String(self.input.as_str().to_owned()),
        );
        ResolvedAction {
            action: self.action.clone(),
            args,
        }
    }

    /// A small centered modal: 50% width (min 20), fixed 3 rows (border
    /// + one input line).
    fn modal_area(outer: Rect, bp: ChromeBreakpoints) -> Rect {
        // Centered width, a fixed three-row height (border + input line).
        let wide = centered_panel(outer, 5, 20, 3, bp);
        let h = 3.min(outer.height);
        let y = outer.y + (outer.height.saturating_sub(h)) / 2;
        Rect::new(wide.x, y, wide.width, h)
    }
}

impl RenderOverlay for PromptOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let modal_area = Self::modal_area(area, self.breakpoints);
        let line = Line::from(self.input.spans(Style::default().fg(self.theme.text)));
        Modal::new(&self.theme, self.title.clone(), vec![line]).render_into(modal_area, buf);
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        Some(Self::modal_area(area, self.breakpoints))
    }

    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.breakpoints = bp;
    }

    fn handle_paste(&mut self, text: &str) {
        self.input.insert(text);
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
                if self.input.as_str().trim().is_empty() {
                    OverlayCommand::Dismiss
                } else {
                    OverlayCommand::Commit(self.committed_action())
                }
            }
            // Readline edits and typed text; any other key is absorbed.
            _ => {
                self.input.handle_key(key);
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

    /// The session prompts ignored `C-u`, `C-w`, `C-a`/`C-e`: a pre-filled
    /// name had to be erased one Backspace at a time.
    #[test]
    fn readline_keys_edit_the_prompt() {
        let ctrl = |key| KeyEvent {
            mods: ModSet::CTRL,
            ..press(key, None)
        };
        let committed = |p: &mut PromptOverlay| {
            let OverlayCommand::Commit(action) = p.handle_key(&press(PhysicalKey::Enter, None))
            else {
                panic!("expected Commit");
            };
            action.args.get("name").cloned()
        };

        let mut p = PromptOverlay::rename_session("old name", &Theme::default());
        assert_eq!(p.handle_key(&ctrl(PhysicalKey::U)), OverlayCommand::Stay);
        for ch in "new".chars() {
            typ(&mut p, ch);
        }
        assert_eq!(
            committed(&mut p),
            Some(toml::Value::String("new".to_owned()))
        );

        let mut p = PromptOverlay::rename_session("work tmp", &Theme::default());
        p.handle_key(&ctrl(PhysicalKey::W));
        p.handle_key(&ctrl(PhysicalKey::A));
        typ(&mut p, 'x');
        p.handle_key(&ctrl(PhysicalKey::E));
        typ(&mut p, 'y');
        assert_eq!(
            committed(&mut p),
            Some(toml::Value::String("xwork y".to_owned()))
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
