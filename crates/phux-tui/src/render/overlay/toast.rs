//! Toast overlay: a dismissable notice (a failed plugin action's output, a
//! config error). Any key dismisses it.
//!
//! A placeholder for a picker whose data is still in flight is a toast too:
//! it swallows keys, Escape cancels, and the real picker replaces it only
//! while it is on top.

use phux_protocol::input::key::{KeyAction, KeyEvent, PhysicalKey};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::widgets::{Modal, centered_panel};
use super::{OverlayCommand, RenderOverlay};
use crate::render::{ChromeBreakpoints, Theme};

/// A dismiss-on-any-key notice modal.
#[derive(Debug)]
pub struct ToastOverlay {
    title: String,
    lines: Vec<String>,
    /// Snapshotted (copied) at construction so the overlay stays `'static`.
    theme: Theme,
    passthrough: bool,
    /// The border's key legend in place of `any key` (a confirm's
    /// `⏎ kill · esc`); `None` keeps the default.
    keys: Option<&'static str>,
    /// The request a placeholder awaits; only Escape dismisses one.
    pending: Option<u32>,
    /// `[chrome]` thresholds, stamped by `OverlayState::push`.
    breakpoints: ChromeBreakpoints,
}

impl ToastOverlay {
    /// Build a toast titled `title` showing `lines`, styled with `theme`.
    #[must_use]
    pub fn new(title: impl Into<String>, lines: Vec<String>, theme: &Theme) -> Self {
        Self {
            title: title.into(),
            lines,
            theme: *theme,
            passthrough: false,
            keys: None,
            pending: None,
            breakpoints: ChromeBreakpoints::default(),
        }
    }

    /// Build a notice that disappears on the next key without consuming it.
    #[must_use]
    pub fn passthrough(title: impl Into<String>, lines: Vec<String>, theme: &Theme) -> Self {
        Self {
            passthrough: true,
            ..Self::new(title, lines, theme)
        }
    }

    /// The placeholder for a `go-to-directory` listing of `path` (empty
    /// means the server user's home) sent as `request_id`, titled like the
    /// picker it turns into.
    #[must_use]
    pub fn listing(path: &str, request_id: u32, theme: &Theme) -> Self {
        let shown = if path.is_empty() { "~" } else { path };
        let line = format!("Listing {shown}\u{2026}");
        Self {
            pending: Some(request_id),
            ..Self::new("New tab in folder", vec![line], theme).with_keys("esc")
        }
    }

    /// Show `keys` in the border instead of `any key`, for a caller that
    /// takes only some keys (a confirm).
    #[must_use]
    pub const fn with_keys(mut self, keys: &'static str) -> Self {
        self.keys = Some(keys);
        self
    }

    fn modal(&self) -> Modal<'_> {
        // Styled, not raw: body text inheriting the terminal foreground is
        // invisible on the `surface` fill of a light terminal.
        let body: Vec<Line<'_>> = self
            .lines
            .iter()
            .map(|l| {
                Line::from(Span::styled(
                    l.as_str(),
                    Style::default().fg(self.theme.text),
                ))
            })
            .collect();
        // A passthrough notice goes away on its own; only a toast that
        // holds the keyboard says how to let go of it.
        let keys = self
            .keys
            .unwrap_or(if self.passthrough { "" } else { "any key" });
        Modal::new(&self.theme, self.title.clone(), body)
            .wrap(true)
            .footer("", keys)
    }
}

