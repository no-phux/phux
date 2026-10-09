//! Paste confirmation: the modal [`crate::attach`] raises when a paste would
//! reach a pane as typed input (Ghostty's `clipboard-paste-protection`).
//! Enter or `y` delivers the held paste; Esc or `n` drops it.

use phux_protocol::input::key::{KeyAction, KeyEvent, PhysicalKey};
use phux_protocol::input::paste::PasteEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

use super::widgets::{Modal, centered_panel};
use super::{OverlayCommand, RenderOverlay};
use crate::render::{ChromeBreakpoints, Theme};

/// Holds one paste until the user confirms or drops it.
#[derive(Debug, Clone)]
pub struct PasteConfirmOverlay {
    paste: PasteEvent,
    message: String,
    theme: Theme,
    breakpoints: ChromeBreakpoints,
}

impl PasteConfirmOverlay {
    /// Ask before delivering `paste` of `lines` lines; `bracketed` is the
    /// pane's DEC 2004 state, which decides what the warning says.
    #[must_use]
    pub fn new(paste: PasteEvent, lines: usize, bracketed: bool, theme: &Theme) -> Self {
        let message = if bracketed {
            "this paste could end bracketed paste early".to_owned()
        } else if lines == 1 {
            "the line break will press Enter".to_owned()
        } else {
            format!("{lines} lines will run as typed input")
        };
        Self {
            paste,
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

impl RenderOverlay for PasteConfirmOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let text = Style::default().fg(self.theme.text);
        let lines = vec![
            Line::styled(self.message.clone(), text),
            Line::styled("Enter paste  ·  Esc cancel", text),
        ];
        Modal::new(&self.theme, "paste?".to_owned(), lines)
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
            PhysicalKey::Enter | PhysicalKey::Y => OverlayCommand::Paste(self.paste.clone()),
            PhysicalKey::Escape | PhysicalKey::N => OverlayCommand::Dismiss,
            _ => OverlayCommand::Stay,
        }
    }
}

#[cfg(test)]
mod tests {
    use phux_protocol::input::key::ModSet;
    use phux_protocol::input::paste::PasteTrust;

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

    fn overlay() -> (PasteConfirmOverlay, PasteEvent) {
        let paste = PasteEvent {
            trust: PasteTrust::Trusted,
            data: b"make\nmake install\n".to_vec(),
        };
        let o = PasteConfirmOverlay::new(paste.clone(), 2, false, &Theme::default());
        (o, paste)
    }

    #[test]
    fn enter_or_y_delivers_the_held_paste() {
        for key in [PhysicalKey::Enter, PhysicalKey::Y] {
            let (mut o, paste) = overlay();
            assert_eq!(o.handle_key(&press(key)), OverlayCommand::Paste(paste));
        }
    }

    #[test]
    fn escape_or_n_drops_it_and_other_keys_wait() {
        for key in [PhysicalKey::Escape, PhysicalKey::N] {
            assert_eq!(overlay().0.handle_key(&press(key)), OverlayCommand::Dismiss);
        }
        assert_eq!(
            overlay().0.handle_key(&press(PhysicalKey::A)),
            OverlayCommand::Stay
        );
    }

    #[test]
    fn the_message_names_what_will_happen() {
        assert_eq!(overlay().0.message, "2 lines will run as typed input");
        let one = PasteEvent {
            trust: PasteTrust::Trusted,
            data: b"ls\n".to_vec(),
        };
        let o = PasteConfirmOverlay::new(one.clone(), 1, false, &Theme::default());
        assert_eq!(o.message, "the line break will press Enter");
        let o = PasteConfirmOverlay::new(one, 1, true, &Theme::default());
        assert_eq!(o.message, "this paste could end bracketed paste early");
    }
}
