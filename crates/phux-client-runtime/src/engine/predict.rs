//! Predictive echo over the shared POD grid: the engine owner's per-view
//! predictor, also driven by the `UniFFI` local engine in `phux-client-ffi`.

use phux_client_core::grid::{Cell, Cursor, GridBuffer};
use phux_client_core::predict::{
    Prediction, PredictionState, PredictiveConfig, reconcile_terminal_output_per_cell_at,
};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use unicode_segmentation::UnicodeSegmentation;

/// A [`PredictionState`] fed from committed text and reconciled against, then
/// overlaid onto, each projected [`GridBuffer`].
#[derive(Debug)]
pub struct Predictor {
    state: PredictionState,
    viewport: (u16, u16),
    /// Rows overlaid in the last publication; restoring their base grid is damage too.
    displayed_rows: Vec<u16>,
    /// Whether guesses are drawn underlined, as a terminal client marks them,
    /// or as plain text for a client that would rather they read as typed.
    underline: bool,
}

impl Predictor {
    /// A predictor for a `cols` x `rows` viewport (each clamped to at least 1).
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        let viewport = (cols.max(1), rows.max(1));
        Self {
            state: PredictionState::new(PredictiveConfig::enabled(), viewport.0, viewport.1),
            viewport,
            displayed_rows: Vec::new(),
            underline: true,
        }
    }

    /// Draw guesses underlined (the default) or as plain text.
    pub const fn set_underline(&mut self, underline: bool) {
        self.underline = underline;
    }

    /// Resize the viewport ahead of the next [`Self::apply`].
    pub fn set_viewport(&mut self, cols: u16, rows: u16) {
        let viewport = (cols.max(1), rows.max(1));
        self.state.set_viewport(viewport.0, viewport.1);
        self.viewport = viewport;
    }

    /// Queue a guess per grapheme of `text`, anchored at `cursor` (`(col, row)`)
    /// when nothing is pending.
    pub fn predict_text(&mut self, text: &str, cursor: (u16, u16), alternate: bool, now_ms: u64) {
        self.state.set_alt_screen(alternate);
        if self.state.pending_len() == 0 {
            self.state.set_cursor(cursor.1, cursor.0);
        }
        for grapheme in text.graphemes(true) {
            if let Some(event) = prediction_event(grapheme) {
                let _ = self.state.predict_key_at(&event, now_ms);
            }
        }
    }

    /// Drop every pending guess.
    pub fn clear(&mut self) {
        self.state.clear();
    }

    /// Guesses still waiting for the server to echo them.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.state.pending_len()
    }

    /// Reconcile pending guesses against `buffer`, then overlay the displayable
    /// ones and move `cursor` to the predicted position.
    ///
    /// An idle predictor returns after syncing the viewport and the screen.
    /// It does not read the grid.
    pub fn apply(
        &mut self,
        cols: u16,
        rows: u16,
        alternate: bool,
        cursor: &mut Cursor,
        buffer: &mut GridBuffer,
        now_ms: u64,
    ) {
        if self.viewport != (cols.max(1), rows.max(1)) {
            self.set_viewport(cols, rows);
        }
        self.state.set_alt_screen(alternate);
        for row in self.displayed_rows.drain(..) {
            if let Some(dirty) = buffer.row_dirty.get_mut(usize::from(row)) {
                *dirty = true;
            }
        }
        if self.state.pending_len() == 0 {
            return;
        }
        let _ = reconcile_terminal_output_per_cell_at(
            &mut self.state,
            cursor.row,
            cursor.col,
            now_ms,
            |row, col| cell_text_at(buffer, cols, row, col),
        );
        if !self.state.should_display(now_ms) {
            return;
        }
        let predictions = self.state.displayable(now_ms).cloned().collect::<Vec<_>>();
        for prediction in &predictions {
            overlay(prediction, cols, buffer, self.underline);
            if let Some(dirty) = buffer.row_dirty.get_mut(usize::from(prediction.row)) {
                *dirty = true;
                self.displayed_rows.push(prediction.row);
            }
        }
        let (row, col) = self.state.cursor();
        cursor.row = row;
        cursor.col = col;
        cursor.visible = true;
    }
}

fn cell_text_at(buffer: &GridBuffer, cols: u16, row: u16, col: u16) -> Option<String> {
    let index = usize::from(row)
        .checked_mul(usize::from(cols))?
        .checked_add(usize::from(col))?;
    cell_text(buffer.cells.get(index)?, &buffer.utf8)
}

