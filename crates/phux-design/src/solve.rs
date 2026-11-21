//! One terminal theme to every colour role, solver
//! `phux-derive/2`.
//!
//! This is the generator's `tools/src/design/derive.ts` statement for statement: IEEE-754
//! binary64 evaluated as written, luminance through the shipped LUT (never
//! `powf`), `round` half away from zero. `tests/vectors.rs` holds it to the
//! generated golden vectors byte for byte, so change both together or neither.

// `a·b + c` stays two roundings: the spec forbids FMA contraction.
#![allow(clippy::suboptimal_flops)]

mod complete;
mod more;

use complete::complete;
pub use complete::flatten;
pub use more::contrast_more;

use crate::color::{Mode, Rgb8, Rgba8};
use crate::roles::Roles;
use crate::tokens::{
    AccentRoles, BorderRoles, LayerRoles, Params, SelectionRoles, TextRoles, WashRoles,
};

/// A terminal theme, the solver's input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Theme {
    pub bg: Rgb8,
    pub fg: Rgb8,
    pub ansi: [Rgb8; 16],
    pub cursor: Option<Rgb8>,
    /// The chrome accent; `ansi[6]` when the theme names none.
    pub accent: Option<Rgb8>,
    pub selection_bg: Option<Rgb8>,
    /// An explicit mode beats the luminance test.
    pub dark: Option<bool>,
}

// --- primitives -------------------------------------------------------------

/// The linear-light value of channel `v`, from the shipped bit patterns.
fn linear(lut: &[u64], v: u8) -> f64 {
    f64::from_bits(lut[usize::from(v)])
}

/// `x` rounded half away from zero, as a byte. Every caller passes a value
/// already within 0..=255.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "rounded and clamped to 0..=255 first"
)]
const fn byte(x: f64) -> u8 {
    x.round().clamp(0.0, 255.0) as u8
}

/// Relative luminance (WCAG), through the LUT.
#[must_use]
pub fn lum(c: Rgb8, lut: &[u64]) -> f64 {
    0.2126 * linear(lut, c.r) + 0.7152 * linear(lut, c.g) + 0.0722 * linear(lut, c.b)
}

/// The WCAG contrast ratio, 1 to 21.
#[must_use]
pub fn contrast(a: Rgb8, b: Rgb8, lut: &[u64]) -> f64 {
    let (la, lb) = (lum(a, lut), lum(b, lut));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `a` moved `t` of the way to `b`, per channel: `round(a + (b − a)·t)`.
#[must_use]
pub fn mix(a: Rgb8, b: Rgb8, t: f64) -> Rgb8 {
    let ch = |x: u8, y: u8| byte(f64::from(x) + (f64::from(y) - f64::from(x)) * t);
    Rgb8 {
        r: ch(a.r, b.r),
        g: ch(a.g, b.g),
        b: ch(a.b, b.b),
    }
}

/// `top` composited over opaque `bottom` in sRGB, per channel.
#[must_use]
pub fn over(top: Rgba8, bottom: Rgb8) -> Rgb8 {
    let a = f64::from(top.a) / 255.0;
    let ch = |t: u8, b: u8| byte(f64::from(b) + (f64::from(t) - f64::from(b)) * a);
    Rgb8 {
        r: ch(top.r, bottom.r),
        g: ch(top.g, bottom.g),
        b: ch(top.b, bottom.b),
    }
}

/// `c` at `alpha`, the byte `round(alpha·255)`.
#[must_use]
pub fn with_alpha(c: Rgb8, alpha: f64) -> Rgba8 {
    c.with_alpha_byte(byte(alpha * 255.0))
}

/// The lowest contrast `c` makes against any of `surfaces`.
#[must_use]
pub fn worst(c: Rgb8, surfaces: &[Rgb8], lut: &[u64]) -> f64 {
    surfaces
        .iter()
        .fold(f64::INFINITY, |m, s| m.min(contrast(c, *s, lut)))
}

/// The least move of `c` toward `toward` that clears `bar` on every surface.
#[must_use]
pub fn reach(c: Rgb8, toward: Rgb8, bar: f64, surfaces: &[Rgb8], lut: &[u64], n: u32) -> Rgb8 {
    bisect_least(c, toward, n, |x| worst(x, surfaces, lut) >= bar)
}

/// [`reach`] for a status tone: each candidate must also clear the bar on its
/// own wash (`candidate @ alpha` over each layer), so the exported `_wash`
/// companion is the one that is tested (V13).
#[must_use]
#[allow(
    clippy::too_many_arguments,
    reason = "the reference signature, argument for argument"
)]
pub fn reach_tone(
    c: Rgb8,
    toward: Rgb8,
    bar: f64,
    floor: &[Rgb8],
    layers: &[Rgb8],
    alpha: f64,
    lut: &[u64],
    n: u32,
) -> Rgb8 {
    bisect_least(c, toward, n, |x| {
        if worst(x, floor, lut) < bar {
            return false;
        }
        let wash = with_alpha(x, alpha);
        layers
            .iter()
            .all(|l| contrast(x, over(wash, *l), lut) >= bar)
    })
}

