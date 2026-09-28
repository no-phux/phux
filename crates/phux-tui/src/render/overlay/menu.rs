//! Anchored context menu overlay (ADR-0058): a small [`Modal`] pinned to the
//! pointer cell, whose rows commit [`ResolvedAction`]s through `run_action`.
//!
//! The box's top-left corner sits on the anchor (flipping left/up to stay
//! inside the pane content rect, never over the sidebar or bar), so the
//! pointer rests on the border: a click-and-release leaves the menu up while
//! press-drag-release commits the row under the pointer, with no mode flag.
//!
//! Up/`k`/`C-p` and Down/`j`/`C-n` move (skipping separators, wrapping),
//! Home/End jump, the wheel steps, Enter commits, Esc/`q` dismisses. A press
//! on a row commits, outside dismisses (consumed). Motion hover-tracks, with
//! the driver raising any-motion reporting while a menu is open.

use phux_config::keybind::ResolvedAction;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::widgets::{MODAL_PAD, Modal, modal_inner_width};
use super::{OverlayCommand, RenderOverlay};
use crate::render::Theme;

/// Blank columns either side of a row's text.
const PAD: u16 = 1;

/// The narrowest menu box, borders included.
const MIN_WIDTH: u16 = 20;

/// One row of a [`ContextMenu`].
#[derive(Debug, Clone)]
pub enum MenuRow {
    /// A selectable row committing `action`.
    Item {
        /// Primary label, e.g. `"Split right"`.
        label: String,
        /// Right-aligned dim chord bound to `action`, if any.
        secondary: Option<String>,
        /// The action this row commits.
        action: ResolvedAction,
    },
    /// A horizontal rule grouping the rows around it. Never selectable.
    Separator,
}

impl MenuRow {
    /// A selectable row labelled `label` committing `action`, unannotated.
    #[must_use]
    pub fn item(label: impl Into<String>, action: ResolvedAction) -> Self {
        Self::Item {
            label: label.into(),
            secondary: None,
            action,
        }
    }

    /// Attach the chord annotation (no-op on a separator).
    #[must_use]
    pub fn secondary(mut self, secondary: Option<String>) -> Self {
        if let Self::Item {
            secondary: slot, ..
        } = &mut self
        {
            *slot = secondary;
        }
        self
    }

    /// `true` when this row can hold the selection.
    #[must_use]
    pub const fn is_selectable(&self) -> bool {
        matches!(self, Self::Item { .. })
    }

    /// Display width of the row's text, padding excluded.
    fn text_width(&self) -> usize {
        match self {
            Self::Item {
                label, secondary, ..
            } => {
                let sec = secondary.as_ref().map_or(0, |s| s.chars().count() + 2);
                label.chars().count() + sec
            }
            Self::Separator => 0,
        }
    }
}

/// A themed menu anchored at a screen cell. Geometry is resolved once at
/// construction: a pinned menu does not reflow.
#[derive(Debug, Clone)]
pub struct ContextMenu {
    /// Border title, e.g. `"pane"` or a window's name.
    title: String,
    /// Rows top to bottom, separators included.
    rows: Vec<MenuRow>,
    /// Index of the selected row; always a selectable row.
    selected: usize,
    /// Color slots snapshotted at construction, like every other overlay.
    theme: Theme,
    /// The resolved box, in absolute viewport cells.
    rect: Rect,
}

impl ContextMenu {
    /// A menu over `rows` with its top-left corner on `anchor`, clamped
    /// inside `area` (absolute viewport cells), selecting the first item.
    #[must_use]
    pub fn new(
        title: impl Into<String>,
        rows: Vec<MenuRow>,
        anchor: (u16, u16),
        area: crate::layout::Rect,
        theme: &Theme,
    ) -> Self {
        let title = title.into();
        let area = Rect::new(area.x, area.y, area.w, area.h);
        let rect = place(anchor, box_size(&title, &rows), area);
        let selected = rows.iter().position(MenuRow::is_selectable).unwrap_or(0);
        Self {
            title,
            rows,
            selected,
            theme: *theme,
            rect,
        }
    }