fn prediction_event(grapheme: &str) -> Option<KeyEvent> {
    let (key, text) = match grapheme {
        "\r" | "\n" => (PhysicalKey::Enter, None),
        "\u{8}" | "\u{7f}" => (PhysicalKey::Backspace, None),
        "\u{1b}" => (PhysicalKey::Escape, None),
        "\t" => (PhysicalKey::Tab, None),
        value if value.chars().any(char::is_control) => return None,
        value => (PhysicalKey::Unidentified, Some(value.to_owned())),
    };
    Some(KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text,
        unshifted_codepoint: None,
    })
}

fn cell_text(cell: &Cell, arena: &[u8]) -> Option<String> {
    if cell.utf8_len == 0 {
        return None;
    }
    let start = cell.utf8_offset as usize;
    let end = start.checked_add(usize::from(cell.utf8_len))?;
    std::str::from_utf8(arena.get(start..end)?)
        .ok()
        .map(str::to_owned)
}

fn overlay(prediction: &Prediction, cols: u16, buffer: &mut GridBuffer, underline: bool) {
    if prediction.width == 0 {
        return;
    }
    let index = usize::from(prediction.row) * usize::from(cols) + usize::from(prediction.col);
    let offset = buffer.utf8.len();
    let Ok(offset) = u32::try_from(offset) else {
        return;
    };
    let Ok(len) = u16::try_from(prediction.text.len()) else {
        return;
    };
    buffer.utf8.extend_from_slice(prediction.text.as_bytes());
    if let Some(cell) = buffer.cells.get_mut(index) {
        cell.utf8_offset = offset;
        cell.utf8_len = len;
        if underline {
            cell.underline = libghostty_vt::style::Underline::Single as u8;
        }
    }
    if prediction.width == 2
        && let Some(tail) = buffer.cells.get_mut(index + 1)
    {
        tail.utf8_len = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composed_grapheme_stays_one_shared_predictor_event() {
        let events = "e\u{301}👨‍👩‍👧‍👦"
            .graphemes(true)
            .filter_map(prediction_event)
            .count();
        assert_eq!(events, 2);
    }

    #[test]
    fn an_idle_apply_syncs_the_viewport_and_does_not_read_the_grid() {
        let mut predictor = Predictor::new(2, 2);
        let mut cursor = Cursor {
            row: 1,
            col: 1,
            visible: false,
            ..Cursor::default()
        };
        let mut buffer = GridBuffer::default();
        predictor.apply(8, 4, true, &mut cursor, &mut buffer, 10);
        assert_eq!(predictor.viewport, (8, 4));
        assert_eq!(predictor.pending_len(), 0);
        assert!(!cursor.visible);
        assert!(buffer.cells.is_empty());
        assert!(buffer.utf8.is_empty());
    }

    #[test]
    fn guesses_are_underlined_unless_a_client_asks_for_plain_text() {
        let guess = |underline: bool| {
            let mut predictor = Predictor::new(4, 2);
            predictor.set_underline(underline);
            predictor.predict_text("Z", (0, 0), false, 1);
            let mut cursor = Cursor::default();
            let mut buffer = GridBuffer {
                cells: vec![Cell::default(); 8],
                row_dirty: vec![false; 2],
                ..GridBuffer::default()
            };
            predictor.apply(4, 2, false, &mut cursor, &mut buffer, 1);
            assert_eq!(buffer.cell_text(0), b"Z");
            buffer.cells[0].underline
        };
        assert_eq!(guess(true), libghostty_vt::style::Underline::Single as u8);
        assert_eq!(guess(false), 0);
    }

    #[test]
    fn a_pending_insert_overlays_its_cell_without_a_grid_snapshot() {
        let mut predictor = Predictor::new(4, 2);
        predictor.predict_text("Z", (0, 0), false, 1);
        let mut cursor = Cursor {
            visible: true,
            ..Cursor::default()
        };
        let mut buffer = GridBuffer {
            cells: vec![Cell::default(); 8],
            row_dirty: vec![false; 2],
            ..GridBuffer::default()
        };
        predictor.apply(4, 2, false, &mut cursor, &mut buffer, 1);
        assert_eq!(buffer.cell_text(0), b"Z");
        assert_eq!(buffer.cell_text(1), b"");
        assert_eq!(buffer.row_dirty, [true, false]);

        predictor.clear();
        let mut restored = GridBuffer {
            cells: vec![Cell::default(); 8],
            row_dirty: vec![false; 2],
            ..GridBuffer::default()
        };
        predictor.apply(4, 2, false, &mut cursor, &mut restored, 2);
        assert_eq!(restored.cell_text(0), b"");
        assert_eq!(
            restored.row_dirty,
            [true, false],
            "clear repaints the old prediction"
        );
    }
}
