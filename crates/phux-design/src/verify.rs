//! The floor checks (spec §2.5), as the generator's `tools/src/design/verify.ts`
//! runs them: V1-V9, V11-V13 and V17 for any look, V14-V16 for the house
//! look's own glass, pulse and shadows. V10 needs `OKLab`, which only the
//! generator computes.

#![allow(
    clippy::many_single_char_names,
    reason = "the reference verify's names"
)]

use crate::color::{Mode, Rgb8, Rgba8};
use crate::roles::{Layer, Roles};
use crate::solve::{Floors, contrast, over, with_alpha};
use crate::status::Tone;
use crate::tokens::{self, Params};

/// How a failed check counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    Error,
    Warning,
}

/// One failed check: the role, the surface it read worst on, and the bar.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    /// `V1` … `V17`.
    pub id: &'static str,
    pub severity: Severity,
    pub role: String,
    pub surface: String,
    pub value: f64,
    pub bar: f64,
}

/// What to check a role set as.
#[derive(Debug, Clone, Copy, Default)]
pub struct Check<'a> {
    /// The house look, or one of its variants: V3, V6, V8, V11 and V17 are
    /// errors. A derived theme reports them as warnings.
    pub house: bool,
    /// Hold to the increased-contrast bars (spec §1.11).
    pub contrast_more: bool,
    /// Also run V14-V16 on the house tokens of the roles' mode.
    pub house_extras: bool,
    /// The terminal cursor and background, for V17.
    pub cursor: Option<(Rgb8, Rgb8)>,
    /// Checks reported as warnings regardless (`contrast_more` lets the tiers
    /// merge: `V3`).
    pub warn: &'a [&'a str],
}

const HOUSE_ERROR_DERIVED_WARNING: [&str; 5] = ["V3", "V6", "V8", "V11", "V17"];
const WASHES: [&str; 4] = ["hover", "press", "current", "selected"];

type Surface = (String, Rgb8);

struct Checker<'a> {
    lut: &'a [u64],
    check: &'a Check<'a>,
    out: Vec<Violation>,
}

impl Checker<'_> {
    fn severity(&self, id: &str) -> Severity {
        let derived_warning = HOUSE_ERROR_DERIVED_WARNING.contains(&id) && !self.check.house;
        if derived_warning || self.check.warn.contains(&id) {
            Severity::Warning
        } else {
            Severity::Error
        }
    }

    fn push(&mut self, id: &'static str, role: &str, surface: &str, value: f64, bar: f64) {
        let severity = self.severity(id);
        self.push_as(id, severity, role, surface, value, bar);
    }

    fn push_as(
        &mut self,
        id: &'static str,
        severity: Severity,
        role: &str,
        surface: &str,
        value: f64,
        bar: f64,
    ) {
        self.out.push(Violation {
            id,
            severity,
            role: role.to_owned(),
            surface: surface.to_owned(),
            value,
            bar,
        });
    }

    /// The lowest contrast of `ink(surface)` against each surface.
    fn lowest<'s>(&self, ink: impl Fn(Rgb8) -> Rgb8, on: &'s [Surface]) -> (f64, &'s str) {
        let mut at = &on[0];
        let mut m = f64::INFINITY;
        for s in on {
            let v = contrast(ink(s.1), s.1, self.lut);
            if v < m {
                m = v;
                at = s;
            }
        }
        (m, &at.0)
    }

    /// Report the worst surface below the bar, once per role.
    fn need(&mut self, id: &'static str, role: &str, color: Rgba8, bar: f64, on: &[Surface]) {
        let (m, at) = self.lowest(|_| color.rgb(), on);
        if m < bar - 1e-9 {
            let at = at.to_owned();
            self.push(id, role, &at, m, bar);
        }
    }
}

/// T: the four layers, named.
#[must_use]
pub fn tier_surfaces(r: &Roles) -> Vec<(String, Rgb8)> {
    Layer::ALL
        .map(|l| (l.key().to_owned(), r.layer(l).rgb()))
        .to_vec()
}

/// F⁺: T, then every wash over every layer, wash-major (`raised+selected`).
#[must_use]
pub fn floor_surfaces(r: &Roles) -> Vec<(String, Rgb8)> {
    let mut out = tier_surfaces(r);
    let w = &r.wash;
    for (name, wash) in WASHES
        .into_iter()
        .zip([w.hover, w.press, w.current, w.selected])
    {
        for l in Layer::ALL {
            out.push((format!("{}+{name}", l.key()), over(wash, r.layer(l).rgb())));
        }
    }
    out
}