    /// The box this menu occupies, in absolute viewport cells.
    #[cfg(test)]
    #[must_use]
    pub const fn rect(&self) -> Rect {
        self.rect
    }

    /// The selectable row under `(x, y)`, or `None` on the border, a
    /// separator, or outside. Saturating: pointer cells may be `u16::MAX`.
    fn row_at(&self, x: u16, y: u16) -> Option<usize> {
        let r = self.rect;
        let inset_x = 1 + MODAL_PAD;
        let interior_x =
            r.x.saturating_add(inset_x)..r.x.saturating_add(r.width).saturating_sub(inset_x);
        let interior_y = r.y.saturating_add(1)..r.y.saturating_add(r.height).saturating_sub(1);
        if !interior_x.contains(&x) || !interior_y.contains(&y) {
            return None;
        }
        let idx = usize::from(y - r.y - 1);
        self.rows.get(idx).filter(|row| row.is_selectable())?;
        Some(idx)
    }

    /// `true` when `(x, y)` is inside the box, borders included.
    fn contains(&self, x: u16, y: u16) -> bool {
        let r = self.rect;
        (r.x..r.x.saturating_add(r.width)).contains(&x)
            && (r.y..r.y.saturating_add(r.height)).contains(&y)
    }

    /// Move the selection one selectable row, wrapping.
    fn step(&mut self, forward: bool) {
        let len = self.rows.len();
        for hop in 1..=len {
            let idx = if forward {
                (self.selected + hop) % len
            } else {
                (self.selected + len - hop) % len
            };
            if self.rows.get(idx).is_some_and(MenuRow::is_selectable) {
                self.selected = idx;
                return;
            }
        }
    }

    /// Select the first or (`to_last`) the last selectable row.
    fn jump(&mut self, to_last: bool) {
        let found = if to_last {
            self.rows.iter().rposition(MenuRow::is_selectable)
        } else {
            self.rows.iter().position(MenuRow::is_selectable)
        };
        if let Some(idx) = found {
            self.selected = idx;
        }
    }

    /// Commit the selected row, or stay when it somehow holds a separator.
    fn commit(&self) -> OverlayCommand {
        match self.rows.get(self.selected) {
            Some(MenuRow::Item { action, .. }) => OverlayCommand::Commit(action.clone()),
            _ => OverlayCommand::Stay,
        }
    }

    /// Select the row under `(x, y)`; the border or a separator keeps the
    /// selection (no flicker crossing a rule).
    fn hover(&mut self, x: u16, y: u16) {
        if let Some(idx) = self.row_at(x, y) {
            self.selected = idx;
        }
    }

    /// One painted line per row, padded so the selection bar spans the box.
    fn body_lines(&self, inner_width: u16) -> Vec<Line<'static>> {
        self.rows
            .iter()
            .enumerate()
            .map(|(i, row)| self.row_line(row, i == self.selected, inner_width))
            .collect()
    }

    /// One painted row: `" label      chord "`, or a rule for a separator.
    fn row_line(&self, row: &MenuRow, selected: bool, inner_width: u16) -> Line<'static> {
        let width = usize::from(inner_width);
        match row {
            MenuRow::Separator => Line::from(""),
            MenuRow::Item {
                label, secondary, ..
            } => {
                let pad = usize::from(PAD);
                let sec = secondary.clone().unwrap_or_default();
                let used = pad * 2 + label.chars().count() + sec.chars().count();
                let gap = width.saturating_sub(used).max(1);
                let lead = " ".repeat(pad);
                let trail = " ".repeat(pad);
                if selected {
                    // One run across the interior: a solid selection bar.
                    Line::from(Span::styled(
                        format!("{lead}{label}{}{sec}{trail}", " ".repeat(gap)),
                        Style::default()
                            .fg(self.theme.selection_fg)
                            .bg(self.theme.selection_bg),
                    ))
                } else {
                    Line::from(vec![
                        Span::styled(
                            format!("{lead}{label}"),
                            Style::default().fg(self.theme.text),
                        ),
                        Span::raw(" ".repeat(gap)),
                        Span::styled(format!("{sec}{trail}"), Style::default().fg(self.theme.dim)),
                    ])
                }
            }
        }
    }
}

