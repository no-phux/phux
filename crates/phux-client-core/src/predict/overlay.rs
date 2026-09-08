//! Render the prediction overlay to the outer terminal.
//!
//! The overlay is painted *after* `phux_tui::attach::render::TerminalRenderer::render`
//! has finished so the predictions sit visually on top of the authoritative
//! frame. Each prediction becomes one positioned write:
//!
//! ```text
//! ESC [ <row+1> ; <col+1> H   CUP — position cursor
//! ESC [ 0 m                    reset SGR
//! ESC [ 4 m                    underline
//! <ch>                          the predicted character
//! ESC [ 0 m                    reset SGR
//! ```
//!
//! The final reset matters because the next reconcile / renderer pass
//! emits its own SGR sequence; we don't want a lingering underline bit
//! to leak into authoritative cells if the next renderer write happens
//! to skip the cell we painted.
//!
//! No allocation per render — the overlay borrows the state and writes
//! directly to the caller's `Write` (typically `io::stdout().lock()`).

use std::io::{self, Write};

use super::state::{PredictionKind, PredictionState};

/// Stateless overlay writer. Owns no buffers; callers pass the
/// prediction state and a writer.
///
/// Kept as a unit struct rather than a free function so future stateful
/// extensions (per-prediction TTL, decoration palette) can land without
/// rippling through call sites.
#[derive(Debug, Default)]
pub struct Overlay;

impl Overlay {
    /// Paint every pending prediction in `state` onto `out`. Returns the
    /// number of cells painted so callers can flush (or skip the flush)
    /// based on whether anything was drawn.
    ///
    /// Predictions are stored in **pane-local** coordinates (0-based within
    /// the focused pane's grid). `origin` is that pane's outer-viewport
    /// top-left `(x, y)`; every prediction's CUP is shifted by it so a pane
    /// offset from the viewport origin (any split that isn't the top-left
    /// leaf) paints its echo over the pane's real cells rather than at the
    /// viewport-absolute coordinate — the mid-screen ghost echo otherwise
    /// (phux-7ry0). Single-pane callers pass `(0, 0)`.
    ///
    /// The cursor position after this call is left at the end of the
    /// last painted cell; the renderer's next pass will reposition.
    /// On an empty queue this is a no-op (no bytes written, no flush).
    #[allow(
        clippy::unused_self,
        reason = "ZST today; reserved as the natural attach point for future per-overlay state (TTL, decoration palette)"
    )]
    pub fn render(
        &self,
        state: &PredictionState,
        origin: (u16, u16),
        out: &mut impl Write,
    ) -> io::Result<usize> {
        let (ox, oy) = origin;
        let mut count = 0;
        for p in state.pending() {
            // Pure cursor-motion predictions paint no cell. Reconcile
            // consumes them when the authoritative cursor catches up.
            // (Newline = Enter at EOL; CursorLeft/Right = arrow over a
            // known cell on the current line, phux-9gw.1.3.)
            if matches!(
                p.kind,
                PredictionKind::Newline | PredictionKind::CursorLeft | PredictionKind::CursorRight
            ) {
                continue;
            }
            write_cup(out, p.row.saturating_add(oy), p.col.saturating_add(ox))?;
            // Reset → underline. We don't merge with the renderer's SGR
            // because we paint after it; what we emit here is a fresh
            // SGR scope owned by the prediction layer.
            out.write_all(b"\x1b[0m\x1b[4m")?;
            // `text` is the full grapheme cluster — a single scalar in the
            // common case, but a flag emoji, ZWJ sequence, or base plus
            // combining marks span several scalars (phux-9gw.1.6). Emit
            // the whole cluster so the painted cell matches the prediction.
            out.write_all(p.text.as_bytes())?;
            // Reset after each cell so a partial overlay never leaks
            // underline into adjacent authoritative cells if the next
            // render skips them.
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
    // 1-indexed per VT100 CUP.
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

    #[test]
    fn single_prediction_emits_cup_underline_char_reset() {
        let mut state = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        state.predict_key(&key_text("z"));
        let mut buf = Vec::new();
        let n = Overlay.render(&state, (0, 0), &mut buf).expect("render");
        assert_eq!(n, 1);
        let s = String::from_utf8(buf).expect("utf8");
        // CUP at row 1, col 1 (0-indexed → 1-indexed for VT100).
        assert!(s.contains("\x1b[1;1H"));
        // Underline turn-on, then the char, then the final reset.
        assert!(s.contains("\x1b[4m"));
        assert!(s.contains('z'));
        assert!(s.ends_with("\x1b[0m"));
    }

    #[test]
    fn run_of_predictions_writes_in_order() {
        let mut state = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        for ch in ["h", "i"] {
            state.predict_key(&key_text(ch));
        }
        let mut buf = Vec::new();
        let n = Overlay.render(&state, (0, 0), &mut buf).expect("render");
        assert_eq!(n, 2);
        let s = String::from_utf8(buf).expect("utf8");
        let h_pos = s.find('h').expect("h painted");
        let i_pos = s.find('i').expect("i painted");
        assert!(h_pos < i_pos, "predictions painted left-to-right");
        // Second prediction lands at col 2 (1-indexed: "\x1b[1;2H").
        assert!(s.contains("\x1b[1;2H"));
    }

    /// phux-7ry0 regression: predictions are pane-local; a non-origin pane
    /// (e.g. the bottom leaf of a horizontal split) must paint its echo
    /// shifted by the pane's outer-viewport origin, NOT at the pane-local
    /// coordinate. Before the fix the overlay painted at the raw pane-local
    /// (or, symmetrically, a mid-screen-clamped) position — the ghost echo.
    #[test]
    fn origin_offsets_every_prediction_cup() {
        let mut state = PredictionState::new(PredictiveConfig::enabled(), 80, 12);
        // First insert anchors at pane-local (0, 0).
        state.predict_key(&key_text("a"));
        state.predict_key(&key_text("b"));
        let mut buf = Vec::new();
        // Bottom pane of a 24-row split sits at outer origin (x=0, y=13).
        let n = Overlay.render(&state, (0, 13), &mut buf).expect("render");
        assert_eq!(n, 2);
        let s = String::from_utf8(buf).expect("utf8");
        // Pane-local (0,0)+origin(0,13) ⇒ 1-based CUP row 14, col 1.
        assert!(
            s.contains("\x1b[14;1H"),
            "first echo must land at the pane origin row, not the viewport top; out = {s:?}"
        );
        // Pane-local (0,1)+origin ⇒ row 14, col 2.
        assert!(
            s.contains("\x1b[14;2H"),
            "second echo off-by-origin; out = {s:?}"
        );
        // It must NOT paint at the viewport-absolute pane-local row 1.
        assert!(
            !s.contains("\x1b[1;1H"),
            "echo leaked to the viewport top (the ghost); out = {s:?}"
        );
    }
}
