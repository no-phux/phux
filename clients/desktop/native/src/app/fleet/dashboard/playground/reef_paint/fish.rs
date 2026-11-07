//! A fish: its species, its mood's aura, its night photophores, and its
//! tier's form (a fry, a reef fish, an elder; `creatures` paints the rest).

use super::*;

pub(super) fn body(l: f32, h: f32, wag: f32) -> Vec<P> {
    Shape::at(l * 0.5, 0.)
        .cubic(
            l * 0.42,
            -h * 0.98,
            -l * 0.06,
            -h * 1.05,
            -l * 0.3,
            -h * 0.22 + wag * 0.4,
        )
        .line(-l * 0.3, h * 0.22 + wag * 0.4)
        .cubic(-l * 0.06, h * 1.05, l * 0.42, h * 0.98, l * 0.5, 0.)
        .0
}

pub(super) fn fish(pen: &mut Pen, sim: &Sim, pal: &Pal, f: &Fish, focused: bool) {
    let (s, nt, t) = (sim.s, sim.nt, sim.t);
    let k = f.species;
    let l = f.len(s);
    let sp = f.vx.hypot(f.vy) / s;
    let wag = f.phase.sin() * l * (0.045 + sp.min(120.) / 120. * 0.06);
    let mc = f.mood.color();
    match f.mood {
        Mood::Waiting => {
            pen.glow((f.x, f.y), l * 1.2, mc, 0.35 + 0.2 * (t * 5.).sin());
            pen.stroke(
                &arc(
                    f.x,
                    f.y,
                    l * (0.6 + f.pulse * 0.9),
                    l * (0.6 + f.pulse * 0.9),
                    0.,
                    0.,
                    TAU,
                    48,
                ),
                true,
                2. * s,
                rgba(mc, (1. - f.pulse) * 0.9),
            );
        }
        Mood::Done => pen.glow(
            (f.x, f.y),
            l * 0.95,
            mc,
            0.22 + 0.12 * (t * 1.6 + f.seed).sin(),
        ),
        _ => {}
    }
    if nt > 0.05 {
        pen.glow((f.x, f.y), l * 1.25, k.body, 0.3 * nt);
    }
    aura(pen, sim, f, l);
    let keep = pen.m;
    let rot = f.tilt * if f.face_s < 0. { -1. } else { 1. };
    pen.translate(f.x, f.y);
    pen.rotate(rot);
    pen.scale(f.face_s, 1.);
    let shade = if f.mood == Mood::Idle { 0.3 } else { 0. };
    let night = nt * 0.45;
    let skin = mix(mix(k.body, [120., 130., 140.], shade), pal.ink, night);
    let belly = mix(mix(k.belly, [170., 180., 190.], shade), pal.ink, night);
    let fin = mix(k.fin, pal.ink, night);
    let hue = Hue {
        body: skin,
        belly,
        fin,
        ink: pal.ink,
        night,
        down: 180. + rot.to_degrees(),
    };
    if f.tier >= 3 {
        form(pen, sim, f, l, wag, &hue);
    } else {
        reef_fish(pen, sim, f, l, wag, &hue);
    }
    pen.m = keep;
    if focused {
        // A dashed ring that turns: 4 points on, 5 off, on screen.
        let z = sim.cam.z;
        let r = l * 0.85;
        let period = 9. / z;
        let count = ((TAU * r) / period).floor().max(4.);
        let share = TAU / count;
        let turn = -t * 12. / z / r;
        let dashes = (0..count as usize)
            .map(|i| {
                let a0 = turn + i as f32 * share;
                arc(f.x, f.y, r, r, 0., a0, a0 + share * 4. / 9., 4)
            })
            .collect::<Vec<_>>();
        pen.stroke_many(&dashes, false, 1.4 * s / z, rgba([230., 250., 255.], 0.85));
    }
}