/// The box size (borders included) that fits `title` and `rows`.
fn box_size(title: &str, rows: &[MenuRow]) -> (u16, u16) {
    let widest = rows.iter().map(MenuRow::text_width).max().unwrap_or(0);
    let text = u16::try_from(widest).unwrap_or(u16::MAX);
    let title_w = u16::try_from(title.chars().count()).unwrap_or(u16::MAX);
    let inner = text
        .saturating_add(PAD * 2)
        .max(title_w.saturating_add(2))
        .max(MIN_WIDTH.saturating_sub(2));
    let rows_h = u16::try_from(rows.len()).unwrap_or(u16::MAX);
    (
        inner.saturating_add(2 + MODAL_PAD * 2),
        rows_h.saturating_add(2),
    )
}

/// Place a `size` box with its top-left on `anchor`, flipping left/up onto
/// the opposite corner (so the pointer still touches it) and clamping into
/// `area`.
fn place(anchor: (u16, u16), size: (u16, u16), area: Rect) -> Rect {
    let (aw, ah) = size;
    let w = aw.min(area.width);
    let h = ah.min(area.height);
    let (ax, ay) = anchor;

    let right_edge = area.x.saturating_add(area.width);
    let bottom_edge = area.y.saturating_add(area.height);

    let mut x = if ax.saturating_add(w) > right_edge {
        ax.saturating_sub(w.saturating_sub(1))
    } else {
        ax
    };
    let mut y = if ay.saturating_add(h) > bottom_edge {
        ay.saturating_sub(h.saturating_sub(1))
    } else {
        ay
    };
    x = x.clamp(area.x, right_edge.saturating_sub(w).max(area.x));
    y = y.clamp(area.y, bottom_edge.saturating_sub(h).max(area.y));
    Rect::new(x, y, w, h)
}

impl RenderOverlay for ContextMenu {
    fn render(&self, _area: Rect, buf: &mut Buffer) {
        // Geometry is pinned; paint the box whole or not at all (a clipped
        // box would disagree with the pinned hit-test after a resize).
        if self.rect.intersection(buf.area) != self.rect
            || self.rect.width < 2
            || self.rect.height < 2
        {
            return;
        }
        let body = self.body_lines(modal_inner_width(self.rect.width));
        Modal::new(&self.theme, self.title.clone(), body).render_into(self.rect, buf);
    }

    fn bounds(&self, _area: Rect) -> Option<Rect> {
        Some(self.rect)
    }

    fn wants_pointer_hover(&self) -> bool {
        true
    }

    /// Pinned to the pre-resize pointer cell, so a resize drops it.
    fn survives_resize(&self) -> bool {
        false
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        if key.action != KeyAction::Press {
            return OverlayCommand::Stay;
        }
        let ctrl = key.mods.contains(ModSet::CTRL);
        match key.key {
            PhysicalKey::Escape => return OverlayCommand::Dismiss,
            PhysicalKey::Enter | PhysicalKey::NumpadEnter | PhysicalKey::Space => {
                return self.commit();
            }
            PhysicalKey::ArrowUp => self.step(false),
            PhysicalKey::ArrowDown => self.step(true),
            PhysicalKey::Home => self.jump(false),
            PhysicalKey::End => self.jump(true),
            // vi and readline navigation; a menu has no query line.
            PhysicalKey::K if !ctrl => self.step(false),
            PhysicalKey::J if !ctrl => self.step(true),
            PhysicalKey::P if ctrl => self.step(false),
            PhysicalKey::N if ctrl => self.step(true),
            PhysicalKey::Q if !ctrl => return OverlayCommand::Dismiss,
            _ => {}
        }
        OverlayCommand::Stay
    }

