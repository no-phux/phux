//! The one projection of a libghostty cell style into phux-core's plain-data
//! [`CellStyle`], shared by the client renderer, the server synthesizer, and
//! the recorder. Each keeps its own cell walk; only the style mapping is
//! common.

use libghostty_vt::{
    color::Rgb,
    style::{Style, StyleColor, Underline},
};
use phux_core::screen::{CellColor, CellStyle};

/// Project a cell's `(Style, resolved fg, resolved bg)` into a [`CellStyle`].
#[inline]
#[must_use]
pub fn cell_style(style: &Style, fg: Option<Rgb>, bg: Option<Rgb>) -> CellStyle {
    CellStyle {
        bold: style.bold,
        faint: style.faint,
        italic: style.italic,
        underline: !matches!(style.underline, Underline::None),
        blink: style.blink,
        inverse: style.inverse,
        invisible: style.invisible,
        strikethrough: style.strikethrough,
        overline: style.overline,
        fg: cell_color(fg, style.fg_color),
        bg: cell_color(bg, style.bg_color),
    }
}

/// Project a cell color to [`CellColor`].
///
/// The explicit per-cell [`StyleColor`] wins so a palette index keeps its
/// identity (re-themed terminals and per-theme exports depend on it), then
/// the resolved RGB, else [`CellColor::Default`].
#[inline]
#[must_use]
pub fn cell_color(resolved: Option<Rgb>, raw: StyleColor) -> CellColor {
    match raw {
        StyleColor::Palette(index) => CellColor::Palette { index: index.0 },
        StyleColor::Rgb(rgb) => rgb_color(rgb),
        StyleColor::None => resolved.map_or(CellColor::Default, rgb_color),
    }
}

#[inline]
const fn rgb_color(rgb: Rgb) -> CellColor {
    CellColor::Rgb {
        r: rgb.r,
        g: rgb.g,
        b: rgb.b,
    }
}

#[cfg(test)]
mod tests {
    use libghostty_vt::color::PaletteIndex;

    use super::*;

    const RED: Rgb = Rgb { r: 255, g: 0, b: 0 };

    #[test]
    fn palette_index_keeps_its_identity_over_a_resolved_rgb() {
        let color = cell_color(Some(RED), StyleColor::Palette(PaletteIndex(196)));
        assert_eq!(color, CellColor::Palette { index: 196 });
    }

    #[test]
    fn explicit_rgb_wins_over_the_resolved_rgb() {
        let explicit = Rgb { r: 1, g: 2, b: 3 };
        let color = cell_color(Some(RED), StyleColor::Rgb(explicit));
        assert_eq!(color, CellColor::Rgb { r: 1, g: 2, b: 3 });
    }

    #[test]
    fn unset_color_falls_back_to_resolved_then_default() {
        assert_eq!(
            cell_color(Some(RED), StyleColor::None),
            CellColor::Rgb { r: 255, g: 0, b: 0 }
        );
        assert_eq!(cell_color(None, StyleColor::None), CellColor::Default);
    }

    #[test]
    fn default_style_projects_to_the_default_cell_style() {
        assert_eq!(
            cell_style(&Style::default(), None, None),
            CellStyle::default()
        );
    }
}
