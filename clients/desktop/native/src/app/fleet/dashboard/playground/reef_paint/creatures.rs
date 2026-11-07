//! What work done adds to a fish: an exotic form by harness from tier 3
//! (Claude lionfish, Codex manta ray, OpenCode seahorse, Gemini cuttlefish,
//! anyone else octopus), a legendary fish's orbiting motes and trail, and
//! the level-up's gold ring. The prototype's painters, ported.

use super::*;

mod cuttlefish;
mod lionfish;
mod manta;
mod octopus;
mod seahorse;

/// A fish's colours this frame, already shaded for mood and night, and the
/// gradient angle that points down its body.
pub(super) struct Hue {
    pub body: Rgb,
    pub belly: Rgb,
    pub fin: Rgb,
    pub ink: Rgb,
    /// How far night has dimmed it, 0 to 0.45.
    pub night: f32,
    pub down: f32,
}

const GOLD: Rgb = [255., 226., 140.];

/// Paint an exotic `f` in its local frame (head along +x), `l` long.
pub(super) fn form(pen: &mut Pen, sim: &Sim, f: &Fish, l: f32, wag: f32, hue: &Hue) {
    use crate::app::pane_identity::Harness;
    match super::super::tier::harness(&f.agent) {
        Harness::Claude => lionfish::paint(pen, sim, f, l, wag, hue),
        Harness::Codex => manta::paint(pen, sim, f, l, wag, hue),
        Harness::OpenCode => seahorse::paint(pen, sim, f, l, hue),
        Harness::Gemini => cuttlefish::paint(pen, sim, f, l, wag, hue),
        _ => octopus::paint(pen, sim, f, l, hue),
    }
}

/// In world space, before the body: six motes orbiting a legendary fish,
/// and a level-up's widening gold ring and glow.
pub(super) fn aura(pen: &mut Pen, sim: &Sim, f: &Fish, l: f32) {
    let s = sim.s;
    if f.tier == 4 {
        for i in 0..6 {
            let a = sim.t * 0.6 + i as f32 * TAU / 6.;
            let at = (f.x + a.cos() * l * 0.78, f.y + a.sin() * l * 0.42);
            pen.glow(at, 6. * s, f.species.fin, 0.6);
        }
    }
    if f.level_up > 0. {
        let r = l * (0.5 + (1. - f.level_up) * 1.8);
        pen.stroke(
            &arc(f.x, f.y, r, r, 0., 0., TAU, 48),
            true,
            3. * s,
            rgba(GOLD, f.level_up),
        );
        pen.glow((f.x, f.y), l * 1.4, GOLD, 0.5 * f.level_up);
    }
}

/// The legendary trail: every mote, rising and fading.
pub(super) fn motes(pen: &mut Pen, sim: &Sim) {
    // A glow each, and bright cores in four fading steps, one path a step.
    let mut cores: [Vec<Vec<P>>; 4] = Default::default();
    for m in &sim.motes {
        pen.glow((m.x, m.y), 4. * sim.s, m.c, m.a * 0.7);
        let step = ((m.a * 4.) as usize).min(3);
        cores[step].push(circle(m.x, m.y, 1.2 * sim.s));
    }
    for (step, polys) in cores.iter().enumerate() {
        let a = (step as f32 + 0.5) / 4.;
        pen.fill_many(polys, rgba(mix(WHITE, AMBER, 0.15), a * 0.8));
    }
}

/// Back dark, flank in the body colour, belly pale, down the body.
pub(super) fn body_fill(pen: &mut Pen, polys: &[Vec<P>], hue: &Hue) {
    pen.fill_many(polys, rgba(hue.body, 1.));
    let dark = mix(hue.body, BLACK, 0.25);
    pen.fill_many(
        polys,
        grad(hue.down, rgba(dark, 1.), 0., rgba(dark, 0.), 0.5),
    );
    pen.fill_many(
        polys,
        grad(hue.down, rgba(hue.belly, 0.), 0.5, rgba(hue.belly, 1.), 1.),
    );
}

/// An eye of radius `r`: white, a pupil that narrows when idle, a glint.
pub(super) fn eye(pen: &mut Pen, x: f32, y: f32, r: f32, idle: bool, alpha: f32) {
    pen.fill(&circle(x, y, r), rgba([246., 251., 255.], alpha));
    let pupil = r * if idle { 0.45 } else { 0.62 };
    pen.fill(&circle(x + r * 0.2, y, pupil), rgba([7., 21., 33.], alpha));
    pen.fill(
        &circle(x + r * 0.35, y - r * 0.3, r * 0.22),
        rgba(WHITE, alpha),
    );
}

/// `a` to `b` as dashes `on` long with `off` between, as `setLineDash`.
pub(super) fn dashes(a: P, b: P, on: f32, off: f32) -> Vec<Vec<P>> {
    let len = (b.0 - a.0).hypot(b.1 - a.1);
    let along = |d: f32| {
        let k = (d / len.max(1e-3)).min(1.);
        (a.0 + (b.0 - a.0) * k, a.1 + (b.1 - a.1) * k)
    };
    let mut out = Vec::new();
    let mut d = 0.;
    while d < len {
        out.push(vec![along(d), along(d + on)]);
        d += on + off;
    }
    out
}

/// A polygon round an ellipse, as the prototype's `ctx.ellipse`. Without
/// `arc`'s closing point: rounding puts it a hair off the first, and that
/// sliver of an edge, wound backwards, would clip everything away.
pub(super) fn oval(cx: f32, cy: f32, rx: f32, ry: f32, rot: f32) -> Vec<P> {
    let mut pts = arc(cx, cy, rx, ry, rot, 0., TAU, 28);
    pts.pop();
    pts
}
