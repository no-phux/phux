//! Anchored context menu overlay (phux-wrnm, ADR-0058).
//!
//! A small themed [`Modal`] pinned to the cell the pointer was over when
//! the menu opened, rather than centered like every other overlay. Each
//! row carries a [`ResolvedAction`] it commits — the same
//! `run_action` dispatch path a keybinding, a palette row, or a sidebar
//! click takes, so a menu introduces no bespoke click semantics.
//!
//! ## Placement
//!
//! The box's top-left corner sits *on* the anchor cell, so the pointer
//! rests on the border corner and the first row is one cell down-right —
//! the placement native menus use. It flips left when it would overflow
//! the right edge and up when it would overflow the bottom (a bottom
//! status-bar click therefore opens upward), then clamps into the area it
//! was built against. That area is the pane content rect, not the raw
//! viewport, so a menu never occludes the sidebar strip or the status bar
//! (the same invariant centered modals hold — phux-foz.14).
//!
//! Because the pointer lands on the border, a release *at the anchor*
//! never hits a row: opening with a click-and-release leaves the menu up
//! (Windows-style click, move, click) while press-drag-release commits the
//! row under the pointer on release (macOS-style). Both work without a
//! mode flag.
//!
//! ## Input model
//!
//! - Up / `k` / `C-p` and Down / `j` / `C-n` move the selection, skipping
//!   separators and wrapping at the ends; Home / End jump to the first /
//!   last row; the wheel moves one row per detent.
//! - Enter commits the selected row; Esc / `q` dismisses.
//! - A press on a row commits it; a press anywhere outside the box
//!   dismisses (the click is consumed — dismissing a menu should not also
//!   act on what is beneath it).
//! - Motion over a row moves the selection to it, so the menu hover-tracks
//!   the pointer. The driver switches the outer terminal to any-motion
//!   tracking while a menu is open ([`RenderOverlay::wants_pointer_hover`])
//!   so hovering works with no button held.
//! - A release on a row commits it (press-drag-release); anywhere else it
//!   is swallowed, leaving the menu open.

use phux_config::keybind::ResolvedAction;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::widgets::Modal;
use super::{OverlayCommand, RenderOverlay};
use crate::render::Theme;

/// Interior padding: one blank column either side of a row's text, so
/// labels don't sit flush against the border.
const PAD: u16 = 1;

/// The narrowest a menu box may be, borders included. Keeps a one-word
/// menu from rendering as a sliver next to its title.
const MIN_WIDTH: u16 = 20;

