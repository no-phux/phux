//! Copy-mode overlay: keyboard and mouse text selection over the live pane.
//!
//! Selection is a client-local projection (ADR-0030): the overlay tracks a
//! pane-local rectangle and on commit the dispatcher resolves it against the
//! pane's own engine and writes the host clipboard via OSC 52.
//!
//! Vi keys move, select (`v`, `V`, `C-v`) and copy (`y`); see the `vi` submodule.
//!
//! Search (`/` forward, `?` backward, `n`/`N` repeat) is typed here and run
//! by the dispatcher against the pane's loaded history; the overlay keeps the
//! hits in document rows so they stay put while the viewport scrolls.

use std::time::{Duration, Instant};

use phux_protocol::ResourceId;
use phux_protocol::input::key::{KeyEvent, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::line_edit::LineEdit;
use super::selection::SearchMatch;
use super::{
    CopyRequest, OverlayCommand, RenderOverlay, ScreenSelectionPoint, SelectionGrab, SelectionMode,
    SelectionRect,
};

mod vi;

pub use vi::{CopyMotionRequest, RowMotion, row_motion_target};

const WHEEL_SCROLL_LINES: isize = 3;

fn quantize_mouse_cell(value: f64, max: u16) -> u16 {
    if !value.is_finite() {
        return 0;
    }
    super::pointer_cell(value).min(max.saturating_sub(1))
}

/// A normalized (start <= end) two-corner cell range.
#[derive(Debug, Clone, Copy)]
struct CellRange {
    start_row: u16,
    start_col: u16,
    end_row: u16,
    end_col: u16,
}

impl CellRange {
    fn from_points(cursor_row: u16, cursor_col: u16, end_row: u16, end_col: u16) -> Self {
        if (cursor_row, cursor_col) <= (end_row, end_col) {
            Self {
                start_row: cursor_row,
                start_col: cursor_col,
                end_row,
                end_col,
            }
        } else {
            Self {
                start_row: end_row,
                start_col: end_col,
                end_row: cursor_row,
                end_col: cursor_col,
            }
        }
    }
}

/// A search the overlay asks the dispatcher to run: find `needle` after
/// (or, `backward`, before) the pane-local cursor, wrapping around.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopySearchRequest {
    /// The text to find (the engine's case policy applies).
    pub needle: String,
    /// Search toward older output (`?`, `N` after `/`).
    pub backward: bool,
    /// Pane-local cursor row the search starts from.
    pub cursor_row: u16,
    /// Pane-local cursor column the search starts from.
    pub cursor_col: u16,
    /// The pane's visible rows: a hit outside them scrolls into view.
    pub pane_rows: u16,
}

/// What a [`CopySearchRequest`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopySearchResult {
    /// Every loaded hit in document order, the one jumped to, and the
    /// document row now at the top of the (possibly scrolled) viewport.
    Found {
        /// All hits, oldest first.
        matches: Vec<SearchMatch>,
        /// Index of the hit the cursor moved to.
        current: usize,
        /// Document row of viewport row 0 after the jump.
        top: u32,
    },
    /// Nothing loaded matches.
    NotFound,
}

/// The last search and what it found.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchState {
    needle: String,
    backward: bool,
    matches: Vec<SearchMatch>,
    current: Option<usize>,
}

/// What the painter shows for copy-mode search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopySearchView<'a> {
    /// Every hit, oldest first, in document rows.
    pub matches: &'a [SearchMatch],
    /// The hit the selection covers, if any.
    pub current: Option<usize>,
}

/// The longest gap between presses that still counts as a repeat click.
const REPEAT_CLICK: Duration = Duration::from_millis(500);

/// The last mouse press that entered copy-mode, kept by
/// [`super::OverlayState`] across overlays so a second and third press on the
/// same cell count as double- and triple-clicks (Ghostty: word, line).
#[derive(Debug, Clone)]
pub(super) struct ClickRecord {
    pane: ResourceId,
    col: u16,
    row: u16,
    at: Instant,
    count: u8,
}

