//! Shared SGR (Select Graphic Rendition) byte encoder.
//!
//! The server's snapshot synthesizer (`phux-server`'s `grid::synthesizer`)
//! and the client's pane renderer (`phux-client`'s `attach::render`) both
//! reconstruct a libghostty [`Style`] plus its resolved foreground/background
//! as an SGR escape sequence. They are near-identical byte emitters, and they
//! drifted in lockstep: both dropped underline and overline entirely, so every
//! neovim undercurl/spell underline, `cursorline`, and powerlevel10k underlined
//! segment rendered flat after a snapshot resync. This module is the single
//! source of truth so the two ends cannot diverge again.

use std::io::Write as _;

use libghostty_vt::style::{RgbColor, Style, StyleColor, Underline};

/// Append a full SGR reset (`CSI 0 m`) followed by the parameters that
/// reproduce `style` plus the resolved `fg`/`bg`, into `out`.
///
/// The leading reset makes the emitted sequence independent of whatever pen
/// was active before it: after these bytes the receiving terminal's pen is
/// exactly `(style, fg, bg)`. Callers decide *when* to emit (delta gating on
/// the server, run coalescing on the client); this function only encodes.
///
/// `fg`/`bg` are the resolved RGB colors (sourced from
/// `CellIteration::fg_color()` / `bg_color()`), passed separately because the
/// renderers read them from the iteration rather than from `Style`'s
/// palette-indexed color fields. The underline color, by contrast, has no
/// resolved-RGB accessor, so it is emitted directly from `style.underline_color`.
pub fn write_reset_and_sgr(
    out: &mut Vec<u8>,
    style: &Style,
    fg: Option<RgbColor>,
    bg: Option<RgbColor>,
) {
    // Always reset first — keeps the parameter list independent of prior state.
    out.extend_from_slice(b"\x1b[0m");
    let mut wrote_any = false;
    write_attrs(out, style, &mut wrote_any);
    // Resolved truecolor fg/bg: the viewport renderer already resolved
    // palette/default to concrete RGB (via `CellIteration::fg_color`/`bg_color`).
    if let Some(rgb) = fg {
        sgr_sep(out, &mut wrote_any);
        let _ = write!(out, "38;2;{};{};{}", rgb.r, rgb.g, rgb.b);
    }
    if let Some(rgb) = bg {
        sgr_sep(out, &mut wrote_any);
        let _ = write!(out, "48;2;{};{};{}", rgb.r, rgb.g, rgb.b);
    }
    write_underline_color(out, style, &mut wrote_any);
    if wrote_any {
        out.push(b'm');
    }
    // else: the leading `\x1b[0m` is the whole sequence (default pen).
}

