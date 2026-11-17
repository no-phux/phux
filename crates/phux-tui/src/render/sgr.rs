//! Shared per-cell SGR emission for the ratatui-backed chrome layers, so
//! the overlay painter and the status bar style identically.

use std::io::{self, Write};

use ratatui::style::{Color, Modifier};

/// Emit the SGR for one cell, only at style boundaries (truecolor per cell
/// costs more than the text). Callers start from `None` after a reset.
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

/// Accumulator for one `\x1b[<params>m` sequence: the introducer is written
/// lazily, so a cell with no parameters emits nothing. Allocation-free.
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

    /// Emit the foreground (`fg`) or background parameter for `color` at
    /// its own depth, or nothing when it does not override the terminal
    /// default.
    fn color(&mut self, color: Color, fg: bool) -> io::Result<()> {
        let Some(code) = SgrColor::of(color, fg) else {
            return Ok(());
        };
        self.separator()?;
        code.write(self.out)
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

/// Emit a standalone foreground or background SGR for `color` (for chrome
/// painted outside the buffer path); `Reset` emits nothing.
pub fn write_sgr_color(out: &mut impl Write, color: Color, fg: bool) -> io::Result<()> {
    let Some(code) = SgrColor::of(color, fg) else {
        return Ok(());
    };
    out.write_all(b"\x1b[")?;
    code.write(out)?;
    out.write_all(b"m")
}

/// One colour parameter at the depth the theme chose for it: a named colour
/// stays an ANSI index (the user's palette), an indexed colour stays
/// `5;n`, and only `Rgb` is 24-bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SgrColor {
    /// `30`–`37`, `90`–`97` (or the `40`/`100` background forms).
    Ansi(u8),
    /// `38;5;n` / `48;5;n`.
    Indexed(u8, u8),
    /// `38;2;r;g;b` / `48;2;r;g;b`.
    Rgb(u8, u8, u8, u8),
}

impl SgrColor {
    const fn of(color: Color, fg: bool) -> Option<Self> {
        let kind = if fg { 38 } else { 48 };
        let base = if fg { 30 } else { 40 };
        let bright = if fg { 90 } else { 100 };
        Some(match color {
            Color::Reset => return None,
            Color::Black => Self::Ansi(base),
            Color::Red => Self::Ansi(base + 1),
            Color::Green => Self::Ansi(base + 2),
            Color::Yellow => Self::Ansi(base + 3),
            Color::Blue => Self::Ansi(base + 4),
            Color::Magenta => Self::Ansi(base + 5),
            Color::Cyan => Self::Ansi(base + 6),
            Color::Gray => Self::Ansi(base + 7),
            Color::DarkGray => Self::Ansi(bright),
            Color::LightRed => Self::Ansi(bright + 1),
            Color::LightGreen => Self::Ansi(bright + 2),
            Color::LightYellow => Self::Ansi(bright + 3),
            Color::LightBlue => Self::Ansi(bright + 4),
            Color::LightMagenta => Self::Ansi(bright + 5),
            Color::LightCyan => Self::Ansi(bright + 6),
            Color::White => Self::Ansi(bright + 7),
            Color::Indexed(n) => Self::Indexed(kind, n),
            Color::Rgb(r, g, b) => Self::Rgb(kind, r, g, b),
        })
    }

    fn write(self, out: &mut impl Write) -> io::Result<()> {
        match self {
            Self::Ansi(code) => write!(out, "{code}"),
            Self::Indexed(kind, n) => write!(out, "{kind};5;{n}"),
            Self::Rgb(kind, r, g, b) => write!(out, "{kind};2;{r};{g};{b}"),
        }
    }
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
        assert_eq!(sgr(Color::Indexed(240), false), "\x1b[48;5;240m");
        assert_eq!(sgr(Color::Indexed(12), true), "\x1b[38;5;12m");
    }

    /// The 16-colour theme speaks the user's own palette, so a named colour
    /// is an ANSI index on both paths, never a fixed 24-bit guess.
    #[test]
    fn named_colors_stay_ansi_and_rgb_stays_truecolor() {
        assert_eq!(sgr(Color::Rgb(10, 20, 30), false), "\x1b[48;2;10;20;30m");
        assert_eq!(sgr(Color::White, true), "\x1b[97m");
        assert_eq!(sgr(Color::Cyan, true), "\x1b[36m");
        assert_eq!(sgr(Color::Gray, false), "\x1b[47m");
        assert_eq!(sgr(Color::LightBlue, true), "\x1b[94m");
        assert_eq!(sgr(Color::DarkGray, false), "\x1b[100m");
        let mut cell = ratatui::buffer::Cell::default();
        cell.set_style(
            ratatui::style::Style::default()
                .fg(Color::Yellow)
                .bg(Color::Indexed(236))
                .add_modifier(Modifier::BOLD),
        );
        let mut out = Vec::new();
        emit_cell_sgr(&mut out, &cell, &mut None).expect("emit");
        assert_eq!(out, b"\x1b[0m\x1b[1;33;48;5;236m");
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