impl ClickRecord {
    /// Record a press on `pane`'s `(col, row)` at `now` and return its count:
    /// 1, 2, or 3, a fourth repeat starting over at 1 as Ghostty's does.
    pub(super) fn press(
        last: &mut Option<Self>,
        pane: &ResourceId,
        col: u16,
        row: u16,
        now: Instant,
    ) -> u8 {
        let count = match last.as_ref() {
            Some(prev)
                if prev.pane == *pane
                    && (prev.col, prev.row) == (col, row)
                    && now.saturating_duration_since(prev.at) <= REPEAT_CLICK =>
            {
                prev.count % 3 + 1
            }
            _ => 1,
        };
        *last = Some(Self {
            pane: pane.clone(),
            col,
            row,
            at: now,
            count,
        });
        count
    }
}

/// Copy-mode overlay state.
#[derive(Debug)]
pub struct CopyModeOverlay {
    /// Current cursor position (row, col) in pane-local coords.
    pub cursor_row: u16,
    /// Column position of cursor in pane-local coords.
    pub cursor_col: u16,
    /// Anchor point where selection started.
    pub anchor_row: u16,
    /// Column position where selection started.
    pub anchor_col: u16,
    /// Selection mode (char, line, rect).
    pub mode: SelectionMode,
    /// Pane columns, used to clamp cursor movement.
    pub pane_cols: u16,
    /// Number of rows in the pane.
    pub pane_rows: u16,
    /// Whether a left-button drag is actively extending the selection.
    selecting_with_mouse: bool,
    /// The first clicked cell in full-screen coordinates (from the
    /// dispatcher, which has the engine).
    mouse_anchor_screen: Option<ScreenSelectionPoint>,
    /// Where the mouse anchor currently appears in the viewport: signed, so
    /// a wheel-scrolled drag keeps the anchor off screen and only the painted
    /// highlight clamps.
    mouse_anchor_viewport_row: Option<i32>,
    /// The search being typed after `/` or `?` (`true` for `?`).
    search_input: Option<(LineEdit, bool)>,
    /// The last committed search, repeated by `n`/`N`.
    search: Option<SearchState>,
    /// What a release without a drag copies: the word or line under a
    /// double- or triple-click (Ghostty). `None` dismisses, as a single
    /// click does.
    click_grab: Option<SelectionGrab>,
    /// A vi selection (`v`, `V`, `C-v`) is open: every cursor move extends it.
    selecting: bool,
    /// The full-screen row on viewport row 0, as the dispatcher last
    /// reported it; `None` until it has.
    screen_top: Option<u32>,
}

impl CopyModeOverlay {
    /// A copy-mode overlay with its cursor (clamped) at the given position.
    #[must_use]
    pub fn new(cursor_row: u16, cursor_col: u16, pane_cols: u16, pane_rows: u16) -> Self {
        // Clamp cursor to valid range
        let cursor_row = cursor_row.min(pane_rows.saturating_sub(1));
        let cursor_col = cursor_col.min(pane_cols.saturating_sub(1));

        Self {
            cursor_row,
            cursor_col,
            anchor_row: cursor_row,
            anchor_col: cursor_col,
            mode: SelectionMode::Char,
            pane_cols,
            pane_rows,
            selecting_with_mouse: false,
            mouse_anchor_screen: None,
            mouse_anchor_viewport_row: None,
            search_input: None,
            search: None,
            click_grab: None,
            selecting: false,
            screen_top: None,
        }
    }