/// Like [`write_reset_and_sgr`] but sources foreground/background from the
/// `Style`'s own [`StyleColor`] fields rather than a separately-resolved
/// `RgbColor`.
///
/// Used where no resolved-RGB accessor exists: the scrollback history walk
/// reads cells via `Terminal::grid_ref` (which exposes `style()` but not the
/// render iterator's resolved colors), so palette colors are emitted as
/// `38;5;n` / `48;5;n` and the client resolves them against its own palette —
/// preserving palette semantics rather than baking a server-resolved RGB
/// (phux-q0x7). A `StyleColor::None` (default) color emits nothing; the leading
/// reset already restored the default pen. The text attributes and underline
/// color share the exact same emitters as [`write_reset_and_sgr`], so the two
/// encoders cannot drift.
pub fn write_reset_and_sgr_unresolved(out: &mut Vec<u8>, style: &Style) {
    out.extend_from_slice(b"\x1b[0m");
    let mut wrote_any = false;
    write_attrs(out, style, &mut wrote_any);
    write_style_color(out, &mut wrote_any, style.fg_color, true);
    write_style_color(out, &mut wrote_any, style.bg_color, false);
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

/// Emit the boolean / underline-style text attributes (everything except the
/// fg/bg and underline colors). Shared by both encoders so the attribute set
/// can never drift between the resolved and unresolved paths.
fn write_attrs(out: &mut Vec<u8>, style: &Style, wrote_any: &mut bool) {
    if style.bold {
        sgr_sep(out, wrote_any);
        out.push(b'1');
    }
    if style.faint {
        sgr_sep(out, wrote_any);
        out.push(b'2');
    }
    if style.italic {
        sgr_sep(out, wrote_any);
        out.push(b'3');
    }
    // Underline: plain `4` (single) and `21` (double), plus the colon
    // sub-parameter styles (`4:3` curly, `4:4` dotted, `4:5` dashed) from the
    // Kitty/ITU underline extension that libghostty parses and emits. The
    // non-`None` cases all open a parameter; only the SGR digits differ.
    if !matches!(style.underline, Underline::None) {
        sgr_sep(out, wrote_any);
        match style.underline {
            Underline::Double => out.extend_from_slice(b"21"),
            Underline::Curly => out.extend_from_slice(b"4:3"),
            Underline::Dotted => out.extend_from_slice(b"4:4"),
            Underline::Dashed => out.extend_from_slice(b"4:5"),
            // `Single` and any future `#[non_exhaustive]` variant degrade to a
            // plain single underline rather than dropping it. (`None` was
            // already excluded above.)
            _ => out.push(b'4'),
        }
    }
    if style.blink {
        sgr_sep(out, wrote_any);
        out.push(b'5');
    }
    if style.inverse {
        sgr_sep(out, wrote_any);
        out.push(b'7');
    }
    if style.invisible {
        sgr_sep(out, wrote_any);
        out.push(b'8');
    }
    if style.strikethrough {
        sgr_sep(out, wrote_any);
        out.push(b'9');
    }
    if style.overline {
        sgr_sep(out, wrote_any);
        out.extend_from_slice(b"53");
    }
}

/// Emit an SGR foreground (`is_fg == true`) or background color from a
/// [`StyleColor`]. `None` (default) emits nothing.
fn write_style_color(out: &mut Vec<u8>, wrote_any: &mut bool, color: StyleColor, is_fg: bool) {
    let base = if is_fg { 38 } else { 48 };
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

/// Emit the underline color (SGR 58) from `style.underline_color` when set so
/// colored undercurls (nvim LSP diagnostics) survive. Independent of the
/// underline-style parameter. Shared by both encoders.
fn write_underline_color(out: &mut Vec<u8>, style: &Style, wrote_any: &mut bool) {
    match style.underline_color {
        StyleColor::None => {}
        StyleColor::Palette(idx) => {
            sgr_sep(out, wrote_any);
            let _ = write!(out, "58:5:{}", idx.0);
        }
        StyleColor::Rgb(rgb) => {
            sgr_sep(out, wrote_any);
            // Empty color-space-id field per the ITU-T form: `58:2::r:g:b`.
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

    #[test]
    fn default_pen_is_just_a_reset() {
        assert_eq!(encode(&Style::default(), None, None), "\x1b[0m");
    }

    #[test]
    fn underline_styles_emit_their_sgr() {
        let curly = Style {
            underline: Underline::Curly,
            ..Style::default()
        };
        assert_eq!(encode(&curly, None, None), "\x1b[0m\x1b[4:3m");

        let single = Style {
            underline: Underline::Single,
            ..Style::default()
        };
        assert_eq!(encode(&single, None, None), "\x1b[0m\x1b[4m");

        let double = Style {
            underline: Underline::Double,
            ..Style::default()
        };
        assert_eq!(encode(&double, None, None), "\x1b[0m\x1b[21m");
    }

    fn encode_unresolved(style: &Style) -> String {
        let mut out = Vec::new();
        write_reset_and_sgr_unresolved(&mut out, style);
        String::from_utf8(out).expect("ascii")
    }

    #[test]
    fn unresolved_default_pen_is_just_a_reset() {
        assert_eq!(encode_unresolved(&Style::default()), "\x1b[0m");
    }

    #[test]
    fn unresolved_emits_palette_fg_bg_as_indexed() {
        // phux-q0x7: a palette-colored history cell keeps its palette index
        // (38;5;n / 48;5;n) rather than a server-resolved truecolor.
        let s = Style {
            fg_color: StyleColor::Palette(PaletteIndex(31)),
            bg_color: StyleColor::Palette(PaletteIndex(236)),
            ..Style::default()
        };
        assert_eq!(encode_unresolved(&s), "\x1b[0m\x1b[38;5;31;48;5;236m");
    }

    #[test]
    fn unresolved_emits_rgb_and_attrs() {
        let s = Style {
            bold: true,
            fg_color: StyleColor::Rgb(RgbColor { r: 1, g: 2, b: 3 }),
            ..Style::default()
        };
        assert_eq!(encode_unresolved(&s), "\x1b[0m\x1b[1;38;2;1;2;3m");
    }

    #[test]
    fn unresolved_matches_resolved_for_attrs_only() {
        // Attribute-only styles must encode identically on both paths (shared
        // `write_attrs`), so the two encoders cannot drift.
        let s = Style {
            bold: true,
            italic: true,
            underline: Underline::Curly,
            overline: true,
            ..Style::default()
        };
        assert_eq!(encode_unresolved(&s), encode(&s, None, None));
    }

    #[test]
    fn overline_emits_sgr_53() {
        let over = Style {
            overline: true,
            ..Style::default()
        };
        assert_eq!(encode(&over, None, None), "\x1b[0m\x1b[53m");
    }

    #[test]
    fn colors_and_attrs_combine_with_semicolons() {
        let style = Style {
            bold: true,
            underline: Underline::Single,
            ..Style::default()
        };
        let fg = Some(RgbColor { r: 1, g: 2, b: 3 });
        let bg = Some(RgbColor {
            r: 10,
            g: 20,
            b: 30,
        });
        assert_eq!(
            encode(&style, fg, bg),
            "\x1b[0m\x1b[1;4;38;2;1;2;3;48;2;10;20;30m"
        );
    }

    #[test]
    fn underline_color_palette_and_rgb() {
        let pal = Style {
            underline: Underline::Curly,
            underline_color: StyleColor::Palette(PaletteIndex::RED),
            ..Style::default()
        };
        // `4:3` curly + `58:5:<idx>` underline color.
        assert!(encode(&pal, None, None).contains("58:5:"));

        let rgb = Style {
            underline: Underline::Curly,
            underline_color: StyleColor::Rgb(RgbColor { r: 7, g: 8, b: 9 }),
            ..Style::default()
        };
        assert_eq!(encode(&rgb, None, None), "\x1b[0m\x1b[4:3;58:2::7:8:9m");
    }
}