fn bisect_least(c: Rgb8, toward: Rgb8, n: u32, ok: impl Fn(Rgb8) -> bool) -> Rgb8 {
    if ok(c) {
        return c;
    }
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for _ in 0..n {
        let mid = f64::midpoint(lo, hi);
        if ok(mix(c, toward, mid)) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    mix(c, toward, hi)
}

/// The most move of `c` toward `toward` that still clears `target`.
#[must_use]
pub fn fade(c: Rgb8, toward: Rgb8, target: f64, surfaces: &[Rgb8], lut: &[u64], n: u32) -> Rgb8 {
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for _ in 0..n {
        let mid = f64::midpoint(lo, hi);
        if worst(mix(c, toward, mid), surfaces, lut) >= target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    mix(c, toward, lo)
}

/// White or black, whichever reads on `fill` (always ≥ 4.58:1).
#[must_use]
pub fn on_fill(fill: Rgb8, lut: &[u64]) -> Rgb8 {
    if contrast(Rgb8::WHITE, fill, lut) >= contrast(Rgb8::BLACK, fill, lut) {
        Rgb8::WHITE
    } else {
        Rgb8::BLACK
    }
}

// --- surfaces ----------------------------------------------------------------

/// The four layers, T, in list order.
pub(crate) fn tiers(layer: &LayerRoles) -> [Rgb8; 4] {
    [layer.ground, layer.sidebar, layer.raised, layer.overlay].map(Rgba8::rgb)
}

/// The four washes in F⁺ order.
pub(crate) const fn washes(wash: &WashRoles) -> [Rgba8; 4] {
    [wash.hover, wash.press, wash.current, wash.selected]
}

/// F⁺: T, then every wash over every layer, wash-major.
pub(crate) fn floor(t: &[Rgb8; 4], washes: &[Rgba8; 4]) -> Vec<Rgb8> {
    let mut out = t.to_vec();
    for w in washes {
        out.extend(t.iter().map(|l| over(*w, *l)));
    }
    out
}

/// The floor faint, unknown, selection, match and diff text clear.
/// Only the increased-contrast variant raises `text_floor` above it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Floors {
    pub(crate) text_floor: f64,
    pub(crate) base_floor: f64,
    pub(crate) control: f64,
    pub(crate) border_strong: f64,
}

impl Floors {
    pub(crate) const fn of(p: &Params, more: bool) -> Self {
        if more {
            Self {
                text_floor: p.contrast_more.text_floor,
                base_floor: p.bars.text_floor,
                control: p.contrast_more.control,
                border_strong: p.contrast_more.border_strong,
            }
        } else {
            Self {
                text_floor: p.bars.text_floor,
                base_floor: p.bars.text_floor,
                control: p.bars.control,
                border_strong: p.bars.border_strong,
            }
        }
    }
}

// --- derive -----------------------------------------------------------------

/// The mode a theme draws in: its explicit flag, else luminance.
#[must_use]
pub fn mode_of(theme: &Theme, p: &Params) -> Mode {
    let light = theme.dark.map_or_else(
        || lum(theme.bg, p.srgb_lut) > p.light_threshold,
        |dark| !dark,
    );
    if light { Mode::Light } else { Mode::Dark }
}

/// The opaque-ink and translucent roles a theme or look supplies; the rest
/// ([`complete`]) is computed from these and the ANSI colours.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Core {
    pub(crate) mode: Mode,
    pub(crate) layer: LayerRoles,
    pub(crate) text: TextRoles,
    pub(crate) border: BorderRoles,
    pub(crate) wash: WashRoles,
    pub(crate) accent: AccentRoles,
    /// `needs_you`, working, done, failed, idle, unknown, bell.
    pub(crate) tones: [Rgb8; 7],
    pub(crate) selection: SelectionRoles,
}

