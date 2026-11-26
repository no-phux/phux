//! Server-backed host path browse/search, distinct from the spawn-only
//! directory picker: a ratcn [`Input`] over a ratcn [`List`] in a
//! [`Dialog`].
//!
//! The field is the query; nonempty, it asks the host for a recursive fuzzy
//! search, empty, it browses one directory level. The rows are the host's
//! answer in the host's order. Up/Down (and `C-p`/`C-n`, the wheel, and the
//! Page keys) move the highlighted row while typing stays in the field;
//! Enter or a click commits a row, Esc closes.

mod rows;

use std::cell::Cell;

use phux_config::keybind::ResolvedAction;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::wire::frame::PathQueryResult;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratcn::runtime::{DeclareCtx, Event, EventResult, FocusState, Ratcn};
use ratcn::{Dialog, Input, InputState, List, ListItem, ListItemState, ListStyle};

use self::rows::{Row, rows};
use super::prompt::{field_key, insert_literal};
use super::widgets::{centered_panel, scroll_into_view};
use super::{OverlayCommand, RenderOverlay};
use crate::render::kit::{self, Surface};
use crate::render::{ChromeBreakpoints, Theme, clip_left, clip_text, display_width, home_to_tilde};

const DIALOG: &str = "path-picker";
const QUERY: &str = "query";
const ROWS: &str = "rows";

/// Dialog rows that are not list rows: border and padding above and below,
/// the query field, the gap row, and the footer.
const CHROME_ROWS: u16 = 2 + 2 + 1 + 1 + 1;

/// Rows the highlight moves per wheel detent (matches copy-mode).
const WHEEL_ROWS: isize = 3;

/// Cells between a row's label and its secondary.
const GAP: usize = 1;

/// What the picker's ratcn surface tracks between frames.
#[derive(Debug)]
struct Model {
    focus: FocusState,
    query: InputState,
    rows: Vec<Row>,
    /// The highlighted row, always one with an action.
    current: Option<usize>,
    /// The first visible row.
    scroll: usize,
}

/// What the field, the rows, and focus traversal ask for.
#[derive(Debug, Clone)]
enum Msg {
    Focus(FocusState),
    Query(InputState),
    Pick(usize),
    Scroll(usize),
    Dismiss,
}

/// A host path picker. Nonempty input requests recursive fuzzy search on
/// the host; empty input browses one directory level.
#[derive(Debug)]
pub struct PathPicker {
    root: String,
    /// The root as the footer shows it (`~` for home).
    status: String,
    theme: Theme,
    breakpoints: ChromeBreakpoints,
    model: Model,
    ui: Surface<Model, Msg>,
    /// The viewport of the last paint, which input is laid out against.
    outer: Cell<Rect>,
    /// The tallest box painted so far: a refresh that brings fewer rows (a
    /// search between keystrokes) leaves the frame still.
    tallest: Cell<u16>,
}

impl PathPicker {
    /// Open a host-backed picker rooted at a directory (empty means home).
    #[must_use]
    pub fn new(root: String, theme: &Theme) -> Self {
        let home = std::env::var("HOME").ok();
        let status = home_to_tilde(&root, home.as_deref());
        let runtime = Ratcn::new().focus(|m: &Model| &m.focus, Msg::Focus);
        Self {
            root,
            status,
            theme: *theme,
            breakpoints: ChromeBreakpoints::default(),
            model: Model {
                focus: FocusState::intent([DIALOG, QUERY]),
                query: InputState::default(),
                rows: searching(),
                current: None,
                scroll: 0,
            },
            ui: Surface::new(runtime),
            outer: Cell::new(Rect::new(0, 0, 80, 24)),
            tallest: Cell::new(0),
        }
    }

