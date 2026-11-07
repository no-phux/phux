//! The frame back to front, and the open water: ridges, jellyfish, rays,
//! sky, bubbles and marine snow.

use super::*;

/// The whole frame, back to front, as the prototype's `draw()`.
pub(super) fn draw(sim: &Sim, focus: Option<usize>, bounds: Bounds<Pixels>, window: &mut Window) {
    let (ox, oy) = (bounds.origin.x.as_f32(), bounds.origin.y.as_f32());
    let pal = Pal::at(sim.nt);
    let (w, h, sy) = (sim.w, sim.h, sim.surface_y);
    let mut pen = Pen {
        win: window,
        m: view(sim, 1., ox, oy),
    };
    // Water: three stops as two gradients.
    let mid = sy + (h - sy) * 0.45;
    let water = |y0: f32, y1: f32| vec![(-20., y0), (w + 20., y0), (w + 20., y1), (-20., y1)];
    pen.fill(
        &water(sy - 10., mid),
        grad(180., rgba(pal.w0, 1.), 0., rgba(pal.w1, 1.), 1.),
    );
    pen.fill(
        &water(mid - 0.5, h + 50.),
        grad(180., rgba(pal.w1, 1.), 0., rgba(pal.w2, 1.), 1.),
    );
    pen.m = view(sim, 0.45, ox, oy);
    far(&mut pen, sim, &pal);
    pen.m = view(sim, 1., ox, oy);
    rays(&mut pen, sim);
    sky(&mut pen, sim, &pal);
    for k in sim.kelp.iter().filter(|k| k.back) {
        kelp(&mut pen, sim, &pal, k, 0.7);
    }
    sand(&mut pen, sim, &pal);
    for (index, zone) in sim.zones.iter().enumerate() {
        buoy(&mut pen, sim, index, zone);
    }
    for k in sim.kelp.iter().filter(|k| !k.back && !k.front) {
        kelp(&mut pen, sim, &pal, k, 1.);
    }
    for zone in &sim.zones {
        reef_back(&mut pen, sim, &pal, zone);
    }
    for zone in &sim.zones {
        reef_front(&mut pen, sim, &pal, zone);
    }
    for (index, f) in sim.fish.iter().enumerate() {
        fish(&mut pen, sim, &pal, f, focus == Some(index));
    }
    bubbles(&mut pen, sim);
    snow(&mut pen, sim);
    for k in sim.kelp.iter().filter(|k| k.front) {
        kelp(&mut pen, sim, &pal, k, 1.);
    }
    // Vignette: the edges darken toward the abyss. An inset shadow of a
    // frame larger than the scene, so only its rounded hole shows.
    let short = w.min(h);
    let reach = short * 0.3;
    let spread = reach + short * 0.04;
    let frame = Bounds {
        origin: point(bounds.origin.x - px(reach), bounds.origin.y - px(reach)),
        size: size(
            bounds.size.width + px(2. * reach),
            bounds.size.height + px(2. * reach),
        ),
    };
    pen.win.paint_inset_shadows(
        frame,
        Corners::all(px(spread + short * 0.3)),
        &[BoxShadow {
            color: rgba([0., 8., 16.], 0.35 + sim.nt * 0.25),
            offset: point(px(0.), px(0.)),
            blur_radius: px(short * 0.22),
            spread_radius: px(spread),
            inset: true,
        }],
    );
}