/// The reef fish in its local frame: tier 1 as ever, a fry (tier 0) faint,
/// plain and big-eyed, an elder (tier 2) with a long forked tail, tall fins
/// and a sheen sliding across its scales.
fn reef_fish(pen: &mut Pen, sim: &Sim, f: &Fish, l: f32, wag: f32, hue: &Hue) {
    let (s, nt, t) = (sim.s, sim.nt, sim.t);
    let k = f.species;
    let h = l * 0.27 * k.fat;
    let (fry, elder) = (f.tier == 0, f.tier == 2);
    let a = if fry { 0.8 } else { 1. };
    let (tip, fork) = if elder { (0.72, 1.25) } else { (0.57, 0.95) };
    let tail = Shape::at(-l * 0.27, wag * 0.4)
        .quad(-l * 0.42, -h * 0.2 + wag * 0.7, -l * tip, -h * fork + wag)
        .quad(-l * (tip - 0.12), wag * 0.95, -l * tip, h * fork + wag)
        .quad(-l * 0.42, h * 0.2 + wag * 0.7, -l * 0.27, wag * 0.4)
        .0;
    let tall = k.tall + if elder { 0.7 } else { 0. };
    let back = if elder { 0.4 } else { 0.24 };
    let mut fins = vec![
        tail,
        Shape::at(l * 0.22, -h * 0.82)
            .quad(
                0.,
                -h * (1.15 + 0.6 * tall),
                -l * back,
                -h * 0.42 + wag * 0.25,
            )
            .0,
    ];
    if tall > 1. {
        let back = if elder { 0.36 } else { 0.24 };
        fins.push(
            Shape::at(l * 0.12, h * 0.85)
                .quad(
                    -l * 0.04,
                    h * (1.1 + 0.5 * tall),
                    -l * back,
                    h * 0.42 + wag * 0.25,
                )
                .0,
        );
    }
    pen.fill_many(&fins, rgba(hue.fin, 0.95 * a));
    let outline = body(l, h, wag);
    // Back dark, flank in the body colour, belly pale: three stops as a
    // fill and two washes, turned with the fish.
    let down = hue.down;
    pen.fill(&outline, rgba(hue.body, a));
    let dark = mix(hue.body, BLACK, 0.22);
    pen.fill(
        &outline,
        grad(down, rgba(dark, a), 0., rgba(dark, 0.), 0.45),
    );
    pen.fill(
        &outline,
        grad(
            down,
            rgba(hue.belly, 0.),
            0.45,
            rgba(hue.belly, 0.9 * a),
            1.,
        ),
    );
    if !fry {
        pattern(pen, sim, f, l, h, &outline, hue);
    }
    if elder {
        // The sheen: a slow band of light across the scales, as two halves
        // of one soft stripe.
        let sx = ((t * 0.4 + f.seed).rem_euclid(1.6) - 0.6) * l;
        let across = if f.face_s < 0. {
            down + 90.
        } else {
            down - 90.
        };
        for (x0, x1, from, to) in [(sx - l * 0.12, sx, 0., 0.35), (sx, sx + l * 0.12, 0.35, 0.)] {
            let band = [(x0, -h * 1.2), (x1, -h * 1.2), (x1, h * 1.2), (x0, h * 1.2)];
            pen.fill(
                &clip_convex(&band, &outline),
                grad(across, rgba(WHITE, from), 0., rgba(WHITE, to), 1.),
            );
        }
    }
    pen.stroke(&outline, true, 1., rgba([0., 10., 20.], 0.25 * a));
    pen.stroke(
        &arc(
            l * 0.36,
            0.,
            h * 0.62,
            h * 0.62,
            0.,
            PI * 0.62,
            PI * 1.38,
            12,
        ),
        false,
        s,
        rgba(BLACK, 0.22 * a),
    );
    let fin_keep = pen.m;
    pen.translate(l * 0.14, h * 0.28);
    pen.rotate(0.5 + (f.phase * 1.3).sin() * 0.4);
    pen.fill(
        &arc(-l * 0.06, 0., l * 0.1, l * 0.035, 0., 0., TAU, 12),
        rgba(hue.fin, 0.8 * a),
    );
    pen.m = fin_keep;
    let er = (l * if fry { 0.095 } else { 0.065 }).max(2.);
    eye(pen, l * 0.33, -h * 0.2, er, f.mood == Mood::Idle, a);
    if nt > 0.05 {
        for i in 0..6 {
            let at = (
                l * (0.2 - i as f32 * 0.085),
                h * 0.12 + wag * 0.06 * i as f32,
            );
            let glow = nt * (0.6 + 0.4 * (t * 3. + i as f32 + f.seed).sin());
            pen.glow(at, 3. * s, k.fin, glow * a);
        }
    }
}

/// The species' markings, inside the body.
fn pattern(pen: &mut Pen, sim: &Sim, f: &Fish, l: f32, h: f32, outline: &[P], hue: &Hue) {
    let (s, k) = (sim.s, f.species);
    match k.pattern {
        Pattern::Bands => {
            for bx in [l * 0.22, -l * 0.1] {
                let band = arc(bx, 0., l * 0.05, h * 1.2, 0., 0., TAU, 20);
                pen.fill(
                    &clip_convex(&band, outline),
                    rgba(mix(WHITE, hue.ink, hue.night), 1.),
                );
                pen.stroke_many(
                    &runs_inside(&band, outline),
                    false,
                    1.1 * s,
                    rgba([20., 10., 10.], 0.55),
                );
            }
        }
        Pattern::Mask => {
            let mask = arc(-l * 0.04, -h * 0.28, l * 0.3, h * 0.38, -0.08, 0., TAU, 24);
            pen.fill(&clip_convex(&mask, outline), rgba([8., 16., 48.], 0.7));
        }
        Pattern::Lines => {
            let lines = [l * 0.2, l * 0.04, -l * 0.12]
                .iter()
                .flat_map(|&bx| {
                    let line = (0..=12)
                        .map(|i| {
                            let u = i as f32 / 12.;
                            (bx + h * 0.2 - u * h * 0.4, -h * 1.1 + u * h * 2.2)
                        })
                        .collect::<Vec<_>>();
                    runs_inside(&line, outline)
                })
                .collect::<Vec<_>>();
            pen.stroke_many(&lines, false, 1.2 * s, rgba([70., 40., 0.], 0.45));
        }
        Pattern::Spots => {
            let spots = [
                (0.18, -0.3),
                (0.04, -0.45),
                (-0.1, -0.25),
                (0.08, 0.05),
                (-0.18, -0.05),
                (0.24, 0.1),
            ]
            .map(|(x, y)| circle(l * x, h * y, 1.6 * s));
            pen.fill_many(&spots, rgba(WHITE, 0.5));
        }
        Pattern::Plain => {}
    }
}
