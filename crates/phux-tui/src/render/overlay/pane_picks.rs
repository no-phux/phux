//! Picking a pane or a sidebar row from the keyboard.
//!
//! `display-panes` numbers the tab's panes until a key, and `focus-sidebar`
//! walks the sidebar's rows. Neither paints a box of its own: the driver
//! draws the numbers over the panes and lights the sidebar row while one is
//! on top.

use std::io::{self, Write};

use phux_config::keybind::ResolvedAction;
use phux_protocol::ResourceId;
use phux_protocol::input::key::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratcn::runtime::{Event, KeyCode};

use super::{OverlayCommand, RenderOverlay, SidebarRows};
use crate::render::Theme;

/// A bare key press's code; `None` for a release or a chord.
fn bare(key: &KeyEvent) -> Option<KeyCode> {
    match crate::render::kit::key(key)? {
        Event::Key(k) if !k.modifiers.ctrl && !k.modifiers.alt => Some(k.code),
        _ => None,
    }
}

/// An overlay that paints nothing itself: an empty box at the content's
/// corner keeps the panes and sidebar painted under it.
const fn no_box(content: Rect) -> Rect {
    Rect::new(content.x, content.y, 0, 0)
}

/// `display-panes`: the tab's panes, numbered from 1 in layout order, each
/// with the `focus-pane` that picks it. A digit picks; any other key
/// dismisses.
#[derive(Debug)]
pub struct PaneNumbers {
    panes: Vec<ResourceId>,
    picks: Vec<ResolvedAction>,
}

impl PaneNumbers {
    /// Number `panes`, at most nine (one key each); `picks[i]` focuses
    /// `panes[i]`.
    #[must_use]
    pub fn new(mut panes: Vec<ResourceId>, mut picks: Vec<ResolvedAction>) -> Self {
        panes.truncate(9);
        picks.truncate(panes.len());
        Self { panes, picks }
    }
}

impl RenderOverlay for PaneNumbers {
    fn render(&self, _area: Rect, _buf: &mut Buffer) {}

    fn bounds(&self, content: Rect) -> Option<Rect> {
        Some(no_box(content))
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        let pick = match bare(key) {
            Some(KeyCode::Char(c)) => c.to_digit(10).and_then(|n| n.checked_sub(1)),
            _ => None,
        };
        pick.and_then(|n| self.picks.get(n as usize))
            .map_or(OverlayCommand::Dismiss, |action| {
                OverlayCommand::Commit(action.clone())
            })
    }

    fn pane_numbers(&self) -> Option<&[ResourceId]> {
        Some(&self.panes)
    }
}

/// Draw each pane's number boxed at its centre: `(number, pane rect,
/// focused)`. A pane too small for the box gets the bare digit.
pub fn paint_numbers(
    out: &mut impl Write,
    spots: impl IntoIterator<Item = (usize, crate::layout::Rect, bool)>,
    theme: &Theme,
) -> io::Result<()> {
    for (number, r, focused) in spots {
        let ink = if focused {
            theme.pane_title_focus
        } else {
            theme.pane_title
        };
        out.write_all(b"\x1b[0;1m")?;
        crate::render::write_sgr_color(out, ink, true)?;
        let (rows, w, h) = if r.w >= 5 && r.h >= 3 {
            let middle = format!("│ {number} │");
            (vec!["╭───╮".to_owned(), middle, "╰───╯".to_owned()], 5, 3)
        } else {
            (vec![number.to_string()], 1, 1)
        };
        let (x, y) = (
            r.x + r.w.saturating_sub(w) / 2,
            r.y + r.h.saturating_sub(h) / 2,
        );
        for (dy, row) in (0..).zip(rows) {
            write!(out, "\x1b[{};{}H{row}", y + dy + 1, x + 1)?;
        }
        out.write_all(b"\x1b[0m")?;
    }
    out.flush()
}

/// `focus-sidebar`: a keyboard cursor over the sidebar's rows. Up/Down or
/// k/j move, Enter commits the row's action, Esc or q goes back to the pane.
#[derive(Debug)]
pub struct SidebarCursor {
    /// The row the cursor starts on, by what it commits.
    start: Option<ResolvedAction>,
    rows: SidebarRows,
    at: Option<usize>,
}

