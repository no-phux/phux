//! Reusable selectable-list overlay on ratcn's [`Input`](ratcn::Input) and
//! [`List`](ratcn::List).
//!
//! A themed [`Modal`] frame around a ratcn query field over a ratcn list of
//! filtered rows, whose rows commit a
//! [`ResolvedAction`](phux_config::keybind::ResolvedAction) through
//! `run_action()`. It backs the command palette and every picker.
//!
//! Keys: Up/`C-p`, Down/`C-n` (and `j`/`k` while the query is empty),
//! `PageUp`/`PageDown`, `Home`/`End` and Enter go to the list; everything
//! else edits the query (ratcn's readline field: Left/Right, `C-w`, `C-u`,
//! Backspace, typing); Esc dismisses. The wheel moves the selection
//! [`WHEEL_SCROLL_ROWS`] rows; a click commits the row under the pointer.
//!
//! The list's rows are value-keyed by item index, so filtering and live
//! refreshes keep the cursor on the same item. Section headers are disabled
//! rows ratcn's navigation skips. Only the rows that fit are painted,
//! windowed around the selection, with a scrollbar on the border when the
//! list overflows. A non-empty query is a scored fuzzy match
//! ([`fuzzy_score`]) ranked best-first; [`SelectKind::Header`] rows group an
//! empty query and vanish once the user types.

use std::cell::Cell;
use std::rc::Rc;

