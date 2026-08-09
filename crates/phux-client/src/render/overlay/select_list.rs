//! Reusable selectable-list overlay (phux-ahv.8 / phux-4li.19).
//!
//! A themed [`Modal`] wrapping a one-line query input and a filtered,
//! scrollable list of items. Each item carries a display label, an
//! optional right-aligned secondary label, and the
//! [`ResolvedAction`] it commits on Enter. This is the shared primitive
//! behind both the command palette and the `<leader> w` window picker:
//! both populate a [`SelectList`] from different sources and let the
//! single `run_action()` dispatch path execute the committed action.
//!
//! ## Input model
//!
//! - Up / `C-p` and Down / `C-n` move the selection (also `j` / `k` when
//!   the query is empty, so a fresh palette is vi-navigable; once the
//!   user starts typing, `j`/`k` are treated as filter text).
//! - `PageUp` / `PageDown` move by a screenful, `Home` / `End` jump to the
//!   first / last selectable row, and the mouse wheel moves the selection
//!   [`WHEEL_SCROLL_ROWS`] rows at a time.
//! - Printable text appends to the query and re-filters.
//! - Backspace edits the query.
//! - Enter commits the selected item's [`ResolvedAction`]
//!   ([`OverlayCommand::Commit`]); on an empty filtered list it is a
//!   no-op.
//! - Esc dismisses ([`OverlayCommand::Dismiss`]).
//!
//! ## Scrolling
//!
//! The rows are a *viewport* over the filtered list, not the whole list:
//! only the rows that fit inside the modal are painted, windowed so the
//! selection is always on screen ([`scroll_into_view`]). A list that
//! overflows its box paints a scrollbar in the right border column
//! ([`paint_scrollbar`]) — without one, navigating past the last visible
//! row walked the selection off the bottom edge with no way to tell where
//! you were, which is the bug this viewport exists to fix (phux-ep9s).
//!
//! ## Filtering and ranking
//!
//! Filtering is a *scored* subsequence (fuzzy) match of the lowercased
//! query against each item's `filter_text` (label + secondary). An empty
//! query matches everything (and preserves source order). A non-empty
//! query keeps only matching rows and sorts them best-first: typing `sp`
//! floats `split-pane` above `toggle-sidebar`. See [`fuzzy_score`].
//!
//! ## Headers and grouping
//!
//! A row may be a non-selectable [`SelectKind::Header`] — a dim section
//! label (category in the palette, session in the grouped window picker).
//! Headers never match a query, are skipped by navigation and Enter, and
//! are hidden entirely once the user starts typing (a filtered list is a
//! flat best-first ranking, not a grouped one). Selectable rows may be
//! [`indented`](SelectItem::indented) to nest under the header above them.

use std::cell::Cell;

use phux_config::keybind::ResolvedAction;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::widgets::{Modal, centered_panel, paint_scrollbar, scroll_into_view};
use super::{OverlayCommand, RenderOverlay};
use crate::render::Theme;
use crate::render::clip_text;

/// Blank columns held between a row's label and its secondary. One is
/// enough to read them as separate facts; more just costs the secondary
/// room it does not have on a narrow list.
const GAP: usize = 1;

/// Rows of the modal box that are *not* list rows: the two borders, the
/// query line, the blank beneath it, and the footer's blank + text. The list
/// viewport is whatever height is left over.
const CHROME_ROWS: u16 = 6;

/// Rows the selection moves per mouse-wheel detent, matching copy-mode's
/// `WHEEL_SCROLL_LINES` so the wheel feels the same everywhere in the client.
pub const WHEEL_SCROLL_ROWS: usize = 3;

/// Whether a [`SelectItem`] is a selectable row or a non-selectable
/// section header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectKind {
    /// A normal selectable row that commits its [`SelectItem::action`].
    Item,
    /// A dim, non-selectable section label (a palette category, or a
    /// session group in the window picker). Skipped by navigation/Enter
    /// and hidden once the user types a query.
    Header,
}

/// One row in a [`SelectList`] — either a selectable item or a header.
#[derive(Debug, Clone)]
pub struct SelectItem {
    /// Primary display label (left column), e.g. an action name or a
    /// window's `index:name`.
    pub label: String,
    /// Optional right-aligned secondary label, e.g. a bound chord or a
    /// pane count. Dimmed when present.
    pub secondary: Option<String>,
    /// The action committed when this item is chosen. Flows straight
    /// into the dispatcher's `run_action()` — the same path a keybind
    /// takes. Ignored for [`SelectKind::Header`] rows.
    pub action: ResolvedAction,
    /// Whether this row is selectable or a section header.
    pub kind: SelectKind,
    /// Indent the label one level (two spaces) so selectable rows nest
    /// visually under the [`SelectKind::Header`] above them.
    pub indented: bool,
    /// phux-foz.7: paint this row's label in the theme's `attention` slot
    /// (the same amber the sidebar tab marker and status-bar asked hint
    /// use), so a row that needs the user reads hot at a glance. The
    /// selected row keeps plain reverse-video regardless.
    pub attention: bool,
}

impl SelectItem {
    /// An item displaying `label` that commits `action`, with no
    /// secondary label.
    #[must_use]
    pub fn new(label: impl Into<String>, action: ResolvedAction) -> Self {
        Self {
            label: label.into(),
            secondary: None,
            action,
            kind: SelectKind::Item,
            indented: false,
            attention: false,
        }
    }

    /// A non-selectable, dim section header labelled `label`. Carries no
    /// runnable action (a no-op [`ResolvedAction`] placeholder); the list
    /// never commits it.
    #[must_use]
    pub fn header(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            secondary: None,
            action: ResolvedAction {
                action: String::new(),
                args: std::collections::BTreeMap::new(),
            },
            kind: SelectKind::Header,
            indented: false,
            attention: false,
        }
    }

    /// Attach a right-aligned, dimmed secondary label.
    #[must_use]
    pub fn secondary(mut self, secondary: impl Into<String>) -> Self {
        self.secondary = Some(secondary.into());
        self
    }

    /// Mark this selectable row as nested under the header above it
    /// (renders with a two-space indent).
    #[must_use]
    pub const fn indented(mut self) -> Self {
        self.indented = true;
        self
    }

    /// Mark this row as needing the user's attention: its label paints in
    /// the theme's `attention` slot (phux-foz.7, the agent-fleet rows with
    /// a pending ADR-0035 question or a declared high-attention state).
    #[must_use]
    pub const fn attention(mut self) -> Self {
        self.attention = true;
        self
    }

    /// `true` for a non-selectable [`SelectKind::Header`] row.
    #[must_use]
    pub const fn is_header(&self) -> bool {
        matches!(self.kind, SelectKind::Header)
    }

    /// The text the query is fuzzy-matched against: label plus secondary
    /// (so a user can filter the palette by a binding chord too).
    fn filter_text(&self) -> String {
        self.secondary
            .as_ref()
            .map_or_else(|| self.label.clone(), |sec| format!("{} {sec}", self.label))
    }
}

