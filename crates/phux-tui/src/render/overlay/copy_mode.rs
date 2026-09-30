//! Copy-mode overlay: keyboard and mouse text selection over the live pane.
//!
//! Selection is a client-local projection (ADR-0030): the overlay tracks a
//! pane-local rectangle and on commit the dispatcher resolves it against the
//! pane's own engine and writes the host clipboard via OSC 52.
//!
//! Search (`/` forward, `?` backward, `n`/`N` repeat) is typed here and run
//! by the dispatcher against the pane's loaded history; the overlay keeps the
//! hits in document rows so they stay put while the viewport scrolls.

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
        let anchor_row = self
            .mouse_anchor_viewport_row
            .map_or(self.anchor_row, |row| {
                u16::try_from(row.clamp(0, i32::from(self.pane_rows.saturating_sub(1))))
                    .unwrap_or_default()
            });
        CellRange::from_points(
            anchor_row,
            self.anchor_col,
            self.cursor_row,
            self.cursor_col,
        )
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

    /// Move cursor by a delta, clamping to pane bounds.
    fn move_cursor(&mut self, delta_row: i16, delta_col: i16) {
        let max_row = self.pane_rows.saturating_sub(1);
        let max_col = self.pane_cols.saturating_sub(1);

        #[allow(clippy::cast_sign_loss)]
        {
            self.cursor_row = if delta_row >= 0 {
                self.cursor_row
                    .saturating_add(delta_row as u16)
                    .min(max_row)
            } else {
                self.cursor_row
                    .saturating_sub(delta_row.unsigned_abs())
                    .min(max_row)
            };

            self.cursor_col = if delta_col >= 0 {
                self.cursor_col
                    .saturating_add(delta_col as u16)
                    .min(max_col)
            } else {
                self.cursor_col
                    .saturating_sub(delta_col.unsigned_abs())
                    .min(max_col)
            };
        }
    }

    fn move_cursor_key(&mut self, delta_row: i16, delta_col: i16, extend_selection: bool) {
        self.move_cursor(delta_row, delta_col);
        if !extend_selection {
            self.anchor_row = self.cursor_row;
            self.anchor_col = self.cursor_col;
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
        CopyRequest {
            start_row: range.start_row,
            start_col: range.start_col,
            end_row: range.end_row,
            end_col: range.end_col,
            mouse_anchor_screen: self.mouse_anchor_screen,
            rectangle: self.mode == SelectionMode::Rect,
            cursor_row: self.cursor_row,
            cursor_col: self.cursor_col,
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

        let shift = key.mods.contains(ModSet::SHIFT);

        match key.key {
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
            // Engine-derived grabs copy and exit at the cursor: `w` word,
            // `v` line (`V` semantic line), `A` all, `]` command output.
            PhysicalKey::W => OverlayCommand::Copy(self.copy_request_with(SelectionGrab::Word)),
            PhysicalKey::V => {
                let grab = if shift {
                    SelectionGrab::LineSemantic
                } else {
                    SelectionGrab::Line
                };
                OverlayCommand::Copy(self.copy_request_with(grab))
            }
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
                    // A click without a drag exits so a mouse-initiated entry
                    // cannot trap the keyboard.
                    OverlayCommand::Dismiss
                } else {
                    OverlayCommand::Copy(self.copy_request())
                }
            }
            _ => OverlayCommand::Stay,
        }
    }
}

#[cfg(test)]
mod tests {
    use phux_protocol::input::key::{KeyAction, ModSet};

    use super::*;

