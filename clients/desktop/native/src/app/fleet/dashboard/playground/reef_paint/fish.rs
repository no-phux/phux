//! A fish: its species, its mood's aura, its night photophores.

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
    let h = l * 0.27 * k.fat;
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
    let tail = Shape::at(-l * 0.27, wag * 0.4)
        .quad(-l * 0.42, -h * 0.2 + wag * 0.7, -l * 0.57, -h * 0.95 + wag)
        .quad(-l * 0.47, wag * 0.95, -l * 0.57, h * 0.95 + wag)
        .quad(-l * 0.42, h * 0.2 + wag * 0.7, -l * 0.27, wag * 0.4)
        .0;
    let mut fins = vec![
        tail,
        Shape::at(l * 0.22, -h * 0.82)
            .quad(
                0.,
                -h * (1.15 + 0.6 * k.tall),
                -l * 0.24,
                -h * 0.42 + wag * 0.25,
            )
            .0,
    ];
    if k.tall > 1. {
        fins.push(
            Shape::at(l * 0.12, h * 0.85)
                .quad(
                    -l * 0.04,
                    h * (1.1 + 0.5 * k.tall),
                    -l * 0.24,
                    h * 0.42 + wag * 0.25,
                )
                .0,
        );
    }
    pen.fill_many(&fins, rgba(fin, 0.95));
    let outline = body(l, h, wag);
    // Back dark, flank in the body colour, belly pale: three stops as a
    // fill and two washes, turned with the fish.
    let down = 180. + rot.to_degrees();
    pen.fill(&outline, rgba(skin, 1.));
    let dark = mix(skin, BLACK, 0.22);
    pen.fill(
        &outline,
        grad(down, rgba(dark, 1.), 0., rgba(dark, 0.), 0.45),
    );
    pen.fill(
        &outline,
        grad(down, rgba(belly, 0.), 0.45, rgba(belly, 0.9), 1.),
    );
    match k.pattern {
        Pattern::Bands => {
            for bx in [l * 0.22, -l * 0.1] {
                let band = arc(bx, 0., l * 0.05, h * 1.2, 0., 0., TAU, 20);
                pen.fill(
                    &clip_convex(&band, &outline),
                    rgba(mix(WHITE, pal.ink, night), 1.),
                );
                pen.stroke_many(
                    &runs_inside(&band, &outline),
                    false,
                    1.1 * s,
                    rgba([20., 10., 10.], 0.55),
                );
            }
        }
        Pattern::Mask => {
            let mask = arc(-l * 0.04, -h * 0.28, l * 0.3, h * 0.38, -0.08, 0., TAU, 24);
            pen.fill(&clip_convex(&mask, &outline), rgba([8., 16., 48.], 0.7));
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
                    runs_inside(&line, &outline)
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
    pen.stroke(&outline, true, 1., rgba([0., 10., 20.], 0.25));
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
        rgba(BLACK, 0.22),
    );
    let fin_keep = pen.m;
    pen.translate(l * 0.14, h * 0.28);
    pen.rotate(0.5 + (f.phase * 1.3).sin() * 0.4);
    pen.fill(
        &arc(-l * 0.06, 0., l * 0.1, l * 0.035, 0., 0., TAU, 12),
        rgba(fin, 0.8),
    );
    pen.m = fin_keep;
    let (ex, ey, er) = (l * 0.33, -h * 0.2, (l * 0.065).max(2.));
    pen.fill(&circle(ex, ey, er), rgba([246., 251., 255.], 1.));
    let pupil = er * if f.mood == Mood::Idle { 0.45 } else { 0.62 };
    pen.fill(&circle(ex + er * 0.2, ey, pupil), rgba([7., 21., 33.], 1.));
    pen.fill(
        &circle(ex + er * 0.35, ey - er * 0.3, er * 0.22),
        rgba(WHITE, 1.),
    );
    if nt > 0.05 {
        for i in 0..6 {
            let at = (
                l * (0.2 - i as f32 * 0.085),
                h * 0.12 + wag * 0.06 * i as f32,
            );
            let a = nt * (0.6 + 0.4 * (t * 3. + i as f32 + f.seed).sin());
            pen.glow(at, 3. * s, k.fin, a);
        }
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