/// A themed, filterable, selectable list rendered as an overlay.
///
/// Build with [`SelectList::new`], then push the boxed value onto the
/// [`OverlayState`](super::OverlayState) stack. Generic only over the
/// supplied [`SelectItem`]s — the palette and window picker differ only
/// in how they build that vector.
#[derive(Debug, Clone)]
pub struct SelectList {
    /// Modal title (e.g. `"command palette"`).
    title: String,
    /// All items, unfiltered. Filtering is recomputed from `query` on
    /// every keystroke rather than cached, since the lists are short.
    items: Vec<SelectItem>,
    /// Current query text.
    query: String,
    /// Selection index into the *filtered* list. Clamped on every filter
    /// change so it never points past the visible rows.
    selected: usize,
    /// Color slots snapshotted from the active [`Theme`] at construction
    /// (captured, not borrowed, so the overlay stays `'static`).
    theme: Theme,
    /// phux-foz.7: when `Some`, this list is a *live* projection of shared
    /// client state and accepts in-place row refreshes tagged with the same
    /// key via [`RenderOverlay::refresh_items`] (the agent-fleet dashboard
    /// re-rendering as agent events land while it is open). `None` (the
    /// default) makes the list a static snapshot — palette and pickers —
    /// that ignores every refresh.
    live_key: Option<&'static str>,
    /// First visible row of the filtered list (an index into the filtered
    /// indices) — the scroll offset.
    ///
    /// Interior-mutable because the window can only be resolved at paint
    /// time, when the viewport height is finally known, and
    /// [`RenderOverlay::render`] takes `&self`. This is the same bargain
    /// ratatui's own `ListState::offset` makes; the offset is pure view
    /// state, derived from `selected` on every paint, so nothing observable
    /// depends on when it is written.
    scroll: Cell<usize>,
    /// Rows the list viewport held at the last paint, recorded so
    /// `PageUp`/`PageDown` can move by a real screenful. Zero until the
    /// first render (page keys then fall back to a single row).
    page: Cell<usize>,
}

impl SelectList {
    /// A list titled `title` over `items`, styled with `theme`. The
    /// selection starts on the first item; the query starts empty (all
    /// items visible).
    #[must_use]
    pub fn new(title: impl Into<String>, items: Vec<SelectItem>, theme: &Theme) -> Self {
        let mut list = Self {
            title: title.into(),
            items,
            query: String::new(),
            selected: 0,
            theme: *theme,
            live_key: None,
            scroll: Cell::new(0),
            page: Cell::new(0),
        };
        // The first row may be a header (grouped pickers always open on
        // one); start the cursor on the first selectable row instead.
        let indices = list.filtered_indices();
        list.snap_to_selectable(&indices);
        list
    }

    /// Opt this list into live row refreshes tagged `key` (phux-foz.7).
    ///
    /// See [`RenderOverlay::refresh_items`]: the driver rebuilds the rows
    /// from fresh client state when a relevant server frame lands and hands
    /// them to the overlay stack; only a list constructed with the matching
    /// key replaces its rows (query and selection position are preserved).
    #[must_use]
    pub const fn with_live_key(mut self, key: &'static str) -> Self {
        self.live_key = Some(key);
        self
    }

    /// Replace the full item set in place, keeping the current query and
    /// clamping/snapping the selection so it stays on a selectable row.
    ///
    /// The selection is positional: it stays at the same visible row index
    /// where possible, which keeps the cursor stable when a refresh only
    /// changes row *content* (an agent flipping working -> blocked) and
    /// degrades gracefully when rows appear or disappear.
    pub fn replace_items(&mut self, items: Vec<SelectItem>) {
        self.items = items;
        let indices = self.filtered_indices();
        self.snap_to_selectable(&indices);
    }