/// Every role for `theme`.
#[must_use]
#[allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "statement for statement with the reference solver, so a diff against it stays readable"
)]
pub fn derive(theme: &Theme, p: &Params) -> Roles {
    let (lut, n, b) = (p.srgb_lut, p.iterations, &p.bars);
    let (bg, fg) = (theme.bg, theme.fg);
    let ansi = theme.ansi;
    let mode = mode_of(theme, p);
    let extreme = mode.extreme();
    let step = |s: f64| {
        if s >= 0.0 {
            mix(bg, fg, s)
        } else {
            mix(bg, mode.away(), -s)
        }
    };
    let steps = match mode {
        Mode::Dark => p.layers.dark,
        Mode::Light => p.layers.light,
    };
    let a = match mode {
        Mode::Dark => p.alpha.dark,
        Mode::Light => p.alpha.light,
    };
    let scrim = match mode {
        Mode::Dark => p.scrim.dark,
        Mode::Light => p.scrim.light,
    };
    let layer = LayerRoles {
        ground: bg.opaque(),
        sidebar: step(steps.sidebar).opaque(),
        raised: step(steps.raised).opaque(),
        overlay: step(steps.overlay).opaque(),
        scrim: with_alpha(Rgb8::BLACK, scrim),
    };
    let accent_raw = theme.accent.unwrap_or_else(|| ansi[slot(p.slots.accent)]);
    let strong = with_alpha(fg, a.border.strong);
    let selected_ink = if p.wash_selected_source == "accent" {
        accent_raw
    } else {
        fg
    };
    let wash = WashRoles {
        hover: with_alpha(fg, a.wash.hover),
        press: with_alpha(fg, a.wash.press),
        current: with_alpha(fg, a.wash.current),
        selected: with_alpha(selected_ink, a.wash.selected),
    };
    let t = tiers(&layer);
    let f = floor(&t, &washes(&wash));

    let mut text = reach(fg, extreme, b.text, &t, lut, n);
    let mut w = t[0];
    for s in t {
        if contrast(text, s, lut) < contrast(text, w, lut) {
            w = s;
        }
    }
    let pc = contrast(text, w, lut);
    let mut dim = fade(text, w, pc / b.tier_ratio, &t, lut, n);
    let mut faint = fade(text, w, pc / (b.tier_ratio * b.tier_ratio), &t, lut, n);
    let signal = |s: u32| reach(ansi[slot(s)], extreme, b.signal, &t, lut, n);
    let mut tone = [
        signal(p.slots.needs_you),
        signal(p.slots.working),
        signal(p.slots.done),
        signal(p.slots.failed),
        mix(dim, faint, 0.5),
        Rgb8::BLACK,
        signal(p.slots.bell),
    ];
    let mut accent_text = reach(accent_raw, extreme, b.signal, &t, lut, n);

    // Floor pass: words read on every wash and on their own tone wash.
    text = reach(text, extreme, b.text_floor, &f, lut, n);
    dim = reach(dim, extreme, b.text_floor, &f, lut, n);
    accent_text = reach(accent_text, extreme, b.text_floor, &f, lut, n);
    // Slot tones first, then idle, in the reference order.
    for i in [0, 1, 2, 3, 6, 4] {
        tone[i] = reach_tone(
            tone[i],
            extreme,
            b.text_floor,
            &f,
            &t,
            p.status_alpha.wash,
            lut,
            n,
        );
    }
    faint = reach(faint, extreme, b.text_floor, &t, lut, n);
    tone[5] = faint;

    let on_accent = on_fill(accent_raw, lut);
    let focus = reach(accent_raw, extreme, b.focus, &f, lut, n);
    let control = reach(over(strong, t[2]), extreme, b.control, &t, lut, n);
    let selection = theme
        .selection_bg
        .unwrap_or_else(|| mix(bg, accent_raw, p.selection_mix));
    let selection_ink = reach(
        text,
        on_fill(selection, lut),
        b.text_floor,
        &[selection],
        lut,
        n,
    );

    let core = Core {
        mode,
        layer,
        text: TextRoles {
            text: text.opaque(),
            dim: dim.opaque(),
            faint: faint.opaque(),
            inverse: bg.opaque(),
        },
        border: BorderRoles {
            muted: with_alpha(fg, a.border.muted),
            base: with_alpha(fg, a.border.base),
            strong,
            control: control.opaque(),
            focus: focus.opaque(),
        },
        wash,
        // A derived theme has no metal stops: hi and lo are the accent.
        accent: AccentRoles {
            accent: accent_raw.opaque(),
            on_accent: on_accent.opaque(),
            accent_text: accent_text.opaque(),
            hi: accent_raw.opaque(),
            lo: accent_raw.opaque(),
        },
        tones: tone,
        selection: SelectionRoles {
            bg: selection.opaque(),
            fg: selection_ink.opaque(),
        },
    };
    complete(&core, &ansi, p, Floors::of(p, false))
}

fn slot(s: u32) -> usize {
    usize::try_from(s).unwrap_or(0).min(15)
}
