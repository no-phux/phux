//! Anchored context menu overlay (ADR-0058): a [`Modal`] frame pinned to the
//! pointer cell around a ratcn [`List`] whose rows commit
//! [`ResolvedAction`]s.
//!
//! The top-left corner sits on the anchor (flipping to stay inside the pane
//! area), so a click-and-release leaves the menu up while press-drag-release
//! commits the row under the pointer, with no mode flag.
//!
//! ratcn's list owns the cursor, hover tracking, separator skipping (they are
//! disabled rows), and the row hit-test. Two phux menu conventions sit on
//! top: a press or a release over a row picks it (the opening press happened
//! before the menu existed, so ratcn never sees a whole click), which is
//! routed as a ratcn click at that cell; and the cursor wraps at either end,
//! including for the wheel, which steps the cursor rather than scrolling.

use std::cell::Cell;
use std::rc::Rc;

use phux_config::keybind::ResolvedAction;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratcn::linear_nav::{Axis, step_key};
use ratcn::runtime::{
    self as rt, DeclareCtx, Event, EventResult, KeyCode, Modifiers, MouseKind, Ratcn, Step,
};
use ratcn::{List, ListItem, ListItemState};

use super::select_list::list_style;
use super::widgets::{MODAL_PAD, Modal, modal_inner_width};
use super::{OverlayCommand, RenderOverlay};
use crate::render::Theme;
use crate::render::kit::{self, Surface};

/// Blank columns either side of a row's text.
const PAD: u16 = 1;

/// The narrowest menu box, borders included: room for the key legend in the
/// bottom border.
const MIN_WIDTH: u16 = 20;

/// The legend inset into the bottom border (every modal carries one).
const MENU_KEYS: &str = "⏎ run · esc";

/// The ratcn id of the row list.
const ROWS: &str = "rows";

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

    /// The row's label, empty for a separator.
    fn label(&self) -> &str {
        match self {
            Self::Item { label, .. } => label,
            Self::Separator => "",
        }
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

/// What the menu's ratcn surface reads: the row the cursor is on (an index
/// into the rows, their ratcn value; always a selectable row).
#[derive(Debug, Clone)]
struct Model {
    selected: usize,
}

/// What the list asks for.
#[derive(Debug, Clone)]
enum Msg {
    Cursor(usize),
    Pick(usize),
}

/// A themed menu anchored at a screen cell. Geometry is resolved once at
/// construction: a pinned menu does not reflow.
#[derive(Debug)]
pub struct ContextMenu {
    /// Border title, e.g. `"pane"` or a window's name.
    title: String,
    /// Rows top to bottom, separators included.
    rows: Rc<[MenuRow]>,
    /// Color slots snapshotted at construction, like every other overlay.
    theme: Theme,
    /// The resolved box, in absolute viewport cells.
    rect: Rect,
    model: Model,
    ui: Surface<Model, Msg>,
    /// Whether the retained surface has been declared (input before the
    /// first paint declares it first, at the pinned rect).
    declared: Cell<bool>,
}

impl Clone for ContextMenu {
    /// The same menu and cursor; the retained surface is rebuilt.
    fn clone(&self) -> Self {
        Self {
            title: self.title.clone(),
            rows: Rc::clone(&self.rows),
            theme: self.theme,
            rect: self.rect,
            model: self.model.clone(),
            ui: Surface::new(Ratcn::new()),
            declared: Cell::new(false),
        }
    }
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
            rows: rows.into(),
            theme: *theme,
            rect,
            // The only focusable leaf, so keys reach the list unbound.
            model: Model { selected },
            ui: Surface::new(Ratcn::new()),
            declared: Cell::new(false),
        }
    }

    /// The box this menu occupies, in absolute viewport cells.
    #[cfg(test)]
    #[must_use]
    pub const fn rect(&self) -> Rect {
        self.rect
    }

    /// The rows' cells: the box interior inside the padding. A pointer on
    /// the border or the padding is on no row.
    const fn rows_area(&self) -> Rect {
        let r = self.rect;
        Rect::new(
            r.x.saturating_add(1 + MODAL_PAD),
            r.y.saturating_add(1),
            modal_inner_width(r.width),
            r.height.saturating_sub(2),
        )
    }

    /// `true` when `(x, y)` is inside the box, borders included.
    fn contains(&self, x: u16, y: u16) -> bool {
        let r = self.rect;
        (r.x..r.x.saturating_add(r.width)).contains(&x)
            && (r.y..r.y.saturating_add(r.height)).contains(&y)
    }

    /// Declare the rows as a ratcn list: separators are disabled rows the
    /// cursor and the pointer pass over.
    fn declare(&self) -> impl FnMut(&mut DeclareCtx<'_, Model, Msg>) + use<> {
        let rows = Rc::clone(&self.rows);
        let items: Vec<ListItem<usize>> = rows
            .iter()
            .enumerate()
            .map(|(i, row)| ListItem::new(i, row.label()).disabled(!row.is_selectable()))
            .collect();
        let theme = self.theme;
        let area = self.rows_area();
        move |ctx| {
            let rows = Rc::clone(&rows);
            let list = List::new(items.clone())
                .item_focus(|m: &Model| Some(m.selected), |row, _| Msg::Cursor(row))
                .selection(|_: &Model| None, Msg::Pick)
                .style(list_style)
                .paint_item(move |_: &Model, row: ListItemState<'_, usize>| {
                    row_line(&theme, &rows[row.index], row.focused, area.width)
                });
            ctx.component(ROWS, list, area);
        }
    }

    /// Declare and paint into `buf` over the pinned rect.
    fn paint_surface(&self, buf: &mut Buffer) {
        let theme = kit::theme(&self.theme);
        self.ui
            .paint(buf, self.rect, &self.model, &theme, self.declare());
        self.declared.set(true);
    }

    /// Route one ratcn event through the list, answering ratcn's own result.
    fn dispatch(&mut self, event: Event) -> EventResult<Msg> {
        if !self.declared.get() {
            self.paint_surface(&mut Buffer::empty(self.rect));
        }
        let theme = kit::theme(&self.theme);
        let declare = self.declare();
        self.ui.route(event, &self.model, &theme, declare)
    }

    /// Route `event` and fold its message into a command.
    fn route(&mut self, event: Event) -> OverlayCommand {
        let result = self.dispatch(event);
        self.apply(&result)
    }

    fn apply(&mut self, result: &EventResult<Msg>) -> OverlayCommand {
        match result {
            &EventResult::Emit(Msg::Cursor(row)) => {
                self.model.selected = row;
                OverlayCommand::Stay
            }
            &EventResult::Emit(Msg::Pick(row)) => {
                self.model.selected = row;
                match self.rows.get(row) {
                    Some(MenuRow::Item { action, .. }) => OverlayCommand::Commit(action.clone()),
                    _ => OverlayCommand::Stay,
                }
            }
            EventResult::Consumed | EventResult::Ignored => OverlayCommand::Stay,
        }
    }

    /// Route a key; a step key ratcn holds at an end (it does not wrap)
    /// wraps onto the other end.
    fn key(&mut self, key: rt::KeyEvent) -> OverlayCommand {
        let result = self.dispatch(Event::Key(key));
        let wrap = match step_key(key, Axis::Vertical) {
            Some(Step::Forward) => KeyCode::Home,
            Some(Step::Backward) => KeyCode::End,
            None => return self.apply(&result),
        };
        if matches!(result, EventResult::Consumed) {
            return self.route(Event::Key(rt::KeyEvent::new(wrap)));
        }
        self.apply(&result)
    }

    /// A pointer pick at `(x, y)`: a ratcn click there, which commits the
    /// row under it and is inert on the border or a separator.
    fn pick_at(&mut self, x: u16, y: u16) -> OverlayCommand {
        let at = |kind| {
            Event::Mouse(rt::MouseEvent {
                kind,
                column: x,
                row: y,
                modifiers: Modifiers::NONE,
            })
        };
        let left = rt::MouseButton::Left;
        let pressed = self.route(at(MouseKind::Down(left)));
        if pressed != OverlayCommand::Stay {
            return pressed;
        }
        self.route(at(MouseKind::Up(left)))
    }
}