    /// Indices of items to display for the current query.
    ///
    /// With an empty query every row is shown in source order, headers
    /// included, so the grouped layout reads as authored. With a non-empty
    /// query, headers are dropped (a filtered view is a flat ranking, not a
    /// grouped one) and the surviving selectable rows are sorted best-first
    /// by [`fuzzy_score`]; ties keep source order (stable sort) so the
    /// ordering is deterministic.
    fn filtered_indices(&self) -> Vec<usize> {
        let q = self.query.to_lowercase();
        if q.is_empty() {
            return (0..self.items.len()).collect();
        }
        let mut scored: Vec<(i32, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| !item.is_header())
            .filter_map(|(i, item)| {
                fuzzy_score(&q, &item.filter_text().to_lowercase()).map(|score| (score, i))
            })
            .collect();
        // Higher score first; ties broken by original index (stable).
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, i)| i).collect()
    }

    /// Whether the visible row at `row` (an index into `indices`) is a
    /// selectable item (not a header).
    fn row_selectable(&self, indices: &[usize], row: usize) -> bool {
        indices
            .get(row)
            .is_some_and(|&idx| !self.items[idx].is_header())
    }

    /// Advance `selected` to the nearest selectable row at or after the
    /// current position, then clamp. Keeps the cursor off header rows when
    /// the list opens or a filter change lands it on one.
    fn snap_to_selectable(&mut self, indices: &[usize]) {
        let visible = indices.len();
        self.clamp_selection(visible);
        if visible == 0 {
            return;
        }
        // Search forward for a selectable row, then backward, so a header
        // at the very bottom still resolves to a real item.
        if self.row_selectable(indices, self.selected) {
            return;
        }
        for row in self.selected..visible {
            if self.row_selectable(indices, row) {
                self.selected = row;
                return;
            }
        }
        for row in (0..self.selected).rev() {
            if self.row_selectable(indices, row) {
                self.selected = row;
                return;
            }
        }
    }

    /// Clamp `selected` so it always points at a visible row (or 0 when
    /// nothing matches).
    const fn clamp_selection(&mut self, visible: usize) {
        if visible == 0 {
            self.selected = 0;
        } else if self.selected >= visible {
            self.selected = visible - 1;
        }
    }

    /// Move the selection down to the next selectable row within `indices`,
    /// saturating at the bottom (no wrap — matches the prompt overlay's
    /// restraint). Header rows are skipped so the cursor only ever lands on
    /// a runnable item.
    fn select_down(&mut self, indices: &[usize]) {
        let visible = indices.len();
        let mut row = self.selected;
        while row + 1 < visible {
            row += 1;
            if self.row_selectable(indices, row) {
                self.selected = row;
                return;
            }
        }
    }

    /// Move the selection up to the previous selectable row, saturating at
    /// the top and skipping header rows.
    fn select_up(&mut self, indices: &[usize]) {
        let mut row = self.selected;
        while row > 0 {
            row -= 1;
            if self.row_selectable(indices, row) {
                self.selected = row;
                return;
            }
        }
    }

    /// Move the selection down a screenful, saturating at the last
    /// selectable row. Repeated single steps rather than an index jump, so
    /// header-skipping and the bottom clamp stay in one place.
    fn select_page_down(&mut self, indices: &[usize]) {
        for _ in 0..self.page_rows() {
            self.select_down(indices);
        }
    }

    /// Move the selection up a screenful, saturating at the first selectable
    /// row.
    fn select_page_up(&mut self, indices: &[usize]) {
        for _ in 0..self.page_rows() {
            self.select_up(indices);
        }
    }

    /// Rows in a page move: the last painted viewport height, or a single row
    /// before the first paint (no viewport measured yet — better a small step
    /// than a wild one).
    fn page_rows(&self) -> usize {
        self.page.get().max(1)
    }

    /// Jump to the first selectable row (Home).
    fn select_first(&mut self, indices: &[usize]) {
        self.selected = 0;
        self.snap_to_selectable(indices);
    }

    /// Jump to the last selectable row (End). `snap_to_selectable` searches
    /// backward once the forward search runs out, so a trailing header row
    /// resolves to the item above it.
    fn select_last(&mut self, indices: &[usize]) {
        self.selected = indices.len().saturating_sub(1);
        self.snap_to_selectable(indices);
    }

    /// The modal rect: 60% of the viewport, min 30x10, clamped to the
    /// outer rect (like the help overlay, but a touch narrower).
    fn modal_area(outer: Rect) -> Rect {
        centered_panel(outer, 6, 30, 10)
    }

    /// Rows available to the list inside `modal_area`, once the borders, the
    /// query line + its blank, and the footer + its blank are taken out.
    const fn list_height(modal_area: Rect) -> usize {
        modal_area.height.saturating_sub(CHROME_ROWS) as usize
    }

    /// The one-column scrollbar track: the modal's right border column,
    /// spanning exactly the list rows (which start below the border, the
    /// query line, and its blank).
    fn scrollbar_track(modal_area: Rect) -> Rect {
        Rect::new(
            modal_area.x + modal_area.width.saturating_sub(1),
            modal_area.y.saturating_add(3),
            1,
            u16::try_from(Self::list_height(modal_area)).unwrap_or(u16::MAX),
        )
    }

    /// Build the body lines: a query line, a separator, then the *visible*
    /// filtered rows (selected row reverse-video). A dimmed notice replaces
    /// the rows when nothing matches.
    ///
    /// `window` is the slice of filtered indices that fits the viewport and
    /// `offset` is where that slice starts in the filtered list, so a row's
    /// absolute position — the thing `selected` indexes — is `offset + row`.
    fn body_lines(&self, window: &[usize], offset: usize, inner_width: u16) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        // Query line: a `> ` prompt, the text, and a reverse-video caret.
        lines.push(Line::from(vec![
            Span::styled("> ".to_owned(), Style::default().fg(self.theme.accent)),
            Span::raw(self.query.clone()),
            Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
        ]));
        lines.push(Line::from(""));

        if window.is_empty() {
            lines.push(Line::from(Span::styled(
                "(no matches)".to_owned(),
                Style::default().fg(self.theme.dim),
            )));
            return lines;
        }

        for (row, &idx) in window.iter().enumerate() {
            let item = &self.items[idx];
            if item.is_header() {
                lines.push(self.header_line(item));
                continue;
            }
            let selected = offset + row == self.selected;
            lines.push(self.item_line(item, selected, inner_width));
        }
        lines
    }

    /// A dim, non-selectable section-header row, styled with the theme's
    /// `section_header` slot (the same slot the help modal uses for its
    /// headings).
    fn header_line(&self, item: &SelectItem) -> Line<'static> {
        Line::from(Span::styled(
            item.label.clone(),
            Style::default()
                .fg(self.theme.section_header)
                .add_modifier(Modifier::DIM),
        ))
    }

    /// One list row: label on the left, optional dimmed secondary
    /// right-aligned within `inner_width`. The selected row is rendered
    /// reverse-video across its visible width.
    ///
    /// The row is laid out to *exactly* `inner_width` cells. It used to
    /// be laid out to at least `label + 1 + secondary` and left "truncated
    /// implicitly by the terminal if the row is too narrow" — which on a
    /// narrow viewport means the text runs straight through the modal's
    /// right border and onto the pane behind it, and a reverse-video
    /// selection bar that stops short of the edge it is supposed to fill.
    ///
    /// Under pressure the **secondary yields first, entirely if need be**.
    /// The label is the row's identity — the thing you are choosing — and
    /// the secondary is context: a branch, a cwd, a bound chord. Losing
    /// the context leaves a usable list; losing the identity does not.
    fn item_line(&self, item: &SelectItem, selected: bool, inner_width: u16) -> Line<'static> {
        let width = inner_width as usize;
        if width == 0 {
            return Line::from(String::new());
        }
        let indent = if item.indented { "  " } else { "" };
        let label_full = format!("{indent}{}", item.label);
        let secondary_full = item.secondary.clone().unwrap_or_default();

        // The secondary gets whatever the full label does not need, less
        // one mandatory blank column: `label` and `secondary` are two
        // different facts and must never run together into one word.
        let secondary = clip_text(
            &secondary_full,
            width.saturating_sub(label_full.chars().count() + GAP),
        );
        let sec_w = secondary.chars().count();
        // The label then takes the rest, reserving the gap only if a
        // secondary actually survived.
        let label = clip_text(
            &label_full,
            width.saturating_sub(sec_w + if sec_w > 0 { GAP } else { 0 }),
        );
        let padding = " ".repeat(width.saturating_sub(label.chars().count() + sec_w));

        if selected {
            // Reverse-video the whole row so the selection reads clearly
            // regardless of theme. Secondary stays in the same run.
            let text = format!("{label}{padding}{secondary}");
            Line::from(Span::styled(
                text,
                Style::default().add_modifier(Modifier::REVERSED),
            ))
        } else {
            // phux-foz.7: an attention row's label paints in the theme's
            // `attention` slot (bold) — the same semantic amber the sidebar
            // marker and status-bar asked hint use — so "needs you" rows
            // stand out inside the fleet dashboard without a new slot.
            let label_style = if item.attention {
                Style::default()
                    .fg(self.theme.attention)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.theme.action)
            };
            Line::from(vec![
                Span::styled(label, label_style),
                Span::raw(padding),
                Span::styled(secondary, Style::default().fg(self.theme.dim)),
            ])
        }
    }
}

impl RenderOverlay for SelectList {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let modal_area = Self::modal_area(area);
        let indices = self.filtered_indices();
        // Body width is the modal interior minus the 1-cell border on
        // each side.
        let inner_width = modal_area.width.saturating_sub(2);

        // Window the rows to what actually fits, keeping the selection in
        // view. Both are view state: recorded here (the only place the
        // viewport height is known) for the next page-key press to use.
        let height = Self::list_height(modal_area);
        self.page.set(height);
        let offset = scroll_into_view(self.scroll.get(), self.selected, indices.len(), height);
        self.scroll.set(offset);
        let window = indices
            .get(offset..(offset + height).min(indices.len()))
            .unwrap_or(&[]);

