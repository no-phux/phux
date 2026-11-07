//! Gemini's exotic: a cuttlefish, its fin skirts rippling and dark bands
//! passing down its mantle.

use super::*;

pub(super) fn paint(pen: &mut Pen, sim: &Sim, f: &Fish, l: f32, wag: f32, hue: &Hue) {
    let t = sim.t;
    let (bl, bh) = (l * 0.42, l * 0.16);
    let x_at = |u: f32| -bl * 0.95 + (bl * 0.75 + bl * 0.95) * u;
    let edge = |x: f32| bh * (1. - (x / bl).powi(2)).max(0.).sqrt();
    let skirts = [-1., 1.].map(|side: f32| {
        let mut pts = (0..=24)
            .map(|i| {
                let u = i as f32 / 24.;
                let x = x_at(u);
                let ripple = l * 0.05 * (0.6 + 0.4 * (u * 16. - t * 9.).sin());
                (x, side * edge(x) + side * ripple)
            })
            .collect::<Vec<_>>();
        pts.extend((0..=24).rev().map(|i| {
            let x = x_at(i as f32 / 24.);
            (x, side * edge(x) * 0.9)
        }));
        pts
    });
    pen.fill_many(&skirts, rgba(hue.fin, 0.45));
    let arms = (0..8)
        .map(|k| {
            let y0 = (k as f32 / 7. - 0.5) * bh * 0.9;
            let curl = (t * 3. + k as f32).sin() * l * 0.015;
            Shape::at(bl * 0.95, y0)
                .quad(bl * 1.2, y0 * 1.3 + curl, bl * 1.38, y0 * 1.1 + wag * 0.3)
                .0
        })
        .collect::<Vec<_>>();
    pen.stroke_many(
        &arms,
        false,
        l * 0.022,
        rgba(mix(hue.body, hue.belly, 0.3), 1.),
    );
    let mantle = oval(0., 0., bl, bh, 0.);
    body_fill(pen, std::slice::from_ref(&mantle), hue);
    let bands = (0..5)
        .map(|k| {
            let x = ((k as f32 / 5.) + t * 0.3 + f.seed).rem_euclid(1.) * 2.4 * bl - 1.2 * bl;
            clip_convex(&oval(x, 0., bl * 0.07, bh * 1.2, 0.25), &mantle)
        })
        .collect::<Vec<_>>();
    pen.fill_many(&bands, rgba(mix(hue.body, [10., 0., 30.], 0.55), 0.4));
    pen.fill(
        &oval(bl * 0.92, 0., bh * 0.75, bh * 0.7, 0.),
        rgba(hue.body, 1.),
    );
    // The W-shaped pupil.
    let (ex, ey, er) = (bl * 0.9, -bh * 0.3, l * 0.04);
    pen.fill(&circle(ex, ey, er), rgba([246., 251., 255.], 1.));
    let w = [
        (-0.6, -0.2),
        (-0.3, 0.35),
        (0., 0.),
        (0.3, 0.35),
        (0.6, -0.2),
    ]
    .map(|(x, y)| (ex + er * x, ey + er * y));
    pen.stroke(&w, false, er * 0.4, rgba([7., 21., 33.], 1.));
}
