//! Codex's exotic: a manta ray seen from the side, its wings beating slowly.

use super::*;

pub(super) fn paint(pen: &mut Pen, sim: &Sim, f: &Fish, l: f32, wag: f32, hue: &Hue) {
    let s = sim.s;
    let flap = (f.phase * 0.5).sin();
    let wy = l * 0.5 * (0.65 + 0.35 * flap);
    let wy2 = l * 0.22 * (0.65 - 0.35 * flap);
    let tail = Shape::at(-l * 0.2, 0.)
        .quad(-l * 0.5, wag * 1.4, -l * 0.9, wag * 2.2)
        .0;
    pen.stroke(&tail, false, 1.6 * s, rgba(hue.body, 0.9));
    let far = Shape::at(l * 0.3, 0.)
        .quad(l * 0.05, wy2 * 0.7, -l * 0.1, wy2)
        .quad(-l * 0.1, wy2 * 0.4, -l * 0.22, 0.)
        .0;
    pen.fill(&far, rgba(mix(hue.body, BLACK, 0.35), 1.));
    let near = Shape::at(l * 0.36, -l * 0.02)
        .quad(l * 0.16, -wy * 0.95, -l * 0.08, -wy)
        .quad(-l * 0.12, -wy * 0.35, -l * 0.24, 0.)
        .quad(0., l * 0.08, l * 0.36, l * 0.03)
        .0;
    body_fill(pen, &[near], hue);
    let edge = Shape::at(l * 0.3, -l * 0.06)
        .quad(l * 0.14, -wy * 0.9, -l * 0.08, -wy)
        .0;
    pen.stroke(&edge, false, 1.6 * s, rgba(hue.fin, 0.8));
    pen.fill(
        &oval(l * 0.1, -l * 0.07, l * 0.07, l * 0.035, -0.5),
        rgba(mix(hue.belly, WHITE, 0.4), 0.75),
    );
    // The cephalic fins, curled forward.
    let horns = [
        Shape::at(l * 0.34, -l * 0.02)
            .quad(l * 0.44, -l * 0.04, l * 0.42, l * 0.03)
            .0,
        Shape::at(l * 0.34, l * 0.02)
            .quad(l * 0.44, l * 0.06, l * 0.4, l * 0.08)
            .0,
    ];
    pen.stroke_many(&horns, false, 2.4 * s, rgba(mix(hue.body, BLACK, 0.2), 1.));
    eye(pen, l * 0.3, -l * 0.035, l * 0.03, f.mood == Mood::Idle, 1.);
}
