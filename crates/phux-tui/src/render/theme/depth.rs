//! Fitting solved roles to the terminal's colour depth (spec §4.2).
//!
//! - 256 colours: the nearest xterm entry in 16–255 by squared sRGB
//!   distance, then re-checked against the bar on the quantised surface;
//!   a miss steps toward the mode's extreme until it clears.
//! - 16 colours: a semantic map per background, never ANSI 8 on dark;
//!   selection is reverse video.
//! - Mono: every slot `Reset`; emphasis is bold and reverse video only.

use phux_design::solve::{mix, worst};
use phux_design::{Mode, Params, Rgb8};
use ratatui::style::Color;

use super::Theme;

/// The six levels of each xterm cube axis.
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The text floor every chrome slot carries on `surface` (`theme/mod.rs`).
const TEXT_BAR: f64 = 4.5;

/// The sRGB value of xterm palette entry `index` (16–255).
#[must_use]
pub fn xterm_rgb(index: u8) -> Rgb8 {
    if index >= 232 {
        let v = 8 + (index - 232) * 10;
        return Rgb8 { r: v, g: v, b: v };
    }
    let i = index.saturating_sub(16);
    Rgb8 {
        r: CUBE[usize::from(i / 36)],
        g: CUBE[usize::from((i / 6) % 6)],
        b: CUBE[usize::from(i % 6)],
    }
}

fn distance(a: Rgb8, b: Rgb8) -> u32 {
    let d = |x: u8, y: u8| u32::from(x.abs_diff(y)).pow(2);
    d(a.r, b.r) + d(a.g, b.g) + d(a.b, b.b)
}

/// The xterm entry in 16–255 nearest `c`; ties keep the lower index.
#[must_use]
pub fn nearest_256(c: Rgb8) -> u8 {
    (16..=255_u8)
        .min_by_key(|&i| distance(c, xterm_rgb(i)))
        .unwrap_or(16)
}

/// `c` snapped to the palette, stepping toward `extreme` until it reaches
/// `bar` against every one of `under` (each already snapped). The bar is
/// never asked above what the 24-bit colour itself made.
fn reach_256(c: Rgb8, extreme: Rgb8, bar: f64, under: &[Rgb8]) -> u8 {
    let lut = Params::house().srgb_lut;
    let worst = |x: Rgb8| worst(x, under, lut);
    let target = bar.min(worst(c));
    let mut index = nearest_256(c);
    for step in 1..=10_u8 {
        if worst(xterm_rgb(index)) >= target {
            break;
        }
        index = nearest_256(mix(c, extreme, f64::from(step) / 10.0));
    }
    index
}

const fn rgb_of(color: Color) -> Option<Rgb8> {
    match color {
        Color::Rgb(r, g, b) => Some(Rgb8 { r, g, b }),
        _ => None,
    }
}

/// Every 24-bit slot of `theme` on the 256 palette. Fills snap to nearest;
/// text and rules keep their bar on the snapped surface (and on the real
/// background, when the roles were solved against it).
pub(super) fn to_256(theme: &mut Theme, mode: Mode, ground: Option<Rgb8>) {
    let snap = |c: Color| rgb_of(c).map_or(c, |rgb| Color::Indexed(nearest_256(rgb)));
    let surface = rgb_of(theme.surface).map(|s| xterm_rgb(nearest_256(s)));
    let selection = rgb_of(theme.selection_bg).map(|s| xterm_rgb(nearest_256(s)));
    let under: Vec<Rgb8> = surface.into_iter().chain(ground).collect();
    let extreme = mode.extreme();
    let ink = |c: Color, under: &[Rgb8]| match rgb_of(c) {
        Some(rgb) if !under.is_empty() => Color::Indexed(reach_256(rgb, extreme, TEXT_BAR, under)),
        _ => snap(c),
    };
    let selection_fg = ink(
        theme.selection_fg,
        &selection.into_iter().collect::<Vec<_>>(),
    );
    for (key, slot) in theme.slots_mut() {
        *slot = match key {
            "surface" | "selection_bg" | "shadow" => snap(*slot),
            "selection_fg" => selection_fg,
            _ => ink(*slot, &under),
        };
    }
}

/// Any identity colour (a `[[sessions]]` or host colour) at `depth` 256.
#[must_use]
pub fn identity_256(color: Color) -> Color {
    rgb_of(color).map_or(color, |rgb| Color::Indexed(nearest_256(rgb)))
}

/// The 16-colour semantic map. `ground_dark` is a known dark background
/// (`dim` may then use ANSI 7); light grounds use ANSI 8 for faint only.
pub(super) fn ansi16(theme: &mut Theme, mode: Mode, ground_known: bool) {
    let dark = mode == Mode::Dark;
    let dim = if dark && ground_known {
        Color::Gray
    } else {
        Color::Reset
    };
    let faint = if dark { Color::Gray } else { Color::DarkGray };
    let working = if dark { Color::LightBlue } else { Color::Blue };
    for (key, slot) in theme.slots_mut() {
        *slot = match key {
            "dim" | "section_header" | "pane_title" => dim,
            "sidebar_section" | "border" | "divider" | "agent_idle" => faint,
            "accent" | "action" | "divider_focus" | "pane_title_focus" => Color::Cyan,
            "attention" | "agent_blocked" => Color::Yellow,
            "agent_working" => working,
            "agent_done" => Color::Green,
            "agent_failed" | "error" => Color::Red,
            // text, title, chord, surface, shadow and the selection pair:
            // the terminal's own. No ANSI fill is safe under every ink on
            // every palette, so a selection is reverse video instead
            // ([`Theme::on_selection`]).
            _ => Color::Reset,
        };
    }
}

/// `NO_COLOR`: nothing but the terminal's default pair.
pub(super) fn mono(theme: &mut Theme) {
    for (_, slot) in theme.slots_mut() {
        *slot = Color::Reset;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_design::solve::contrast;

    #[test]
    fn the_palette_and_its_nearest_entry() {
        assert_eq!(xterm_rgb(16), Rgb8::hex(0x00_0000));
        assert_eq!(xterm_rgb(231), Rgb8::hex(0xff_ffff));
        assert_eq!(xterm_rgb(196), Rgb8::hex(0xff_0000));
        assert_eq!(xterm_rgb(232), Rgb8::hex(0x08_0808));
        assert_eq!(xterm_rgb(255), Rgb8::hex(0xee_eeee));
        for i in 16..=255_u8 {
            assert!(
                nearest_256(xterm_rgb(i)) <= i,
                "an entry maps to itself or an identical lower one: {i}"
            );
            assert_eq!(xterm_rgb(nearest_256(xterm_rgb(i))), xterm_rgb(i));
        }
        assert!((16..=255).contains(&nearest_256(Rgb8::hex(0x12_3456))));
    }

    /// A colour whose nearest entry loses the bar steps toward the extreme
    /// until it clears it.
    #[test]
    fn a_snapped_ink_keeps_its_bar() {
        let lut = Params::house().srgb_lut;
        let surface = xterm_rgb(nearest_256(Rgb8::hex(0x19_1c20)));
        // A grey that just clears 4.5 at 24 bits.
        let ink = Rgb8::hex(0x8d_9298);
        assert!(contrast(ink, Rgb8::hex(0x19_1c20), lut) >= 4.5);
        let snapped = xterm_rgb(reach_256(ink, Rgb8::WHITE, 4.5, &[surface]));
        assert!(contrast(snapped, surface, lut) >= 4.5, "{snapped:?}");
    }
}