    /// The modal rect: a box just tall enough for every row, up to half the
    /// viewport (min 30 wide), with a fixed query row; a row-starved
    /// viewport takes the full height. The box never shrinks below the
    /// tallest one painted.
    fn modal_area(&self, outer: Rect) -> Rect {
        let bp = self.breakpoints;
        let roomy = centered_panel(outer, 5, 30, CHROME_ROWS + 3, bp);
        if bp.is_row_starved(outer.height) {
            return roomy;
        }
        let rows = u16::try_from(self.model.rows.len().max(1)).unwrap_or(u16::MAX);
        let fit = rows
            .saturating_add(CHROME_ROWS)
            .max(self.tallest.get())
            .min(roomy.height);
        self.tallest.set(fit);
        Rect::new(roomy.x, roomy.y, roomy.width, fit)
    }

    /// Rows the list shows in the box laid out over the last viewport.
    fn list_height(&self) -> usize {
        let modal = self.modal_area(self.outer.get());
        usize::from(modal.height.saturating_sub(CHROME_ROWS))
    }

    /// Declare the dialog filling `area` (the modal rect).
    fn declare(&self, area: Rect) -> impl FnMut(&mut DeclareCtx<'_, Model, Msg>) + use<> {
        let theme = self.theme;
        let inner = area.width.saturating_sub(4);
        let footer = footer_line(
            &self.status,
            self.selected_action().is_some(),
            inner,
            &theme,
        );
        move |ctx| {
            let footer = footer.clone();
            let list_rows = area.height.saturating_sub(CHROME_ROWS);
            let dialog = Dialog::new()
                .title("Insert path")
                .outer_width(area.width)
                .outer_height(area.height)
                .on_dismiss(|| Msg::Dismiss)
                .content(1 + list_rows, move |ctx| {
                    let strip = ctx.area();
                    // A viewport too short for the field: ratcn's input writes its
                    // prefix and placeholder without clipping, so declare nothing.
                    if strip.height == 0 || strip.width == 0 || strip.y >= area.bottom() {
                        return;
                    }
                    let field = Input::new()
                        .value(|m: &Model| &m.query, Msg::Query)
                        .prefix(">")
                        .placeholder("search the host");
                    ctx.component(QUERY, field, Rect::new(strip.x, strip.y, strip.width, 1));
                    let below = Rect::new(
                        strip.x,
                        strip.y.saturating_add(1),
                        strip.width,
                        strip.height.saturating_sub(1),
                    );
                    ctx.component(ROWS, rows_list(ctx.state(), theme, below.width), below);
                })
                .footer(1, move |ctx| ctx.paint_widget(footer, ctx.area()));
            ctx.component(DIALOG, dialog, area);
        }
    }

    /// Route one ratcn event and fold its message into a command.
    fn route(&mut self, event: Event) -> OverlayCommand {
        let area = self.modal_area(self.outer.get());
        let declare = self.declare(area);
        let theme = kit::theme(&self.theme);
        match self.ui.route(event, &self.model, &theme, declare) {
            EventResult::Emit(Msg::Focus(focus)) => self.model.focus = focus,
            EventResult::Emit(Msg::Query(query)) => self.set_query(query),
            EventResult::Emit(Msg::Pick(index)) => return self.commit(Some(index)),
            EventResult::Emit(Msg::Scroll(offset)) => self.model.scroll = offset,
            EventResult::Emit(Msg::Dismiss) => return OverlayCommand::Dismiss,
            EventResult::Consumed | EventResult::Ignored => {}
        }
        OverlayCommand::Stay
    }

    /// Store an edited query; a changed one drops the rows the host found
    /// for the old one, so a stale row can never be committed.
    fn set_query(&mut self, query: InputState) {
        let changed = query.value() != self.model.query.value();
        self.model.query = query;
        if changed {
            self.set_rows(searching());
        }
    }

    /// Replace the rows, keeping the highlighted row's action highlighted
    /// when it is still there, else highlighting the first choosable row.
    fn set_rows(&mut self, rows: Vec<Row>) {
        let kept = self.selected_action().cloned();
        let same = kept.and_then(|kept| rows.iter().position(|r| r.action.as_ref() == Some(&kept)));
        self.model.current = same.or_else(|| rows.iter().position(|r| r.action.is_some()));
        self.model.rows = rows;
        self.model.scroll = 0;
        self.reveal();
    }

