//! An agent's node: a glowing orb whose ring says its state, grown by its
//! work. A fry is a faint orb, an elder wears a halo ring, an exotic a moon
//! and a glow in its harness colour, a legend two moons, a pulsing corona
//! and a trail along its orbit. A level-up flashes a widening gold ring.

use super::super::tier::TIERS;
use super::*;

const GOLD: Hsla = Hsla {
    h: 44. / 360.,
    s: 1.,
    l: 0.77,
    a: 1.,
};

impl Paint {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn node(
        &self,
        at: Point<Pixels>,
        planet: Point<Pixels>,
        node: &Node,
        glow: f32,
        k: f32,
        time: f32,
        window: &mut gpui::Window,
    ) {
        let k = k.max(0.6);
        let z = k * TIERS[node.tier].size;
        let fry = node.tier == 0;
        let fade = if fry { 0.6 } else { 1. };
        let c = node.color.opacity(fade);
        let r = 7. * z;
        if node.tier == 4 {
            trail(window, at, planet, node.hue, glow, k);
        }
        if node.tier >= 3 {
            halo(window, at, r * 3.4, node.hue, 0.2 * glow);
        }
        let idle = node.mood == Mood::Idle;
        if !fry {
            halo(window, at, 24. * z, c, if idle { 0.1 } else { 0.22 } * glow);
        }
        fill(
            window,
            &ellipse(at, r, r),
            c.opacity(if idle { 0.6 } else { 1. } * fade),
        );
        let rim = Hsla {
            l: (c.l + 0.2).min(0.95),
            ..c
        };
        ring(window, at, r, rim.opacity(0.8 * fade), 1.);
        match node.mood {
            Mood::Working => {
                let spin = time / 1.6 * TAU;
                arc_stroke(window, at, 12. * z, spin, 100f32.to_radians(), c, 1.5);
            }
            Mood::Waiting => {
                let breathe = 0.75 + 0.25 * (time * TAU).sin();
                ring(window, at, 14. * z, c.opacity(breathe * fade), 1.5);
                let swell = time.rem_euclid(1.5) / 1.5;
                ring(
                    window,
                    at,
                    (30. + 26. * swell) * z,
                    c.opacity(0.6 * (1. - swell) * fade),
                    2.,
                );
            }
            Mood::Done => ring(window, at, 12. * z, c, 1.),
            Mood::Idle => {}
        }
        if node.tier >= 2 {
            ring(window, at, r * 1.6, c.opacity(0.45), 1.);
        }
        if node.tier >= 3 {
            let moons = if node.tier == 4 { 2 } else { 1 };
            for moon in 0..moons {
                let a = time / 6. * TAU + moon as f32 * PI;
                let p = point(at.x + px(a.cos() * r * 2.2), at.y + px(a.sin() * r * 2.2));
                fill(window, &ellipse(p, 2.2 * k, 2.2 * k), rim);
            }
        }
        if node.tier == 4 {
            for ray in 0..8 {
                let a = ray as f32 * TAU / 8. + time * 0.2;
                let pulse = 0.5 + 0.5 * (time * 3. + ray as f32).sin();
                let (c0, c1) = (r * 1.25, r * (1.6 + 0.5 * pulse));
                let (x, y) = (a.cos(), a.sin());
                line(
                    window,
                    point(at.x + px(x * c0), at.y + px(y * c0)),
                    point(at.x + px(x * c1), at.y + px(y * c1)),
                    node.hue.opacity(0.4 + 0.5 * pulse),
                    1.5,
                );
            }
        }
        if node.level_up > 0. {
            let lu = node.level_up;
            ring(window, at, r * (1.5 + (1. - lu) * 4.), GOLD.opacity(lu), 2.);
            halo(window, at, r * 3., GOLD, 0.5 * lu);
        }
    }
}

/// Motes fading behind a legend along its orbit round its planet.
fn trail(
    window: &mut gpui::Window,
    at: Point<Pixels>,
    planet: Point<Pixels>,
    hue: Hsla,
    glow: f32,
    k: f32,
) {
    let (dx, dy) = ((at.x - planet.x).as_f32(), (at.y - planet.y).as_f32());
    let (radius, angle) = (dx.hypot(dy), dy.atan2(dx));
    if radius < 1. {
        return;
    }
    for step in 1..=14 {
        let a = angle - step as f32 * 7. / radius;
        let p = point(
            planet.x + px(a.cos() * radius),
            planet.y + px(a.sin() * radius),
        );
        let size = (3.2 - step as f32 * 0.15) * k;
        let alpha = 0.75 * (1. - step as f32 / 15.) * glow.max(0.6);
        fill(window, &ellipse(p, size, size), hue.opacity(alpha));
    }
}