    fn press(key: PhysicalKey, mods: ModSet) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key,
            mods,
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        }
    }

    fn mouse(action: MouseAction, button: MouseButton, x: f64, y: f64) -> MouseEvent {
        MouseEvent {
            action,
            button,
            x,
            y,
            mods: ModSet::empty(),
        }
    }

    fn wheel(button: MouseButton) -> MouseEvent {
        mouse(MouseAction::Press, button, 0.0, 0.0)
    }

    fn corners(overlay: &CopyModeOverlay) -> (u16, u16, u16, u16) {
        let sel = overlay
            .copy_selection()
            .expect("copy-mode always has a selection");
        (sel.start_row, sel.start_col, sel.end_row, sel.end_col)
    }

    #[test]
    fn keys_map_to_grabs_scrolls_and_exits() {
        let none = ModSet::empty();
        for (key, mods, expected) in [
            (PhysicalKey::W, none, Some(SelectionGrab::Word)),
            (PhysicalKey::V, none, Some(SelectionGrab::Line)),
            (
                PhysicalKey::V,
                ModSet::SHIFT,
                Some(SelectionGrab::LineSemantic),
            ),
            (PhysicalKey::A, ModSet::SHIFT, Some(SelectionGrab::All)),
            (PhysicalKey::BracketRight, none, Some(SelectionGrab::Output)),
            (PhysicalKey::Enter, none, Some(SelectionGrab::Rect)),
            (PhysicalKey::A, none, None),
        ] {
            // Cursor at (2, 5) so engine-derived grabs carry it.
            let cmd = CopyModeOverlay::new(2, 5, 80, 24).handle_key(&press(key, mods));
            match (cmd, expected) {
                (OverlayCommand::Copy(req), Some(grab)) => {
                    assert_eq!(req.grab, grab, "{key:?} {mods:?}");
                    assert_eq!((req.cursor_row, req.cursor_col), (2, 5));
                }
                (OverlayCommand::Stay, None) => {}
                (other, _) => panic!("{key:?} {mods:?}: {other:?}"),
            }
        }
        let mut overlay = CopyModeOverlay::new(2, 5, 80, 24);
        let key = |overlay: &mut CopyModeOverlay, k| overlay.handle_key(&press(k, none));
        assert_eq!(
            key(&mut overlay, PhysicalKey::PageUp),
            OverlayCommand::ScrollViewport(-23)
        );
        assert_eq!(
            key(&mut overlay, PhysicalKey::PageDown),
            OverlayCommand::ScrollViewport(23)
        );
        assert_eq!(
            key(&mut overlay, PhysicalKey::Escape),
            OverlayCommand::Dismiss
        );
        // Arrows at the edges scroll the viewport instead of moving.
        let mut top = CopyModeOverlay::new(0, 5, 80, 24);
        assert_eq!(
            key(&mut top, PhysicalKey::ArrowUp),
            OverlayCommand::ScrollViewport(-1)
        );
        let mut bottom = CopyModeOverlay::new(23, 5, 80, 24);
        assert_eq!(
            key(&mut bottom, PhysicalKey::ArrowDown),
            OverlayCommand::ScrollViewport(1)
        );
        assert_eq!(bottom.cursor_row, 23);
    }

    /// Tab cycles Char -> Line -> Rect -> Char; Rect requests block
    /// extraction, Char and Line linear, and Line spans whole rows on both
    /// the highlight and the copy request (ADR-0045).
    #[test]
    fn tab_cycles_modes_and_each_mode_shapes_the_selection() {
        let mut overlay = CopyModeOverlay::new(1, 3, 80, 24);
        overlay.move_cursor(1, 2);
        assert_eq!(corners(&overlay), (1, 3, 2, 5));
        assert!(!overlay.copy_request().rectangle, "Char is linear");

        let tab = press(PhysicalKey::Tab, ModSet::empty());
        assert_eq!(overlay.handle_key(&tab), OverlayCommand::Stay);
        assert_eq!(overlay.mode, SelectionMode::Line);
        assert_eq!(corners(&overlay), (1, 0, 2, 79));
        let req = overlay.copy_request();
        assert_eq!(
            (req.start_row, req.start_col, req.end_row, req.end_col),
            (1, 0, 2, 79)
        );
        assert!(!req.rectangle, "Line is linear");

        overlay.handle_key(&tab);
        assert_eq!(overlay.mode, SelectionMode::Rect);
        let req = overlay.copy_request();
        assert!(req.rectangle && req.grab == SelectionGrab::Rect);
        overlay.handle_key(&tab);
        assert_eq!(overlay.mode, SelectionMode::Char, "wraps");
    }

    #[test]
    fn arrows_move_the_cursor_and_shift_extends() {
        let overlay = CopyModeOverlay::new(100, 100, 80, 24);
        assert_eq!(
            (overlay.cursor_row, overlay.cursor_col),
            (23, 79),
            "clamped"
        );
        assert_eq!(
            corners(&CopyModeOverlay::new(5, 10, 80, 24)),
            (5, 10, 5, 10)
        );

        let mut overlay = CopyModeOverlay::new(2, 3, 80, 24);
        overlay.handle_key(&press(PhysicalKey::ArrowRight, ModSet::empty()));
        assert_eq!(corners(&overlay), (2, 4, 2, 4), "plain arrows move");
        overlay.handle_key(&press(PhysicalKey::ArrowRight, ModSet::SHIFT));
        assert_eq!(corners(&overlay), (2, 4, 2, 5), "shift extends");
        // A backwards range normalizes.
        let range = CellRange::from_points(5, 10, 2, 3);
        assert_eq!(
            (
                range.start_row,
                range.start_col,
                range.end_row,
                range.end_col
            ),
            (2, 3, 5, 10)
        );
    }

    #[test]
    fn mouse_drags_select_and_copy_while_a_click_exits() {
        let mut overlay = CopyModeOverlay::new(0, 0, 80, 24);
        assert_eq!(
            overlay.handle_mouse(&wheel(MouseButton::Four)),
            OverlayCommand::ScrollViewport(-WHEEL_SCROLL_LINES)
        );
        assert_eq!(
            overlay.handle_mouse(&wheel(MouseButton::Five)),
            OverlayCommand::ScrollViewport(WHEEL_SCROLL_LINES)
        );

        let anchor = ScreenSelectionPoint { col: 4, row: 123 };
        overlay.set_mouse_anchor_screen(anchor);
        overlay.handle_mouse(&mouse(MouseAction::Press, MouseButton::Left, 4.0, 2.0));
        overlay.handle_mouse(&mouse(MouseAction::Motion, MouseButton::Left, 8.0, 3.0));
        assert_eq!(corners(&overlay), (2, 4, 3, 8));
        let OverlayCommand::Copy(req) =
            overlay.handle_mouse(&mouse(MouseAction::Release, MouseButton::Left, 8.0, 3.0))
        else {
            panic!("a dragged release copies");
        };
        assert_eq!(req.grab, SelectionGrab::Rect);
        assert_eq!(
            req.mouse_anchor_screen,
            Some(anchor),
            "the stable press point"
        );

        // A click without a drag must exit rather than trap the keyboard.
        let mut overlay = CopyModeOverlay::new(0, 0, 80, 24);
        overlay.handle_mouse(&mouse(MouseAction::Press, MouseButton::Left, 4.0, 2.0));
        assert_eq!(
            overlay.handle_mouse(&mouse(MouseAction::Release, MouseButton::Left, 4.0, 2.0)),
            OverlayCommand::Dismiss
        );
    }

    /// While a drag wheel-scrolls, the highlight stays on the clicked cell
    /// (it moves down as older rows appear); the copy still resolves the
    /// full-screen press point.
    #[test]
    fn mouse_drag_highlight_stays_with_the_clicked_cell_while_scrolling() {
        let mut overlay = CopyModeOverlay::new(6, 5, 80, 24);
        overlay.set_mouse_anchor_screen(ScreenSelectionPoint { col: 5, row: 106 });
        overlay.handle_mouse(&mouse(MouseAction::Press, MouseButton::Left, 5.0, 6.0));
        overlay.handle_mouse(&wheel(MouseButton::Four));
        let (start_row, _, end_row, _) = corners(&overlay);
        assert_eq!((start_row, end_row), (6, 9));
        assert_eq!(
            overlay.copy_request().mouse_anchor_screen.map(|p| p.row),
            Some(106)
        );
    }

    /// Clamps, Line mode's right edge, and the page span all follow a resize:
    /// a stale-large corner makes the copy resolve to nothing, a stale-small
    /// one leaves new cells unreachable. An in-bounds selection survives.
    #[test]
    fn a_resize_reclamps_and_rederives_pane_geometry() {
        let mut overlay = CopyModeOverlay::new(20, 70, 80, 24);
        overlay.move_cursor(3, 9);
        overlay.on_viewport_resize(60, 18);
        assert_eq!((overlay.cursor_row, overlay.cursor_col), (17, 59));
        assert_eq!((overlay.anchor_row, overlay.anchor_col), (17, 59));

        let mut overlay = CopyModeOverlay::new(0, 0, 60, 20);
        overlay.on_viewport_resize(100, 30);
        overlay.move_cursor(i16::MAX, i16::MAX);
        assert_eq!((overlay.cursor_row, overlay.cursor_col), (29, 99));

        let mut overlay = CopyModeOverlay::new(1, 0, 60, 20);
        overlay.cycle_mode();
        assert_eq!(overlay.copy_request().end_col, 59);
        overlay.on_viewport_resize(100, 30);
        assert_eq!(overlay.copy_request().end_col, 99);
        assert_eq!(overlay.page_scroll_delta(), 29);

        let mut overlay = CopyModeOverlay::new(2, 3, 60, 20);
        overlay.move_cursor(1, 2);
        overlay.on_viewport_resize(100, 30);
        assert_eq!(corners(&overlay), (2, 3, 3, 5));
    }

    fn typed(overlay: &mut CopyModeOverlay, text: &str) -> OverlayCommand {
        let mut last = OverlayCommand::Stay;
        for ch in text.chars() {
            last = overlay.handle_key(&KeyEvent {
                text: Some(ch.to_string()),
                ..press(PhysicalKey::A, ModSet::empty())
            });
        }
        last
    }

    fn found(start: (u32, u16), end: (u32, u16)) -> SearchMatch {
        SearchMatch {
            start_row: start.0,
            start_col: start.1,
            end_row: end.0,
            end_col: end.1,
        }
    }

    /// `/` and `?` open a search line that edits like a prompt; Enter asks the
    /// dispatcher to search from the cursor, Esc closes only the line.
    #[test]
    fn slash_and_question_mark_type_a_search_and_enter_runs_it() {
        let none = ModSet::empty();
        let mut overlay = CopyModeOverlay::new(3, 4, 80, 24);
        assert_eq!(typed(&mut overlay, "/fop"), OverlayCommand::Stay);
        overlay.handle_key(&press(PhysicalKey::Backspace, none));
        overlay.handle_paste("o\n");
        assert_eq!(overlay.search_status().as_deref(), Some("/foo_"));
        assert_eq!(
            overlay.handle_key(&press(PhysicalKey::Enter, none)),
            OverlayCommand::Search(CopySearchRequest {
                needle: "foo".to_owned(),
                backward: false,
                cursor_row: 3,
                cursor_col: 4,
                pane_rows: 24,
            })
        );

        typed(&mut overlay, "?bar");
        assert_eq!(overlay.search_status().as_deref(), Some("?bar_"));
        assert_eq!(
            overlay.handle_key(&press(PhysicalKey::Escape, none)),
            OverlayCommand::Stay
        );
        assert_eq!(overlay.search_status().as_deref(), Some("/foo: no match"));
        assert_eq!(
            overlay.handle_key(&press(PhysicalKey::Escape, none)),
            OverlayCommand::Dismiss,
            "a second Esc leaves copy-mode"
        );
    }

    /// A result selects the current hit (so Enter copies it); `n` repeats in
    /// the search's direction from there and `N` reverses it.
    #[test]
    fn a_search_result_selects_the_hit_and_n_repeats_it() {
        let none = ModSet::empty();
        let mut overlay = CopyModeOverlay::new(0, 0, 80, 24);
        assert_eq!(
            overlay.handle_key(&press(PhysicalKey::N, none)),
            OverlayCommand::Stay
        );
        typed(&mut overlay, "?ab");
        let OverlayCommand::Search(req) = overlay.handle_key(&press(PhysicalKey::Enter, none))
        else {
            panic!("expected a search");
        };
        assert!(req.backward);
        overlay.apply_search_result(CopySearchResult::Found {
            matches: vec![found((100, 2), (100, 3)), found((107, 70), (108, 1))],
            current: 1,
            top: 100,
        });
        assert_eq!(corners(&overlay), (7, 70, 8, 1));
        assert_eq!((overlay.cursor_row, overlay.cursor_col), (7, 70));
        assert_eq!(overlay.search_status().as_deref(), Some("?ab 2/2"));
        let view = overlay.search_view().expect("searching");
        assert_eq!((view.matches.len(), view.current), (2, Some(1)));

        for (mods, backward) in [(none, true), (ModSet::SHIFT, false)] {
            assert_eq!(
                overlay.handle_key(&press(PhysicalKey::N, mods)),
                OverlayCommand::Search(CopySearchRequest {
                    needle: "ab".to_owned(),
                    backward,
                    cursor_row: 7,
                    cursor_col: 70,
                    pane_rows: 24,
                }),
                "{mods:?}"
            );
        }
        // An empty search line repeats the last search.
        typed(&mut overlay, "/");
        assert!(matches!(
            overlay.handle_key(&press(PhysicalKey::Enter, none)),
            OverlayCommand::Search(CopySearchRequest { ref needle, backward: true, .. })
                if needle == "ab"
        ));
        overlay.apply_search_result(CopySearchResult::NotFound);
        assert_eq!(overlay.search_status().as_deref(), Some("?ab: no match"));
    }
}
