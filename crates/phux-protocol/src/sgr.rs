//! Shared SGR byte encoder: the one source of truth for the server's
//! snapshot synthesizer and the client's pane renderer, so their underline,
//! overline, and color output cannot drift apart again.

use std::io::Write as _;

use libghostty_vt::style::{RgbColor, Style, StyleColor, Underline};

/// Append `CSI 0 m` plus the SGR parameters for `style` and resolved colors.
///
/// `fg`/`bg` come from `CellIteration::fg_color()` / `bg_color()`. The
/// leading reset makes the pen exactly `(style, fg, bg)` whatever came before.
pub fn write_reset_and_sgr(
    out: &mut Vec<u8>,
    style: &Style,
    fg: Option<RgbColor>,
    bg: Option<RgbColor>,
) {
    write_sgr(
        out,
        style,
        fg.map_or(StyleColor::None, StyleColor::Rgb),
        bg.map_or(StyleColor::None, StyleColor::Rgb),
    );
}

/// Like [`write_reset_and_sgr`], but with the `Style`'s own palette-indexed
/// colors (the history walk has no resolved-RGB accessor), so the client
/// resolves palette colors against its own palette.
pub fn write_reset_and_sgr_unresolved(out: &mut Vec<u8>, style: &Style) {
    write_sgr(out, style, style.fg_color, style.bg_color);
}

fn write_sgr(out: &mut Vec<u8>, style: &Style, fg: StyleColor, bg: StyleColor) {
    out.extend_from_slice(b"\x1b[0m");
    let mut wrote_any = false;
    write_attrs(out, style, &mut wrote_any);
    write_style_color(out, &mut wrote_any, fg, 38);
    write_style_color(out, &mut wrote_any, bg, 48);
    write_underline_color(out, style, &mut wrote_any);
    if wrote_any {
        out.push(b'm');
    }
}

/// Open `CSI` on the first parameter, emit `;` between subsequent ones.
fn sgr_sep(out: &mut Vec<u8>, wrote: &mut bool) {
    if *wrote {
        out.push(b';');
    } else {
        out.extend_from_slice(b"\x1b[");
        *wrote = true;
    }
}

/// Emit the text attributes (everything but colors), in SGR number order.
fn write_attrs(out: &mut Vec<u8>, style: &Style, wrote_any: &mut bool) {
    let underline: &[u8] = match style.underline {
        Underline::None => b"",
        Underline::Double => b"21",
        Underline::Curly => b"4:3",
        Underline::Dotted => b"4:4",
        Underline::Dashed => b"4:5",
        // `Single` and any future variant degrade to a plain underline.
        _ => b"4",
    };
    let attrs: [(bool, &[u8]); 9] = [
        (style.bold, b"1"),
        (style.faint, b"2"),
        (style.italic, b"3"),
        (!underline.is_empty(), underline),
        (style.blink, b"5"),
        (style.inverse, b"7"),
        (style.invisible, b"8"),
        (style.strikethrough, b"9"),
        (style.overline, b"53"),
    ];
    for (_, param) in attrs.into_iter().filter(|(on, _)| *on) {
        sgr_sep(out, wrote_any);
        out.extend_from_slice(param);
    }
}

/// Emit an SGR foreground (`base` 38) or background (`base` 48) color;
/// `None` (default) emits nothing.
fn write_style_color(out: &mut Vec<u8>, wrote_any: &mut bool, color: StyleColor, base: u8) {
    match color {
        StyleColor::None => {}
        StyleColor::Palette(idx) => {
            sgr_sep(out, wrote_any);
            let _ = write!(out, "{base};5;{}", idx.0);
        }
        StyleColor::Rgb(rgb) => {
            sgr_sep(out, wrote_any);
            let _ = write!(out, "{base};2;{};{};{}", rgb.r, rgb.g, rgb.b);
        }
    }
}

