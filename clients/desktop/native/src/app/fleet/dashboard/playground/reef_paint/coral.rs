//! Buoys and the reefs: rocks, fans, branching and brain coral, anemones.

use super::*;

pub(super) fn buoy(pen: &mut Pen, sim: &Sim, index: usize, z: &Zone) {
    let (s, nt, t) = (sim.s, sim.nt, sim.t);
    let bx = z.cx;
    let by = sim.wave_y(bx);
    let tilt = (sim.wave_y(bx + 8.) - sim.wave_y(bx - 8.)).atan2(16.);
    let waiting = sim
        .fish
        .iter()
        .any(|f| f.zone == index && f.mood == Mood::Waiting);
    let rope = Shape::at(bx, by + 8. * s)
        .quad(
            bx + (t * 0.6 + index as f32).sin() * 14. * s,
            (by + z.home_y) / 2.,
            bx + 4. * s,
            z.home_y,
        )
        .0;
    pen.stroke(
        &rope,
        false,
        1.2 * s,
        grad(
            180.,
            rgba([230., 240., 240.], 0.35),
            0.,
            rgba([230., 240., 240.], 0.),
            1.,
        ),
    );
    let keep = pen.m;
    pen.translate(bx, by);
    pen.rotate(tilt);
    pen.fill(
        &arc(0., 0., 11. * s, 9. * s, 0., PI, TAU, 14),
        rgba(mix([200., 52., 48.], [90., 24., 30.], nt), 1.),
    );
    pen.fill(
        &[
            (-11. * s, -3.5 * s),
            (11. * s, -3.5 * s),
            (11. * s, 0.),
            (-11. * s, 0.),
        ],
        rgba(mix([238., 238., 230.], [110., 116., 124.], nt), 1.),
    );
    pen.fill(
        &arc(0., 0., 11. * s, 6. * s, 0., 0., PI, 14),
        rgba(mix([150., 40., 40.], [40., 14., 20.], nt), 0.85),
    );
    pen.stroke(
        &[(0., -9. * s), (0., -17. * s)],
        false,
        1.5 * s,
        rgba(mix([60., 60., 60.], [20., 20., 20.], nt), 1.),
    );
    let lit = if waiting {
        0.5 + 0.5 * (t * 6.).sin()
    } else {
        0.85
    };
    let lc = if waiting {
        Mood::Waiting.color()
    } else {
        Mood::Done.color()
    };
    pen.fill(&circle(0., -19. * s, 3. * s), rgba(lc, 0.4 + lit * 0.6));
    pen.glow(
        (0., -19. * s),
        (10. + 14. * nt) * s,
        lc,
        lit * (0.35 + 0.5 * nt),
    );
    pen.m = keep;
}

pub(super) fn tint(pal: &Pal, nt: f32, c: Rgb) -> Rgb {
    mix(c, pal.w1, 0.14 + nt * 0.55)
}

pub(super) fn reef_back(pen: &mut Pen, sim: &Sim, pal: &Pal, z: &Zone) {
    let (s, nt, t) = (sim.s, sim.nt, sim.t);
    for k in &z.rocks {
        let by = sim.sand_y(k.x) + k.ry * 0.35;
        pen.fill(
            &arc(k.x, by, k.rx, k.ry, 0., PI, TAU, 18),
            grad(
                160.,
                rgba(mix([96., 112., 120.], pal.w1, 0.3 + nt * 0.5), 1.),
                0.,
                rgba(mix([30., 44., 54.], pal.w2, 0.3 + nt * 0.5), 1.),
                1.,
            ),
        );
    }
    for f in &z.fans {
        let (bx, by) = (f.x, sim.sand_y(f.x) + 2. * s);
        let n = 9;
        let sway = (t * 0.9 + f.seed).sin() * 5. * s;
        let c = tint(pal, nt, f.col);
        let ends = (0..n)
            .map(|k| {
                let a = -PI * 0.86 + k as f32 / (n - 1) as f32 * PI * 0.72;
                let len = f.h * (0.78 + 0.22 * (k as f32 * 1.7 + f.seed).sin());
                (a.cos() * len * 0.8, a.sin() * len)
            })
            .collect::<Vec<_>>();
        let at = |e: P, fr: f32| (bx + e.0 * fr + sway * fr * fr, by + e.1 * fr);
        let mut web = vec![(bx, by)];
        web.extend(ends.iter().map(|&e| at(e, 1.)));
        pen.fill(&web, rgba(c, 0.28));
        let mut lines = ends
            .iter()
            .map(|&e| {
                let (cx, cy) = at(e, 0.55);
                let (x, y) = at(e, 1.);
                Shape::at(bx, by).quad(cx + e.0 * 0.04, cy, x, y).0
            })
            .collect::<Vec<_>>();
        for fr in [0.38, 0.62, 0.84] {
            lines.push(ends.iter().map(|&e| at(e, fr)).collect());
        }
        pen.stroke_many(&lines, false, 1.6 * s, rgba(c, 0.85));
    }
    for c in &z.corals {
        let (bx, by) = (c.x, sim.sand_y(c.x) + 3. * s);
        let sway = (t * 1.05 + c.seed).sin() * 4. * s;
        let sw = |y: f32| sway * (-y / c.hmax).max(0.).powf(1.5);
        let max_d = (c.by_depth.len().max(2) - 1) as f32;
        for (d, segs) in c.by_depth.iter().enumerate() {
            let color = tint(
                pal,
                nt,
                mix(mix(c.col, [40., 20., 40.], 0.35), c.col, d as f32 / max_d),
            );
            let caps = segs
                .iter()
                .map(|seg| {
                    capsule(
                        (bx + seg.x1 + sw(seg.y1), by + seg.y1),
                        (bx + seg.x2 + sw(seg.y2), by + seg.y2),
                        seg.w,
                    )
                })
                .collect::<Vec<_>>();
            pen.fill_many(&caps, rgba(color, 1.));
        }
        let tips = c
            .tips
            .iter()
            .map(|seg| circle(bx + seg.x2 + sw(seg.y2), by + seg.y2, seg.w * 0.7))
            .collect::<Vec<_>>();
        pen.fill_many(&tips, rgba(tint(pal, nt, mix(c.col, WHITE, 0.35)), 1.));
        if nt > 0.05 {
            for seg in &c.tips {
                let a = 0.55 * nt * (0.7 + 0.3 * (t * 2. + seg.x2).sin());
                pen.glow((bx + seg.x2 + sw(seg.y2), by + seg.y2), 7. * s, c.col, a);
            }
        }
    }
}

