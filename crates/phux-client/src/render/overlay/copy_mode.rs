//! Copy-mode overlay (phux-wave-a-copy-mode).
//!
//! Provides terminal-based text selection with visual feedback. The overlay
//! captures arrow keys to adjust selection boundaries and Enter to copy the
//! selected text. Per [ADR-0030](../../../../ADR/0030-engine-delegated-wire-and-projection-consumers.md),
//! selection is a *client-local projection*: the overlay tracks the selection
//! rectangle in pane-local viewport cells, and on Enter the dispatcher
//! resolves it against the focused pane's own libghostty engine
//! (`format_selection_alloc`) and writes the text to the host clipboard via
//! OSC 52. Nothing about the selection touches the wire.

use phux_protocol::input::key::{KeyEvent, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{
    CopyRequest, OverlayCommand, RenderOverlay, SelectionGrab, SelectionMode, SelectionRect,
};

const WHEEL_SCROLL_LINES: isize = 3;

fn quantize_mouse_cell(value: f64, max: u16) -> u16 {
    if !value.is_finite() || max == 0 {
        return 0;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let cell = value.floor().max(0.0) as u16;
    cell.min(max.saturating_sub(1))
}

/// Rectangular selection state: (row, col) coordinates for start and end.
/// Normalized so that start <= end.
#[derive(Debug, Clone, Copy)]
struct CellRange {
    start_row: u16,
    start_col: u16,
    end_row: u16,
    end_col: u16,
}

impl CellRange {
    /// Create a range from cursor and endpoint. Normalizes so start <= end.
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
    /// Pane dimensions (cols, rows) — used to clamp cursor movement.
    pub pane_cols: u16,
    /// Number of rows in the pane.
    pub pane_rows: u16,
    /// Whether a left-button drag is actively extending the selection.
    selecting_with_mouse: bool,
}

impl CopyModeOverlay {
    /// Create a copy-mode overlay with cursor at the given position.
    /// `pane_cols` and `pane_rows` are used to clamp cursor movement.
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
        }
    }

    /// The current selection mode.
    ///
    /// The lockstep read side of the mode state machine: the in-overlay `Tab`
    /// key flips the mode via [`Self::cycle_mode`], the renderer and the
    /// copy path read it back through here. Mode is client-local UI state, so
    /// nothing about it touches the wire (ADR-0030).
    #[must_use]
    pub const fn mode(&self) -> SelectionMode {
        self.mode
    }

    /// Advance the selection mode `Char -> Line -> Rect -> Char`.
    ///
    /// The lockstep write side of the mode state machine, driven by the
    /// in-overlay `Tab` key: the overlay captures every keystroke while it is
    /// up, so the cycle key must live here — see
    /// [`RenderOverlay::handle_key`]. No wire traffic — the mode is a
    /// consumer-side projection detail (ADR-0030).
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

    /// The selection range adjusted for the active [`SelectionMode`].
    ///
    /// `Char` and `Rect` use the raw two-corner range as-is (the linear-vs-block
    /// distinction is carried by the `rectangle` flag downstream). `Line`
    /// expands the range to whole visible lines — column `0` through the last
    /// pane column — so a line selection covers the full rows it spans instead
    /// of collapsing to the same geometry as `Char` (ADR-0045). Both the
    /// highlight ([`copy_selection`]) and the copy request
    /// ([`copy_request_with`]) resolve through here, so what the renderer
    /// inverts and what the bridge extracts stay in lockstep.
    ///
    /// [`copy_selection`]: RenderOverlay::copy_selection
    fn effective_range(&self) -> CellRange {
        let range = self.selection_range();
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

    /// Build the client-local copy request for the current two-corner
    /// selection: the normalized inclusive viewport rectangle plus the
    /// block/linear flag. The dispatcher resolves it against the focused
    /// pane's own engine.
    fn copy_request(&self) -> CopyRequest {
        self.copy_request_with(SelectionGrab::Rect)
    }

    /// Build a copy request tagged with `grab`. For the engine-derived grabs
    /// (`Word`/`Line`/`LineSemantic`/`All`/`Output`) the `start`/`end`
    /// rectangle is still carried (so the highlight stays coherent) but the
    /// dispatcher resolves against `cursor_row`/`cursor_col` instead.
    fn copy_request_with(&self, grab: SelectionGrab) -> CopyRequest {
        let range = self.effective_range();
        CopyRequest {
            start_row: range.start_row,
            start_col: range.start_col,
            end_row: range.end_row,
            end_col: range.end_col,
            rectangle: self.mode == SelectionMode::Rect,
            cursor_row: self.cursor_row,
            cursor_col: self.cursor_col,
            grab,
        }
    }
}

impl RenderOverlay for CopyModeOverlay {
    /// Copy-mode is **not** a modal overlay and paints nothing of its own.
    ///
    /// Unlike help/palette/prompts (which draw a surface onto a cleared
    /// screen), copy-mode is a selection highlight over the live pane. The
    /// driver detects it via [`Self::copy_selection`] and repaints the focused
    /// pane with the selected cells reverse-videoed — the screen content is
    /// otherwise untouched. So this `render` is intentionally empty.
    fn render(&self, _area: Rect, _buf: &mut Buffer) {}

    /// phux-d26y: adopt the focused pane's new size and pull the selection
    /// back inside it.
    ///
    /// Copy-mode is not a pinned box — it is a selection over the live pane,
    /// so unlike the context menu it *survives* a resize
    /// ([`RenderOverlay::survives_resize`] stays `true`): dropping it would
    /// discard an in-progress selection the user is still building. What it
    /// cannot do is keep clamping against dimensions the pane no longer has.
    /// Stale-large strands the cursor outside the grid, and the copy path
    /// resolves that corner through `terminal.grid_ref(..).ok()?` — so the
    /// extraction returns `None` and Enter dismisses copy-mode having
    /// silently copied nothing. Stale-small does the mirror thing: the newly
    /// revealed columns and rows are unreachable, and a Line-mode copy stops
    /// at the old right edge.
    ///
    /// Both corners are clamped, not just the cursor. Clamping the anchor
    /// keeps a smaller selection alive across a shrink, which is the
    /// friendlier of the two options — the alternative (collapsing to a
    /// cursor-only state) throws away a selection the user may have spent
    /// several keystrokes building, and a partially-preserved selection is
    /// still visible and still correct about the cells it names.
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

    fn copy_selection(&self) -> Option<SelectionRect> {
        // `effective_range` applies the Line-mode whole-line expansion, and
        // `from_range` sets the block/linear flag from the mode, so the renderer
        // highlights exactly what a copy of the current mode would extract
        // (ADR-0045).
        let range = self.effective_range();
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

        let shift = key.mods.contains(ModSet::SHIFT);

        match key.key {
            // Arrow keys adjust the selection in-place; the driver repaints
            // the overlay after every key while it is active, so `Stay` is
            // enough to reflect the moved cursor. No wire traffic (ADR-0030).
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
            // Tab cycles the selection geometry `Char -> Line -> Rect -> Char`
            // (ADR-0045). The overlay captures every keystroke while it is up,
            // so the mode-cycle must be an in-overlay key rather than a global
            // keybind resolved past the capture; `Stay` triggers the driver's
            // per-key overlay repaint, so the new geometry's highlight shows
            // immediately. Client-local UI state — no wire traffic (ADR-0030).
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
            // Engine-derived one-shot grabs (phux-7143). These copy-and-exit
            // immediately (tmux-style): the dispatcher resolves the grab at the
            // overlay cursor against the focused pane's own libghostty engine
            // (`select_word`/`select_line`/`select_all`/`select_output`) and
            // emits OSC 52. No wire traffic (ADR-0030).
            //
            // `w` = word under cursor.
            PhysicalKey::W => OverlayCommand::Copy(self.copy_request_with(SelectionGrab::Word)),
            // `v` = whole line; `V` (shift) = line bounded by semantic-prompt
            // state changes (OSC-133 zones).
            PhysicalKey::V => {
                let grab = if shift {
                    SelectionGrab::LineSemantic
                } else {
                    SelectionGrab::Line
                };
                OverlayCommand::Copy(self.copy_request_with(grab))
            }
            // `A` (shift) = select all selectable content.
            PhysicalKey::A if shift => {
                OverlayCommand::Copy(self.copy_request_with(SelectionGrab::All))
            }
            // `]` = command-output span under cursor (best-effort; no-op when
            // the pane lacks OSC-133 zones).
            PhysicalKey::BracketRight => {
                OverlayCommand::Copy(self.copy_request_with(SelectionGrab::Output))
            }
            // Enter copies the current two-corner selection client-locally (the
            // dispatcher resolves it against the focused pane's engine and emits
            // OSC 52) and exits copy-mode, tmux-style.
            PhysicalKey::Enter => OverlayCommand::Copy(self.copy_request()),
            PhysicalKey::Escape => OverlayCommand::Dismiss,
            _ => OverlayCommand::Stay,
        }
    }

    fn handle_mouse(&mut self, mouse: &MouseEvent) -> OverlayCommand {
        match (mouse.action, mouse.button) {
            (MouseAction::Press, MouseButton::Four) => {
                OverlayCommand::ScrollViewport(-WHEEL_SCROLL_LINES)
            }
            (MouseAction::Press, MouseButton::Five) => {
                OverlayCommand::ScrollViewport(WHEEL_SCROLL_LINES)
            }
            (MouseAction::Press, MouseButton::Left) => {
                self.set_cursor_from_mouse(mouse);
                self.anchor_row = self.cursor_row;
                self.anchor_col = self.cursor_col;
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
                    // A click without a drag selects nothing — exit copy-mode
                    // (tmux-style) so a mouse-initiated entry can't trap the
                    // user with the keyboard captured.
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

    /// A press `KeyEvent` for `key` with `mods`.
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

    fn mouse_event(action: MouseAction, button: MouseButton, x: f64, y: f64) -> MouseEvent {
        MouseEvent {
            action,
            button,
            x,
            y,
            mods: ModSet::empty(),
        }
    }

    fn mouse_wheel(button: MouseButton) -> MouseEvent {
        mouse_event(MouseAction::Press, button, 0.0, 0.0)
    }

    /// Drive `key` through a fresh overlay and return the resulting command.
    fn dispatch(key: PhysicalKey, mods: ModSet) -> OverlayCommand {
        // Cursor at (2, 5) so engine-derived grabs carry a non-zero cursor.
        let mut overlay = CopyModeOverlay::new(2, 5, 80, 24);
        overlay.handle_key(&press(key, mods))
    }

    fn grab_of(cmd: &OverlayCommand) -> SelectionGrab {
        match cmd {
            OverlayCommand::Copy(req) => req.grab,
            other => panic!("expected Copy, got {other:?}"),
        }
    }

    #[test]
    fn key_w_grabs_word() {
        let cmd = dispatch(PhysicalKey::W, ModSet::empty());
        assert_eq!(grab_of(&cmd), SelectionGrab::Word);
        if let OverlayCommand::Copy(req) = cmd {
            assert_eq!((req.cursor_row, req.cursor_col), (2, 5));
        }
    }

    #[test]
    fn key_v_grabs_line() {
        let cmd = dispatch(PhysicalKey::V, ModSet::empty());
        assert_eq!(grab_of(&cmd), SelectionGrab::Line);
    }

    #[test]
    fn shift_v_grabs_semantic_line() {
        let cmd = dispatch(PhysicalKey::V, ModSet::SHIFT);
        assert_eq!(grab_of(&cmd), SelectionGrab::LineSemantic);
    }

    #[test]
    fn shift_a_grabs_all() {
        let cmd = dispatch(PhysicalKey::A, ModSet::SHIFT);
        assert_eq!(grab_of(&cmd), SelectionGrab::All);
    }

    #[test]
    fn unshifted_a_is_inert() {
        // `a` without shift is not a grab key — it must be consumed (Stay),
        // not mistaken for select-all.
        assert_eq!(
            dispatch(PhysicalKey::A, ModSet::empty()),
            OverlayCommand::Stay
        );
    }

    #[test]
    fn key_bracket_right_grabs_output() {
        let cmd = dispatch(PhysicalKey::BracketRight, ModSet::empty());
        assert_eq!(grab_of(&cmd), SelectionGrab::Output);
    }

    #[test]
    fn enter_grabs_rect() {
        let cmd = dispatch(PhysicalKey::Enter, ModSet::empty());
        assert_eq!(grab_of(&cmd), SelectionGrab::Rect);
    }

    #[test]
    fn cycle_mode_advances_char_line_rect_char() {
        let mut overlay = CopyModeOverlay::new(0, 0, 80, 24);
        assert_eq!(overlay.mode(), SelectionMode::Char, "default is Char");
        overlay.cycle_mode();
        assert_eq!(overlay.mode(), SelectionMode::Line);
        overlay.cycle_mode();
        assert_eq!(overlay.mode(), SelectionMode::Rect);
        overlay.cycle_mode();
        assert_eq!(overlay.mode(), SelectionMode::Char, "wraps back to Char");
    }

    #[test]
    fn rect_mode_copy_request_is_block() {
        let mut overlay = CopyModeOverlay::new(2, 5, 80, 24);
        overlay.cycle_mode(); // Char -> Line
        overlay.cycle_mode(); // Line -> Rect
        assert_eq!(overlay.mode(), SelectionMode::Rect);
        let req = overlay.copy_request();
        assert!(
            req.rectangle,
            "Rect mode must request block (rectangular) extraction"
        );
        assert_eq!(
            req.grab,
            SelectionGrab::Rect,
            "the two-corner Enter path always tags SelectionGrab::Rect"
        );
    }

    #[test]
    fn char_and_line_mode_copy_request_is_linear() {
        let mut overlay = CopyModeOverlay::new(2, 5, 80, 24);
        assert_eq!(overlay.mode(), SelectionMode::Char);
        assert!(
            !overlay.copy_request().rectangle,
            "Char mode is linear, not block"
        );
        overlay.cycle_mode(); // Char -> Line
        assert_eq!(overlay.mode(), SelectionMode::Line);
        assert!(
            !overlay.copy_request().rectangle,
            "Line mode is linear over the two-corner range, not block"
        );
    }

    #[test]
    fn tab_key_cycles_selection_mode() {
        // The overlay captures every keystroke while up, so the mode-cycle is
        // an in-overlay key (ADR-0045). Tab advances Char -> Line -> Rect ->
        // Char and stays in copy-mode (the driver repaints on `Stay`).
        let mut overlay = CopyModeOverlay::new(0, 0, 80, 24);
        assert_eq!(overlay.mode(), SelectionMode::Char);
        assert_eq!(
            overlay.handle_key(&press(PhysicalKey::Tab, ModSet::empty())),
            OverlayCommand::Stay
        );
        assert_eq!(overlay.mode(), SelectionMode::Line);
        overlay.handle_key(&press(PhysicalKey::Tab, ModSet::empty()));
        assert_eq!(overlay.mode(), SelectionMode::Rect);
        overlay.handle_key(&press(PhysicalKey::Tab, ModSet::empty()));
        assert_eq!(overlay.mode(), SelectionMode::Char, "wraps back to Char");
    }

    #[test]
    fn line_mode_expands_selection_to_whole_lines() {
        // Anchor (1,3), cursor dragged to (2,5) on an 80-col pane. Char mode
        // keeps the two-corner range; Line mode expands the columns to the full
        // line width (0 .. cols-1) on both the highlight and the copy request,
        // so a line selection covers whole rows instead of collapsing to Char.
        let mut overlay = CopyModeOverlay::new(1, 3, 80, 24);
        overlay.move_cursor(1, 2); // cursor -> (2, 5)

        // Char mode: the raw two-corner range.
        let sel = overlay.copy_selection().expect("selection");
        assert_eq!(
            (sel.start_row, sel.start_col, sel.end_row, sel.end_col),
            (1, 3, 2, 5)
        );
        assert!(!sel.rectangle, "Char mode is linear");

        overlay.cycle_mode(); // Char -> Line
        assert_eq!(overlay.mode(), SelectionMode::Line);
        let sel = overlay.copy_selection().expect("selection");
        assert_eq!(
            (sel.start_row, sel.start_col, sel.end_row, sel.end_col),
            (1, 0, 2, 79),
            "Line mode spans whole visible lines (col 0 .. cols-1)"
        );
        assert!(!sel.rectangle, "Line mode is linear, not block");
        // The copy request carries the same expanded corners.
        let req = overlay.copy_request();
        assert_eq!(
            (req.start_row, req.start_col, req.end_row, req.end_col),
            (1, 0, 2, 79)
        );
        assert!(!req.rectangle);
    }

    #[test]
    fn page_up_scrolls_one_visible_page() {
        assert_eq!(
            dispatch(PhysicalKey::PageUp, ModSet::empty()),
            OverlayCommand::ScrollViewport(-23)
        );
    }

    #[test]
    fn page_down_scrolls_one_visible_page() {
        assert_eq!(
            dispatch(PhysicalKey::PageDown, ModSet::empty()),
            OverlayCommand::ScrollViewport(23)
        );
    }

    #[test]
    fn arrow_up_at_top_scrolls_one_line() {
        let mut overlay = CopyModeOverlay::new(0, 5, 80, 24);
        assert_eq!(
            overlay.handle_key(&press(PhysicalKey::ArrowUp, ModSet::empty())),
            OverlayCommand::ScrollViewport(-1)
        );
        assert_eq!(overlay.cursor_row, 0);
    }

    #[test]
    fn arrow_down_at_bottom_scrolls_one_line() {
        let mut overlay = CopyModeOverlay::new(23, 5, 80, 24);
        assert_eq!(
            overlay.handle_key(&press(PhysicalKey::ArrowDown, ModSet::empty())),
            OverlayCommand::ScrollViewport(1)
        );
        assert_eq!(overlay.cursor_row, 23);
    }

    #[test]
    fn mouse_wheel_scrolls_viewport() {
        let mut overlay = CopyModeOverlay::new(2, 5, 80, 24);
        assert_eq!(
            overlay.handle_mouse(&mouse_wheel(MouseButton::Four)),
            OverlayCommand::ScrollViewport(-WHEEL_SCROLL_LINES)
        );
        assert_eq!(
            overlay.handle_mouse(&mouse_wheel(MouseButton::Five)),
            OverlayCommand::ScrollViewport(WHEEL_SCROLL_LINES)
        );
    }

    #[test]
    fn click_without_drag_dismisses() {
        let mut overlay = CopyModeOverlay::new(0, 0, 80, 24);
        overlay.handle_mouse(&mouse_event(
            MouseAction::Press,
            MouseButton::Left,
            4.0,
            2.0,
        ));
        assert_eq!(
            overlay.handle_mouse(&mouse_event(
                MouseAction::Release,
                MouseButton::Left,
                4.0,
                2.0
            )),
            OverlayCommand::Dismiss,
            "an empty selection must exit copy-mode, not trap the user"
        );
    }

    #[test]
    fn cell_range_normalization() {
        let range = CellRange::from_points(5, 10, 2, 3);
        assert_eq!(range.start_row, 2);
        assert_eq!(range.start_col, 3);
        assert_eq!(range.end_row, 5);
        assert_eq!(range.end_col, 10);
    }

    #[test]
    fn cursor_clamped_to_pane() {
        let overlay = CopyModeOverlay::new(100, 100, 80, 24);
        assert_eq!(overlay.cursor_row, 23);
        assert_eq!(overlay.cursor_col, 79);
    }

    #[test]
    fn copy_selection_tracks_normalized_range() {
        let mut overlay = CopyModeOverlay::new(2, 3, 80, 24); // anchor = (2, 3)
        overlay.move_cursor(1, 2); // cursor -> (3, 5)
        let sel = overlay
            .copy_selection()
            .expect("copy-mode always has a selection");
        assert_eq!(
            (sel.start_row, sel.start_col, sel.end_row, sel.end_col),
            (2, 3, 3, 5)
        );
    }

    #[test]
    fn arrow_keys_move_cursor_without_extending_unless_shift_is_held() {
        let mut overlay = CopyModeOverlay::new(2, 3, 80, 24);
        assert_eq!(
            overlay.handle_key(&press(PhysicalKey::ArrowRight, ModSet::empty())),
            OverlayCommand::Stay
        );
        let sel = overlay.copy_selection().expect("selection");
        assert_eq!(
            (sel.start_row, sel.start_col, sel.end_row, sel.end_col),
            (2, 4, 2, 4),
            "plain arrows move the cursor instead of selecting from the original anchor"
        );

        assert_eq!(
            overlay.handle_key(&press(PhysicalKey::ArrowRight, ModSet::SHIFT)),
            OverlayCommand::Stay
        );
        let sel = overlay.copy_selection().expect("selection");
        assert_eq!(
            (sel.start_row, sel.start_col, sel.end_row, sel.end_col),
            (2, 4, 2, 5),
            "shift-arrows extend the selection"
        );
    }

    #[test]
    fn mouse_drag_updates_selection_and_copies_on_release() {
        let mut overlay = CopyModeOverlay::new(0, 0, 80, 24);
        assert_eq!(
            overlay.handle_mouse(&mouse_event(
                MouseAction::Press,
                MouseButton::Left,
                4.0,
                2.0
            )),
            OverlayCommand::Stay
        );
        assert_eq!(
            overlay.handle_mouse(&mouse_event(
                MouseAction::Motion,
                MouseButton::Left,
                8.0,
                3.0
            )),
            OverlayCommand::Stay
        );
        let sel = overlay
            .copy_selection()
            .expect("copy-mode always has a selection");
        assert_eq!(
            (sel.start_row, sel.start_col, sel.end_row, sel.end_col),
            (2, 4, 3, 8)
        );
        let cmd = overlay.handle_mouse(&mouse_event(
            MouseAction::Release,
            MouseButton::Left,
            8.0,
            3.0,
        ));
        assert_eq!(grab_of(&cmd), SelectionGrab::Rect);
    }

    // ---------- phux-d26y: pane dims must track a resize ----------

    /// Shrink: the clamp must follow the pane down, or the overlay keeps
    /// permitting a cursor past the new edge. The copy path resolves that
    /// point with `terminal.grid_ref(...).ok()?`, so an out-of-range corner
    /// makes `extract_selection_text` return `None` — Enter dismisses
    /// copy-mode having silently copied nothing.
    #[test]
    fn a_shrink_reclamps_the_cursor_and_the_anchor() {
        let mut overlay = CopyModeOverlay::new(20, 70, 80, 24);
        // Drag out a selection that fills the old pane.
        overlay.move_cursor(3, 9); // cursor -> (23, 79), the old bottom-right
        assert_eq!((overlay.cursor_row, overlay.cursor_col), (23, 79));

        overlay.on_viewport_resize(60, 18);

        assert_eq!(
            (overlay.cursor_row, overlay.cursor_col),
            (17, 59),
            "the cursor must be pulled inside the new pane",
        );
        assert_eq!(
            (overlay.anchor_row, overlay.anchor_col),
            (17, 59),
            "the anchor is clamped too — a corner left outside the pane is the \
             one that makes the copy resolve to nothing",
        );
        // And the request it would commit now names only real cells.
        let req = overlay.copy_request();
        assert!(
            req.end_row < 18 && req.end_col < 60 && req.start_row < 18 && req.start_col < 60,
            "every corner of the committed request is inside the pane: {req:?}",
        );
    }

    /// Grow: the clamp must follow the pane up, or the cursor stops at the
    /// old edge and the newly revealed region is unreachable.
    #[test]
    fn a_grow_lets_the_cursor_reach_the_new_edges() {
        let mut overlay = CopyModeOverlay::new(0, 0, 60, 20);
        overlay.on_viewport_resize(100, 30);
        // Walk hard into the bottom-right; `move_cursor` saturates at the clamp.
        overlay.move_cursor(i16::MAX, i16::MAX);
        assert_eq!(
            (overlay.cursor_row, overlay.cursor_col),
            (29, 99),
            "the cursor must reach the corner of the grown pane",
        );
    }

    /// Line mode takes its right edge from `pane_cols`, so a stale value
    /// silently truncates a whole-line copy at the old width.
    #[test]
    fn line_mode_spans_the_new_width_after_a_grow() {
        let mut overlay = CopyModeOverlay::new(1, 0, 60, 20);
        overlay.cycle_mode(); // Char -> Line
        assert_eq!(overlay.mode(), SelectionMode::Line);
        assert_eq!(
            overlay.copy_request().end_col,
            59,
            "precondition: the line ends at the old right edge",
        );

        overlay.on_viewport_resize(100, 30);

        assert_eq!(
            overlay.copy_request().end_col,
            99,
            "a whole-line copy must reach the new right edge, not the old one",
        );
    }

    /// A resize that does not move the cursor leaves the selection alone —
    /// growing must not disturb an in-progress selection.
    #[test]
    fn a_grow_preserves_an_in_bounds_selection() {
        let mut overlay = CopyModeOverlay::new(2, 3, 60, 20);
        overlay.move_cursor(1, 2); // cursor -> (3, 5), anchor stays (2, 3)
        overlay.on_viewport_resize(100, 30);
        let sel = overlay
            .copy_selection()
            .expect("copy-mode always has a selection");
        assert_eq!(
            (sel.start_row, sel.start_col, sel.end_row, sel.end_col),
            (2, 3, 3, 5),
            "an in-bounds selection survives the resize untouched",
        );
    }

    /// Page scrolling reads `pane_rows`; it must follow the resize too.
    #[test]
    fn the_page_span_follows_the_resize() {
        let mut overlay = CopyModeOverlay::new(0, 0, 80, 24);
        assert_eq!(overlay.page_scroll_delta(), 23);
        overlay.on_viewport_resize(80, 40);
        assert_eq!(overlay.page_scroll_delta(), 39);
    }
}