    fn handle_mouse(&mut self, mouse: &MouseEvent) -> OverlayCommand {
        let (x, y) = (super::pointer_cell(mouse.x), super::pointer_cell(mouse.y));
        match mouse.action {
            MouseAction::Motion => {
                self.hover(x, y);
                OverlayCommand::Stay
            }
            MouseAction::Release => {
                // Releasing over a row picks it; the opening click's release
                // lands on the border and leaves the menu up.
                if self.row_at(x, y).is_some() {
                    self.hover(x, y);
                    return self.commit();
                }
                OverlayCommand::Stay
            }
            MouseAction::Press => match mouse.button {
                MouseButton::Four => {
                    self.step(false);
                    OverlayCommand::Stay
                }
                MouseButton::Five => {
                    self.step(true);
                    OverlayCommand::Stay
                }
                _ => {
                    if self.contains(x, y) {
                        self.hover(x, y);
                        if self.row_at(x, y).is_some() {
                            return self.commit();
                        }
                        // A press on the frame or a separator is inert.
                        return OverlayCommand::Stay;
                    }
                    OverlayCommand::Dismiss
                }
            },
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    fn area(x: u16, y: u16, w: u16, h: u16) -> crate::layout::Rect {
        crate::layout::Rect { x, y, w, h }
    }

    fn action(name: &str) -> ResolvedAction {
        ResolvedAction {
            action: name.to_owned(),
            args: std::collections::BTreeMap::new(),
        }
    }

    /// split-pane, toggle-zoom, a separator, kill-pane.
    fn rows() -> Vec<MenuRow> {
        vec![
            MenuRow::item("Split right", action("split-pane")).secondary(Some("C-a |".to_owned())),
            MenuRow::item("Zoom", action("toggle-zoom")),
            MenuRow::Separator,
            MenuRow::item("Close pane", action("kill-pane")),
        ]
    }

    fn menu_in(anchor: (u16, u16), area: crate::layout::Rect) -> ContextMenu {
        ContextMenu::new("pane", rows(), anchor, area, &Theme::default())
    }

    fn menu_at(x: u16, y: u16) -> ContextMenu {
        menu_in((x, y), area(0, 0, 80, 24))
    }

    fn mouse(action: MouseAction, button: MouseButton, x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            action,
            button,
            mods: ModSet::empty(),
            x: f64::from(x),
            y: f64::from(y),
        }
    }

    fn press(x: u16, y: u16) -> MouseEvent {
        mouse(MouseAction::Press, MouseButton::Left, x, y)
    }

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

    fn committed(cmd: OverlayCommand) -> String {
        match cmd {
            OverlayCommand::Commit(a) => a.action,
            other => panic!("expected a commit, got {other:?}"),
        }
    }

    fn painted(menu: &ContextMenu, cols: u16, rows_h: u16) -> String {
        let area = Rect::new(0, 0, cols, rows_h);
        let mut buf = Buffer::empty(area);
        menu.render(area, &mut buf);
        (0..rows_h)
            .flat_map(|y| (0..cols).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].symbol().to_owned())
            .collect()
    }

    #[test]
    fn placement_anchors_flips_and_clamps_into_the_area() {
        let r = menu_at(10, 4).rect();
        assert_eq!((r.x, r.y, r.height), (10, 4, 6), "corner on the anchor");

        // Bottom-right: flips up and left, still touching the anchor.
        let r = menu_at(79, 23).rect();
        assert_eq!((r.x + r.width - 1, r.y + r.height - 1), (79, 23));

        let r = menu_in((7, 2), area(0, 0, 8, 3)).rect();
        assert!(r.x + r.width <= 8 && r.y + r.height <= 3, "{r:?}");

        // Never over a left sidebar or a top status bar.
        let r = menu_in((3, 0), area(20, 1, 60, 23)).rect();
        assert!(r.x >= 20 && r.y >= 1, "{r:?}");

        let titled = ContextMenu::new(
            "a very long window name indeed",
            rows(),
            (0, 0),
            area(0, 0, 80, 24),
            &Theme::default(),
        );
        assert!(titled.rect().width >= 32, "the title fits");
    }

