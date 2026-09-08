//! Shared per-cell SGR emission for the ratatui-backed chrome layers.
//!
//! Both the overlay painter ([`super::overlay`]) and the status bar
//! ([`super::chrome::status_bar`]) walk a ratatui [`ratatui::buffer::Buffer`] and emit one
//! cell at a time. This module owns the per-cell SGR delta so the two
//! paths style identically. It lives under `render/` so the ratatui
//! dependency stays within the ADR-0020 boundary.

use std::io::{self, Write};

use ratatui::style::{Color, Modifier};

/// Emit the SGR for one ratatui cell.
///
/// Emit only at style boundaries. Filled sidebar and modal rows are long
/// runs of identical styles; resetting and resending truecolor for every
/// cell costs far more bytes than the text itself. Callers start with `None`
/// after a hard reset and reset again at the end of each independently
/// positioned run. A transition to plain text still clears the old style.
pub(super) fn emit_cell_sgr(
    out: &mut impl Write,
    cell: &ratatui::buffer::Cell,
    prev_styled: &mut Option<(Color, Color, Modifier)>,
) -> io::Result<()> {
    let style = (cell.fg, cell.bg, cell.modifier);
    if *prev_styled == Some(style) {
        return Ok(());
    }
    let styled = cell_is_styled(cell);
    if !styled && prev_styled.is_none() {
        return Ok(());
    }
    out.write_all(b"\x1b[0m")?;
    *prev_styled = Some(style);
    if !styled {
        return Ok(());
    }
    let mut params = SgrParams::new(out);
    params.modifiers(cell.modifier)?;
    params.color(cell.fg, true)?;
    params.color(cell.bg, false)?;
    params.finish()
}

/// Whether a cell carries any styling at all: a non-empty modifier set, or a
/// foreground/background that overrides the terminal default.
fn cell_is_styled(cell: &ratatui::buffer::Cell) -> bool {
    cell.modifier != Modifier::empty()
        || !matches!(cell.fg, Color::Reset)
        || !matches!(cell.bg, Color::Reset)
}

/// The ratatui modifier bits this chrome renders, each paired with the SGR
/// parameter that carries it, in the order they are emitted.
const MODIFIER_CODES: [(Modifier, &[u8]); 5] = [
    (Modifier::BOLD, b"1"),
    (Modifier::DIM, b"2"),
    (Modifier::ITALIC, b"3"),
    (Modifier::UNDERLINED, b"4"),
    (Modifier::REVERSED, b"7"),
];

/// Accumulator for one `\x1b[<params>m` sequence.
///
/// Writes the `\x1b[` introducer lazily with the first parameter and a `;`
/// before every one after it, so a cell that contributes no parameter emits
/// nothing at all. Borrows the sink instead of buffering: this runs per
/// changed cell and has to stay allocation-free.
struct SgrParams<'w, W: Write> {
    out: &'w mut W,
    wrote_any: bool,
}

impl<'w, W: Write> SgrParams<'w, W> {
    const fn new(out: &'w mut W) -> Self {
        Self {
            out,
            wrote_any: false,
        }
    }

    /// Open the sequence, or separate this parameter from the previous one.
    fn separator(&mut self) -> io::Result<()> {
        if self.wrote_any {
            return self.out.write_all(b";");
        }
        self.out.write_all(b"\x1b[")?;
        self.wrote_any = true;
        Ok(())
    }

    /// Emit one parameter per set modifier bit, in [`MODIFIER_CODES`] order.
    fn modifiers(&mut self, m: Modifier) -> io::Result<()> {
        for (bit, code) in MODIFIER_CODES {
            if m.contains(bit) {
                self.separator()?;
                self.out.write_all(code)?;
            }
        }
        Ok(())
    }

    /// Emit the 24-bit foreground (`fg`) or background parameter for `color`,
    /// or nothing when it does not override the terminal default.
    fn color(&mut self, color: Color, fg: bool) -> io::Result<()> {
        let Some((kind, r, g, b)) = color_rgb(color, fg) else {
            return Ok(());
        };
        self.separator()?;
        write!(self.out, "{kind};2;{r};{g};{b}")
    }

    /// Close the sequence with the `m` terminator; a no-op when the cell
    /// contributed no parameter.
    fn finish(self) -> io::Result<()> {
        if self.wrote_any {
            self.out.write_all(b"m")
        } else {
            Ok(())
        }
    }
}