        let body = self.body_lines(window, offset, inner_width);
        Modal::new(&self.theme, self.title.clone(), body)
            .footer_hints(["Enter select", "Esc cancel", "type to filter"])
            .render_into(modal_area, buf);
        // Over the border the modal just drew, so an overflowing list shows
        // its extent and position instead of silently clipping.
        paint_scrollbar(
            buf,
            Self::scrollbar_track(modal_area),
            &self.theme,
            indices.len(),
            offset,
        );
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        Some(Self::modal_area(area))
    }

    fn refresh_items(&mut self, key: &str, items: &[SelectItem]) -> bool {
        // Only a live list whose key matches accepts the refresh; the
        // palette and the static pickers (no live key) ignore it.
        if self.live_key != Some(key) {
            return false;
        }
        self.replace_items(items.to_vec());
        true
    }

    /// The wheel moves the *selection*, not the view on its own — the
    /// viewport follows the selection ([`scroll_into_view`]), so scrolling
    /// the box away from the cursor would only be undone on the next paint.
    /// Moving the selection keeps the two in lockstep and leaves Enter
    /// meaning what the user just scrolled to.
    fn handle_mouse(&mut self, mouse: &MouseEvent) -> OverlayCommand {
        if mouse.action != MouseAction::Press {
            return OverlayCommand::Stay;
        }
        let indices = self.filtered_indices();
        match mouse.button {
            MouseButton::Four => {
                for _ in 0..WHEEL_SCROLL_ROWS {
                    self.select_up(&indices);
                }
            }
            MouseButton::Five => {
                for _ in 0..WHEEL_SCROLL_ROWS {
                    self.select_down(&indices);
                }
            }
            _ => {}
        }
        OverlayCommand::Stay
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        if key.action != KeyAction::Press {
            return OverlayCommand::Stay;
        }
        let indices = self.filtered_indices();
        self.snap_to_selectable(&indices);

        // Ctrl-modified navigation works regardless of query content.
        if key.mods.contains(ModSet::CTRL) {
            match key.key {
                PhysicalKey::N => {
                    self.select_down(&indices);
                    return OverlayCommand::Stay;
                }
                PhysicalKey::P => {
                    self.select_up(&indices);
                    return OverlayCommand::Stay;
                }
                _ => {}
            }
        }

        match key.key {
            PhysicalKey::Escape => OverlayCommand::Dismiss,
            // Enter commits only a selectable row; a header (or empty list)
            // is a no-op.
            PhysicalKey::Enter => indices
                .get(self.selected)
                .map_or(OverlayCommand::Stay, |&idx| {
                    let item = &self.items[idx];
                    if item.is_header() {
                        OverlayCommand::Stay
                    } else {
                        OverlayCommand::Commit(item.action.clone())
                    }
                }),
            PhysicalKey::ArrowDown => {
                self.select_down(&indices);
                OverlayCommand::Stay
            }
            PhysicalKey::ArrowUp => {
                self.select_up(&indices);
                OverlayCommand::Stay
            }
            PhysicalKey::PageDown => {
                self.select_page_down(&indices);
                OverlayCommand::Stay
            }
            PhysicalKey::PageUp => {
                self.select_page_up(&indices);
                OverlayCommand::Stay
            }
            PhysicalKey::Home => {
                self.select_first(&indices);
                OverlayCommand::Stay
            }
            PhysicalKey::End => {
                self.select_last(&indices);
                OverlayCommand::Stay
            }
            PhysicalKey::Backspace => {
                self.query.pop();
                let indices = self.filtered_indices();
                self.snap_to_selectable(&indices);
                OverlayCommand::Stay
            }
            // `j`/`k` navigate only while the query is empty (so a fresh
            // list is vi-navigable); once the user types, they're filter
            // text like any other letter.
            PhysicalKey::J if self.query.is_empty() => {
                self.select_down(&indices);
                OverlayCommand::Stay
            }
            PhysicalKey::K if self.query.is_empty() => {
                self.select_up(&indices);
                OverlayCommand::Stay
            }
            _ => {
                if let Some(t) = &key.text
                    && !t.chars().any(char::is_control)
                {
                    self.query.push_str(t);
                    let indices = self.filtered_indices();
                    self.snap_to_selectable(&indices);
                }
                OverlayCommand::Stay
            }
        }
    }
}

