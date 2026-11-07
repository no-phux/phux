//! OpenCode's exotic: an upright seahorse, its dorsal fin fluttering.

use super::*;

const SPINE: [P; 11] = [
    (0.06, -0.36),
    (0.0, -0.25),
    (0.07, -0.12),
    (0.1, 0.02),
    (0.05, 0.16),
    (-0.03, 0.26),
    (-0.09, 0.33),
    (-0.11, 0.41),
    (-0.05, 0.46),
    (0.0, 0.41),
    (-0.03, 0.37),
];
const WIDTH: [f32; 11] = [
    0.08, 0.08, 0.11, 0.12, 0.1, 0.075, 0.055, 0.04, 0.03, 0.022, 0.018,
];

fn lerp(a: f32, b: f32, k: f32) -> f32 {
    a + (b - a) * k
}

pub(super) fn paint(pen: &mut Pen, sim: &Sim, f: &Fish, l: f32, hue: &Hue) {
    let (s, t) = (sim.s, sim.t);
    let sway = (t * 1.4 + f.seed).sin() * 0.02;
    let at = |i: usize| {
        let (x, y) = SPINE[i];
        (l * (x + sway * (i as f32 / SPINE.len() as f32)), l * y)
    };
    let (dx, dy) = at(3);
    let mut fin = vec![(dx - l * 0.08, dy - l * 0.06)];
    for i in 0..=6 {
        let i = i as f32;
        fin.push((
            dx - l * (0.13 + 0.03 * (t * 22. + i).sin()),
            dy - l * 0.06 + i * l * 0.025,
        ));
    }
    fin.push((dx - l * 0.08, dy + l * 0.09));
    pen.fill(&fin, rgba(hue.fin, 0.55));
    // The body as a chain of discs along the spine, one gradient over all.
    let mut discs = Vec::new();
    for i in 0..SPINE.len() - 1 {
        let ((x0, y0), (x1, y1)) = (at(i), at(i + 1));
        for k in 0..4 {
            let u = k as f32 / 4.;
            let r = l * lerp(WIDTH[i], WIDTH[i + 1], u) * 0.5;
            discs.push(arc(lerp(x0, x1, u), lerp(y0, y1, u), r, r, 0., 0., TAU, 12));
        }
    }
    body_fill(pen, &discs, hue);
    let rings = (1..8)
        .map(|i| {
            let (x, y) = at(i);
            let half = l * WIDTH[i] * 0.5;
            vec![(x - half, y), (x + half, y)]
        })
        .collect::<Vec<_>>();
    pen.stroke_many(&rings, false, s, rgba(mix(hue.body, BLACK, 0.35), 0.5));
    let (hx, hy) = at(0);
    pen.fill(
        &oval(hx + l * 0.03, hy - l * 0.02, l * 0.075, l * 0.06, 0.3),
        rgba(hue.body, 1.),
    );
    pen.fill(
        &capsule(
            (hx + l * 0.06, hy - l * 0.01),
            (hx + l * 0.2, hy + l * 0.02),
            l * 0.04,
        ),
        rgba(hue.body, 1.),
    );
    let crown = [-0.03, 0., 0.03].map(|k: f32| {
        vec![
            (hx + l * (k - 0.012), hy - l * 0.07),
            (hx + l * k, hy - l * 0.12),
            (hx + l * (k + 0.012), hy - l * 0.07),
        ]
    });
    pen.fill_many(&crown, rgba(hue.fin, 1.));
    eye(
        pen,
        hx + l * 0.045,
        hy - l * 0.025,
        l * 0.025,
        f.mood == Mood::Idle,
        1.,
    );
}
