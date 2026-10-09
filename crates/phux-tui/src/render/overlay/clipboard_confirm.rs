//! Clipboard-write confirmation: the modal `defaults.clipboard-write = "ask"`
//! raises when the focused pane's program sets the clipboard with OSC 52
//! (ADR-0158). Enter or `y` sets it; Esc or `n` drops the write.

use phux_client_core::engine::ClipboardText;
use phux_protocol::input::key::{KeyAction, KeyEvent, PhysicalKey};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

use super::widgets::{Modal, centered_panel};
use super::{OverlayCommand, RenderOverlay};
use crate::render::{ChromeBreakpoints, Theme};

/// Holds one clipboard write until the user allows or drops it.
#[derive(Debug, Clone)]
pub struct ClipboardConfirmOverlay {
    text: ClipboardText,
    message: String,
    theme: Theme,
    breakpoints: ChromeBreakpoints,
}

impl ClipboardConfirmOverlay {
    /// Ask before `source` (a pane label) sets the clipboard to `text`.
    #[must_use]
    pub fn new(source: &str, text: ClipboardText, theme: &Theme) -> Self {
        let message = format!(
            "{source} wants to set the clipboard ({} bytes)",
            text.0.len()
        );
        Self {
            text,
            message,
            theme: *theme,
            breakpoints: ChromeBreakpoints::default(),
        }
    }

    fn modal_area(outer: Rect, bp: ChromeBreakpoints) -> Rect {
        let wide = centered_panel(outer, 5, 20, 4, bp);
        let h = 4.min(outer.height);
        let y = outer.y + (outer.height.saturating_sub(h)) / 2;
        Rect::new(wide.x, y, wide.width, h)
    }
}

impl RenderOverlay for ClipboardConfirmOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let text = Style::default().fg(self.theme.text);
        let lines = vec![
            Line::styled(self.message.clone(), text),
            Line::styled("Enter allow  ·  Esc deny", text),
        ];
        Modal::new(&self.theme, "clipboard?".to_owned(), lines)
            .render_into(Self::modal_area(area, self.breakpoints), buf);
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        Some(Self::modal_area(area, self.breakpoints))
    }

    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.breakpoints = bp;
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        if key.action != KeyAction::Press {
            return OverlayCommand::Stay;
        }
        match key.key {
            PhysicalKey::Enter | PhysicalKey::Y => OverlayCommand::SetClipboard(self.text.clone()),
            PhysicalKey::Escape | PhysicalKey::N => OverlayCommand::Dismiss,
            _ => OverlayCommand::Stay,
        }
    }
}

#[cfg(test)]
mod tests {
    use phux_protocol::input::key::ModSet;

    use super::*;

    fn press(key: PhysicalKey) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        }
    }

    fn overlay() -> ClipboardConfirmOverlay {
        ClipboardConfirmOverlay::new(
            "pane 3",
            ClipboardText("secret".to_owned()),
            &Theme::default(),
        )
    }

    #[test]
    fn enter_or_y_allows_the_held_write() {
        for key in [PhysicalKey::Enter, PhysicalKey::Y] {
            assert_eq!(
                overlay().handle_key(&press(key)),
                OverlayCommand::SetClipboard(ClipboardText("secret".to_owned()))
            );
        }
    }

    #[test]
    fn escape_or_n_drops_it_and_the_message_hides_the_text() {
        for key in [PhysicalKey::Escape, PhysicalKey::N] {
            assert_eq!(overlay().handle_key(&press(key)), OverlayCommand::Dismiss);
        }
        assert_eq!(
            overlay().message,
            "pane 3 wants to set the clipboard (6 bytes)"
        );
        assert!(!format!("{:?}", overlay()).contains("secret"));
    }
}
