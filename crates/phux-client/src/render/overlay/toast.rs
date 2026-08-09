//! Toast overlay (phux-r82.5): a small dismissable notice modal.
//!
//! Used by the plugin-action runtime to surface a failed action's captured
//! output without freezing the TUI — the action runs in a spawned task and
//! the driver pushes this overlay when the failure report lands. Any key
//! dismisses it (there is nothing to select or edit), so it costs the user
//! exactly one keystroke.
//!
//! Deliberately generic (a title + plain body lines) so other one-shot
//! notices can reuse it; it renders through the shared [`Modal`] widget so
//! it composes with the rest of the overlay chrome.

use phux_protocol::input::key::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;

use super::widgets::{Modal, centered_panel};
use super::{OverlayCommand, RenderOverlay};
use crate::render::Theme;

/// A dismiss-on-any-key notice modal.
#[derive(Debug)]
pub struct ToastOverlay {
    title: String,
    lines: Vec<String>,
    /// Snapshotted (copied) at construction so the overlay stays `'static`.
    theme: Theme,
}

impl ToastOverlay {
    /// Build a toast titled `title` showing `lines`, styled with `theme`.
    #[must_use]
    pub fn new(title: impl Into<String>, lines: Vec<String>, theme: &Theme) -> Self {
        Self {
            title: title.into(),
            lines,
            theme: *theme,
        }
    }
}

impl RenderOverlay for ToastOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let modal_area = self.bounds(area).unwrap_or(area);
        let body: Vec<Line<'_>> = self.lines.iter().map(|l| Line::from(l.as_str())).collect();
        Modal::new(&self.theme, self.title.clone(), body)
            .footer("Press any key to close")
            .wrap(true)
            .render_into(modal_area, buf);
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        // ~60% of the viewport, min 40x8, clamped to the outer rect.
        Some(centered_panel(area, 6, 40, 8))
    }

    fn handle_key(&mut self, _key: &KeyEvent) -> OverlayCommand {
        // Any key dismisses — a toast has no inner interaction.
        OverlayCommand::Dismiss
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use phux_protocol::input::key::{KeyAction, ModSet, PhysicalKey};

    fn key(k: PhysicalKey) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key: k,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        }
    }

    fn render_to_string(overlay: &ToastOverlay, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        overlay.render(area, &mut buf);
        let mut out = String::new();
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            out.push_str(row.trim_end());
            out.push('\n');
        }
        out
    }

    #[test]
    fn any_key_dismisses() {
        let mut toast = ToastOverlay::new("t", vec!["body".to_owned()], &Theme::default());
        assert_eq!(
            toast.handle_key(&key(PhysicalKey::A)),
            OverlayCommand::Dismiss
        );
        let mut toast = ToastOverlay::new("t", vec![], &Theme::default());
        assert_eq!(
            toast.handle_key(&key(PhysicalKey::Escape)),
            OverlayCommand::Dismiss
        );
    }

    #[test]
    fn renders_title_body_and_footer() {
        let toast = ToastOverlay::new(
            "plugin: p a failed",
            vec!["exit code 2".to_owned(), "boom".to_owned()],
            &Theme::default(),
        );
        let text = render_to_string(&toast, 80, 24);
        assert!(text.contains("plugin: p a failed"), "title:\n{text}");
        assert!(text.contains("exit code 2"), "body line 1:\n{text}");
        assert!(text.contains("boom"), "body line 2:\n{text}");
        assert!(text.contains("Press any key to close"), "footer:\n{text}");
    }

    #[test]
    fn bounds_are_centered_and_clamped() {
        let toast = ToastOverlay::new("t", vec![], &Theme::default());
        let b = toast
            .bounds(Rect::new(0, 0, 100, 40))
            .expect("toast is bounded");
        assert!(b.width >= 40 && b.width <= 100);
        assert!(b.height >= 8 && b.height <= 40);
        // Tiny viewport still yields a rect inside it.
        let tiny = toast.bounds(Rect::new(0, 0, 20, 6)).expect("bounded");
        assert!(tiny.width <= 20 && tiny.height <= 6);
    }
}