impl SidebarCursor {
    /// A cursor that starts on the row committing `start`, else the first.
    #[must_use]
    pub const fn new(start: Option<ResolvedAction>) -> Self {
        Self {
            start,
            rows: Vec::new(),
            at: None,
        }
    }
}

impl RenderOverlay for SidebarCursor {
    fn render(&self, _area: Rect, _buf: &mut Buffer) {}

    fn bounds(&self, content: Rect) -> Option<Rect> {
        Some(no_box(content))
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        let last = self.rows.len().saturating_sub(1);
        let at = self.at.unwrap_or(0);
        match bare(key) {
            Some(KeyCode::Up | KeyCode::Char('k')) => self.at = Some(at.saturating_sub(1)),
            Some(KeyCode::Down | KeyCode::Char('j')) => self.at = Some((at + 1).min(last)),
            Some(KeyCode::Enter) => {
                return self
                    .rows
                    .get(at)
                    .map_or(OverlayCommand::Dismiss, |(_, action)| {
                        OverlayCommand::Commit(action.clone())
                    });
            }
            Some(KeyCode::Esc | KeyCode::Char('q')) => return OverlayCommand::Dismiss,
            _ => {}
        }
        OverlayCommand::Stay
    }

    fn sidebar_rows(&mut self, rows: &dyn Fn() -> SidebarRows) -> Option<u16> {
        self.rows = rows();
        let start = || {
            let start = self.start.as_ref()?;
            self.rows.iter().position(|(_, action)| action == start)
        };
        let at = self.at.or_else(start).unwrap_or(0);
        self.at = Some(at.min(self.rows.len().saturating_sub(1)));
        self.rows.get(self.at?).map(|(row, _)| *row)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use std::collections::BTreeMap;

    use phux_protocol::input::key::PhysicalKey;

    use super::*;
    use crate::render::overlay::OverlayState;
    use crate::test_util::{press, press_text};

    fn action(name: &str) -> ResolvedAction {
        crate::attach::resolved(name, BTreeMap::new())
    }

    #[test]
    fn a_digit_picks_its_pane_and_anything_else_dismisses() {
        let panes: Vec<ResourceId> = (1..=4).map(ResourceId::local).collect();
        let picks: Vec<_> = ["a", "b", "c", "d"].map(action).into();
        let mut numbers = PaneNumbers::new(panes.clone(), picks);
        assert_eq!(numbers.pane_numbers(), Some(panes.as_slice()));
        let three = press_text(PhysicalKey::Digit3, Some("3"));
        assert_eq!(
            numbers.handle_key(&three),
            OverlayCommand::Commit(action("c"))
        );
        for key in [
            press_text(PhysicalKey::Digit5, Some("5")),
            press_text(PhysicalKey::Digit0, Some("0")),
            press(PhysicalKey::Escape),
        ] {
            assert_eq!(numbers.handle_key(&key), OverlayCommand::Dismiss);
        }
    }

    #[test]
    fn the_sidebar_cursor_starts_on_its_row_moves_and_commits() {
        let rows = || vec![(2, action("a")), (5, action("b")), (6, action("c"))];
        let mut overlays = OverlayState::new();
        overlays.push(Box::new(SidebarCursor::new(Some(action("b")))));
        assert_eq!(overlays.sidebar_rows(&rows), Some(5));
        overlays.handle_key(&press(PhysicalKey::ArrowDown));
        overlays.handle_key(&press_text(PhysicalKey::J, Some("j")));
        assert_eq!(overlays.sidebar_rows(&rows), Some(6), "stops at the end");
        overlays.handle_key(&press_text(PhysicalKey::K, Some("k")));
        overlays.handle_key(&press(PhysicalKey::ArrowUp));
        overlays.handle_key(&press(PhysicalKey::ArrowUp));
        assert_eq!(overlays.sidebar_rows(&rows), Some(2), "stops at the top");
        let outcome = overlays.handle_key(&press(PhysicalKey::Enter));
        assert_eq!(
            outcome,
            crate::render::overlay::OverlayOutcome::RunAction(action("a"))
        );
        assert!(!overlays.is_active());
        overlays.push(Box::new(SidebarCursor::new(None)));
        assert_eq!(overlays.sidebar_rows(&rows), Some(2));
        overlays.handle_key(&press(PhysicalKey::Escape));
        assert!(!overlays.is_active());
    }
}
