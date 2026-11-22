//! The ratcn side of a [`SelectList`]: the model its surface reads, the
//! messages it emits, the declaration, and input routing.

use std::cell::Cell;
use std::rc::Rc;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratcn::runtime::{DeclareCtx, Event, EventResult, FocusState, KeyCode, Ratcn};
use ratcn::{Input, InputState, List, ListItem, ListItemState};

use super::paint::{header_line, input_style, item_line, list_style};
use super::{QUERY, ROWS, SelectList};
use crate::render::kit::{self, Surface};
use crate::render::overlay::OverlayCommand;

/// What a list's ratcn surface reads: the query field's text, the cursor
/// (an index into the unfiltered items, the rows' ratcn value), and the
/// list's scroll offset.
pub(super) struct Model {
    /// Parked on the query field, so its caret shows while the list paints
    /// its own cursor; list keys are routed with it briefly on the rows.
    pub(super) focus: FocusState,
    pub(super) query: InputState,
    pub(super) cursor: Option<usize>,
    /// The first visible filtered row. Interior-mutable because the viewport
    /// height is only known at paint time and `render` takes `&self`.
    pub(super) scroll: Cell<usize>,
}

impl Model {
    pub(super) fn new() -> Self {
        Self {
            focus: FocusState::intent([QUERY]),
            query: InputState::default(),
            cursor: None,
            scroll: Cell::new(0),
        }
    }
}

impl Clone for Model {
    fn clone(&self) -> Self {
        Self {
            focus: self.focus.clone(),
            query: self.query.clone(),
            cursor: self.cursor,
            scroll: Cell::new(self.scroll.get()),
        }
    }
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Model")
            .field("query", &self.query.value())
            .field("cursor", &self.cursor)
            .field("scroll", &self.scroll.get())
            .finish_non_exhaustive()
    }
}

/// What the query field and the list ask for.
#[derive(Debug, Clone)]
pub(super) enum Msg {
    /// A focus move (a click on the list): ignored, the field keeps focus.
    Focus,
    Query(InputState),
    Cursor(usize, usize),
    Pick(usize),
    Scroll(usize),
}

pub(super) fn surface() -> Surface<Model, Msg> {
    Surface::new(Ratcn::new().focus(|m: &Model| &m.focus, |_| Msg::Focus))
}

impl SelectList {
    /// Declare the query field and the rows into `modal` (the box rect).
    /// The rows are this frame's filtered items, keyed by item index; the
    /// painter draws them with phux's row layout and match highlighting.
    pub(super) fn declare(
        &self,
        modal: Rect,
    ) -> impl FnMut(&mut DeclareCtx<'_, Model, Msg>) + use<> {
        let indices = self.filtered_indices();
        let items = Rc::clone(&self.items);
        let hits: Rc<[Option<Vec<usize>>]> = indices
            .iter()
            .map(|&idx| self.label_hits(&items[idx]))
            .collect();
        let rows: Vec<ListItem<usize>> = indices
            .iter()
            .map(|&idx| {
                ListItem::new(idx, items[idx].label.clone()).disabled(items[idx].is_header())
            })
            .collect();
        let theme = self.theme;
        let (query_area, rows_area) = (Self::query_area(modal), Self::rows_area(modal));
        let width = rows_area.width;
        let placeholder = self.placeholder;
        let empty = format!("No matches for \u{201c}{}\u{201d}", self.query().trim());
        move |ctx| {
            let prompt = Span::styled(
                "›",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            );
            let input = Input::new()
                .value(|m: &Model| &m.query, Msg::Query)
                .prefix(prompt)
                .placeholder(placeholder)
                .style(input_style);
            ctx.component(QUERY, input, query_area);
            if rows.is_empty() {
                let none = Line::styled(empty.clone(), Style::default().fg(theme.dim));
                ctx.paint_widget(none, rows_area);
                return;
            }
            let (items, hits) = (Rc::clone(&items), Rc::clone(&hits));
            let list = List::new(rows.clone())
                .item_focus(|m: &Model| m.cursor, Msg::Cursor)
                .selection(|_: &Model| None, Msg::Pick)
                .scroll(|m: &Model| m.scroll.get(), Msg::Scroll)
                .style(list_style)
                .paint_item(move |m: &Model, row: ListItemState<'_, usize>| {
                    let item = &items[*row.value];
                    if item.is_header() {
                        return header_line(&theme, item);
                    }
                    let selected = m.cursor == Some(*row.value);
                    item_line(&theme, item, hits[row.index].as_deref(), selected, width)
                });
            ctx.component(ROWS, list, rows_area);
        }
    }

    /// Paint the ratcn surface into `modal` and retain it for routing.
    pub(super) fn paint_surface(&self, buf: &mut Buffer, modal: Rect) {
        let theme = kit::theme(&self.theme);
        self.ui
            .paint(buf, modal, &self.model, &theme, self.declare(modal));
        self.declared.set(true);
    }

    /// Route one ratcn event to the `target` component (keys follow focus;
    /// the pointer hit-tests), re-declaring first when the rows or query
    /// changed since the last paint, and fold its message into a command.
    pub(super) fn route(&mut self, event: Event, target: &'static str) -> OverlayCommand {
        let outer = self.outer.get();
        let modal = self.modal_area(outer);
        if !self.declared.get() {
            self.paint_surface(&mut Buffer::empty(outer), modal);
        }
        self.model.focus = FocusState::intent([target]);
        let declare = self.declare(modal);
        let theme = kit::theme(&self.theme);
        let result = self.ui.route(event, &self.model, &theme, declare);
        self.model.focus = FocusState::intent([QUERY]);
        match result {
            EventResult::Emit(msg) => self.apply(msg),
            EventResult::Consumed | EventResult::Ignored => OverlayCommand::Stay,
        }
    }

    /// Apply one message from the field or the list.
    pub(super) fn apply(&mut self, msg: Msg) -> OverlayCommand {
        match msg {
            Msg::Focus => {}
            Msg::Query(query) => {
                let changed = query.value() != self.query();
                self.model.query = query;
                if changed {
                    self.select_best_match();
                }
            }
            Msg::Cursor(item, offset) => {
                self.model.cursor = Some(item);
                self.model.scroll.set(offset);
            }
            Msg::Scroll(offset) => self.model.scroll.set(offset),
            Msg::Pick(item) => {
                self.model.cursor = Some(item);
                if let Some(item) = self.cursor_item() {
                    if let Some(path) = &self.recents {
                        crate::attach::palette_recent::note(path, &item.action);
                    }
                    return OverlayCommand::Commit(item.action.clone());
                }
            }
        }
        OverlayCommand::Stay
    }

    /// Whether `event` moves or commits the list rather than editing the
    /// query: the vertical keys, Enter, the readline `C-n`/`C-p`, and bare
    /// `j`/`k` while vi navigation is on and nothing has been typed.
    pub(super) fn is_list_key(&self, event: &Event) -> bool {
        let Event::Key(key) = event else {
            return false;
        };
        let m = key.modifiers;
        match key.code {
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Enter => true,
            KeyCode::Char('n' | 'p') => m.ctrl && !m.alt,
            KeyCode::Char('j' | 'k') => !m.any() && self.vi_navigation && self.query().is_empty(),
            _ => false,
        }
    }

    /// Route a list key, unless no row can take the cursor.
    pub(super) fn route_rows(&mut self, event: Event) -> OverlayCommand {
        if !self.has_selectable() {
            return OverlayCommand::Stay;
        }
        self.route(event, ROWS)
    }
}
