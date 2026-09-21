//! Mobile text-commit adapter for the shared predictor over POD cells.

use libghostty_vt::{screen::Screen, terminal::Terminal};
use phux_client_core::grid::{Cell, Cursor};
use phux_client_core::predict::{
    Prediction, PredictionState, PredictiveConfig, reconcile_terminal_output_per_cell_at,
};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug)]
pub(crate) struct Predictor {
    state: PredictionState,
}

impl Default for Predictor {
    fn default() -> Self {
        Self {
            state: PredictionState::new(PredictiveConfig::enabled(), 1, 1),
        }
    }
}

impl Predictor {
    pub(crate) fn set_viewport(&mut self, cols: u16, rows: u16) {
        self.state.set_viewport(cols, rows);
    }

    pub(crate) fn clear(&mut self) {
        self.state.clear();
    }

    pub(crate) fn predict(&mut self, terminal: &Terminal<'static, 'static>, text: &str) {
        self.state
            .set_alt_screen(matches!(terminal.active_screen(), Ok(Screen::Alternate)));
        if self.state.pending_len() == 0 {
            self.state.set_cursor(
                terminal.cursor_y().unwrap_or(0),
                terminal.cursor_x().unwrap_or(0),
            );
        }
        let now = monotonic_ms();
        for event in prediction_events(text) {
            let _ = self.state.predict_key_at(&event, now);
        }
    }

    pub(crate) fn apply(
        &mut self,
        terminal: &Terminal<'static, 'static>,
        cols: u16,
        cursor: &mut Cursor,
        cells: &mut [Cell],
        utf8: &mut Vec<u8>,
    ) {
        self.state
            .set_alt_screen(matches!(terminal.active_screen(), Ok(Screen::Alternate)));
        let authoritative = cells
            .iter()
            .map(|cell| cell_text(cell, utf8))
            .collect::<Vec<_>>();
        let now = monotonic_ms();
        let _ = reconcile_terminal_output_per_cell_at(
            &mut self.state,
            cursor.row,
            cursor.col,
            now,
            |row, col| {
                authoritative
                    .get(usize::from(row) * usize::from(cols) + usize::from(col))
                    .cloned()
                    .flatten()
            },
        );
        let predictions = self.state.displayable(now).cloned().collect::<Vec<_>>();
        for prediction in &predictions {
            overlay(prediction, cols, cells, utf8);
        }
        if self.state.should_display(now) {
            let (row, col) = self.state.cursor();
            cursor.row = row;
            cursor.col = col;
            cursor.visible = true;
        }
    }
}

fn prediction_events(text: &str) -> Vec<KeyEvent> {
    text.graphemes(true).filter_map(prediction_event).collect()
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

fn overlay(prediction: &Prediction, cols: u16, cells: &mut [Cell], utf8: &mut Vec<u8>) {
    if prediction.width == 0 {
        return;
    }
    let index = usize::from(prediction.row) * usize::from(cols) + usize::from(prediction.col);
    let Ok(offset) = u32::try_from(utf8.len()) else {
        return;
    };
    let Ok(len) = u16::try_from(prediction.text.len()) else {
        return;
    };
    utf8.extend_from_slice(prediction.text.as_bytes());
    if let Some(cell) = cells.get_mut(index) {
        cell.utf8_offset = offset;
        cell.utf8_len = len;
        cell.underline = libghostty_vt::style::Underline::Single as u8;
    }
    if prediction.width == 2
        && let Some(tail) = cells.get_mut(index + 1)
    {
        tail.utf8_len = 0;
    }
}

fn monotonic_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    u64::try_from(EPOCH.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composed_grapheme_stays_one_shared_predictor_event() {
        let events = prediction_events("e\u{301}👨‍👩‍👧‍👦");
        assert_eq!(events.len(), 2);
    }
}