impl RenderOverlay for ToastOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let modal_area = self.bounds(area).unwrap_or(area);
        self.modal().render_into(modal_area, buf);
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        // 50% of the viewport, min 32 wide, clamped to the outer rect (the
        // whole width when columns are starved); then only as tall as the
        // wrapped body needs, so a short notice is never a mostly-empty box,
        // however few rows the viewport has.
        let panel = centered_panel(area, 5, 32, 6, self.breakpoints);
        let height = self.modal().height_for(panel.width).min(panel.height);
        let y = area.y + area.height.saturating_sub(height) / 2;
        Some(Rect { y, height, ..panel })
    }

    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.breakpoints = bp;
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        let escape = key.action == KeyAction::Press && key.key == PhysicalKey::Escape;
        if self.pending.is_some() && !escape {
            return OverlayCommand::Stay;
        }
        OverlayCommand::Dismiss
    }

    fn pending_request(&self) -> Option<u32> {
        self.pending
    }

    fn is_input_passthrough(&self) -> bool {
        self.passthrough
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::render::overlay::{OverlayState, SelectList};
    use crate::test_util::press as key;

    use crate::test_util::rendered as render_to_string;

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
    fn renders_title_and_body() {
        let toast = ToastOverlay::new(
            "Plugin action failed",
            vec!["exit code 2".to_owned(), "boom".to_owned()],
            &Theme::default(),
        );
        let text = render_to_string(&toast, 80, 24);
        assert!(text.contains("Plugin action failed"), "title:\n{text}");
        assert!(text.contains("exit code 2"), "body line 1:\n{text}");
        assert!(text.contains("boom"), "body line 2:\n{text}");
        assert!(
            !text.contains("Press any key"),
            "no teaching footer:\n{text}"
        );
        assert!(text.contains("any key"), "border legend:\n{text}");
    }

    #[test]
    fn passthrough_notice_reports_transparency() {
        let toast = ToastOverlay::passthrough("t", vec!["body".to_owned()], &Theme::default());
        assert!(toast.is_input_passthrough());
        let text = render_to_string(&toast, 80, 24);
        assert!(text.contains("body"), "body:\n{text}");
        assert!(
            !text.contains("Start typing"),
            "no teaching footer:\n{text}"
        );
        assert!(!text.contains("any key"), "no legend on a notice:\n{text}");
    }

    #[test]
    fn bounds_are_centered_and_clamped() {
        let toast = ToastOverlay::new("t", vec![], &Theme::default());
        let b = toast
            .bounds(Rect::new(0, 0, 100, 40))
            .expect("toast is bounded");
        assert!(b.width >= 32 && b.width <= 100);
        assert!(b.height >= 2 && b.height <= 40);
        // Tiny viewport still yields a rect inside it.
        let tiny = toast.bounds(Rect::new(0, 0, 20, 6)).expect("bounded");
        assert!(tiny.width <= 20 && tiny.height <= 6);
    }

    /// Starved of rows, a toast takes the panel's width but still only the
    /// rows its body needs: no frame around empty rows.
    #[test]
    fn a_row_starved_toast_is_still_only_as_tall_as_its_body() {
        let mut toast = ToastOverlay::new("t", vec!["one".into()], &Theme::default());
        toast.set_breakpoints(ChromeBreakpoints {
            compact_rows: 30,
            ..ChromeBreakpoints::default()
        });
        let area = Rect::new(0, 0, 60, 20);
        assert!(
            toast.breakpoints.is_row_starved(area.height),
            "the fixture is starved"
        );
        let b = toast.bounds(area).expect("bounded");
        assert_eq!(b.height, 3, "one body row plus two borders: {b:?}");
    }

    /// A short notice is only as tall as its body plus borders, centered;
    /// a long one still stops at half the viewport.
    #[test]
    fn bounds_fit_the_body_and_cap_at_half_the_viewport() {
        let outer = Rect::new(0, 0, 100, 40);
        let short = ToastOverlay::new("t", vec!["one".into(), "two".into()], &Theme::default());
        let b = short.bounds(outer).expect("bounded");
        assert_eq!(b.height, 4, "two body rows plus two borders");
        assert_eq!(b.y, (40 - 4) / 2, "vertically centered");

        let long = ToastOverlay::new("t", vec!["line".into(); 100], &Theme::default());
        let b = long.bounds(outer).expect("bounded");
        assert_eq!(b.height, 20, "capped at half the viewport");
    }

    fn placeholder(request_id: u32) -> Box<ToastOverlay> {
        Box::new(ToastOverlay::listing("/srv", request_id, &Theme::default()))
    }

    fn picker() -> Box<SelectList> {
        Box::new(SelectList::new("picker", Vec::new(), &Theme::default()))
    }

    #[test]
    fn a_placeholder_cancels_on_escape_and_swallows_other_keys() {
        let mut pending = ToastOverlay::listing("", 1, &Theme::default());
        assert_eq!(
            pending.handle_key(&key(PhysicalKey::A)),
            OverlayCommand::Stay
        );
        assert_eq!(
            pending.handle_key(&key(PhysicalKey::Escape)),
            OverlayCommand::Dismiss
        );
        assert_eq!(pending.lines, ["Listing ~\u{2026}"]);
        assert_eq!(pending.title, "New tab in folder");
        let b = pending.bounds(Rect::new(0, 0, 100, 40)).expect("bounded");
        assert_eq!(b.height, 3, "border, one line, border: {b:?}");
        let text = render_to_string(&pending, 100, 40);
        assert!(text.contains("New tab in folder"), "title:\n{text}");
        assert!(
            text.contains(" esc ") && !text.contains("cancel"),
            "legend:\n{text}"
        );
    }

    #[test]
    fn the_reply_replaces_only_its_uncovered_placeholder() {
        let mut overlays = OverlayState::new();
        overlays.push(placeholder(7));
        assert!(overlays.replace_pending(7, picker()));
        assert_eq!(overlays.depth(), 1);
        assert_eq!(overlays.top_pending_request(), None, "the picker is on top");

        let mut overlays = OverlayState::new();
        overlays.push(placeholder(7));
        overlays.handle_key(&key(PhysicalKey::Escape));
        assert!(!overlays.awaits(7), "Escape cancelled the listing");
        assert!(!overlays.replace_pending(7, picker()));
        assert_eq!(overlays.depth(), 0, "no picker opens over the pane");

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