/// Emit the underline color (SGR 58) so colored undercurls survive.
fn write_underline_color(out: &mut Vec<u8>, style: &Style, wrote_any: &mut bool) {
    match style.underline_color {
        StyleColor::None => {}
        StyleColor::Palette(idx) => {
            sgr_sep(out, wrote_any);
            let _ = write!(out, "58:5:{}", idx.0);
        }
        StyleColor::Rgb(rgb) => {
            sgr_sep(out, wrote_any);
            // ITU-T form with an empty color-space id: `58:2::r:g:b`.
            let _ = write!(out, "58:2::{}:{}:{}", rgb.r, rgb.g, rgb.b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::style::PaletteIndex;

    fn encode(style: &Style, fg: Option<RgbColor>, bg: Option<RgbColor>) -> String {
        let mut out = Vec::new();
        write_reset_and_sgr(&mut out, style, fg, bg);
        String::from_utf8(out).expect("ascii")
    }

    fn encode_unresolved(style: &Style) -> String {
        let mut out = Vec::new();
        write_reset_and_sgr_unresolved(&mut out, style);
        String::from_utf8(out).expect("ascii")
    }

    fn with(edit: impl FnOnce(&mut Style)) -> Style {
        let mut style = Style::default();
        edit(&mut style);
        style
    }

    #[test]
    fn resolved_encoding_table() {
        let rgb = |r, g, b| Some(RgbColor { r, g, b });
        let cases = [
            (Style::default(), None, None, "\x1b[0m"),
            (
                with(|s| s.underline = Underline::Curly),
                None,
                None,
                "\x1b[0m\x1b[4:3m",
            ),
            (
                with(|s| s.underline = Underline::Single),
                None,
                None,
                "\x1b[0m\x1b[4m",
            ),
            (
                with(|s| s.underline = Underline::Double),
                None,
                None,
                "\x1b[0m\x1b[21m",
            ),
            (with(|s| s.overline = true), None, None, "\x1b[0m\x1b[53m"),
            (
                with(|s| {
                    s.bold = true;
                    s.underline = Underline::Single;
                }),
                rgb(1, 2, 3),
                rgb(10, 20, 30),
                "\x1b[0m\x1b[1;4;38;2;1;2;3;48;2;10;20;30m",
            ),
            (
                with(|s| {
                    s.underline = Underline::Curly;
                    s.underline_color = StyleColor::Rgb(RgbColor { r: 7, g: 8, b: 9 });
                }),
                None,
                None,
                "\x1b[0m\x1b[4:3;58:2::7:8:9m",
            ),
            (
                with(|s| s.underline_color = StyleColor::Palette(PaletteIndex(1))),
                None,
                None,
                "\x1b[0m\x1b[58:5:1m",
            ),
        ];
        for (style, fg, bg, want) in cases {
            assert_eq!(encode(&style, fg, bg), want, "{style:?}");
        }
    }

    #[test]
    fn unresolved_keeps_palette_indices_and_shares_attrs() {
        let palette = with(|s| {
            s.fg_color = StyleColor::Palette(PaletteIndex(31));
            s.bg_color = StyleColor::Palette(PaletteIndex(236));
        });
        assert_eq!(encode_unresolved(&palette), "\x1b[0m\x1b[38;5;31;48;5;236m");
        let bold_rgb = with(|s| {
            s.bold = true;
            s.fg_color = StyleColor::Rgb(RgbColor { r: 1, g: 2, b: 3 });
        });
        assert_eq!(encode_unresolved(&bold_rgb), "\x1b[0m\x1b[1;38;2;1;2;3m");
        assert_eq!(encode_unresolved(&Style::default()), "\x1b[0m");
        let attrs = with(|s| {
            s.bold = true;
            s.italic = true;
            s.underline = Underline::Curly;
            s.overline = true;
        });
        assert_eq!(encode_unresolved(&attrs), encode(&attrs, None, None));
    }
}