/// Every check over `r`, in the generator's order.
#[must_use]
pub fn verify(r: &Roles, p: &Params, check: &Check<'_>) -> Vec<Violation> {
    let b = &p.bars;
    let fl = Floors::of(p, check.contrast_more);
    let mut c = Checker {
        lut: p.srgb_lut,
        check,
        out: Vec::new(),
    };
    let t = tier_surfaces(r);
    let f = floor_surfaces(r);
    words(&mut c, r, b, fl, &t, &f);
    borders_and_layers(&mut c, r, p, fl, &t, &f);
    fills(&mut c, r, fl, &t);
    if check.house_extras {
        house(&mut c, r, p, &t);
    }
    if let Some((cursor, bg)) = check.cursor {
        let surface = [("terminal.bg".to_owned(), bg)];
        c.need(
            "V17",
            "terminal.cursor",
            cursor.opaque(),
            b.cursor,
            &surface,
        );
    }
    c.out
}

/// V1-V4: every word on every surface it can land on, and the tiers apart.
fn words(
    c: &mut Checker<'_>,
    r: &Roles,
    b: &tokens::Bars,
    fl: Floors,
    t: &[Surface],
    f: &[Surface],
) {
    for (role, ink) in [
        ("text.text", r.text.text),
        ("text.dim", r.text.dim),
        ("accent.accent_text", r.accent.accent_text),
    ] {
        c.need("V1", role, ink, fl.text_floor, f);
    }
    for tone in Tone::ALL.into_iter().filter(|t| *t != Tone::Unknown) {
        c.need(
            "V1",
            &format!("status.{}", tone.key()),
            r.tone(tone).ink,
            fl.text_floor,
            f,
        );
    }
    c.need("V2", "text.faint", r.text.faint, fl.base_floor, t);
    c.need("V2", "status.unknown", r.status.unknown, fl.base_floor, t);

    let (text, dim, faint) = (r.text.text.rgb(), r.text.dim.rgb(), r.text.faint.rgb());
    let mut w = &t[0];
    for s in t {
        if contrast(text, s.1, c.lut) < contrast(text, w.1, c.lut) {
            w = s;
        }
    }
    let ct = contrast(text, w.1, c.lut);
    let cd = contrast(dim, w.1, c.lut);
    let cf = contrast(faint, w.1, c.lut);
    if ct / cd < b.tier_min - 1e-9 {
        c.push("V3", "text.dim", &w.0, ct / cd, b.tier_min);
    }
    if cd / cf < b.tier_min - 1e-9 {
        c.push("V3", "text.faint", &w.0, cd / cf, b.tier_min);
    }
    let accent = [("accent.accent".to_owned(), r.accent.accent.rgb())];
    c.need(
        "V4",
        "accent.on_accent",
        r.accent.on_accent,
        b.on_accent,
        &accent,
    );
    let on_text = [("text.text".to_owned(), text)];
    c.need("V4", "text.inverse", r.text.inverse, b.on_accent, &on_text);
}

/// V5-V9, V11, V12: focus, borders, selection, the layer step and washes.
fn borders_and_layers(
    c: &mut Checker<'_>,
    r: &Roles,
    p: &Params,
    fl: Floors,
    t: &[Surface],
    f: &[Surface],
) {
    let b = &p.bars;
    c.need("V5", "border.focus", r.border.focus, b.focus, f);
    let (sm, at) = c.lowest(|s| over(r.border.strong, s), t);
    if sm < fl.border_strong - 1e-9 {
        let at = at.to_owned();
        c.push("V6", "border.strong", &at, sm, fl.border_strong);
    }
    let sel = [("selection.bg".to_owned(), r.selection.bg.rgb())];
    c.need("V7", "selection.fg", r.selection.fg, fl.base_floor, &sel);
    let step = contrast(r.layer.ground.rgb(), r.layer.raised.rgb(), c.lut);
    if step < b.layer_step - 1e-9 {
        c.push("V8", "layer.raised", "ground", step, b.layer_step);
    }
    let (press, hover) = (r.wash.press.a, r.wash.hover.a);
    if press <= hover {
        let (value, bar) = (f64::from(press) / 255.0, f64::from(hover) / 255.0);
        c.push_as(
            "V9",
            Severity::Error,
            "wash.press",
            "wash.hover",
            value,
            bar,
        );
    }
    c.need(
        "V11",
        "accent.accent",
        r.accent.accent,
        b.accent_indicator,
        t,
    );
    c.need("V12", "border.control", r.border.control, fl.control, t);
}