/// Scored subsequence (fuzzy) match.
///
/// Returns `Some(score)` when every char of `needle` appears in `haystack`
/// in order (not necessarily contiguously), or `None` when it does not.
/// An empty needle scores `0` (matches everything). Both arguments are
/// expected lowercased by the caller. Higher scores are better matches.
///
/// The score rewards the qualities that make one subsequence match read as
/// "more relevant" than another:
///
/// - **Contiguous runs.** Consecutive matched chars compound (each step in
///   a run is worth more than the last), so `sp` against `split-pane`
///   (a two-char run at the front) outranks `sp` against `toggle-sidebar`
///   (the `s` and `p` are far apart).
/// - **Word-boundary / prefix hits.** A matched char at position 0, or
///   immediately after a separator (`-`, `_`, space), earns a bonus — it
///   reads as the start of a meaningful token.
/// - **Earliness.** A small penalty grows with the gap skipped before each
///   match, so matches that cluster near the front of the haystack win.
///
/// The exact constants are tuned for short action/window labels, not a
/// general-purpose corpus; only the *ordering* they induce is contractual
/// (see the unit tests).
#[must_use]
pub fn fuzzy_score(needle: &str, haystack: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay: Vec<char> = haystack.chars().collect();
    let mut score: i32 = 0;
    let mut run: i32 = 0;
    let mut hay_idx: usize = 0;
    let mut last_match: Option<usize> = None;

    for nc in needle.chars() {
        // Advance through the haystack to the next occurrence of `nc`.
        let found = hay[hay_idx..].iter().position(|&hc| hc == nc)?;
        let pos = hay_idx + found;

        // Gap penalty: chars skipped since the previous match (or the start
        // of the haystack for the first char). Earlier, tighter matches win.
        let gap = last_match.map_or(pos, |prev| pos - prev - 1);
        score -= i32::try_from(gap).unwrap_or(i32::MAX).min(20);

        // Word-boundary / prefix bonus.
        let at_boundary = pos == 0
            || hay
                .get(pos - 1)
                .is_some_and(|c| matches!(c, '-' | '_' | ' ' | ':' | '/'));
        if at_boundary {
            score += 10;
        }

        // Contiguous-run bonus: matching right after the previous match
        // continues a run; each additional step in the run is worth more.
        if last_match.is_some_and(|prev| prev + 1 == pos) {
            run += 1;
            score += 5 + run * 5;
        } else {
            run = 0;
        }

        last_match = Some(pos);
        hay_idx = pos + 1;
    }
    Some(score)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn action(name: &str) -> ResolvedAction {
        ResolvedAction {
            action: name.to_owned(),
            args: BTreeMap::new(),
        }
    }

    fn press(key: PhysicalKey, text: Option<&str>) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: text.map(ToOwned::to_owned),
            unshifted_codepoint: None,
        }
    }

    fn ctrl(key: PhysicalKey) -> KeyEvent {
        let mut ev = press(key, None);
        ev.mods = ModSet::CTRL;
        ev
    }

    fn sample() -> SelectList {
        let items = vec![
            SelectItem::new("split-pane", action("split-pane")).secondary("C-a |"),
            SelectItem::new("new-window", action("new-window")).secondary("C-a c"),
            SelectItem::new("detach", action("detach")).secondary("C-a d"),
        ];
        SelectList::new("command palette", items, &Theme::default())
    }

    #[test]
    fn fuzzy_score_matches_subsequence() {
        assert!(fuzzy_score("", "anything").is_some());
        assert!(fuzzy_score("sp", "split-pane").is_some());
        assert!(fuzzy_score("spn", "split-pane").is_some()); // s-p-(a)n
        assert!(fuzzy_score("nw", "new-window").is_some());
        assert!(fuzzy_score("zzz", "new-window").is_none());
        // Order matters: "wen" can't be a subsequence — there is no 'e'
        // after the first 'w' in "new-window".
        assert!(fuzzy_score("wen", "new-window").is_none());
    }

    #[test]
    fn fuzzy_score_none_for_non_subsequence() {
        assert_eq!(fuzzy_score("zzz", "new-window"), None);
        assert_eq!(fuzzy_score("wen", "new-window"), None);
    }

    #[test]
    fn fuzzy_score_empty_needle_scores_zero() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn fuzzy_score_prefers_split_pane_for_sp() {
        // The worked example: "sp" should rank "split-pane" (a contiguous,
        // front-anchored run) above another row that also matches "sp" only
        // as a scattered subsequence. "previous-pane" has an `s` (end of
        // "previous") followed later by the `p` of "pane", so it matches
        // too — but with no contiguous run and a wide gap.
        let split = fuzzy_score("sp", "split-pane").expect("split-pane matches sp");
        let scattered = fuzzy_score("sp", "previous-pane").expect("previous-pane matches sp");
        assert!(
            split > scattered,
            "split-pane ({split}) should outrank previous-pane ({scattered}) for `sp`",
        );
    }

    #[test]
    fn fuzzy_score_rewards_word_boundary() {
        // "p" hitting the start of the "pane" token beats "p" buried
        // mid-word.
        let boundary = fuzzy_score("p", "split-pane").expect("matches");
        let mid = fuzzy_score("p", "copy-mode").expect("matches");
        assert!(
            boundary > mid,
            "boundary hit ({boundary}) should beat mid-word hit ({mid})",
        );
    }

    #[test]
    fn ranking_floats_best_match_to_top() {
        let items = vec![
            SelectItem::new("toggle-sidebar", action("toggle-sidebar")),
            SelectItem::new("split-pane", action("split-pane")),
            SelectItem::new("previous-pane", action("previous-pane")),
        ];
        let mut sl = SelectList::new("palette", items, &Theme::default());
        for ch in ['s', 'p'] {
            sl.handle_key(&press(PhysicalKey::A, Some(&ch.to_string())));
        }
        let idx = sl.filtered_indices();
        assert_eq!(
            sl.items[idx[0]].label,
            "split-pane",
            "`sp` must rank split-pane first, got {:?}",
            idx.iter().map(|&i| &sl.items[i].label).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn ranking_is_stable_for_equal_scores() {
        // Two labels for which the query matches at the identical position
        // and context score identically; a stable sort then keeps source
        // order, so the ranking is deterministic frame-to-frame. Both start
        // "x-…", so "x" hits index 0 (a front-anchored, boundary match) in
        // each.
        let items = vec![
            SelectItem::new("x-alpha", action("a")),
            SelectItem::new("x-bravo", action("b")),
        ];
        let a = fuzzy_score("x", "x-alpha");
        let b = fuzzy_score("x", "x-bravo");
        assert_eq!(a, b, "the two labels must score equally for the test");
        let mut sl = SelectList::new("palette", items, &Theme::default());
        sl.handle_key(&press(PhysicalKey::A, Some("x")));
        let idx = sl.filtered_indices();
        assert_eq!(idx, vec![0, 1], "equal scores keep source order");
    }

    #[test]
    fn empty_query_preserves_source_order_with_headers() {
        let items = vec![
            SelectItem::header("Pane"),
            SelectItem::new("split-pane", action("split-pane")).indented(),
            SelectItem::header("Window"),
            SelectItem::new("new-window", action("new-window")).indented(),
        ];
        let sl = SelectList::new("palette", items, &Theme::default());
        assert_eq!(sl.filtered_indices(), vec![0, 1, 2, 3]);
    }

    #[test]
    fn headers_drop_out_when_filtering() {
        let items = vec![
            SelectItem::header("Pane"),
            SelectItem::new("split-pane", action("split-pane")).indented(),
            SelectItem::header("Window"),
            SelectItem::new("new-window", action("new-window")).indented(),
        ];
        let mut sl = SelectList::new("palette", items, &Theme::default());
        sl.handle_key(&press(PhysicalKey::A, Some("s")));
        let idx = sl.filtered_indices();
        assert!(
            idx.iter().all(|&i| !sl.items[i].is_header()),
            "no headers survive a non-empty query",
        );
    }

    #[test]
    fn navigation_skips_header_rows() {
        let items = vec![
            SelectItem::header("Pane"),
            SelectItem::new("split-pane", action("split-pane")).indented(),
            SelectItem::header("Window"),
            SelectItem::new("new-window", action("new-window")).indented(),
        ];
        let mut sl = SelectList::new("palette", items, &Theme::default());
        // Opens on the first selectable row (skips the leading header).
        let cmd = sl.handle_key(&press(PhysicalKey::Enter, None));
        let OverlayCommand::Commit(a) = cmd else {
            panic!("expected Commit, got {cmd:?}");
        };
        assert_eq!(a.action, "split-pane");
        // Down jumps over the "Window" header straight to new-window.
        sl.handle_key(&press(PhysicalKey::ArrowDown, None));
        let cmd = sl.handle_key(&press(PhysicalKey::Enter, None));
        let OverlayCommand::Commit(a) = cmd else {
            panic!("expected Commit");
        };
        assert_eq!(a.action, "new-window");
    }

    #[test]
    fn enter_on_header_does_not_commit() {
        // A list with only a header has no selectable row; Enter is a
        // no-op rather than committing the header's placeholder action.
        let items = vec![SelectItem::header("Pane")];
        let mut sl = SelectList::new("palette", items, &Theme::default());
        assert_eq!(
            sl.handle_key(&press(PhysicalKey::Enter, None)),
            OverlayCommand::Stay,
        );
    }

    #[test]
    fn typing_narrows_the_list() {
        let mut sl = sample();
        assert_eq!(sl.filtered_indices().len(), 3);
        sl.handle_key(&press(PhysicalKey::N, Some("n")));
        sl.handle_key(&press(PhysicalKey::W, Some("w")));
        // "nw" subsequence matches only "new-window".
        let idx = sl.filtered_indices();
        assert_eq!(idx.len(), 1);
        assert_eq!(sl.items[idx[0]].label, "new-window");
    }

    #[test]
    fn filter_can_match_secondary_chord() {
        let mut sl = sample();
        // The detach chord is "C-a d"; querying "det" via the label is the
        // simple case, but the filter text includes the secondary too.
        for ch in ['d', 'e', 't'] {
            sl.handle_key(&press(PhysicalKey::A, Some(&ch.to_string())));
        }
        let idx = sl.filtered_indices();
        assert_eq!(idx.len(), 1);
        assert_eq!(sl.items[idx[0]].label, "detach");
    }

    #[test]
    fn enter_commits_selected_action() {
        let mut sl = sample();
        // Move down once → "new-window".
        sl.handle_key(&press(PhysicalKey::ArrowDown, None));
        let cmd = sl.handle_key(&press(PhysicalKey::Enter, None));
        let OverlayCommand::Commit(a) = cmd else {
            panic!("expected Commit, got {cmd:?}");
        };
        assert_eq!(a.action, "new-window");
    }

    #[test]
    fn enter_on_empty_filter_is_noop() {
        let mut sl = sample();
        for ch in ['z', 'z', 'z'] {
            sl.handle_key(&press(PhysicalKey::A, Some(&ch.to_string())));
        }
        assert_eq!(sl.filtered_indices().len(), 0);
        assert_eq!(
            sl.handle_key(&press(PhysicalKey::Enter, None)),
            OverlayCommand::Stay,
            "Enter with no matches should not commit"
        );
    }

    #[test]
    fn esc_dismisses() {
        let mut sl = sample();
        assert_eq!(
            sl.handle_key(&press(PhysicalKey::Escape, None)),
            OverlayCommand::Dismiss
        );
    }

    #[test]
    fn ctrl_n_p_navigate() {
        let mut sl = sample();
        sl.handle_key(&ctrl(PhysicalKey::N));
        sl.handle_key(&ctrl(PhysicalKey::N));
        // Two downs → third item "detach". Saturates (no wrap).
        sl.handle_key(&ctrl(PhysicalKey::N));
        let cmd = sl.handle_key(&press(PhysicalKey::Enter, None));
        let OverlayCommand::Commit(a) = cmd else {
            panic!("expected Commit");
        };
        assert_eq!(a.action, "detach");
        sl.handle_key(&ctrl(PhysicalKey::P));
        let cmd = sl.handle_key(&press(PhysicalKey::Enter, None));
        let OverlayCommand::Commit(a) = cmd else {
            panic!("expected Commit");
        };
        assert_eq!(a.action, "new-window");
    }

    #[test]
    fn jk_navigate_only_when_query_empty() {
        let mut sl = sample();
        // j with empty query moves down.
        sl.handle_key(&press(PhysicalKey::J, Some("j")));
        let cmd = sl.handle_key(&press(PhysicalKey::Enter, None));
        let OverlayCommand::Commit(a) = cmd else {
            panic!("expected Commit");
        };
        assert_eq!(a.action, "new-window");
    }

    #[test]
    fn j_becomes_filter_text_once_query_nonempty() {
        let mut sl = sample();
        // Type "d" (query now non-empty), then "j" should be filter text,
        // not navigation. "dj" matches nothing.
        sl.handle_key(&press(PhysicalKey::A, Some("d")));
        sl.handle_key(&press(PhysicalKey::J, Some("j")));
        assert_eq!(sl.query, "dj");
        assert_eq!(sl.filtered_indices().len(), 0);
    }

    // ---------- phux-ep9s: scroll viewport ----------

    /// A list of `n` rows labelled `item-0 ..= item-(n-1)`, long enough to
    /// overflow any modal a test viewport can produce.
    fn long_list(n: usize) -> SelectList {
        let items = (0..n)
            .map(|i| SelectItem::new(format!("item-{i}"), action(&format!("act-{i}"))))
            .collect();
        SelectList::new("command palette", items, &Theme::default())
    }

    /// Paint `sl` into a `w`x`h` viewport and flatten it to text.
    fn render_to_string(sl: &SelectList, w: u16, h: u16) -> String {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        sl.render(area, &mut buf);
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    /// The label of the row painted reverse-video — what the user sees as
    /// selected. `None` when no row is highlighted anywhere on screen, which
    /// is exactly the bug: the cursor walked off the bottom of the box.
    fn painted_selection(sl: &SelectList, w: u16, h: u16) -> Option<String> {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        sl.render(area, &mut buf);
        for y in 0..area.height {
            // The query line's caret is reverse-video too; a *row* is a run of
            // reversed cells carrying a label, so require some non-space text.
            let mut row = String::new();
            let mut reversed = false;
            for x in 0..area.width {
                let cell = &buf[(x, y)];
                if cell.style().add_modifier.contains(Modifier::REVERSED) {
                    reversed = true;
                    row.push_str(cell.symbol());
                }
            }
            let row = row.trim().to_owned();
            if reversed && row.starts_with("item-") {
                return Some(row.split_whitespace().next().unwrap_or_default().to_owned());
            }
        }
        None
    }

    #[test]
    fn selection_stays_on_screen_when_navigating_past_the_viewport() {
        // The reported bug (phux-ep9s): the palette painted every filtered row
        // into a Paragraph that clipped at the modal's bottom edge, so walking
        // the cursor down a long list marched it off-screen — no highlighted
        // row anywhere, no way to tell where you were.
        let mut sl = long_list(40);
        // A 40x16 viewport ⇒ a 10-row modal ⇒ 4 visible rows. Step well past it.
        for _ in 0..20 {
            sl.handle_key(&press(PhysicalKey::ArrowDown, None));
        }
        assert_eq!(sl.selected, 20, "20 downs move the cursor 20 rows");
        assert_eq!(
            painted_selection(&sl, 40, 16).as_deref(),
            Some("item-20"),
            "the selected row must be painted inside the modal, not clipped away",
        );
    }

    #[test]
    fn the_viewport_scrolls_back_up_with_the_selection() {
        let mut sl = long_list(40);
        for _ in 0..20 {
            sl.handle_key(&press(PhysicalKey::ArrowDown, None));
        }
        render_to_string(&sl, 40, 16);
        assert!(
            sl.scroll.get() > 0,
            "the view scrolled to follow the cursor"
        );
        for _ in 0..20 {
            sl.handle_key(&press(PhysicalKey::ArrowUp, None));
        }
        let text = render_to_string(&sl, 40, 16);
        assert_eq!(sl.scroll.get(), 0, "returning to the top rewinds the view");
        assert!(text.contains("item-0"), "first row visible again:\n{text}");
    }

    #[test]
    fn an_overflowing_list_paints_a_scrollbar() {
        // 4 rows visible out of 40 ⇒ the box must say so.
        let sl = long_list(40);
        let text = render_to_string(&sl, 40, 16);
        assert!(
            text.contains('█'),
            "an overflowing list must paint a scrollbar thumb:\n{text}"
        );
        // A list that fits paints a plain border — no bar, no lie about extent.
        let sl = long_list(3);
        let text = render_to_string(&sl, 40, 16);
        assert!(
            !text.contains('█'),
            "a list that fits must not paint a scrollbar:\n{text}"
        );
    }

    #[test]
    fn filtering_rewinds_the_viewport() {
        // Scroll deep, then type a query that narrows the list to rows above
        // the current offset. A stranded offset would paint a blank window.
        let mut sl = long_list(40);
        for _ in 0..30 {
            sl.handle_key(&press(PhysicalKey::ArrowDown, None));
        }
        render_to_string(&sl, 40, 16);
        assert!(sl.scroll.get() > 0);
        // "item-7" is the only exact hit for the 7 at the end; whatever the
        // filter keeps, it is a short list that must be visible from row 0.
        for ch in ['i', 't', 'e', 'm', '-', '7'] {
            sl.handle_key(&press(PhysicalKey::A, Some(&ch.to_string())));
        }
        let text = render_to_string(&sl, 40, 16);
        assert_eq!(sl.scroll.get(), 0, "a narrowed list rewinds to the top");
        assert!(text.contains("item-7"), "matches must be visible:\n{text}");
    }

    #[test]
    fn page_keys_move_by_a_screenful() {
        let mut sl = long_list(40);
        // Before the first paint the viewport height is unknown; a page key
        // then steps one row rather than guessing.
        sl.handle_key(&press(PhysicalKey::PageDown, None));
        assert_eq!(sl.selected, 1, "no measured viewport ⇒ a single-row step");
        // After a paint, a page is a real screenful. A 40x16 viewport is
        // compact on both axes, so the picker is full-bleed: 16 rows less
        // the 6 of shared chrome leaves 10 visible.
        render_to_string(&sl, 40, 16);
        sl.handle_key(&press(PhysicalKey::PageDown, None));
        assert_eq!(sl.selected, 11);
        sl.handle_key(&press(PhysicalKey::PageUp, None));
        assert_eq!(sl.selected, 1);
        // And both saturate rather than wrapping.
        for _ in 0..40 {
            sl.handle_key(&press(PhysicalKey::PageUp, None));
        }
        assert_eq!(sl.selected, 0);
        for _ in 0..40 {
            sl.handle_key(&press(PhysicalKey::PageDown, None));
        }
        assert_eq!(sl.selected, 39);
    }

    #[test]
    fn home_and_end_jump_to_the_ends() {
        let mut sl = long_list(40);
        sl.handle_key(&press(PhysicalKey::End, None));
        assert_eq!(sl.selected, 39);
        assert_eq!(
            painted_selection(&sl, 40, 16).as_deref(),
            Some("item-39"),
            "End must land the last row inside the viewport",
        );
        sl.handle_key(&press(PhysicalKey::Home, None));
        assert_eq!(sl.selected, 0);
    }

    #[test]
    fn end_skips_a_trailing_header() {
        // The window picker's grouped rows can end on a header (a session with
        // no windows yet). End must land on the last *selectable* row.
        let items = vec![
            SelectItem::new("only-item", action("only")),
            SelectItem::header("Empty session"),
        ];
        let mut sl = SelectList::new("picker", items, &Theme::default());
        sl.handle_key(&press(PhysicalKey::End, None));
        let cmd = sl.handle_key(&press(PhysicalKey::Enter, None));
        let OverlayCommand::Commit(a) = cmd else {
            panic!("End must select a committable row, got {cmd:?}");
        };
        assert_eq!(a.action, "only");
    }

    #[test]
    fn wheel_moves_the_selection() {
        fn wheel(button: MouseButton) -> MouseEvent {
            MouseEvent {
                action: MouseAction::Press,
                button,
                mods: ModSet::empty(),
                x: 0.0,
                y: 0.0,
            }
        }
        let mut sl = long_list(40);
        assert_eq!(
            sl.handle_mouse(&wheel(MouseButton::Five)),
            OverlayCommand::Stay
        );
        assert_eq!(
            sl.selected, WHEEL_SCROLL_ROWS,
            "wheel-down advances a detent"
        );
        sl.handle_mouse(&wheel(MouseButton::Four));
        assert_eq!(sl.selected, 0, "wheel-up rewinds it");
        // Saturates at the top instead of underflowing.
        sl.handle_mouse(&wheel(MouseButton::Four));
        assert_eq!(sl.selected, 0);
    }

    #[test]
    fn scrolled_list_render_is_stable() {
        // Pin the painted mid-scroll box: a windowed row set, the selected row
        // reverse-video inside it, and the scrollbar thumb sitting away from
        // both ends of the border. A 44x16 viewport is compact on both
        // axes, so the picker is full-bleed and shows 10 of the 24 rows —
        // the deeper scroll keeps the thumb off both ends.
        let mut sl = long_list(24);
        for _ in 0..15 {
            sl.handle_key(&press(PhysicalKey::ArrowDown, None));
        }
        let area = Rect::new(0, 0, 44, 16);
        let mut buf = Buffer::empty(area);
        sl.render(area, &mut buf);
        let mut out = String::new();
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            out.push_str(row.trim_end());
            out.push('\n');
        }
        insta::assert_snapshot!(out);
    }

    // ---------- phux-foz.7: live refresh + attention rows ----------

    #[test]
    fn refresh_items_requires_a_matching_live_key() {
        let fresh = vec![SelectItem::new("fresh-row", action("fresh"))];
        // A static list (no live key) ignores every refresh.
        let mut sl = sample();
        assert!(!sl.refresh_items("agent-fleet", &fresh));
        assert_eq!(sl.items.len(), 3, "static list keeps its rows");
        // A live list with a different key ignores it too.
        let mut sl = sample().with_live_key("other-live-list");
        assert!(!sl.refresh_items("agent-fleet", &fresh));
        // The matching key replaces the rows in place.
        let mut sl = sample().with_live_key("agent-fleet");
        assert!(sl.refresh_items("agent-fleet", &fresh));
        assert_eq!(sl.items.len(), 1);
        assert_eq!(sl.items[0].label, "fresh-row");
    }

    #[test]
    fn replace_items_preserves_query_and_reclamps_selection() {
        let mut sl = sample().with_live_key("agent-fleet");
        // Filter down to "detach" and select it.
        for ch in ['d', 'e', 't'] {
            sl.handle_key(&press(PhysicalKey::A, Some(&ch.to_string())));
        }
        assert_eq!(sl.filtered_indices().len(), 1);
        // A refresh lands: the row set changes but the query survives, so
        // the user's in-progress filter keeps applying to the new rows.
        sl.refresh_items(
            "agent-fleet",
            &[
                SelectItem::new("detach-me", action("a")),
                SelectItem::new("other", action("b")),
            ],
        );
        assert_eq!(sl.query, "det", "query survives the refresh");
        let idx = sl.filtered_indices();
        assert_eq!(idx.len(), 1);
        assert_eq!(sl.items[idx[0]].label, "detach-me");
        // Enter commits against the refreshed rows without a stale index.
        let cmd = sl.handle_key(&press(PhysicalKey::Enter, None));
        let OverlayCommand::Commit(a) = cmd else {
            panic!("expected Commit, got {cmd:?}");
        };
        assert_eq!(a.action, "a");
    }

    #[test]
    fn replace_items_snaps_selection_off_a_vanished_row() {
        let mut sl = sample().with_live_key("agent-fleet");
        // Select the last row (index 2), then shrink the list to one row.
        sl.handle_key(&ctrl(PhysicalKey::N));
        sl.handle_key(&ctrl(PhysicalKey::N));
        sl.refresh_items("agent-fleet", &[SelectItem::new("only", action("only"))]);
        let cmd = sl.handle_key(&press(PhysicalKey::Enter, None));
        let OverlayCommand::Commit(a) = cmd else {
            panic!("expected Commit after reclamp, got {cmd:?}");
        };
        assert_eq!(a.action, "only");
    }

    #[test]
    fn attention_row_label_paints_in_the_attention_slot() {
        let theme = Theme::default();
        let sl = SelectList::new(
            "agent fleet",
            vec![
                SelectItem::new("calm", action("a")),
                SelectItem::new("needs-you", action("b")).attention(),
            ],
            &theme,
        );
        // Unselected rows: attention label takes the theme's attention
        // color; a calm row keeps the action slot.
        let calm = sl.item_line(&sl.items[0], false, 40);
        assert_eq!(calm.spans[0].style.fg, Some(theme.action));
        let hot = sl.item_line(&sl.items[1], false, 40);
        assert_eq!(hot.spans[0].style.fg, Some(theme.attention));
        assert!(hot.spans[0].style.add_modifier.contains(Modifier::BOLD));
        // The selected row stays plain reverse-video regardless.
        let selected = sl.item_line(&sl.items[1], true, 40);
        assert!(
            selected.spans[0]
                .style
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert_eq!(selected.spans[0].style.fg, None);
    }

    /// phux-foz.7: pin the painted fleet-shaped layout (session header,
    /// glyphed rows, attention row present) so layout churn is caught —
    /// the same snapshot pattern as `render_byte_output_is_stable`.
    #[test]
    fn render_fleet_shaped_list_is_stable() {
        let items = vec![
            SelectItem::header("work (current)"),
            SelectItem::new("! 0:main.0 reviewer [claude]", action("focus-pane"))
                .secondary("blocked - main")
                .indented()
                .attention(),
            SelectItem::new("* 0:main.1 builder", action("focus-pane"))
                .secondary("working - main")
                .indented(),
            SelectItem::header("scratch"),
            SelectItem::new("switch to this session", action("switch-session"))
                .secondary("2 windows")
                .indented(),
        ];
        let sl =
            SelectList::new("agent fleet", items, &Theme::default()).with_live_key("agent-fleet");
        let area = Rect::new(0, 0, 52, 14);
        let mut buf = Buffer::empty(area);
        sl.render(area, &mut buf);
        let mut out = String::new();
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            out.push_str(row.trim_end());
            out.push('\n');
        }
        insta::assert_snapshot!(out);
    }

    #[test]
    fn render_byte_output_is_stable() {
        // Pin the painted layout (query line, separator, rows with the
        // first selected reverse-video) so accidental layout churn is
        // caught. Fixed small viewport for a compact snapshot.
        let sl = sample();
        let area = Rect::new(0, 0, 44, 12);
        let mut buf = Buffer::empty(area);
        sl.render(area, &mut buf);
        let mut out = String::new();
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            out.push_str(row.trim_end());
            out.push('\n');
        }
        insta::assert_snapshot!(out);
    }

    /// The other side of the breakpoint. Above [`COMPACT_COLS`] /
    /// [`COMPACT_ROWS`] the picker must still *float*: a centered box with
    /// live panes visible around it, not a screen. Pinned alongside the
    /// compact snapshots so a change to the responsive rule has to face
    /// both shapes at once.
    ///
    /// [`COMPACT_COLS`]: crate::render::overlay::widgets::COMPACT_COLS
    /// [`COMPACT_ROWS`]: crate::render::overlay::widgets::COMPACT_ROWS
    #[test]
    fn roomy_viewport_render_still_floats() {
        let sl = sample();
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        sl.render(area, &mut buf);
        let mut out = String::new();
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            out.push_str(row.trim_end());
            out.push('\n');
        }
        // The box occupies neither the full width nor the full height.
        let bounds = sl.bounds(area).expect("a floating picker is bounded");
        assert!(bounds.width < area.width, "{bounds:?}");
        assert!(bounds.height < area.height, "{bounds:?}");
        assert!(bounds.x > 0 && bounds.y > 0, "{bounds:?}");
        insta::assert_snapshot!(out);
    }

    /// A row must never be wider than the box that contains it.
    ///
    /// The old layout gave the gap a `.max(1)` floor and left the overflow
    /// to "the terminal", so a long label plus a long secondary painted
    /// straight through the modal's right border and onto the pane behind
    /// it — worst exactly where it hurts most, on the narrow viewport that
    /// made the row too long in the first place. Every row, at every
    /// width, now measures exactly the interior width.
    #[test]
    fn a_row_never_overruns_the_modal_interior() {
        let long = SelectItem::new(
            "a-very-long-window-name-that-will-not-fit-anywhere",
            action("x"),
        )
        .secondary("~/some/deeply/nested/working/directory  feature/branch");
        let sl = SelectList::new("t", vec![long], &Theme::default());

        for width in 1u16..=80 {
            let line = sl.item_line(&sl.items[0], false, width);
            let painted: String = line
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>();
            assert_eq!(
                painted.chars().count(),
                usize::from(width),
                "row at interior width {width}: {painted:?}"
            );
        }
    }

    /// Under pressure the secondary yields before the label does: the
    /// label is what you are choosing, the secondary is context about it.
    #[test]
    fn a_narrow_row_sacrifices_the_secondary_before_the_label() {
        let item = SelectItem::new("builder", action("x")).secondary("working - main");
        let sl = SelectList::new("t", vec![item], &Theme::default());
        let painted = |w| -> String {
            sl.item_line(&sl.items[0], false, w)
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        };
        // Roomy: both, right-aligned against the far edge.
        assert_eq!(painted(30), "builder         working - main");
        // Tight: the secondary shortens while the label stays whole.
        assert!(painted(16).starts_with("builder"), "{:?}", painted(16));
        // Tighter still: the secondary is gone entirely before the label
        // gives up a single column.
        assert_eq!(painted(7), "builder");
    }

    #[test]
    fn render_does_not_panic_and_shows_title() {
        let sl = sample();
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        sl.render(area, &mut buf);
        let mut text = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(text.contains("command palette"), "{text}");
        assert!(text.contains("split-pane"), "{text}");
    }
}