    /// Adopt a search result: jump the cursor to the current hit and select
    /// it, so Enter copies it and the painter inverts it.
    pub fn apply_search_result(&mut self, result: CopySearchResult) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        let CopySearchResult::Found {
            matches,
            current,
            top,
        } = result
        else {
            search.matches.clear();
            search.current = None;
            return;
        };
        let Some(hit) = matches.get(current).copied() else {
            return;
        };
        search.matches = matches;
        search.current = Some(current);
        let local = |row: u32| u16::try_from(row.saturating_sub(top)).unwrap_or(u16::MAX);
        let max_row = self.pane_rows.saturating_sub(1);
        let max_col = self.pane_cols.saturating_sub(1);
        self.cursor_row = local(hit.start_row).min(max_row);
        self.cursor_col = hit.start_col.min(max_col);
        self.anchor_row = local(hit.end_row).min(max_row);
        self.anchor_col = hit.end_col.min(max_col);
        self.mode = SelectionMode::Char;
        self.selecting = false;
        self.mouse_anchor_screen = None;
        self.mouse_anchor_viewport_row = None;
    }

    /// The search hits for the painter, or `None` before any search.
    #[must_use]
    pub fn search_view(&self) -> Option<CopySearchView<'_>> {
        self.search.as_ref().map(|search| CopySearchView {
            matches: &search.matches,
            current: search.current,
        })
    }

    /// The search part of the status strip: the line being typed, or the
    /// last search with its position (`/foo 2/7`, `/foo: no match`).
    #[must_use]
    pub fn search_status(&self) -> Option<String> {
        if let Some((edit, backward)) = &self.search_input {
            let prompt = if *backward { '?' } else { '/' };
            return Some(format!("{prompt}{}_", edit.as_str()));
        }
        let search = self.search.as_ref()?;
        let prompt = if search.backward { '?' } else { '/' };
        Some(search.current.map_or_else(
            || format!("{prompt}{}: no match", search.needle),
            |index| {
                format!(
                    "{prompt}{} {}/{}",
                    search.needle,
                    index + 1,
                    search.matches.len()
                )
            },
        ))
    }

    /// A search request for `needle` from the cursor, remembered for `n`/`N`.
    fn start_search(&mut self, needle: String, backward: bool) -> OverlayCommand {
        self.search = Some(SearchState {
            needle: needle.clone(),
            backward,
            matches: Vec::new(),
            current: None,
        });
        self.search_request(needle, backward)
    }

    const fn search_request(&self, needle: String, backward: bool) -> OverlayCommand {
        OverlayCommand::Search(CopySearchRequest {
            needle,
            backward,
            cursor_row: self.cursor_row,
            cursor_col: self.cursor_col,
            pane_rows: self.pane_rows,
        })
    }

    /// `n` (`reverse` false) or `N`: the last search again, in its own
    /// direction or the opposite one. Nothing searched yet is a no-op.
    fn repeat_search(&self, reverse: bool) -> OverlayCommand {
        self.search.as_ref().map_or(OverlayCommand::Stay, |search| {
            self.search_request(search.needle.clone(), search.backward != reverse)
        })
    }

    /// A key while the search line is open: edits, Enter runs (an empty
    /// line repeats the last search), Esc closes the line but not copy-mode.
    fn handle_search_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        let Some((edit, backward)) = self.search_input.as_mut() else {
            return OverlayCommand::Stay;
        };
        match key.key {
            PhysicalKey::Escape => {
                self.search_input = None;
                OverlayCommand::Stay
            }
            PhysicalKey::Enter | PhysicalKey::NumpadEnter => {
                let needle = edit.as_str().to_owned();
                let backward = *backward;
                self.search_input = None;
                if needle.is_empty() {
                    return self.repeat_search(false);
                }
                self.start_search(needle, backward)
            }
            _ => {
                edit.handle_key(key);
                OverlayCommand::Stay
            }
        }
    }

    /// Record the stable terminal cell under a mouse press.
    pub fn set_mouse_anchor_screen(&mut self, anchor: ScreenSelectionPoint) {
        self.mouse_anchor_screen = Some(anchor);
        self.mouse_anchor_viewport_row = Some(i32::from(self.anchor_row));
    }

    /// Make a release without a drag copy `grab` at the pressed cell (the
    /// dispatcher counts clicks: two grab the word, three the line).
    pub const fn set_click_grab(&mut self, grab: Option<SelectionGrab>) {
        self.click_grab = grab;
    }

    /// Advance the selection mode `Char -> Line -> Rect -> Char` (the
    /// in-overlay `Tab` key).
    pub const fn cycle_mode(&mut self) {
        self.mode = match self.mode {
            SelectionMode::Char => SelectionMode::Line,
            SelectionMode::Line => SelectionMode::Rect,
            SelectionMode::Rect => SelectionMode::Char,
        };
    }

    fn set_cursor_from_mouse(&mut self, mouse: &MouseEvent) {
        self.cursor_row = quantize_mouse_cell(mouse.y, self.pane_rows);
        self.cursor_col = quantize_mouse_cell(mouse.x, self.pane_cols);
    }

    /// Get the current normalized selection range.
    fn selection_range(&self) -> CellRange {
        CellRange::from_points(
            self.anchor_row,
            self.anchor_col,
            self.cursor_row,
            self.cursor_col,
        )
    }

    /// The visible part of a mouse selection (presentation only).
    fn visible_selection_range(&self) -> CellRange {
        let max_row = self.pane_rows.saturating_sub(1);
        // An anchor scrolled off screen selects through the viewport's edge.
        let (anchor_row, anchor_col) = match self.mouse_anchor_viewport_row {
            None => (self.anchor_row, self.anchor_col),
            Some(row) if row < 0 => (0, 0),
            Some(row) if row > i32::from(max_row) => (max_row, self.pane_cols.saturating_sub(1)),
            Some(row) => (u16::try_from(row).unwrap_or_default(), self.anchor_col),
        };
        CellRange::from_points(anchor_row, anchor_col, self.cursor_row, self.cursor_col)
    }

    fn apply_selection_mode(&self, range: CellRange) -> CellRange {
        if self.mode == SelectionMode::Line {
            CellRange {
                start_row: range.start_row,
                start_col: 0,
                end_row: range.end_row,
                end_col: self.pane_cols.saturating_sub(1),
            }
        } else {
            range
        }
    }

    /// The range adjusted for the mode: `Line` spans whole visible rows. Both
    /// the highlight and the copy request resolve through here (ADR-0045).
    fn effective_range(&self) -> CellRange {
        self.apply_selection_mode(self.selection_range())
    }

    /// The range used only to paint the viewport-local highlight.
    fn visible_effective_range(&self) -> CellRange {
        self.apply_selection_mode(self.visible_selection_range())
    }

    /// Keep the painted mouse anchor on its terminal cell as the viewport
    /// scrolls (scrolling up moves it down on screen).
    fn scroll_mouse_anchor(&mut self, delta: isize) {
        if !self.selecting_with_mouse {
            return;
        }
        let Some(row) = self.mouse_anchor_viewport_row.as_mut() else {
            return;
        };
        let delta = i32::try_from(delta).unwrap_or_else(|_| {
            if delta.is_negative() {
                i32::MIN
            } else {
                i32::MAX
            }
        });
        *row = row.saturating_sub(delta);
    }

    /// Move cursor by a delta, clamping to pane bounds, leaving the anchor.
    #[cfg(test)]
    fn move_cursor(&mut self, delta_row: i16, delta_col: i16) {
        let max_row = self.pane_rows.saturating_sub(1);
        let max_col = self.pane_cols.saturating_sub(1);

        self.cursor_row = self
            .cursor_row
            .saturating_add_signed(delta_row)
            .min(max_row);
        self.cursor_col = self
            .cursor_col
            .saturating_add_signed(delta_col)
            .min(max_col);
    }

    fn move_cursor_key(&mut self, delta_row: i16, delta_col: i16, extend_selection: bool) {
        let row = self.cursor_row.saturating_add_signed(delta_row);
        let col = self.cursor_col.saturating_add_signed(delta_col);
        self.place_cursor(row, col, extend_selection);
    }

    /// Put the cursor on `(row, col)`, clamped. The move extends the
    /// selection when asked or while a vi selection is open; otherwise the
    /// selection collapses onto the cursor.
    fn place_cursor(&mut self, row: u16, col: u16, extend_selection: bool) {
        let extend = extend_selection || self.selecting;
        if extend {
            self.pin_anchor();
        }
        self.cursor_row = row.min(self.pane_rows.saturating_sub(1));
        self.cursor_col = col.min(self.pane_cols.saturating_sub(1));
        if !extend {
            self.collapse();
        }
    }

    fn page_scroll_delta(&self) -> isize {
        let rows = u32::from(self.pane_rows.saturating_sub(1).max(1));
        isize::try_from(rows).unwrap_or(1)
    }

    /// The copy request for the current two-corner selection.
    fn copy_request(&self) -> CopyRequest {
        self.copy_request_with(SelectionGrab::Rect)
    }

    /// A copy request tagged with `grab`; engine-derived grabs resolve at the
    /// cursor, the rectangle keeps the highlight coherent.
    fn copy_request_with(&self, grab: SelectionGrab) -> CopyRequest {
        let range = self.effective_range();
        let mut anchor = self.mouse_anchor_screen;
        let mut cursor_col = self.cursor_col;
        // A pinned anchor and the cursor are the two corners; a line
        // selection widens each to its own end of the row.
        if grab == SelectionGrab::Rect
            && self.mode == SelectionMode::Line
            && let Some(point) = anchor.as_mut()
        {
            let anchor_first = self
                .mouse_anchor_viewport_row
                .is_none_or(|row| row <= i32::from(self.cursor_row));
            let max_col = self.pane_cols.saturating_sub(1);
            (point.col, cursor_col) = if anchor_first {
                (0, max_col)
            } else {
                (max_col, 0)
            };
        }
        CopyRequest {
            start_row: range.start_row,
            start_col: range.start_col,
            end_row: range.end_row,
            end_col: range.end_col,
            mouse_anchor_screen: anchor,
            rectangle: self.mode == SelectionMode::Rect,
            cursor_row: self.cursor_row,
            cursor_col,
            grab,
        }
    }
}