/// One row of a [`ContextMenu`].
#[derive(Debug, Clone)]
pub enum MenuRow {
    /// A selectable row: `label` on the left, the optional `secondary`
    /// (its bound chord) dim on the right, committing `action` when
    /// chosen.
    Item {
        /// Primary label, e.g. `"Split right"`.
        label: String,
        /// Right-aligned dim annotation — the chord bound to `action`, or
        /// `None` when it is unbound.
        secondary: Option<String>,
        /// The action this row commits, identical in shape to what a
        /// keybinding resolves to.
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

    /// Attach the right-aligned chord annotation `secondary`. A no-op on a
    /// [`MenuRow::Separator`].
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

/// A themed menu anchored at a screen cell.
///
/// Build with [`ContextMenu::new`] and push the boxed value onto the
/// [`OverlayState`](super::OverlayState) stack. Geometry is resolved once,
/// at construction, against the area the caller passes: the menu is a
/// transient object pinned to a pointer position, so it does not reflow.
#[derive(Debug, Clone)]
pub struct ContextMenu {
    /// Border title, e.g. `"pane"` or a window's name.
    title: String,
    /// Rows top to bottom, separators included.
    rows: Vec<MenuRow>,
    /// Index into `rows` of the selected row. Always points at a
    /// selectable row once constructed (menus are built with at least one).
    selected: usize,
    /// Color slots snapshotted at construction, like every other overlay.
    theme: Theme,
    /// The resolved box, in absolute viewport cells.
    rect: Rect,
}

impl ContextMenu {
    /// A menu titled `title` over `rows`, anchored so its top-left corner
    /// sits on cell `anchor` and clamped inside `area` (both in absolute
    /// viewport cells).
    ///
    /// The selection starts on the first selectable row.
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
    #[must_use]
    pub const fn rect(&self) -> Rect {
        self.rect
    }

    /// The row index under cell `(x, y)`, or `None` when the cell is on the
    /// border, on a separator, or outside the box entirely.
    ///
    /// Saturating throughout: pointer coordinates come off the wire via
    /// `quantize`, which clamps a malformed report to `u16::MAX` rather
    /// than rejecting it.
    fn row_at(&self, x: u16, y: u16) -> Option<usize> {
        let r = self.rect;
        let interior_x = r.x.saturating_add(1)..r.x.saturating_add(r.width).saturating_sub(1);
        let interior_y = r.y.saturating_add(1)..r.y.saturating_add(r.height).saturating_sub(1);
        if !interior_x.contains(&x) || !interior_y.contains(&y) {
            return None;
        }
        let idx = usize::from(y - r.y - 1);
        self.rows.get(idx).filter(|row| row.is_selectable())?;
        Some(idx)
    }

    /// `true` when cell `(x, y)` is anywhere inside the box, borders
    /// included.
    fn contains(&self, x: u16, y: u16) -> bool {
        let r = self.rect;
        (r.x..r.x.saturating_add(r.width)).contains(&x)
            && (r.y..r.y.saturating_add(r.height)).contains(&y)
    }

    /// Move the selection by `delta` rows, skipping separators and
    /// wrapping at both ends. A menu with no selectable row is a no-op.
    fn step(&mut self, delta: isize) {
        let len = self.rows.len();
        if len == 0 {
            return;
        }
        for hop in 1..=len {
            #[allow(
                clippy::cast_possible_wrap,
                reason = "menu rows are a handful; the cast cannot overflow isize"
            )]
            let len_i = len as isize;
            #[allow(
                clippy::cast_possible_wrap,
                reason = "selected is an index into the same short vec"
            )]
            let cur = self.selected as isize;
            #[allow(clippy::cast_possible_wrap, reason = "hop is bounded by rows.len()")]
            let idx = (cur + delta * hop as isize).rem_euclid(len_i);
            #[allow(
                clippy::cast_sign_loss,
                reason = "rem_euclid against a positive modulus is non-negative"
            )]
            let idx = idx as usize;
            if self.rows.get(idx).is_some_and(MenuRow::is_selectable) {
                self.selected = idx;
                return;
            }
        }
    }

    /// Select the first (`Forward`) or last (`Backward`) selectable row.
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

    /// Move the selection to the row under `(x, y)`, if the pointer is over
    /// one. A pointer on the border or a separator leaves the selection
    /// where it is rather than clearing it — the highlight should not
    /// flicker off as the pointer crosses a rule.
    fn hover(&mut self, x: u16, y: u16) {
        if let Some(idx) = self.row_at(x, y) {
            self.selected = idx;
        }
    }

    /// The painted body: one [`Line`] per row, padded to the interior
    /// width so a selected row's highlight spans the full box.
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
            MenuRow::Separator => Line::from(Span::styled(
                "─".repeat(width),
                Style::default().fg(self.theme.border),
            )),
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
                    // One styled run across the whole interior so the
                    // highlight reads as a solid bar, chord included.
                    Line::from(Span::styled(
                        format!("{lead}{label}{}{sec}{trail}", " ".repeat(gap)),
                        Style::default()
                            .fg(self.theme.selection_fg)
                            .bg(self.theme.selection_bg)
                            .add_modifier(Modifier::BOLD),
                    ))
                } else {
                    Line::from(vec![
                        Span::styled(
                            format!("{lead}{label}"),
                            Style::default().fg(self.theme.action),
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
    // Interior = padding + text; the title needs two spaces of its own
    // plus the corners it is centered between.
    let title_w = u16::try_from(title.chars().count()).unwrap_or(u16::MAX);
    let inner = text
        .saturating_add(PAD * 2)
        .max(title_w.saturating_add(2))
        .max(MIN_WIDTH.saturating_sub(2));
    let rows_h = u16::try_from(rows.len()).unwrap_or(u16::MAX);
    (inner.saturating_add(2), rows_h.saturating_add(2))
}

/// Place a `size` box with its top-left corner on `anchor`, flipping left
/// / up when it would overflow `area` and clamping into it.
///
/// Flipping is anchored on the opposite corner (`anchor - (size - 1)`) so
/// the pointer still touches the box after the flip — a menu opened from
/// the bottom status-bar row grows upward with its bottom border under the
/// pointer.
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
        // Geometry was resolved against the content rect at construction
        // (menus pin to a pointer, they do not reflow), so `area` is not
        // consulted here.
        //
        // phux-fsb: paint the box whole or not at all. A resize between the
        // open and this paint can leave `self.rect` hanging off the new
        // viewport, and a clipped paint is worse than none: the hit-test
        // reads the pinned rect, so a truncated box shows one set of rows
        // while `row_at` resolves another — including rows past the fold
        // that were never drawn. Bailing keeps paint and hit-test talking
        // about the same cells; the driver drops the menu on the same
        // SIGWINCH edge (`OverlayState::dismiss_stale_on_resize`), so the
        // blank frame lasts at most until that arm runs.
        if self.rect.intersection(buf.area) != self.rect
            || self.rect.width < 2
            || self.rect.height < 2
        {
            return;
        }
        let body = self.body_lines(self.rect.width.saturating_sub(2));
        Modal::new(&self.theme, self.title.clone(), body).render_into(self.rect, buf);
    }

    fn bounds(&self, _area: Rect) -> Option<Rect> {
        Some(self.rect)
    }

    /// A menu tracks the pointer with no button held, so the driver flips
    /// the outer terminal to any-motion reporting while it is open.
    fn wants_pointer_hover(&self) -> bool {
        true
    }

    /// phux-fsb: a menu is pinned to the pointer cell it was opened at,
    /// against the content rect of the viewport that existed then. A resize
    /// invalidates that box, so the driver drops the menu instead of
    /// leaving an invisible overlay eating input.
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
            PhysicalKey::ArrowUp => self.step(-1),
            PhysicalKey::ArrowDown => self.step(1),
            PhysicalKey::Home => self.jump(false),
            PhysicalKey::End => self.jump(true),
            // vi navigation and the readline pair, matching the select-list
            // overlays. A menu has no query line, so `j`/`k` are always
            // navigation here rather than filter text.
            PhysicalKey::K if !ctrl => self.step(-1),
            PhysicalKey::J if !ctrl => self.step(1),
            PhysicalKey::P if ctrl => self.step(-1),
            PhysicalKey::N if ctrl => self.step(1),
            PhysicalKey::Q if !ctrl => return OverlayCommand::Dismiss,
            _ => {}
        }
        OverlayCommand::Stay
    }

    fn handle_mouse(&mut self, mouse: &MouseEvent) -> OverlayCommand {
        let (x, y) = (quantize(mouse.x), quantize(mouse.y));
        match mouse.action {
            MouseAction::Motion => {
                self.hover(x, y);
                OverlayCommand::Stay
            }
            MouseAction::Release => {
                // Press-drag-release: releasing over a row picks it. The
                // opening press left the pointer on the border corner, so a
                // click-and-release that never moved lands here with no row
                // and simply leaves the menu up.
                if self.row_at(x, y).is_some() {
                    self.hover(x, y);
                    return self.commit();
                }
                OverlayCommand::Stay
            }
            MouseAction::Press => match mouse.button {
                MouseButton::Four => {
                    self.step(-1);
                    OverlayCommand::Stay
                }
                MouseButton::Five => {
                    self.step(1);
                    OverlayCommand::Stay
                }
                _ => {
                    if self.contains(x, y) {
                        self.hover(x, y);
                        if self.row_at(x, y).is_some() {
                            return self.commit();
                        }
                        // A press on the border or a separator is inert,
                        // not a dismissal: clicking the frame of a menu
                        // should never fire the row nearest to it.
                        return OverlayCommand::Stay;
                    }
                    OverlayCommand::Dismiss
                }
            },
        }
    }
}

