//! Render the prediction overlay to the outer terminal, after the
//! authoritative frame: per cell `CUP`, `SGR 0;4`, the cluster, `SGR 0`,
//! so underline never leaks into authoritative cells.

use std::io::{self, Write};

use super::state::{PredictionKind, PredictionState};

/// Stateless overlay writer.
#[derive(Debug, Default)]
pub struct Overlay;

impl Overlay {
    /// Paint every pending prediction in `state` onto `out` and return the
    /// number of cells painted (flushing only when non-zero).
    ///
    /// Predictions are pane-local; `origin` is the pane's top-left in the
    /// outer viewport.
    #[allow(clippy::unused_self, reason = "stable call shape")]
    pub fn render(
        &self,
        state: &PredictionState,
        origin: (u16, u16),
        out: &mut impl Write,
    ) -> io::Result<usize> {
        let (ox, oy) = origin;
        let mut count = 0;
        for p in state.pending() {
            // Cursor-motion predictions paint no cell.
            if matches!(
                p.kind,
                PredictionKind::Newline | PredictionKind::CursorLeft | PredictionKind::CursorRight
            ) {
                continue;
            }
            write_cup(out, p.row.saturating_add(oy), p.col.saturating_add(ox))?;
            out.write_all(b"\x1b[0m\x1b[4m")?;
            out.write_all(p.text.as_bytes())?;
            out.write_all(b"\x1b[0m")?;
            count += 1;
        }
        if count > 0 {
            out.flush()?;
        }
        Ok(count)
    }
}

fn write_cup(out: &mut impl Write, row: u16, col: u16) -> io::Result<()> {
    let r = row.saturating_add(1);
    let c = col.saturating_add(1);
    write!(out, "\x1b[{r};{c}H")
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::predict::state::{PredictionState, PredictiveConfig};
    use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

    fn key_text(s: &str) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::A,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: Some(s.to_owned()),
            unshifted_codepoint: s.chars().next().map(u32::from),
        }
    }

    #[test]
    fn empty_queue_writes_nothing() {
        let state = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let mut buf = Vec::new();
        let n = Overlay
            .render(&state, (0, 0), &mut buf)
            .expect("overlay render");
        assert_eq!(n, 0);
        assert!(buf.is_empty());
    }

    /// Predictions are pane-local; the pane origin offsets every CUP so a
    /// non-origin pane never ghosts at the viewport top.
    #[test]
    fn predictions_paint_in_order_offset_by_the_pane_origin() {
        let mut state = PredictionState::new(PredictiveConfig::enabled(), 80, 12);
        state.predict_key(&key_text("a"));
        state.predict_key(&key_text("b"));
        let mut buf = Vec::new();
        let n = Overlay.render(&state, (0, 13), &mut buf).expect("render");
        assert_eq!(n, 2);
        assert_eq!(
            String::from_utf8(buf).expect("utf8"),
            "\x1b[14;1H\x1b[0m\x1b[4ma\x1b[0m\x1b[14;2H\x1b[0m\x1b[4mb\x1b[0m"
        );
    }
}