/// Emit a standalone foreground (`fg = true`) or background SGR for `color`.
///
/// For chrome painted outside the ratatui-buffer path (e.g. the copy-mode
/// status strip the driver writes directly). Unlike `color_rgb`, indexed
/// colors are preserved as `38;5;n` / `48;5;n` rather than flattened, so a
/// theme's `Indexed(240)` renders as the terminal's palette entry 240.
/// `Color::Reset` emits nothing (the caller's prior `\x1b[0m` stands).
pub fn write_sgr_color(out: &mut impl Write, color: Color, fg: bool) -> io::Result<()> {
    let kind = if fg { 38 } else { 48 };
    match color {
        Color::Reset => Ok(()),
        Color::Indexed(n) => write!(out, "\x1b[{kind};5;{n}m"),
        other => {
            if let Some((k, r, g, b)) = color_rgb(other, fg) {
                write!(out, "\x1b[{k};2;{r};{g};{b}m")
            } else {
                Ok(())
            }
        }
    }
}

/// Convert a ratatui [`Color`] into a 24-bit SGR triple, plus the SGR
/// kind prefix (`"38"` foreground / `"48"` background). Returns `None`
/// for `Color::Reset` (no override). Indexed ANSI colors map to a small
/// fixed palette so chrome renders consistently across terminal themes.
const fn color_rgb(color: Color, fg: bool) -> Option<(&'static str, u8, u8, u8)> {
    let kind = if fg { "38" } else { "48" };
    let (r, g, b) = match color {
        Color::Reset => return None,
        Color::Black => (0, 0, 0),
        Color::Red => (170, 0, 0),
        Color::Green => (0, 170, 0),
        Color::Yellow => (170, 85, 0),
        Color::Blue => (0, 0, 170),
        Color::Magenta => (170, 0, 170),
        Color::Cyan => (0, 170, 170),
        Color::Gray => (170, 170, 170),
        Color::DarkGray => (85, 85, 85),
        Color::LightRed => (255, 85, 85),
        Color::LightGreen => (85, 255, 85),
        Color::LightYellow => (255, 255, 85),
        Color::LightBlue => (85, 85, 255),
        Color::LightMagenta => (255, 85, 255),
        Color::LightCyan => (85, 255, 255),
        Color::White => (255, 255, 255),
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(_) => (200, 200, 200),
    };
    Some((kind, r, g, b))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn sgr(color: Color, fg: bool) -> String {
        let mut out = Vec::new();
        write_sgr_color(&mut out, color, fg).expect("write");
        String::from_utf8(out).expect("utf8")
    }

    #[test]
    fn indexed_color_is_preserved_not_flattened() {
        // The reason this helper exists: the buffer path flattens Indexed to
        // a gray; here it must emit the real palette index.
        assert_eq!(sgr(Color::Indexed(240), false), "\x1b[48;5;240m");
        assert_eq!(sgr(Color::Indexed(12), true), "\x1b[38;5;12m");
    }

    #[test]
    fn rgb_and_named_emit_truecolor() {
        assert_eq!(sgr(Color::Rgb(10, 20, 30), false), "\x1b[48;2;10;20;30m");
        assert_eq!(sgr(Color::White, true), "\x1b[38;2;255;255;255m");
    }

    #[test]
    fn reset_emits_nothing() {
        assert_eq!(sgr(Color::Reset, true), "");
        assert_eq!(sgr(Color::Reset, false), "");
    }

    #[test]
    fn styled_runs_coalesce_and_a_plain_cell_resets_them() {
        let mut cell = ratatui::buffer::Cell::default();
        cell.set_style(
            ratatui::style::Style::default()
                .fg(Color::Rgb(190, 242, 100))
                .bg(Color::Rgb(23, 27, 35))
                .add_modifier(Modifier::BOLD),
        );
        let mut out = Vec::new();
        let mut previous = None;
        emit_cell_sgr(&mut out, &cell, &mut previous).expect("first style");
        let initial = out.len();
        for _ in 0..200 {
            emit_cell_sgr(&mut out, &cell, &mut previous).expect("same style");
        }
        assert_eq!(
            out.len(),
            initial,
            "a 200-cell run sends its colors only once"
        );
        cell.set_style(ratatui::style::Style::default().remove_modifier(Modifier::BOLD));
        emit_cell_sgr(&mut out, &cell, &mut previous).expect("modifier transition");
        assert!(out.len() > initial);
        let before_plain = out.len();
        emit_cell_sgr(&mut out, &ratatui::buffer::Cell::default(), &mut previous)
            .expect("plain transition");
        assert_eq!(&out[before_plain..], b"\x1b[0m");
    }
}