use phux_protocol::input::key::{KeyAction, KeyEvent, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratcn::linear_nav::cursor_visible_offset;
use ratcn::runtime::{self as rt, Event, KeyCode};

use super::widgets::{Modal, footer_status_budget, paint_scrollbar};
use super::{OverlayCommand, RenderOverlay};
use crate::render::kit::{self, Surface};
use crate::render::{ChromeBreakpoints, Theme, clip_left};

/// Blank columns between a row's label and its secondary.
const GAP: usize = 1;

/// Modal rows that are not list rows: two borders, the query line and the
/// hairline under it.
const CHROME_ROWS: u16 = 4;

/// What an empty query field says when a list gives it nothing better.
const DEFAULT_PLACEHOLDER: &str = "Type to filter";

/// Rows the selection moves per wheel detent (matches copy-mode).
pub const WHEEL_SCROLL_ROWS: usize = 3;

/// The ratcn ids of the query field and the row list.
const QUERY: &str = "query";
const ROWS: &str = "rows";

mod fuzzy;
mod item;
mod model;
mod paint;
mod rows;

pub(super) use fuzzy::fuzzy_rank;
pub use fuzzy::fuzzy_score;
pub use item::{SelectItem, SelectKind};
pub(super) use paint::list_style;

use model::{Model, Msg, surface};

/// A themed, filterable, selectable list rendered as an overlay.
#[derive(Debug)]
pub struct SelectList {
    /// Modal title (e.g. `"command palette"`).
    title: String,
    /// All items, unfiltered (filtering is recomputed per keystroke); shared
    /// with the row painter each frame declares.
    items: Rc<[SelectItem]>,
    /// Theme snapshot (copied, so the overlay stays `'static`).
    theme: Theme,
    /// `Some` for a live projection that accepts in-place refreshes tagged
    /// with this key ([`RenderOverlay::refresh_items`]); `None` ignores them.
    live_key: Option<&'static str>,
    /// Path search uses every printable letter, including j/k, as query text.
    vi_navigation: bool,
    /// Tab opens management for the selected session, never for host/status rows.
    session_management: bool,
    /// Replaces the `N of M` footer status when set (clipped from the left to fit).
    footer_status: Option<String>,
    /// What the empty query field says.
    placeholder: &'static str,
    /// Where a committed row is remembered as recent (the command palette).
    recents: Option<std::path::PathBuf>,
    /// The tallest box painted so far: a list sized to its rows never
    /// shrinks while it is up, so a live list whose refresh brings fewer
    /// rows (a path search between keystrokes) keeps its frame still.
    tallest: Cell<u16>,
    /// `[chrome]` thresholds, stamped by `OverlayState::push`.
    breakpoints: ChromeBreakpoints,
    model: Model,
    ui: Surface<Model, Msg>,
    /// The viewport of the last paint, which input is laid out against.
    outer: Cell<Rect>,
    /// Whether the retained surface was declared from the current rows and
    /// query; input re-declares first when they changed since.
    declared: Cell<bool>,
}

impl Clone for SelectList {
    /// The same list, query, and cursor; ratcn's retained surface is per
    /// instance and is rebuilt on the clone's first frame or event.
    fn clone(&self) -> Self {
        Self {
            title: self.title.clone(),
            items: Rc::clone(&self.items),
            theme: self.theme,
            live_key: self.live_key,
            vi_navigation: self.vi_navigation,
            session_management: self.session_management,
            footer_status: self.footer_status.clone(),
            placeholder: self.placeholder,
            recents: self.recents.clone(),
            tallest: Cell::new(self.tallest.get()),
            breakpoints: self.breakpoints,
            model: self.model.clone(),
            ui: surface(),
            outer: Cell::new(self.outer.get()),
            declared: Cell::new(false),
        }
    }
}

impl SelectList {
    /// Current filter, for server-backed lists which refresh as it changes.
    pub fn query(&self) -> &str {
        self.model.query.value()
    }

    /// A list titled `title` over `items`, selection on the first selectable
    /// row, empty query.
    #[must_use]
    pub fn new(title: impl Into<String>, items: Vec<SelectItem>, theme: &Theme) -> Self {
        let mut list = Self {
            title: title.into(),
            items: items.into(),
            theme: *theme,
            live_key: None,
            vi_navigation: true,
            session_management: false,
            footer_status: None,
            placeholder: DEFAULT_PLACEHOLDER,
            recents: None,
            tallest: Cell::new(0),
            breakpoints: ChromeBreakpoints::default(),
            model: Model::new(),
            ui: surface(),
            outer: Cell::new(Rect::new(0, 0, 80, 24)),
            declared: Cell::new(false),
        };
        // Grouped pickers open on a header; start on the first selectable row.
        list.select_best_match();
        list
    }

    /// Opt this list into live row refreshes tagged `key`.
    #[must_use]
    pub const fn with_live_key(mut self, key: &'static str) -> Self {
        self.live_key = Some(key);
        self
    }

    /// Enable Tab → management on session rows, preserving their exact identity args.
    #[must_use]
    pub const fn with_session_management(mut self) -> Self {
        self.session_management = true;
        self
    }

    /// Show `status` at the footer's left instead of the `N of M` count.
    #[must_use]
    pub fn with_footer_status(mut self, status: impl Into<String>) -> Self {
        self.footer_status = Some(status.into());
        self
    }

    /// Say `placeholder` in the empty query field (what can be searched).
    #[must_use]
    pub const fn with_placeholder(mut self, placeholder: &'static str) -> Self {
        self.placeholder = placeholder;
        self
    }

    /// Remember each committed row at `path`, so the palette learns what
    /// this user runs.
    #[must_use]
    pub fn with_recents(mut self, path: std::path::PathBuf) -> Self {
        self.recents = Some(path);
        self
    }

    /// Open on the row committing `action`, when it is listed.
    #[must_use]
    pub fn with_selected(mut self, action: Option<&phux_config::keybind::ResolvedAction>) -> Self {
        let indices = self.filtered_indices();
        if let Some(row) = action.and_then(|action| self.row_of_action(&indices, action)) {
            self.model.cursor = Some(indices[row]);
        }
        self
    }

    /// Reserve bare `j`/`k` for filter text, even before typing starts.
    #[must_use]
    pub const fn without_vi_navigation(mut self) -> Self {
        self.vi_navigation = false;
        self
    }

    /// The selected session, if Tab has a management action for it.
    fn manageable_session(&self) -> Option<&SelectItem> {
        self.cursor_item().filter(|item| {
            self.session_management
                && matches!(
                    item.action.action.as_str(),
                    "switch-session" | "switch-host"
                )
                && item.action.args.contains_key("name")
        })
    }

    /// Put optional hints first so the footer can drop them in tight spaces.
    fn footer_keys(&self) -> &'static str {
        if self.manageable_session().is_some() {
            "↑↓ move · Tab manage · ⏎ select · Esc"
        } else if self.model.cursor.is_some() {
            "↑↓ move · ⏎ select · Esc"
        } else {
            "Esc close"
        }
    }
}

