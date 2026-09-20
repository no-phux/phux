//! Owner-thread predictive echo over the shared POD grid.

use phux_client_core::grid::{Cell, Cursor, GridBuffer};
use phux_client_core::predict::{
    Prediction, PredictionState, PredictiveConfig, reconcile_terminal_output_per_cell_at,
};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug)]
pub(super) struct Predictor {
    state: PredictionState,
    viewport: (u16, u16),
}

impl Predictor {
    pub(super) fn new(cols: u16, rows: u16) -> Self {
        let viewport = (cols.max(1), rows.max(1));
        Self {
            state: PredictionState::new(PredictiveConfig::enabled(), viewport.0, viewport.1),
            viewport,
        }
    }

    pub(super) fn predict_text(
        &mut self,
        text: &str,
        cursor: (u16, u16),
        alternate: bool,
        now_ms: u64,
    ) {
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

    pub(super) fn clear(&mut self) {
        self.state.clear();
    }

    pub(super) fn apply(
        &mut self,
        cols: u16,
        rows: u16,
        alternate: bool,
        cursor: &mut Cursor,
        buffer: &mut GridBuffer,
        now_ms: u64,
    ) {
        let viewport = (cols.max(1), rows.max(1));
        if self.viewport != viewport {
            self.state.set_viewport(viewport.0, viewport.1);
            self.viewport = viewport;
        }
        self.state.set_alt_screen(alternate);
        let authoritative = buffer
            .cells
            .iter()
            .map(|cell| cell_text(cell, &buffer.utf8))
            .collect::<Vec<_>>();
        let _ = reconcile_terminal_output_per_cell_at(
            &mut self.state,
            cursor.row,
            cursor.col,
            now_ms,
            |row, col| {
                authoritative
                    .get(usize::from(row) * usize::from(cols) + usize::from(col))
                    .cloned()
                    .flatten()
            },
        );
        let predictions = self.state.displayable(now_ms).cloned().collect::<Vec<_>>();
        for prediction in &predictions {
            overlay(prediction, cols, buffer);
        }
        if self.state.should_display(now_ms) {
            let (row, col) = self.state.cursor();
            cursor.row = row;
            cursor.col = col;
            cursor.visible = true;
        }
    }
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

fn overlay(prediction: &Prediction, cols: u16, buffer: &mut GridBuffer) {
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
        cell.underline = libghostty_vt::style::Underline::Single as u8;
    }
    if prediction.width == 2
        && let Some(tail) = buffer.cells.get_mut(index + 1)
    {
        tail.utf8_len = 0;
    }
}
