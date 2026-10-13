//! Turn a composited [`RenderedFrame`] into VT bytes a cast can replay.
//!
//! The interactive `--rec` tee records the bytes the glass was sent. A
//! headless composited capture has no glass, so it emits one full redraw
//! of the same cells: clear, then each row with SGR, then the cursor.
//! Wide-glyph tails are empty and are not written; the leading cluster
//! advances the cursor across them.

use std::io::Write as _;

use phux_core::screen::{CellColor, CellStyle, RenderedFrame};

/// Serialize `frame` as a full-screen redraw.
pub(super) fn frame_to_vt(frame: &RenderedFrame) -> Vec<u8> {
    let mut out = Vec::new();
    // Hide the cursor for the redraw, then place it once the cells are down.
    out.extend_from_slice(b"\x1b[?25l\x1b[2J\x1b[H");
    let mut pen = CellStyle::default();
    let mut pen_set = false;
    for row in 0..frame.rows {
        write_cup(&mut out, row, 0);
        for col in 0..frame.cols {
            let Some(cell) = frame.cell(row, col) else {
                break;
            };
            // The right half of a wide glyph. The lead cluster already
            // advanced the cursor.
            if cell.grapheme.is_empty() {
                continue;
            }
            if !pen_set || pen != cell.style {
                write_sgr(&mut out, &cell.style);
                pen = cell.style;
                pen_set = true;
            }
            out.extend_from_slice(cell.grapheme.as_bytes());
        }
    }
    out.extend_from_slice(b"\x1b[0m");
    if let Some(cursor) = &frame.cursor {
        write_cup(&mut out, cursor.y, cursor.x);
        if cursor.visible {
            out.extend_from_slice(b"\x1b[?25h");
        }
    }
    out
}

/// CUP is 1-based.
fn write_cup(out: &mut Vec<u8>, row: u16, col: u16) {
    let _ = write!(
        out,
        "\x1b[{};{}H",
        row.saturating_add(1),
        col.saturating_add(1)
    );
}

fn write_sgr(out: &mut Vec<u8>, style: &CellStyle) {
    out.extend_from_slice(b"\x1b[0");
    if style.bold {
        out.extend_from_slice(b";1");
    }
    if style.faint {
        out.extend_from_slice(b";2");
    }
    if style.italic {
        out.extend_from_slice(b";3");
    }
    if style.underline {
        out.extend_from_slice(b";4");
    }
    if style.blink {
        out.extend_from_slice(b";5");
    }
    if style.inverse {
        out.extend_from_slice(b";7");
    }
    if style.invisible {
        out.extend_from_slice(b";8");
    }
    if style.strikethrough {
        out.extend_from_slice(b";9");
    }
    if style.overline {
        out.extend_from_slice(b";53");
    }
    write_color(out, style.fg, true);
    write_color(out, style.bg, false);
    out.push(b'm');
}

fn write_color(out: &mut Vec<u8>, color: CellColor, foreground: bool) {
    match color {
        CellColor::Default => {}
        CellColor::Palette { index } if index < 8 => {
            let base: u8 = if foreground { 30 } else { 40 };
            push_param(out, u16::from(base.saturating_add(index)));
        }
        CellColor::Palette { index } if index < 16 => {
            let base: u8 = if foreground { 90 } else { 100 };
            push_param(out, u16::from(base.saturating_add(index - 8)));
        }
        CellColor::Palette { index } => {
            push_param(out, if foreground { 38 } else { 48 });
            out.extend_from_slice(b";5;");
            let _ = write!(out, "{index}");
        }
        CellColor::Rgb { r, g, b } => {
            push_param(out, if foreground { 38 } else { 48 });
            let _ = write!(out, ";2;{r};{g};{b}");
        }
    }
}

fn push_param(out: &mut Vec<u8>, value: u16) {
    out.push(b';');
    let _ = write!(out, "{value}");
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;
    use phux_core::screen::{CellColor, CursorState, RenderedCell};

    #[test]
    fn a_frame_round_trips_its_text_and_places_the_cursor() {
        let mut frame = RenderedFrame::blank(8, 2);
        frame.cell_mut(0, 0).unwrap().grapheme = "h".to_owned();
        frame.cell_mut(0, 1).unwrap().style = CellStyle {
            bold: true,
            fg: CellColor::Rgb { r: 1, g: 2, b: 3 },
            ..CellStyle::default()
        };
        frame.cell_mut(0, 1).unwrap().grapheme = "i".to_owned();
        frame.cursor = Some(CursorState {
            x: 2,
            y: 0,
            visible: true,
        });
        let vt = frame_to_vt(&frame);
        let text = String::from_utf8(vt).unwrap();
        // The two glyphs differ in style, so an SGR sits between them.
        let h = text.find('h').expect("lead glyph");
        let i = text[h..].find('i').expect("second glyph");
        assert!(i > 0, "{text:?}");
        assert!(text.contains("\x1b[2J"), "a redraw starts by clearing");
        assert!(text.contains("\x1b[1;3H"), "cursor is 1-based at col 3");
        assert!(text.contains("38;2;1;2;3"), "{text:?}");
    }

    #[test]
    fn an_empty_wide_tail_is_not_written_as_a_character() {
        let mut frame = RenderedFrame::blank(4, 1);
        frame.cells[0] = RenderedCell {
            grapheme: "你".to_owned(),
            style: CellStyle::default(),
        };
        frame.cells[1].grapheme.clear();
        let text = String::from_utf8(frame_to_vt(&frame)).unwrap();
        assert!(text.contains('你'));
        assert_eq!(text.matches('你').count(), 1);
    }
}