impl RenderOverlay for CopyModeOverlay {
    /// Paints nothing: copy-mode is a highlight over the live pane, which the
    /// driver repaints via [`Self::copy_selection`].
    fn render(&self, _area: Rect, _buf: &mut Buffer) {}

    /// Adopt the pane's new size and clamp both corners into it. Copy-mode
    /// survives a resize (dropping it would discard the selection), but stale
    /// dimensions would leave a corner off the grid (copying nothing) or the
    /// new area unreachable.
    fn on_viewport_resize(&mut self, pane_cols: u16, pane_rows: u16) {
        self.pane_cols = pane_cols;
        self.pane_rows = pane_rows;
        let max_row = self.pane_rows.saturating_sub(1);
        let max_col = self.pane_cols.saturating_sub(1);
        self.cursor_row = self.cursor_row.min(max_row);
        self.cursor_col = self.cursor_col.min(max_col);
        self.anchor_row = self.anchor_row.min(max_row);
        self.anchor_col = self.anchor_col.min(max_col);
    }

    fn copy_search_view(&self) -> Option<CopySearchView<'_>> {
        self.search_view()
    }

    fn copy_search_status(&self) -> Option<String> {
        self.search_status()
    }

    fn apply_copy_search(&mut self, result: CopySearchResult) {
        self.apply_search_result(result);
    }

    fn apply_copy_motion(&mut self, col: u16) {
        self.place_cursor(self.cursor_row, col, false);
    }

    fn set_copy_viewport_top(&mut self, screen_top: u32) {
        self.viewport_top_changed(screen_top);
    }

    /// A paste lands in the open search line; otherwise copy-mode ignores it.
    fn handle_paste(&mut self, text: &str) {
        if let Some((edit, _)) = self.search_input.as_mut() {
            edit.insert(text);
        }
    }

    fn copy_selection(&self) -> Option<SelectionRect> {
        // Same mode-adjusted range the copy request uses (ADR-0045).
        let range = self.visible_effective_range();
        Some(SelectionRect::from_range(
            range.start_row,
            range.start_col,
            range.end_row,
            range.end_col,
            self.mode,
        ))
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        use phux_protocol::input::key::{KeyAction, ModSet};

        if key.action != KeyAction::Press {
            return OverlayCommand::Stay;
        }

        if self.search_input.is_some() {
            return self.handle_search_key(key);
        }
        match key.text.as_deref() {
            Some("/") => {
                self.search_input = Some((LineEdit::default(), false));
                return OverlayCommand::Stay;
            }
            Some("?") => {
                self.search_input = Some((LineEdit::default(), true));
                return OverlayCommand::Stay;
            }
            _ => {}
        }

        if let Some(command) = self.handle_vi_key(key) {
            return command;
        }
        let shift = key.mods.contains(ModSet::SHIFT);

        match vi::arrow_alias(key.key) {
            // `n` repeats the last search, `N` reverses it (vi, tmux).
            PhysicalKey::N => self.repeat_search(shift),
            // Arrows move the cursor (shift extends); at the top or bottom
            // edge they scroll the viewport instead.
            PhysicalKey::ArrowUp => {
                if self.cursor_row == 0 {
                    OverlayCommand::ScrollViewport(-1)
                } else {
                    self.move_cursor_key(-1, 0, shift);
                    OverlayCommand::Stay
                }
            }
            PhysicalKey::ArrowDown => {
                if self.cursor_row == self.pane_rows.saturating_sub(1) {
                    OverlayCommand::ScrollViewport(1)
                } else {
                    self.move_cursor_key(1, 0, shift);
                    OverlayCommand::Stay
                }
            }
            PhysicalKey::ArrowLeft => {
                self.move_cursor_key(0, -1, shift);
                OverlayCommand::Stay
            }
            PhysicalKey::ArrowRight => {
                self.move_cursor_key(0, 1, shift);
                OverlayCommand::Stay
            }
            // Tab cycles the selection mode (the overlay captures every key,
            // so the cycle cannot be a global binding).
            PhysicalKey::Tab => {
                self.cycle_mode();
                OverlayCommand::Stay
            }
            PhysicalKey::PageUp | PhysicalKey::NumpadPageUp => {
                OverlayCommand::ScrollViewport(-self.page_scroll_delta())
            }
            PhysicalKey::PageDown | PhysicalKey::NumpadPageDown => {
                OverlayCommand::ScrollViewport(self.page_scroll_delta())
            }
            // Engine-derived grabs copy and exit at the cursor: `A` all,
            // `]` command output (`y` word and `Y` line are vi keys).
            PhysicalKey::A if shift => {
                OverlayCommand::Copy(self.copy_request_with(SelectionGrab::All))
            }
            PhysicalKey::BracketRight => {
                OverlayCommand::Copy(self.copy_request_with(SelectionGrab::Output))
            }
            // Enter copies the two-corner selection and exits, tmux-style.
            PhysicalKey::Enter => OverlayCommand::Copy(self.copy_request()),
            PhysicalKey::Escape => OverlayCommand::Dismiss,
            _ => OverlayCommand::Stay,
        }
    }

    fn handle_mouse(&mut self, mouse: &MouseEvent) -> OverlayCommand {
        match (mouse.action, mouse.button) {
            (MouseAction::Press, MouseButton::Four) => {
                self.scroll_mouse_anchor(-WHEEL_SCROLL_LINES);
                OverlayCommand::ScrollViewport(-WHEEL_SCROLL_LINES)
            }
            (MouseAction::Press, MouseButton::Five) => {
                self.scroll_mouse_anchor(WHEEL_SCROLL_LINES);
                OverlayCommand::ScrollViewport(WHEEL_SCROLL_LINES)
            }
            (MouseAction::Press, MouseButton::Left) => {
                self.set_cursor_from_mouse(mouse);
                self.anchor_row = self.cursor_row;
                self.anchor_col = self.cursor_col;
                if self.mouse_anchor_screen.is_some() {
                    self.mouse_anchor_viewport_row = Some(i32::from(self.anchor_row));
                }
                self.selecting_with_mouse = true;
                OverlayCommand::Stay
            }
            (MouseAction::Motion, MouseButton::Left) if self.selecting_with_mouse => {
                self.set_cursor_from_mouse(mouse);
                OverlayCommand::Stay
            }
            (MouseAction::Release, MouseButton::Left) if self.selecting_with_mouse => {
                self.set_cursor_from_mouse(mouse);
                self.selecting_with_mouse = false;
                if self.anchor_row == self.cursor_row && self.anchor_col == self.cursor_col {
                    // A repeated click copies its word or line; a single
                    // click exits so a mouse-initiated entry cannot trap the
                    // keyboard.
                    self.click_grab.map_or(OverlayCommand::Dismiss, |grab| {
                        OverlayCommand::Copy(self.copy_request_with(grab))
                    })
                } else {
                    OverlayCommand::Copy(self.copy_request())
                }
            }
            _ => OverlayCommand::Stay,
        }
    }
}

#[cfg(test)]
mod tests;
