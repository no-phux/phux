//! Placeholder overlay for a picker whose data is still on its way from the
//! server (the `go-to-directory` listing).
//!
//! It exists so the gap between sending a request and receiving its reply is
//! modal like the picker that follows: keystrokes are swallowed instead of
//! reaching the focused pane, and Escape cancels. It remembers the request it
//! stands in for, so the driver can swap the real picker in only while this
//! placeholder is still the active overlay
//! ([`OverlayState::replace_pending`](super::OverlayState::replace_pending)).
//! Once it is dismissed, a late reply finds nothing to replace and is dropped.

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
    /// means the server user's home) sent as `request_id`.
    #[must_use]
    pub fn listing(path: &str, request_id: u32, theme: &Theme) -> Self {
        let shown = if path.is_empty() { "~" } else { path };
        Self {
            title: "go to directory".to_owned(),
            line: format!("Listing {shown}..."),
            request_id,
            theme: *theme,
            breakpoints: ChromeBreakpoints::default(),
        }
    }
}

impl RenderOverlay for PendingOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let modal_area = self.bounds(area).unwrap_or(area);
        let body = vec![Line::from(Span::styled(
            self.line.as_str(),
            Style::default().fg(self.theme.text),
        ))];
        Modal::new(&self.theme, self.title.clone(), body)
            .footer("Esc cancel")
            .wrap(true)
            .render_into(modal_area, buf);
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        Some(centered_panel(area, 6, 40, 8, self.breakpoints))
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
        assert_eq!(pending.line, "Listing ~...");
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