/// V13: tones on their own washes, diff and match text on their fills.
fn fills(c: &mut Checker<'_>, r: &Roles, fl: Floors, t: &[Surface]) {
    for tone in Tone::ALL.into_iter().filter(|t| *t != Tone::Unknown) {
        let ink = r.tone(tone);
        let key = tone.key();
        let on: Vec<Surface> = t
            .iter()
            .map(|(l, c)| (format!("{l}+{key}_wash"), over(ink.wash, *c)))
            .collect();
        c.need("V13", &format!("status.{key}"), ink.ink, fl.text_floor, &on);
    }
    let raised = r.layer.raised.rgb();
    let add = [("raised+diff.add".to_owned(), over(r.diff.add, raised))];
    c.need("V13", "diff.add_text", r.diff.add_text, fl.base_floor, &add);
    let remove = [("raised+diff.remove".to_owned(), over(r.diff.remove, raised))];
    c.need(
        "V13",
        "diff.remove_text",
        r.diff.remove_text,
        fl.base_floor,
        &remove,
    );
    let current = [("match.current_bg".to_owned(), r.r#match.current_bg.rgb())];
    c.need(
        "V13",
        "match.current_fg",
        r.r#match.current_fg,
        fl.base_floor,
        &current,
    );
}

/// V14-V16: glass over worst-case backdrops, the pulse trough, tier sizes.
fn house(c: &mut Checker<'_>, r: &Roles, p: &Params, t: &[Surface]) {
    let dark = r.mode == Mode::Dark;
    let glass = &tokens::MATERIAL.glass.tint;
    let glass = if dark { &glass.dark } else { &glass.light };
    let tint_min = if dark {
        p.glass.tint_min.dark
    } else {
        p.glass.tint_min.light
    };
    let tint = Layer::ALL
        .into_iter()
        .find(|l| l.key() == glass.layer)
        .map_or(r.layer.raised, |l| r.layer(l))
        .rgb();
    let alpha = f64::from(glass.alpha);
    for (a, name) in [
        (tint_min, tint_min.to_string()),
        (alpha, glass.alpha.to_string()),
    ] {
        let g = with_alpha(tint, a);
        let backdrops = [
            (format!("glass@{name}+black"), over(g, Rgb8::BLACK)),
            (format!("glass@{name}+white"), over(g, Rgb8::WHITE)),
        ];
        c.need(
            "V14",
            "text.text",
            r.text.text,
            p.bars.text_floor,
            &backdrops,
        );
        c.need("V14", "text.dim", r.text.dim, p.bars.text_floor, &backdrops);
    }
    if alpha < tint_min - 1e-9 {
        let s = Severity::Error;
        c.push_as("V14", s, "material.glass.tint", "tint_min", alpha, tint_min);
    }
    let pulse = f64::from(tokens::MOTION.r#loop.pulse.min_opacity);
    let mark = r.status.working.rgb();
    let (m, at) = c.lowest(|s| over(with_alpha(mark, pulse), s), t);
    if m < p.bars.pulse_trough - 1e-9 {
        let at = at.to_owned();
        c.push_as(
            "V15",
            Severity::Error,
            "status.working",
            &at,
            m,
            p.bars.pulse_trough,
        );
    }
    let e = if dark {
        &tokens::ELEVATION.dark
    } else {
        &tokens::ELEVATION.light
    };
    let max = p.max_shadow_layers;
    for (tier, layers) in [
        ("flat", e.flat),
        ("raised", e.raised),
        ("floating", e.floating),
        ("overlay", e.overlay),
    ] {
        let count = |k: &str| layers.iter().filter(|x| x.kind == k).count();
        let too_many = layers.len() > max as usize;
        if too_many || count("ring") > 1 || count("lit") > 1 || count("glow") > 1 {
            let role = format!("elevation.{tier}");
            c.push_as(
                "V16",
                Severity::Error,
                &role,
                tier,
                f64::from(u32::try_from(layers.len()).unwrap_or(u32::MAX)),
                f64::from(max),
            );
        }
    }
}