    fn selected_action(&self) -> Option<&ResolvedAction> {
        self.model.rows.get(self.model.current?)?.action.as_ref()
    }

    /// The commit for row `index`, or nothing for a note or no row.
    fn commit(&self, index: Option<usize>) -> OverlayCommand {
        index
            .and_then(|i| self.model.rows.get(i)?.action.clone())
            .map_or(OverlayCommand::Stay, OverlayCommand::Commit)
    }

    /// Move the highlight `steps` choosable rows down (negative: up),
    /// stopping at either end, and scroll it into view.
    fn step(&mut self, steps: isize) {
        let choosable: Vec<usize> = (0..self.model.rows.len())
            .filter(|&i| self.model.rows[i].action.is_some())
            .collect();
        let Some(last) = choosable.len().checked_sub(1) else {
            return;
        };
        let at = self
            .model
            .current
            .and_then(|c| choosable.iter().position(|&i| i == c))
            .unwrap_or(0);
        let to = at.saturating_add_signed(steps).min(last);
        self.model.current = Some(choosable[to]);
        self.reveal();
    }

    fn reveal(&mut self) {
        let height = self.list_height();
        let cursor = self.model.current.unwrap_or(0);
        self.model.scroll =
            scroll_into_view(self.model.scroll, cursor, self.model.rows.len(), height);
    }

    /// The highlight move a key asks for, if it is one.
    fn movement(&self, key: &KeyEvent) -> Option<isize> {
        let page = isize::try_from(self.list_height().max(1)).unwrap_or(1);
        let ctrl = key.mods.contains(ModSet::CTRL);
        Some(match key.key {
            PhysicalKey::ArrowDown | PhysicalKey::NumpadDown => 1,
            PhysicalKey::ArrowUp | PhysicalKey::NumpadUp => -1,
            PhysicalKey::N if ctrl => 1,
            PhysicalKey::P if ctrl => -1,
            PhysicalKey::PageDown | PhysicalKey::NumpadPageDown => page,
            PhysicalKey::PageUp | PhysicalKey::NumpadPageUp => -page,
            // Home/End jump the highlight, as in every other picker.
            PhysicalKey::Home | PhysicalKey::NumpadHome => isize::MIN,
            PhysicalKey::End | PhysicalKey::NumpadEnd => isize::MAX,
            _ => return None,
        })
    }
}

/// The single row shown while the host answers.
fn searching() -> Vec<Row> {
    vec![Row::note("Searching host…")]
}

/// The rows as a ratcn list `width` cells wide: notes disabled, the
/// highlighted row on the selection surface.
fn rows_list(model: &Model, theme: Theme, width: u16) -> List<usize, Model, Msg> {
    let items = (0..model.rows.len()).map(|i| {
        let row = &model.rows[i];
        ListItem::new(i, row.label.clone()).disabled(row.action.is_none())
    });
    List::new(items)
        .selection(|m: &Model| m.current, Msg::Pick)
        .scroll(|m: &Model| m.scroll, Msg::Scroll)
        .style(|t| {
            // The rows sit on the dialog's surface whatever the pointer does.
            let mut style = ListStyle::from_theme(t);
            style.background = t.surface;
            style.focused_background = t.surface;
            style.hovered_background = t.surface;
            style.disabled_background = t.surface;
            style
        })
        .paint_item(move |m: &Model, item: ListItemState<'_, usize>| {
            m.rows
                .get(item.index)
                .map(|row| row_line(row, item.selected, usize::from(width), &theme))
                .unwrap_or_default()
        })
}

