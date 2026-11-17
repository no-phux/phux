//! `verify(roles) → [Violation]` (spec §2.5), as phux-brand's `derive.ts`
//! computes it. V10 needs `OKLab`, which only phux-brand computes; its results
//! ship in the vectors' `verify_oklab` and in `results.oklab_delta_e`.

use crate::color::{Rgb8, Rgba8};
use crate::roles::Roles;
use crate::solve::{Floors, contrast, floor, over, tiers, washes, with_alpha};
use crate::tokens::Params;

/// Whether the roles are the authored house look or a derived theme; a few
/// checks are errors for the house and warnings for a theme the user chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    House,
    Derived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// One check a role failed, at its worst surface.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    pub id: &'static str,
    pub severity: Severity,
    pub role: String,
    pub surface: String,
    pub value: f64,
    pub bar: f64,
}

/// The house-only inputs of V14–V16.
#[derive(Debug, Clone, Default)]
pub struct HouseExtras {
    /// `material.glass.tint[mode]`: the layer colour, its authored alpha and
    /// the least alpha V14 allows.
    pub glass: Option<(Rgb8, f64, f64)>,
    /// `motion.loop.pulse.min_opacity`.
    pub pulse_min_opacity: Option<f64>,
    /// `elevation[mode]`: each tier's layer kinds, in order.
    pub elevation: Vec<(&'static str, Vec<&'static str>)>,
}

#[derive(Debug, Clone)]
pub struct VerifyOptions {
    pub context: Context,
    /// The theme's cursor and background, for V17.
    pub cursor: Option<(Rgb8, Rgb8)>,
    pub house: Option<HouseExtras>,
    /// Checks reported as warnings here (the increased-contrast variant lets
    /// its tiers merge: `V3`).
    pub warn: &'static [&'static str],
    /// Check against the increased-contrast bars (§1.11).
    pub contrast_more: bool,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        Self {
            context: Context::Derived,
            cursor: None,
            house: None,
            warn: &[],
            contrast_more: false,
        }
    }
}

const HOUSE_ERROR_DERIVED_WARNING: [&str; 5] = ["V3", "V6", "V8", "V11", "V17"];
const LAYERS: [&str; 4] = ["ground", "sidebar", "raised", "overlay"];
const WASHES: [&str; 4] = ["hover", "press", "current", "selected"];
const TONES: [&str; 7] = [
    "needs_you",
    "working",
    "done",
    "failed",
    "idle",
    "unknown",
    "bell",
];

struct Check<'a> {
    lut: &'a [u64],
    opts: &'a VerifyOptions,
    out: Vec<Violation>,
}

