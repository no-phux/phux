//! Every other harness's exotic: an octopus, its mantle pulsing and eight
//! tapering arms trailing behind.

use super::*;

pub(super) fn paint(pen: &mut Pen, sim: &Sim, f: &Fish, l: f32, hue: &Hue) {
    let (s, t) = (sim.s, sim.t);
    let pulse = ((f.phase * 0.6).sin() + 1.) / 2.;
    // Each arm is ten tapering segments; one path per shade and width.
    let mut segs: [[Vec<Vec<P>>; 10]; 2] = Default::default();
    for k in 0..8 {
        let b = (k as f32 / 7. - 0.5) * l * 0.16;
        let mut from = (l * 0.02, b);
        for j in 1..=10 {
            let jf = j as f32;
            let x = l * 0.02 - jf * l * 0.065 * (1. + pulse * 0.25);
            let y =
                b * (1. + jf * 0.12) + (jf * 0.7 - t * 4. + k as f32).sin() * l * 0.03 * (jf / 10.);
            segs[k % 2][j - 1].push(capsule(from, (x, y), l * (0.045 - jf * 0.0038)));
            from = (x, y);
        }
    }
    for (shade, widths) in segs.iter().enumerate() {
        let c = rgba(mix(hue.body, BLACK, 0.15 + 0.1 * shade as f32), 1.);
        for polys in widths {
            pen.fill_many(polys, c);
        }
    }
    let keep = pen.m;
    pen.translate(l * 0.18, 0.);
    pen.rotate(-0.15);
    let mantle = oval(
        0.,
        0.,
        l * 0.22 * (1. - pulse * 0.08),
        l * 0.16 * (1. + pulse * 0.1),
        0.,
    );
    body_fill(pen, &[mantle], hue);
    let spots = [(0.05, -0.08), (-0.06, -0.04), (0.1, 0.02), (-0.02, 0.06)]
        .map(|(x, y)| circle(l * x, l * y, 2. * s));
    pen.fill_many(&spots, rgba(mix(hue.belly, WHITE, 0.3), 0.5));
    pen.m = keep;
    eye(pen, l * 0.02, -l * 0.07, l * 0.04, f.mood == Mood::Idle, 1.);
}