/// One row laid out to exactly `width` cells: label left, dimmed secondary
/// right. Under pressure the secondary yields first; the label is the row's
/// identity.
fn row_line(row: &Row, selected: bool, width: usize, theme: &Theme) -> Line<'static> {
    if row.action.is_none() {
        return Line::styled(
            clip_text(&row.label, width),
            Style::default().fg(theme.section_header),
        );
    }
    let secondary_full = row.secondary.clone().unwrap_or_default();
    let secondary = clip_text(
        &secondary_full,
        width.saturating_sub(display_width(&row.label) + GAP),
    );
    let sec_w = display_width(&secondary);
    let label = clip_text(
        &row.label,
        width.saturating_sub(sec_w + if sec_w > 0 { GAP } else { 0 }),
    );
    let padding = " ".repeat(width.saturating_sub(display_width(&label) + sec_w));
    if selected {
        // Own both colors so the selection reads on light terminals too.
        let style = theme.on_selection(Style::default().fg(theme.selection_fg));
        return Line::from(vec![
            Span::styled(label, style.add_modifier(Modifier::BOLD)),
            Span::styled(format!("{padding}{secondary}"), style),
        ]);
    }
    Line::from(vec![
        Span::styled(label, Style::default().fg(theme.text)),
        Span::raw(padding),
        Span::styled(secondary, Style::default().fg(theme.dim)),
    ])
}

/// Keep the root's tail and advertise selection only when there is a result.
fn footer_line(root: &str, can_select: bool, width: u16, theme: &Theme) -> Line<'static> {
    let width = usize::from(width);
    let keys = if can_select {
        "⏎ select · Esc"
    } else {
        "Esc close"
    };
    let keys = clip_text(keys, width);
    let keys_width = display_width(&keys);
    let status = clip_left(root, width.saturating_sub(keys_width + 1));
    let dim = Style::default().fg(theme.dim);
    let gap = width.saturating_sub(display_width(&status) + keys_width);
    Line::from(vec![
        Span::styled(status, dim),
        Span::raw(" ".repeat(gap)),
        Span::styled(keys, dim),
    ])
}

impl RenderOverlay for PathPicker {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.outer.set(area);
        let modal = self.modal_area(area);
        let theme = kit::theme(&self.theme);
        self.ui
            .paint(buf, modal, &self.model, &theme, self.declare(modal));
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        Some(self.modal_area(area))
    }

    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.breakpoints = bp;
        self.ui.invalidate();
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = *theme;
    }

    fn on_viewport_resize(&mut self, _pane_cols: u16, _pane_rows: u16) {
        self.ui.invalidate();
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        if key.action != KeyAction::Press {
            return OverlayCommand::Stay;
        }
        match key.key {
            PhysicalKey::Escape => return OverlayCommand::Dismiss,
            PhysicalKey::Enter | PhysicalKey::NumpadEnter => {
                return self.commit(self.model.current);
            }
            _ => {}
        }
        if let Some(steps) = self.movement(key) {
            self.step(steps);
            return OverlayCommand::Stay;
        }
        let before = self.model.query.value().to_owned();
        let event = field_key(&mut self.model.query, key);
        if self.model.query.value() != before {
            self.set_rows(searching());
        }
        event.map_or(OverlayCommand::Stay, |event| self.route(event))
    }

    fn handle_paste(&mut self, text: &str) {
        let query = insert_literal(&self.model.query, text);
        self.set_query(query);
    }

    /// The wheel moves the highlight; the view follows it.
    fn handle_mouse(&mut self, mouse: &MouseEvent) -> OverlayCommand {
        match (mouse.action, mouse.button) {
            (MouseAction::Press, MouseButton::Four) => self.step(-WHEEL_ROWS),
            (MouseAction::Press, MouseButton::Five) => self.step(WHEEL_ROWS),
            _ => return kit::mouse(mouse).map_or(OverlayCommand::Stay, |e| self.route(e)),
        }
        OverlayCommand::Stay
    }

    fn path_search(&self) -> Option<(&str, &str)> {
        Some((&self.root, self.model.query.value()))
    }

    fn update_paths(&mut self, result: &PathQueryResult) -> bool {
        self.set_rows(rows(result, self.model.query.value().is_empty()));
        true
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests;