fn session_management_command(item: &SelectItem) -> OverlayCommand {
    let mut action = item.action.clone();
    action.args.insert(
        "switch-action".to_owned(),
        toml::Value::String(action.action.clone()),
    );
    "session-actions".clone_into(&mut action.action);
    OverlayCommand::RunAbove(action)
}

impl RenderOverlay for SelectList {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.outer.set(area);
        let modal = self.modal_area(area);
        let indices = self.filtered_indices();
        let cursor_row = self.cursor_row(&indices);

        // Window the rows around the selection; the height is known only
        // here. ratcn settles the same offset from the same request, so the
        // scrollbar and the rows agree.
        let height = usize::from(Self::rows_area(modal).height);
        let offset =
            cursor_visible_offset(indices.len(), height, self.model.scroll.get(), cursor_row);
        self.model.scroll.set(offset);

        let keys = self.footer_keys();
        let status = self.footer_status.as_deref().map_or_else(
            || cursor_row.map_or_else(String::new, |row| self.footer(&indices, row)),
            |status| clip_left(status, footer_status_budget(modal.width, "⏎ select · Esc")),
        );
        Modal::new(&self.theme, self.title.clone(), Vec::new())
            .footer(status, keys)
            .render_into(modal, buf);
        self.paint_surface(buf, modal);
        paint::rule_under_query(buf, modal, &self.theme);
        if let Some(row) = cursor_row.and_then(|row| row.checked_sub(offset)) {
            paint::selection_bar(buf, modal, Self::rows_area(modal), row, &self.theme);
        }
        paint_scrollbar(
            buf,
            Self::scrollbar_track(modal),
            &self.theme,
            indices.len(),
            offset,
        );
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        Some(self.modal_area(area))
    }

    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.breakpoints = bp;
        self.declared.set(false);
    }

    fn on_viewport_resize(&mut self, _pane_cols: u16, _pane_rows: u16) {
        self.declared.set(false);
    }

    fn refresh_items(&mut self, key: &str, items: &[SelectItem]) -> bool {
        if self.live_key != Some(key) {
            return false;
        }
        self.replace_items(items.to_vec());
        true
    }

    /// The wheel moves the selection (the viewport follows it); a click
    /// commits the row under the pointer; a click in the field places its
    /// caret.
    fn handle_mouse(&mut self, mouse: &MouseEvent) -> OverlayCommand {
        let step = match (mouse.action, mouse.button) {
            (MouseAction::Press, MouseButton::Four) => Some(KeyCode::Up),
            (MouseAction::Press, MouseButton::Five) => Some(KeyCode::Down),
            _ => None,
        };
        if let Some(code) = step {
            for _ in 0..WHEEL_SCROLL_ROWS {
                self.route_rows(Event::Key(rt::KeyEvent::new(code)));
            }
            return OverlayCommand::Stay;
        }
        kit::mouse(mouse).map_or(OverlayCommand::Stay, |event| self.route(event, QUERY))
    }

    fn handle_paste(&mut self, text: &str) {
        self.route(Event::Paste(text.to_owned()), QUERY);
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        if key.action != KeyAction::Press {
            return OverlayCommand::Stay;
        }
        self.snap_cursor();
        match key.key {
            PhysicalKey::Escape => return OverlayCommand::Dismiss,
            PhysicalKey::Tab => {
                return self
                    .manageable_session()
                    .map_or(OverlayCommand::Stay, session_management_command);
            }
            _ => {}
        }
        let Some(event) = kit::key(key) else {
            // Text of several characters at once (an IME commit) types as
            // one insertion.
            return match key.text.as_deref() {
                Some(text) if !key.composing && !text.chars().any(char::is_control) => {
                    self.route(Event::Paste(text.to_owned()), QUERY)
                }
                _ => OverlayCommand::Stay,
            };
        };
        if self.is_list_key(&event) {
            // Any Enter commits, as it always has; ratcn's list answers only
            // an unmodified one.
            self.route_rows(kit::plain_enter(event))
        } else {
            self.route(event, QUERY)
        }
    }
}

#[cfg(test)]
mod tests;
