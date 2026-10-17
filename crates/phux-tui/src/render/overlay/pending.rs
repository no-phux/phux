//! Placeholder overlay for a picker whose data is still in flight.
//!
//! Used for the `go-to-directory` listing: it swallows keys, Escape cancels, and the
//! real picker replaces it only while it is still on top
//! ([`OverlayState::replace_pending`](super::OverlayState::replace_pending)).

use phux_protocol::input::key::{KeyAction, KeyEvent, PhysicalKey};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::widgets::{Modal, centered_panel};
use super::{OverlayCommand, RenderOverlay};
use crate::render::{ChromeBreakpoints, Theme};

/// A "working on it" note standing in for the overlay a request will open.
#[derive(Debug)]
pub struct PendingOverlay {
    title: String,
    line: String,
    request_id: u32,
    /// Snapshotted (copied) at construction so the overlay stays `'static`.
    theme: Theme,
    /// `[chrome]` thresholds, stamped by `OverlayState::push`.
    breakpoints: ChromeBreakpoints,
}

impl PendingOverlay {
    /// The placeholder for a `go-to-directory` listing of `path` (empty
    /// means the server user's home) sent as `request_id`. It wears the
    /// title of the directory picker it turns into, so the box does not
    /// retitle itself when the listing lands.
    #[must_use]
    pub fn listing(path: &str, request_id: u32, theme: &Theme) -> Self {
        let shown = if path.is_empty() { "~" } else { path };
        Self {
            title: "New tab in folder".to_owned(),
            line: format!("Listing {shown}\u{2026}"),
            request_id,
            theme: *theme,
            breakpoints: ChromeBreakpoints::default(),
        }
    }
}

impl PendingOverlay {
    fn modal(&self) -> Modal<'_> {
        let body = vec![Line::from(Span::styled(
            self.line.as_str(),
            Style::default().fg(self.theme.text),
        ))];
        Modal::new(&self.theme, self.title.clone(), body)
            .wrap(true)
            .footer("", "esc cancel")
    }
}

impl RenderOverlay for PendingOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let modal_area = self.bounds(area).unwrap_or(area);
        self.modal().render_into(modal_area, buf);
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        // The same box as a toast: half the viewport wide, only as tall as
        // its one line needs, so a placeholder is not a mostly empty frame.
        let panel = centered_panel(area, 5, 32, 6, self.breakpoints);
        let height = self.modal().height_for(panel.width).min(panel.height);
        let y = area.y + area.height.saturating_sub(height) / 2;
        Some(Rect { y, height, ..panel })
    }

    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.breakpoints = bp;
    }

    /// Escape cancels; every other key is swallowed, so nothing typed while
    /// the request is in flight lands in the pane underneath.
    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        if key.action == KeyAction::Press && key.key == PhysicalKey::Escape {
            return OverlayCommand::Dismiss;
        }
        OverlayCommand::Stay
    }

    fn pending_request(&self) -> Option<u32> {
        Some(self.request_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::overlay::{OverlayState, SelectList};
    use phux_protocol::input::key::ModSet;

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

    fn placeholder(request_id: u32) -> Box<PendingOverlay> {
        Box::new(PendingOverlay::listing(
            "/srv",
            request_id,
            &Theme::default(),
        ))
    }

    fn picker() -> Box<SelectList> {
        Box::new(SelectList::new("picker", Vec::new(), &Theme::default()))
    }

    #[test]
    fn escape_cancels_and_other_keys_are_swallowed() {
        let mut pending = PendingOverlay::listing("", 1, &Theme::default());
        assert_eq!(
            pending.handle_key(&key(PhysicalKey::A)),
            OverlayCommand::Stay
        );
        assert_eq!(
            pending.handle_key(&key(PhysicalKey::Escape)),
            OverlayCommand::Dismiss
        );
        assert_eq!(pending.line, "Listing ~\u{2026}");
        assert_eq!(pending.title, "New tab in folder");
    }

    #[test]
    fn the_placeholder_is_only_as_tall_as_its_line() {
        let pending = PendingOverlay::listing("/srv", 1, &Theme::default());
        let area = Rect::new(0, 0, 100, 40);
        let b = pending.bounds(area).expect("bounded");
        assert_eq!(b.height, 3, "border, one line, border: {b:?}");
        assert!(b.y > 0 && b.y + b.height < area.height, "centered: {b:?}");
        let mut buf = Buffer::empty(area);
        pending.render(area, &mut buf);
        let text: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("New tab in folder"), "title:\n{text}");
        assert!(text.contains("esc cancel"), "legend:\n{text}");
    }

    #[test]
    fn the_reply_replaces_its_placeholder() {
        let mut overlays = OverlayState::new();
        overlays.push(placeholder(7));

        assert!(overlays.replace_pending(7, picker()));

        assert_eq!(overlays.depth(), 1);
        assert_eq!(overlays.top_pending_request(), None, "the picker is on top");
    }

    #[test]
    fn a_reply_after_the_placeholder_was_dismissed_is_dropped() {
        let mut overlays = OverlayState::new();
        overlays.push(placeholder(7));
        overlays.handle_key(&key(PhysicalKey::Escape));
        assert!(!overlays.awaits(7), "Escape cancelled the listing");

        assert!(!overlays.replace_pending(7, picker()));

        assert_eq!(overlays.depth(), 0, "no picker opens over the pane");
    }

    #[test]
    fn a_reply_does_not_replace_another_overlay_or_request() {
        let mut overlays = OverlayState::new();
        overlays.push(placeholder(7));
        overlays.push(picker());
        assert!(
            !overlays.replace_pending(7, picker()),
            "covered placeholder"
        );

        let mut overlays = OverlayState::new();
        overlays.push(placeholder(8));
        assert!(!overlays.replace_pending(7, picker()), "stale request id");
        assert_eq!(overlays.top_pending_request(), Some(8));
    }
}