/// One painted row: `" label      chord "` across the interior, the cursor
/// row a solid selection bar, or a blank for a separator.
fn row_line(theme: &Theme, row: &MenuRow, selected: bool, inner_width: u16) -> Line<'static> {
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
                    theme.on_selection(Style::default().fg(theme.selection_fg)),
                ))
            } else {
                Line::from(vec![
                    Span::styled(format!("{lead}{label}"), Style::default().fg(theme.text)),
                    Span::raw(" ".repeat(gap)),
                    Span::styled(format!("{sec}{trail}"), Style::default().fg(theme.dim)),
                ])
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
        Modal::new(&self.theme, self.title.clone(), Vec::new())
            .footer("", MENU_KEYS)
            .render_into(self.rect, buf);
        self.paint_surface(buf);
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
            PhysicalKey::Q if !ctrl => return OverlayCommand::Dismiss,
            _ => {}
        }
        // ratcn's list keys: arrows, Home/End, vi `j`/`k` and readline
        // `C-n`/`C-p` move; Enter and Space commit.
        match kit::key(key) {
            Some(Event::Key(key)) => self.key(key),
            _ => OverlayCommand::Stay,
        }
    }

    fn handle_mouse(&mut self, mouse: &MouseEvent) -> OverlayCommand {
        let (x, y) = (super::pointer_cell(mouse.x), super::pointer_cell(mouse.y));
        match mouse.action {
            // Hover moves the cursor, held or not (press-drag-release).
            MouseAction::Motion => self.route(Event::Mouse(rt::MouseEvent {
                kind: MouseKind::Moved,
                column: x,
                row: y,
                modifiers: Modifiers::NONE,
            })),
            // Releasing over a row picks it; the opening click's release
            // lands on the border and leaves the menu up.
            MouseAction::Release if self.contains(x, y) => self.pick_at(x, y),
            MouseAction::Release => OverlayCommand::Stay,
            MouseAction::Press => match mouse.button {
                MouseButton::Four => self.key(rt::KeyEvent::new(KeyCode::Up)),
                MouseButton::Five => self.key(rt::KeyEvent::new(KeyCode::Down)),
                // A press on the frame or a separator is inert.
                _ if self.contains(x, y) => self.pick_at(x, y),
                _ => OverlayCommand::Dismiss,
            },
        }
    }
}

#[cfg(test)]
mod tests;