pub(super) fn far(pen: &mut Pen, sim: &Sim, pal: &Pal) {
    let (w, h, s, nt) = (sim.w, sim.h, sim.s, sim.nt);
    let ridge = |pts: &[P]| {
        let mut out = vec![(-80., h + 40.)];
        out.extend_from_slice(pts);
        out.push((w + 80., h + 40.));
        out
    };
    pen.fill(&ridge(&sim.far_b), rgba(pal.far2, 0.55));
    let school = sim
        .amb
        .iter()
        .flat_map(|a| {
            let size = 5. * s;
            let dir = if a.vx < 0. { -1. } else { 1. };
            [
                arc(a.x, a.y, size, size * 0.38, 0., 0., TAU, 10),
                vec![
                    (a.x - dir * size * 0.8, a.y),
                    (a.x - dir * size * 1.6, a.y - size * 0.5),
                    (a.x - dir * size * 1.6, a.y + size * 0.5),
                ],
            ]
        })
        .collect::<Vec<_>>();
    pen.fill_many(
        &school,
        rgba(mix(pal.far2, [210., 240., 250.], 0.35 * (1. - nt)), 0.5),
    );
    pen.fill(&ridge(&sim.far_a), rgba(mix(pal.far, pal.w2, 0.15), 0.9));
    for j in &sim.jellies {
        let k = j.s * s;
        let y = j.by + (sim.t * 0.18 + j.ph).sin() * 50. * s;
        let c = ((sim.t * 1.7 + j.ph).sin() + 1.) / 2.;
        let (bw, bh) = (20. * k * (1. + c * 0.12), 15. * k * (1. - c * 0.15));
        let keep = pen.m;
        pen.translate(j.x, y);
        if nt > 0.02 {
            pen.glow((0., 0.), 60. * k, [255., 120., 220.], 0.35 * nt);
        }
        let tentacles = (0..6)
            .map(|n| {
                let x0 = (n as f32 / 5. - 0.5) * bw * 1.4;
                (0..=8)
                    .map(|i| {
                        let wave = (sim.t * 2. + j.ph + n as f32 + i as f32 * 0.7).sin();
                        (x0 + wave * 3. * k * i as f32 / 8., i as f32 * 7. * k)
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        pen.stroke_many(
            &tentacles,
            false,
            1.1 * s,
            rgba([255., 190. + nt * 40., 240.], 0.32 + nt * 0.35),
        );
        let mut bell = arc(0., 0., bw, bh, 0., PI, TAU, 18);
        bell = Shape(bell).quad(0., bh * 0.35, -bw, 0.).0;
        pen.fill(
            &bell,
            grad(
                180.,
                rgba([255., 236., 250.], 0.55 + nt * 0.3),
                0.,
                rgba([255., 140., 210.], 0.12 + nt * 0.2),
                1.,
            ),
        );
        pen.m = keep;
    }
}

pub(super) fn rays(pen: &mut Pen, sim: &Sim) {
    let (nt, s, sy, t) = (sim.nt, sim.s, sim.surface_y, sim.t);
    if nt > 0.97 {
        return;
    }
    let depth = sim.floor_y - sy + 40. * s;
    for r in &sim.rays {
        let a = (0.09 + 0.06 * (t * r.sp + r.ph).sin()) * (1. - nt);
        let x = r.x + (t * 0.13 + r.ph).sin() * 20. * s;
        pen.fill(
            &[
                (x - r.w / 2., sy),
                (x + r.w / 2., sy),
                (x + r.skew + r.w * 1.3, sy + depth),
                (x + r.skew - r.w * 1.3, sy + depth),
            ],
            grad(
                180.,
                rgba([235., 255., 250.], a * 1.4),
                0.,
                rgba([235., 255., 250.], 0.),
                1.,
            ),
        );
    }
}

pub(super) fn wave(sim: &Sim) -> Vec<P> {
    let mut pts = Vec::new();
    let mut x = -20.;
    while x <= sim.w + 20. {
        pts.push((x, sim.wave_y(x)));
        x += 10.;
    }
    pts
}

pub(super) fn sky(pen: &mut Pen, sim: &Sim, pal: &Pal) {
    let (w, h, s, sy, nt, t) = (sim.w, sim.h, sim.s, sim.surface_y, sim.nt, sim.t);
    // The bright underside of the surface; the sky covers its top half.
    pen.fill(
        &[
            (-20., sy - 4.),
            (w + 20., sy - 4.),
            (w + 20., sy + 34. * s),
            (-20., sy + 34. * s),
        ],
        grad(
            180.,
            rgba([220., 255., 255.], 0.32 - nt * 0.2),
            0.,
            rgba([220., 255., 255.], 0.),
            1.,
        ),
    );
    let mut air = wave(sim);
    air.push((w + 20., -h * 0.2));
    air.push((-20., -h * 0.2));
    pen.fill(
        &air,
        grad(180., rgba(pal.sky0, 1.), 0., rgba(pal.sky1, 1.), 1.),
    );
    let lowest = sy - 4.6 * s;
    let clip = rect(pen, (-20., -h), (w + 20., lowest));
    pen.clipped(clip, |pen| {
        if nt > 0.02 {
            // Stars twinkle in four brightness steps, one path each.
            let mut steps: [Vec<Vec<P>>; 4] = Default::default();
            for star in &sim.stars {
                let a = 0.45 + 0.45 * (t * 2. + star.tw).sin();
                let step = ((a / 0.9) * 3.99) as usize;
                let (x, y, r) = (star.x, star.y, star.r);
                steps[step.min(3)].push(vec![(x, y), (x + r, y), (x + r, y + r), (x, y + r)]);
            }
            for (i, polys) in steps.iter().enumerate() {
                let a = (i as f32 + 0.5) / 4. * 0.9;
                pen.fill_many(polys, rgba([230., 240., 255.], nt * a));
            }
        }
        let (ox, oy, sr) = (w * 0.8, sy * 0.42, 22. * s);
        pen.glow(
            (ox, oy),
            sr * 5.,
            mix([255., 248., 220.], [200., 220., 255.], nt),
            0.55,
        );
        pen.fill(
            &circle(ox, oy, sr * (1. - nt * 0.35)),
            rgba(mix([255., 252., 236.], [226., 234., 255.], nt), 1.),
        );
    });
    pen.stroke(&wave(sim), false, 1.4 * s, rgba(WHITE, 0.6 - nt * 0.3));
    for r in &sim.ripples {
        let k = r.a / 1.8;
        let rx = (8. + r.a * 46.) * s;
        let color = if r.amber {
            rgba([255., 190., 90.], (1. - k) * 0.85)
        } else {
            rgba(WHITE, (1. - k) * 0.5)
        };
        pen.stroke(
            &arc(r.x, sim.wave_y(r.x) + 1., rx, rx * 0.16, 0., 0., TAU, 24),
            true,
            1.3 * s,
            color,
        );
    }
}

pub(super) fn bubbles(pen: &mut Pen, sim: &Sim) {
    let (s, nt) = (sim.s, sim.nt);
    let rings = sim
        .bubbles
        .iter()
        .map(|b| circle(b.x, b.y, b.r))
        .collect::<Vec<_>>();
    pen.fill_many(&rings, rgba(WHITE, 0.12));
    pen.stroke_many(&rings, true, s, rgba([230., 250., 255.], 0.55 - nt * 0.15));
    let shine = sim
        .bubbles
        .iter()
        .map(|b| {
            let (x, y, q) = (b.x - b.r * 0.45, b.y - b.r * 0.5, b.r * 0.35);
            vec![(x, y), (x + q, y), (x + q, y + q), (x, y + q)]
        })
        .collect::<Vec<_>>();
    pen.fill_many(&shine, rgba(WHITE, 0.7));
}

pub(super) fn snow(pen: &mut Pen, sim: &Sim) {
    let (s, nt) = (sim.s, sim.nt);
    let mut sizes: [Vec<Vec<P>>; 3] = Default::default();
    for p in &sim.snow {
        if p.y < sim.wave_y(p.x) {
            continue;
        }
        if nt > 0.05 && p.g > 0.05 {
            pen.glow(
                (p.x, p.y),
                (2. + p.g * 7.) * s,
                [110., 240., 255.],
                nt * p.g * 0.9,
            );
        }
        let q = p.r * s;
        sizes[(((p.r - 0.4) / 1.6 * 3.) as usize).min(2)].push(vec![
            (p.x, p.y),
            (p.x + q, p.y),
            (p.x + q, p.y + q),
            (p.x, p.y + q),
        ]);
    }
    for (i, flakes) in sizes.iter().enumerate() {
        let r = 0.4 + (i as f32 + 0.5) / 3. * 1.6;
        let color = if nt > 0.05 {
            rgba(
                [150., 240., 255.],
                nt * (0.15 + r * 0.1) + (1. - nt) * (0.18 + r * 0.12),
            )
        } else {
            rgba([240., 250., 250.], 0.18 + r * 0.12)
        };
        pen.fill_many(flakes, color);
    }
}