    /// The opening click's release lands on the border corner and leaves
    /// the menu up; a release or press on a row commits it; a press on the
    /// border or separator is inert; any press outside dismisses.
    #[test]
    fn pointer_input_commits_rows_and_dismisses_outside() {
        let mut menu = menu_at(10, 4);
        assert_eq!(
            menu.handle_mouse(&mouse(MouseAction::Release, MouseButton::Left, 10, 4)),
            OverlayCommand::Stay
        );
        assert_eq!(
            committed(menu.handle_mouse(&mouse(MouseAction::Release, MouseButton::Left, 12, 5))),
            "split-pane"
        );
        assert_eq!(
            committed(menu_at(10, 4).handle_mouse(&press(12, 8))),
            "kill-pane"
        );
        assert_eq!(
            menu.handle_mouse(&press(12, 7)),
            OverlayCommand::Stay,
            "separator"
        );
        assert_eq!(
            menu.handle_mouse(&press(10, 5)),
            OverlayCommand::Stay,
            "border"
        );
        for button in [MouseButton::Left, MouseButton::Right] {
            assert_eq!(
                menu_at(10, 4).handle_mouse(&mouse(MouseAction::Press, button, 2, 2)),
                OverlayCommand::Dismiss
            );
        }

        let mut menu = menu_at(10, 4);
        menu.handle_mouse(&mouse(MouseAction::Motion, MouseButton::Left, 12, 6));
        assert_eq!(
            committed(menu.handle_key(&key(PhysicalKey::Enter))),
            "toggle-zoom",
            "hover"
        );
        menu.handle_mouse(&mouse(MouseAction::Press, MouseButton::Four, 12, 5));
        assert_eq!(menu.selected, 0, "wheel up");
        menu.handle_mouse(&mouse(MouseAction::Press, MouseButton::Five, 12, 5));
        assert_eq!(menu.selected, 1, "wheel down");
    }

    #[test]
    fn keys_skip_separators_wrap_jump_and_dismiss() {
        let mut menu = menu_at(10, 4);
        menu.handle_key(&key(PhysicalKey::ArrowDown));
        menu.handle_key(&key(PhysicalKey::ArrowDown));
        assert_eq!(
            committed(menu.handle_key(&key(PhysicalKey::Enter))),
            "kill-pane"
        );
        let mut menu = menu_at(10, 4);
        menu.handle_key(&key(PhysicalKey::ArrowUp));
        assert_eq!(
            committed(menu.handle_key(&key(PhysicalKey::Enter))),
            "kill-pane",
            "wraps"
        );
        menu.handle_key(&key(PhysicalKey::Home));
        assert_eq!(menu.selected, 0);
        menu.handle_key(&key(PhysicalKey::End));
        assert_eq!(menu.selected, 3);
        assert_eq!(
            menu.handle_key(&key(PhysicalKey::Escape)),
            OverlayCommand::Dismiss
        );
    }

    /// Painted whole or not at all: after a resize a truncated box would
    /// show rows that disagree with the hit-test (which reads the pinned
    /// rect). The driver then drops it because it does not survive resize.
    #[test]
    fn a_menu_paints_whole_or_not_at_all() {
        let menu = menu_at(10, 4);
        let row: String = painted(&menu, 80, 24);
        assert!(row.contains("Split right") && row.contains("C-a |"));

        let menu = menu_at(70, 18);
        assert!(painted(&menu, 40, 10).trim().is_empty(), "fully clipped");
        assert!(
            painted(&menu, 60, 20).trim().is_empty(),
            "partially clipped"
        );
        assert!(painted(&menu, 80, 24).contains("Split right"));
        assert!(!menu.survives_resize());
    }
}
