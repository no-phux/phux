//! The increased-contrast variant, solved from a role set.

use super::complete::complete;
use super::{Core, Floors, byte, contrast, floor, on_fill, over, reach, reach_tone, tiers, washes};
use crate::color::{Rgb8, Rgba8};
use crate::roles::Roles;
use crate::tokens::{AccentRoles, BorderRoles, Params, SelectionRoles, TextRoles, WashRoles};

/// The increased-contrast variant of `base`.
///
/// It is solved by the same primitives as the generator's `solveContrastMore`: text, dim, accent text and the tones
/// to `contrast_more.text_floor` over washes scaled by `wash_scale`, opaque
/// `border.strong` and `border.control`. `ansi` is the theme's palette (the
/// house terminal's for the house look).
#[must_use]
#[allow(
    clippy::many_single_char_names,
    reason = "the reference solver's names"
)]
pub fn contrast_more(base: &Roles, ansi: &[Rgb8; 16], p: &Params) -> Roles {
    let (lut, n) = (p.srgb_lut, p.iterations);
    let fl = Floors::of(p, true);
    let extreme = base.mode.extreme();
    let t = tiers(&base.layer);
    let scale = |c: Rgba8| {
        c.rgb().with_alpha_byte(byte(
            (f64::from(c.a) * p.contrast_more.wash_scale).min(255.0),
        ))
    };
    let wash = WashRoles {
        hover: scale(base.wash.hover),
        press: scale(base.wash.press),
        current: scale(base.wash.current),
        selected: scale(base.wash.selected),
    };
    let f = floor(&t, &washes(&wash));
    let up = |c: Rgba8, bar: f64, s: &[Rgb8]| reach(c.rgb(), extreme, bar, s, lut, n).opaque();
    let s = &base.status;
    let mut tones = [
        s.needs_you,
        s.working,
        s.done,
        s.failed,
        s.idle,
        s.unknown,
        s.bell,
    ]
    .map(Rgba8::rgb);
    for i in [0, 1, 2, 3, 6, 4] {
        tones[i] = reach_tone(
            tones[i],
            extreme,
            fl.text_floor,
            &f,
            &t,
            p.status_alpha.wash,
            lut,
            n,
        );
    }
    let faint = up(base.text.faint, fl.base_floor, &t);
    tones[5] = faint.rgb();
    let sel_bg = base.selection.bg.rgb();
    let mut core = Core {
        mode: base.mode,
        layer: base.layer,
        text: TextRoles {
            text: up(base.text.text, fl.text_floor, &f),
            dim: up(base.text.dim, fl.text_floor, &f),
            faint,
            inverse: base.text.inverse,
        },
        border: BorderRoles {
            muted: base.border.muted,
            base: base.border.base,
            strong: reach(
                over(base.border.strong, t[2]),
                extreme,
                fl.border_strong,
                &t,
                lut,
                n,
            )
            .opaque(),
            control: up(base.border.control, fl.control, &t),
            focus: up(base.border.focus, p.bars.focus, &f),
        },
        wash,
        accent: AccentRoles {
            accent_text: up(base.accent.accent_text, fl.text_floor, &f),
            ..base.accent
        },
        tones,
        selection: SelectionRoles {
            bg: base.selection.bg,
            fg: reach(
                base.selection.fg.rgb(),
                on_fill(sel_bg, lut),
                fl.base_floor,
                &[sel_bg],
                lut,
                n,
            )
            .opaque(),
        },
    };
    space_tiers(&mut core, p, fl);
    complete(&core, ansi, p, fl)
}

/// Push dim toward the extreme until faint sits `tier_min` below it.
fn space_tiers(core: &mut Core, p: &Params, fl: Floors) {
    let lut = p.srgb_lut;
    let t = tiers(&core.layer);
    let text = core.text.text.rgb();
    let mut w = t[0];
    for s in t {
        if contrast(text, s, lut) < contrast(text, w, lut) {
            w = s;
        }
    }
    let c = |x: Rgba8| contrast(x.rgb(), w, lut);
    if c(core.text.dim) / c(core.text.faint) >= p.bars.tier_min {
        return;
    }
    let target = c(core.text.faint).max(fl.base_floor) * p.bars.tier_min;
    core.text.dim = reach(
        core.text.dim.rgb(),
        core.mode.extreme(),
        target,
        &[w],
        lut,
        p.iterations,
    )
    .opaque();
}