/// Quantise an f64 pointer position (1-px-per-cell per SPEC L1 §9.2.1) to a
/// viewport cell, saturating like the dispatcher's own hit-test.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "cell-quantised SGR input; saturate rather than let a malformed report break routing"
)]
fn quantize(p: f64) -> u16 {
    if p.is_nan() || p < 0.0 {
        0
    } else if p >= f64::from(u16::MAX) {
        u16::MAX
    } else {
        p as u16
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    fn area() -> crate::layout::Rect {
        crate::layout::Rect {
            x: 0,
            y: 0,
            w: 80,
            h: 24,
        }
    }

    fn action(name: &str) -> ResolvedAction {
        ResolvedAction {
            action: name.to_owned(),
            args: std::collections::BTreeMap::new(),
        }
    }

    fn rows() -> Vec<MenuRow> {
        vec![
            MenuRow::item("Split right", action("split-pane")).secondary(Some("C-a |".to_owned())),
            MenuRow::item("Zoom", action("toggle-zoom")),
            MenuRow::Separator,
            MenuRow::item("Close pane", action("kill-pane")),
        ]
    }

    fn menu_at(x: u16, y: u16) -> ContextMenu {
        ContextMenu::new("pane", rows(), (x, y), area(), &Theme::default())
    }

    fn press(button: MouseButton, x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            action: MouseAction::Press,
            button,
            mods: ModSet::empty(),
            x: f64::from(x),
            y: f64::from(y),
        }
    }

    fn at(action: MouseAction, x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            action,
            button: MouseButton::Left,
            mods: ModSet::empty(),
            x: f64::from(x),
            y: f64::from(y),
        }
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

    #[test]
    fn box_corner_sits_on_the_anchor() {
        let menu = menu_at(10, 4);
        assert_eq!(menu.rect().x, 10);
        assert_eq!(menu.rect().y, 4);
        // Four rows plus two borders.
        assert_eq!(menu.rect().height, 6);
    }

    #[test]
    fn overflowing_right_or_bottom_flips_back_onto_the_pointer() {
        // Anchored in the bottom-right corner: the box must flip both ways
        // and still touch the anchor cell.
        let menu = ContextMenu::new("pane", rows(), (79, 23), area(), &Theme::default());
        let r = menu.rect();
        assert_eq!(r.x + r.width - 1, 79, "right edge lands on the anchor");
        assert_eq!(r.y + r.height - 1, 23, "bottom edge lands on the anchor");
        assert!(r.x < 79 && r.y < 23, "the box grew up and to the left");
    }

    #[test]
    fn a_box_wider_than_the_area_clamps_instead_of_underflowing() {
        let narrow = crate::layout::Rect {
            x: 0,
            y: 0,
            w: 8,
            h: 3,
        };
        let menu = ContextMenu::new("pane", rows(), (7, 2), narrow, &Theme::default());
        let r = menu.rect();
        assert!(r.x + r.width <= 8, "clamped to the area width: {r:?}");
        assert!(r.y + r.height <= 3, "clamped to the area height: {r:?}");
    }

    #[test]
    fn the_area_offset_is_honoured() {
        // Sidebar docked left (x = 20) with a top status bar (y = 1): a
        // menu anchored inside the strip clamps out of the chrome.
        let content = crate::layout::Rect {
            x: 20,
            y: 1,
            w: 60,
            h: 23,
        };
        let menu = ContextMenu::new("window", rows(), (3, 0), content, &Theme::default());
        assert!(menu.rect().x >= 20, "never over the sidebar strip");
        assert!(menu.rect().y >= 1, "never over the status bar");
    }

    #[test]
    fn a_release_on_the_anchor_corner_leaves_the_menu_open() {
        let mut menu = menu_at(10, 4);
        assert_eq!(
            menu.handle_mouse(&at(MouseAction::Release, 10, 4)),
            OverlayCommand::Stay,
            "the opening click's release is on the border, not a row",
        );
    }

    #[test]
    fn a_release_over_a_row_commits_it() {
        let mut menu = menu_at(10, 4);
        // First interior row is y = 5; interior x starts at 11.
        let OverlayCommand::Commit(a) = menu.handle_mouse(&at(MouseAction::Release, 12, 5)) else {
            panic!("release over a row commits");
        };
        assert_eq!(a.action, "split-pane");
    }

    #[test]
    fn a_press_over_a_row_commits_and_outside_dismisses() {
        let mut menu = menu_at(10, 4);
        let OverlayCommand::Commit(a) = menu.handle_mouse(&press(MouseButton::Left, 12, 8)) else {
            panic!("press over the fourth row commits");
        };
        assert_eq!(a.action, "kill-pane");

        let mut menu = menu_at(10, 4);
        assert_eq!(
            menu.handle_mouse(&press(MouseButton::Left, 2, 2)),
            OverlayCommand::Dismiss,
        );
        // Right-click outside dismisses too — the button does not matter.
        let mut menu = menu_at(10, 4);
        assert_eq!(
            menu.handle_mouse(&press(MouseButton::Right, 2, 2)),
            OverlayCommand::Dismiss,
        );
    }

    #[test]
    fn a_press_on_the_separator_or_border_is_inert() {
        let mut menu = menu_at(10, 4);
        // Separator is the third row (y = 7); the left border is x = 10.
        assert_eq!(
            menu.handle_mouse(&press(MouseButton::Left, 12, 7)),
            OverlayCommand::Stay,
        );
        assert_eq!(
            menu.handle_mouse(&press(MouseButton::Left, 10, 5)),
            OverlayCommand::Stay,
        );
    }

    #[test]
    fn motion_hover_tracks_the_pointer() {
        let mut menu = menu_at(10, 4);
        menu.handle_mouse(&at(MouseAction::Motion, 12, 6));
        let OverlayCommand::Commit(a) = menu.handle_key(&key(PhysicalKey::Enter)) else {
            panic!("Enter commits the hovered row");
        };
        assert_eq!(a.action, "toggle-zoom");
    }

    #[test]
    fn keyboard_navigation_skips_separators_and_wraps() {
        let mut menu = menu_at(10, 4);
        // Rows: split-pane, toggle-zoom, separator, kill-pane.
        menu.handle_key(&key(PhysicalKey::ArrowDown));
        menu.handle_key(&key(PhysicalKey::ArrowDown));
        let OverlayCommand::Commit(a) = menu.handle_key(&key(PhysicalKey::Enter)) else {
            panic!("Enter commits");
        };
        assert_eq!(a.action, "kill-pane", "the separator was stepped over");

        let mut menu = menu_at(10, 4);
        menu.handle_key(&key(PhysicalKey::ArrowUp));
        let OverlayCommand::Commit(a) = menu.handle_key(&key(PhysicalKey::Enter)) else {
            panic!("Enter commits");
        };
        assert_eq!(a.action, "kill-pane", "Up from the first row wraps");
    }

    #[test]
    fn end_and_home_jump_to_the_edges() {
        let mut menu = menu_at(10, 4);
        menu.handle_key(&key(PhysicalKey::End));
        assert_eq!(menu.selected, 3);
        menu.handle_key(&key(PhysicalKey::Home));
        assert_eq!(menu.selected, 0);
    }

    #[test]
    fn escape_dismisses() {
        let mut menu = menu_at(10, 4);
        assert_eq!(
            menu.handle_key(&key(PhysicalKey::Escape)),
            OverlayCommand::Dismiss,
        );
    }

    #[test]
    fn the_wheel_moves_the_selection() {
        let mut menu = menu_at(10, 4);
        menu.handle_mouse(&press(MouseButton::Five, 12, 5));
        assert_eq!(menu.selected, 1);
        menu.handle_mouse(&press(MouseButton::Four, 12, 5));
        assert_eq!(menu.selected, 0);
    }

    #[test]
    fn the_box_is_wide_enough_for_its_widest_row_and_its_title() {
        let menu = ContextMenu::new(
            "a very long window name indeed",
            rows(),
            (0, 0),
            area(),
            &Theme::default(),
        );
        assert!(
            menu.rect().width >= 32,
            "the title must fit inside the border: {:?}",
            menu.rect(),
        );
    }

    #[test]
    fn the_menu_paints_its_rows_inside_its_box() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        let menu = menu_at(10, 4);
        menu.render(Rect::new(0, 0, 80, 24), &mut buf);
        let row: String = (0..80).map(|x| buf[(x, 5)].symbol()).collect();
        assert!(row.contains("Split right"), "first row painted: {row:?}");
        assert!(row.contains("C-a |"), "chord annotation painted: {row:?}");
    }

    // ---------- phux-fsb: a resize invalidates a pinned menu ----------

    /// Anything painted into `buf`, as one flat string.
    fn painted(menu: &ContextMenu, cols: u16, rows_h: u16) -> String {
        let area = Rect::new(0, 0, cols, rows_h);
        let mut buf = Buffer::empty(area);
        menu.render(area, &mut buf);
        (0..rows_h)
            .flat_map(|y| (0..cols).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].symbol().to_owned())
            .collect()
    }

    /// phux-fsb: a menu is painted whole or not at all.
    ///
    /// A partial paint is the dangerous state: the box on screen shows one
    /// set of rows while `row_at` — which reads the pinned, pre-resize
    /// `rect` — resolves a different set, including rows below the fold
    /// that were never drawn. Committing one of those is a click on a
    /// command the user cannot see.
    #[test]
    fn a_menu_clipped_by_a_resize_paints_nothing() {
        // Built for 80x24, anchored bottom-right so the box lands around
        // x 56.., y 14.. — then the terminal shrinks under it.
        let menu = ContextMenu::new("pane", rows(), (70, 18), area(), &Theme::default());
        let r = menu.rect();
        assert!(
            r.x > 40 || r.y > 10,
            "fixture must land outside 40x10: {r:?}"
        );

        // Fully outside the new viewport.
        assert!(
            painted(&menu, 40, 10).trim().is_empty(),
            "a fully clipped menu must paint nothing",
        );
        // Partially overlapping it: the box would be truncated, so it must
        // also paint nothing rather than a sliver whose rows disagree with
        // the hit-test.
        assert!(
            painted(&menu, 60, 20).trim().is_empty(),
            "a partially clipped menu must paint nothing, not a truncated box",
        );
        // Unchanged viewport still paints normally.
        assert!(
            painted(&menu, 80, 24).contains("Split right"),
            "the un-resized case is untouched",
        );
    }

    /// phux-fsb: the menu declares that its geometry does not survive a
    /// resize, which is what makes the driver drop it. Without this the
    /// overlay stays active and invisible, capturing every keystroke —
    /// and Enter commits whatever row the selection happens to hold.
    #[test]
    fn a_menu_does_not_survive_a_resize() {
        assert!(!menu_at(10, 4).survives_resize());
    }
}
