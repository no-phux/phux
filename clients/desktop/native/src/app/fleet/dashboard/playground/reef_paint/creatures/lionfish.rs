//! Claude's exotic: a lionfish, its webbed dorsal spines, slow pectoral fans
//! and striped body.

use super::*;

pub(super) fn paint(pen: &mut Pen, sim: &Sim, f: &Fish, l: f32, wag: f32, hue: &Hue) {
    let (s, t) = (sim.s, sim.t);
    let h = l * 0.24;
    let ray = |bx: f32, by: f32, a: f32, len: f32| (bx + a.cos() * len, by + a.sin() * len);
    // Dorsal spines with a translucent web.
    let spines = (0..=10)
        .map(|i| {
            let u = i as f32 / 10.;
            let (bx, by) = (l * (0.24 - u * 0.5), -h * (0.75 - u * 0.25));
            let a = -PI / 2. - 0.35 - u * 0.6 + (t * 1.8 + i as f32).sin() * 0.05;
            ((bx, by), ray(bx, by, a, l * (0.5 - u * 0.22)))
        })
        .collect::<Vec<_>>();
    let mut web = vec![spines[0].0];
    web.extend(spines.iter().map(|(_, tip)| *tip));
    web.push(spines[10].0);
    pen.fill(&web, rgba(hue.fin, 0.2));
    let rays = spines
        .iter()
        .map(|(base, tip)| vec![*base, *tip])
        .collect::<Vec<_>>();
    pen.stroke_many(&rays, false, 1.3 * s, rgba(hue.fin, 0.95));
    // Pectoral fans, slowly opening and closing.
    let flap = (f.phase * 0.7).sin() * 0.22;
    let (fx, fy) = (l * 0.1, h * 0.35);
    let fan = (0..=9)
        .map(|i| {
            let len = l * (0.55 - (i as f32 - 4.5).abs() * 0.03);
            ray(fx, fy, 0.35 + flap + i as f32 * 0.2, len)
        })
        .collect::<Vec<_>>();
    let mut web = vec![(fx, fy)];
    web.extend_from_slice(&fan);
    pen.fill(&web, rgba(mix(hue.fin, hue.belly, 0.3), 0.3));
    let dashed = fan
        .iter()
        .flat_map(|&tip| dashes((fx, fy), tip, 3. * s, 2.5 * s))
        .collect::<Vec<_>>();
    pen.stroke_many(&dashed, false, 1.1 * s, rgba(hue.fin, 0.85));
    // A rounded tail fan.
    let tail = Shape::at(-l * 0.28, wag * 0.4)
        .quad(-l * 0.6, -h * 1.1 + wag, -l * 0.62, wag)
        .quad(-l * 0.6, h * 1.1 + wag, -l * 0.28, wag * 0.4)
        .0;
    pen.fill(&tail, rgba(hue.fin, 0.55));
    let outline = body(l, h, wag);
    body_fill(pen, std::slice::from_ref(&outline), hue);
    let stripes = (0..7)
        .flat_map(|i| {
            let bx = l * (0.36 - i as f32 * 0.1);
            let line = Shape::at(bx + h * 0.15, -h * 1.2)
                .quad(bx - h * 0.2, 0., bx + h * 0.1, h * 1.2)
                .0;
            runs_inside(&line, &outline)
        })
        .collect::<Vec<_>>();
    pen.stroke_many(
        &stripes,
        false,
        l * 0.035,
        rgba(mix([255., 246., 236.], hue.ink, hue.night), 0.9),
    );
    eye(
        pen,
        l * 0.34,
        -h * 0.25,
        l * 0.055,
        f.mood == Mood::Idle,
        1.,
    );
}