pub(super) fn reef_front(pen: &mut Pen, sim: &Sim, pal: &Pal, z: &Zone) {
    let (s, nt, t) = (sim.s, sim.nt, sim.t);
    for b in &z.brains {
        let by = sim.sand_y(b.x) + 4. * s;
        let c = tint(pal, nt, b.col);
        pen.fill(
            &arc(b.x, by, b.r, b.r * 0.78, 0., PI, TAU, 20),
            grad(
                160.,
                rgba(mix(c, WHITE, 0.25), 1.),
                0.,
                rgba(mix(c, [20., 10., 30.], 0.45), 1.),
                1.,
            ),
        );
        let ridges = (0..5)
            .map(|j| {
                let rise = b.r * (0.12 + j as f32 * 0.15);
                let half = b.r * (1. - (rise / (b.r * 0.78)).powi(2)).max(0.).sqrt() - 1.;
                let mut line = Vec::new();
                let mut x = b.x - half;
                while x <= b.x + half {
                    line.push((x, by - rise + (x * 0.35 + j as f32 * 2.).sin() * 2.2 * s));
                    x += 3.;
                }
                line
            })
            .collect::<Vec<_>>();
        pen.stroke_many(
            &ridges,
            false,
            1.2 * s,
            rgba(mix(c, [30., 10., 30.], 0.5), 0.55),
        );
    }
    for a in &z.anemones {
        let by = sim.sand_y(a.x) + 3. * s;
        let top = by - a.r * 0.9;
        let c = tint(pal, nt, a.col);
        pen.fill(
            &arc(
                a.x,
                top + a.r * 0.45,
                a.r * 0.6,
                a.r * 0.55,
                0.,
                0.,
                TAU,
                16,
            ),
            rgba(mix(c, [40., 20., 30.], 0.4), 1.),
        );
        let mut tips = Vec::new();
        let arms = (0..a.n)
            .map(|k| {
                let u = k as f32 / (a.n - 1) as f32 - 0.5;
                let x0 = a.x + u * a.r * 1.1;
                let ang = -PI / 2. + u * 2.3;
                let sw = (t * 1.7 + k as f32 * 0.55 + a.seed).sin() * 0.35;
                let tip = (
                    x0 + (ang + sw).cos() * a.len,
                    top + (ang + sw).sin() * a.len,
                );
                tips.push(tip);
                Shape::at(x0, top)
                    .quad(
                        x0 + ang.cos() * a.len * 0.55,
                        top + ang.sin() * a.len * 0.55,
                        tip.0,
                        tip.1,
                    )
                    .0
            })
            .collect::<Vec<_>>();
        pen.stroke_many(&arms, false, 3.2 * s, rgba(c, 1.));
        let dots = tips
            .iter()
            .map(|&(x, y)| circle(x, y, 2. * s))
            .collect::<Vec<_>>();
        pen.fill_many(&dots, rgba(mix(c, WHITE, 0.45), 1.));
        if nt > 0.05 {
            for &tip in &tips {
                pen.glow(tip, 6. * s, a.col, 0.5 * nt);
            }
        }
    }
}
