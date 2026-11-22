//! Emission: a frame's chrome cells as positioned, style-coalesced VT runs
//! between SGR resets.

use std::io::{self, Write};

use ratatui::style::{Color, Modifier, Style};

use super::cells::ChromeCell;

/// Emit `cells` as positioned VT: consecutive same-style cells in a row are
/// one CUP + SGR run; a gap, style change, or empty symbol re-anchors.
pub(super) fn emit_cells<W: Write>(out: &mut W, cells: &[ChromeCell]) -> io::Result<()> {
    out.write_all(b"\x1b[0m")?;
    let mut style: Option<Style> = None;
    // The column the current run would continue at; `None` = no open run.
    let mut run: Option<(u16, u16)> = None;
    for cell in cells {
        if cell.sym.is_empty() {
            run = None;
            continue;
        }
        if style != Some(cell.style) {
            write_style(out, cell.style)?;
            style = Some(cell.style);
            run = None;
        }
        if run != Some((cell.y, cell.x)) {
            // CUP is 1-based.
            write!(
                out,
                "\x1b[{};{}H",
                cell.y.saturating_add(1),
                cell.x.saturating_add(1)
            )?;
        }
        out.write_all(cell.sym.as_str().as_bytes())?;
        run = Some((cell.y, cell.x.saturating_add(1)));
    }
    // Trailing reset so the next layer (status bar, focused pane render)
    // doesn't inherit any chrome SGR.
    out.write_all(b"\x1b[0m")?;
    out.flush()
}

/// Emit the SGR for one chrome cell: a full reset, then bold, then the
/// foreground and any ground fill. The reset is what makes a run boundary
/// cheap to reason about — no attribute can survive from the previous run.
fn write_style<W: Write>(out: &mut W, style: Style) -> io::Result<()> {
    out.write_all(b"\x1b[0m")?;
    if style.add_modifier.contains(Modifier::BOLD) {
        out.write_all(b"\x1b[1m")?;
    }
    if let Some(color) = style.fg {
        crate::render::sgr::write_sgr_color(out, color, true)?;
    }
    match style.bg {
        None | Some(Color::Reset) => Ok(()),
        Some(color) => crate::render::sgr::write_sgr_color(out, color, false),
    }
}