impl Check<'_> {
    fn severity(&self, id: &str) -> Severity {
        let soft =
            HOUSE_ERROR_DERIVED_WARNING.contains(&id) && self.opts.context == Context::Derived;
        if soft || self.opts.warn.contains(&id) {
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

    /// Report the worst surface below the bar, once per role.
    fn need(
        &mut self,
        id: &'static str,
        role: &str,
        c: Rgba8,
        bar: f64,
        surfaces: &[(String, Rgb8)],
    ) {
        let mut at = 0;
        let mut m = f64::INFINITY;
        for (i, (_, s)) in surfaces.iter().enumerate() {
            let v = contrast(c.rgb(), *s, self.lut);
            if v < m {
                m = v;
                at = i;
            }
        }
        if m < bar - 1e-9 {
            let name = surfaces.get(at).map_or("", |s| s.0.as_str()).to_owned();
            self.push(id, role, &name, m, bar);
        }
    }
}

fn named(name: &str, c: Rgb8) -> Vec<(String, Rgb8)> {
    vec![(name.to_owned(), c)]
}

const fn tone(r: &Roles, i: usize) -> (Rgba8, Rgba8) {
    let s = &r.status;
    match i {
        0 => (s.needs_you, s.needs_you_wash),
        1 => (s.working, s.working_wash),
        2 => (s.done, s.done_wash),
        3 => (s.failed, s.failed_wash),
        4 => (s.idle, s.idle_wash),
        5 => (s.unknown, s.unknown_wash),
        _ => (s.bell, s.bell_wash),
    }
}

/// Every §2.5 check `r` fails (V10 excepted), in the reference order.
#[must_use]
#[allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    reason = "check for check with the reference verify, in its order"
)]
pub fn verify(r: &Roles, p: &Params, opts: &VerifyOptions) -> Vec<Violation> {
    let lut = p.srgb_lut;
    let b = &p.bars;
    let fl = Floors::of(p, opts.contrast_more);
    let mut ck = Check {
        lut,
        opts,
        out: Vec::new(),
    };
    let t = tiers(&r.layer);
    let tn: Vec<(String, Rgb8)> = LAYERS
        .iter()
        .zip(t)
        .map(|(n, c)| ((*n).to_owned(), c))
        .collect();
    let f_colors = floor(&t, &washes(&r.wash));
    let mut f = tn.clone();
    for w in WASHES {
        for l in LAYERS {
            f.push((format!("{l}+{w}"), Rgb8::default()));
        }
    }
    for (slot, c) in f.iter_mut().zip(f_colors) {
        slot.1 = c;
    }

    for (role, c) in [
        ("text.text", r.text.text),
        ("text.dim", r.text.dim),
        ("accent.accent_text", r.accent.accent_text),
    ] {
        ck.need("V1", role, c, fl.text_floor, &f);
    }
    for (i, name) in TONES.iter().enumerate() {
        if *name != "unknown" {
            ck.need(
                "V1",
                &format!("status.{name}"),
                tone(r, i).0,
                fl.text_floor,
                &f,
            );
        }
    }
    ck.need("V2", "text.faint", r.text.faint, fl.base_floor, &tn);
    ck.need("V2", "status.unknown", r.status.unknown, fl.base_floor, &tn);

    let (text, dim, faint) = (r.text.text.rgb(), r.text.dim.rgb(), r.text.faint.rgb());
    let mut w = 0;
    for (i, s) in t.iter().enumerate() {
        if contrast(text, *s, lut) < contrast(text, t[w], lut) {
            w = i;
        }
    }
    let (ct, cd, cf) = (
        contrast(text, t[w], lut),
        contrast(dim, t[w], lut),
        contrast(faint, t[w], lut),
    );
    if ct / cd < b.tier_min - 1e-9 {
        ck.push("V3", "text.dim", LAYERS[w], ct / cd, b.tier_min);
    }
    if cd / cf < b.tier_min - 1e-9 {
        ck.push("V3", "text.faint", LAYERS[w], cd / cf, b.tier_min);
    }

    ck.need(
        "V4",
        "accent.on_accent",
        r.accent.on_accent,
        b.on_accent,
        &named("accent.accent", r.accent.accent.rgb()),
    );
    ck.need(
        "V4",
        "text.inverse",
        r.text.inverse,
        b.on_accent,
        &named("text.text", text),
    );
    ck.need("V5", "border.focus", r.border.focus, b.focus, &f);

    let mut sm = f64::INFINITY;
    let mut s_at = 0;
    for (i, s) in t.iter().enumerate() {
        let v = contrast(over(r.border.strong, *s), *s, lut);
        if v < sm {
            sm = v;
            s_at = i;
        }
    }
    if sm < fl.border_strong - 1e-9 {
        ck.push("V6", "border.strong", LAYERS[s_at], sm, fl.border_strong);
    }

    ck.need(
        "V7",
        "selection.fg",
        r.selection.fg,
        fl.base_floor,
        &named("selection.bg", r.selection.bg.rgb()),
    );
    let step = contrast(t[0], t[2], lut);
    if step < b.layer_step - 1e-9 {
        ck.push("V8", "layer.raised", "ground", step, b.layer_step);
    }
    if r.wash.press.a <= r.wash.hover.a {
        let (press, hover) = (
            f64::from(r.wash.press.a) / 255.0,
            f64::from(r.wash.hover.a) / 255.0,
        );
        ck.push_as(
            "V9",
            Severity::Error,
            "wash.press",
            "wash.hover",
            press,
            hover,
        );
    }

    ck.need(
        "V11",
        "accent.accent",
        r.accent.accent,
        b.accent_indicator,
        &tn,
    );
    ck.need("V12", "border.control", r.border.control, fl.control, &tn);
    for (i, name) in TONES.iter().enumerate() {
        if *name == "unknown" {
            continue;
        }
        let (ink, wash) = tone(r, i);
        let own: Vec<(String, Rgb8)> = LAYERS
            .iter()
            .zip(t)
            .map(|(l, c)| (format!("{l}+{name}_wash"), over(wash, c)))
            .collect();
        ck.need("V13", &format!("status.{name}"), ink, fl.text_floor, &own);
    }
    let raised = r.layer.raised.rgb();
    ck.need(
        "V13",
        "diff.add_text",
        r.diff.add_text,
        fl.base_floor,
        &named("raised+diff.add", over(r.diff.add, raised)),
    );
    ck.need(
        "V13",
        "diff.remove_text",
        r.diff.remove_text,
        fl.base_floor,
        &named("raised+diff.remove", over(r.diff.remove, raised)),
    );
    ck.need(
        "V13",
        "match.current_fg",
        r.r#match.current_fg,
        fl.base_floor,
        &named("match.current_bg", r.r#match.current_bg.rgb()),
    );

    if let Some(h) = &opts.house {
        house_checks(&mut ck, r, p, h, &tn, fl);
    }
    if let Some((cursor, bg)) = opts.cursor {
        ck.need(
            "V17",
            "terminal.cursor",
            cursor.opaque(),
            b.cursor,
            &named("terminal.bg", bg),
        );
    }
    ck.out
}

#[allow(clippy::many_single_char_names, reason = "the reference names")]
fn house_checks(
    ck: &mut Check<'_>,
    r: &Roles,
    p: &Params,
    h: &HouseExtras,
    tn: &[(String, Rgb8)],
    fl: Floors,
) {
    let lut = ck.lut;
    if let Some((tint, alpha, tint_min)) = h.glass {
        for a in [tint_min, alpha] {
            let glass = with_alpha(tint, a);
            let backdrops = vec![
                (format!("glass@{a}+black"), over(glass, Rgb8::BLACK)),
                (format!("glass@{a}+white"), over(glass, Rgb8::WHITE)),
            ];
            ck.need("V14", "text.text", r.text.text, fl.text_floor, &backdrops);
            ck.need("V14", "text.dim", r.text.dim, fl.text_floor, &backdrops);
        }
        if alpha < tint_min - 1e-9 {
            ck.push_as(
                "V14",
                Severity::Error,
                "material.glass.tint",
                "tint_min",
                alpha,
                tint_min,
            );
        }
    }
    if let Some(min_opacity) = h.pulse_min_opacity {
        let mark = r.status.working.rgb();
        let mut m = f64::INFINITY;
        let mut at = 0;
        for (i, (_, s)) in tn.iter().enumerate() {
            let v = contrast(over(with_alpha(mark, min_opacity), *s), *s, lut);
            if v < m {
                m = v;
                at = i;
            }
        }
        if m < p.bars.pulse_trough - 1e-9 {
            let name = tn[at].0.clone();
            ck.push_as(
                "V15",
                Severity::Error,
                "status.working",
                &name,
                m,
                p.bars.pulse_trough,
            );
        }
    }
    for (tier, kinds) in &h.elevation {
        let count = |k: &str| kinds.iter().filter(|x| **x == k).count();
        let max = usize::try_from(p.max_shadow_layers).unwrap_or(usize::MAX);
        if kinds.len() > max || count("ring") > 1 || count("lit") > 1 || count("glow") > 1 {
            #[allow(clippy::cast_precision_loss, reason = "a handful of layers")]
            let value = kinds.len() as f64;
            ck.push_as(
                "V16",
                Severity::Error,
                &format!("elevation.{tier}"),
                tier,
                value,
                f64::from(p.max_shadow_layers),
            );
        }
    }
}
