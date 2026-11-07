//! The galaxy's painter, in phux-hud's language: a ticked bezel with a radar
//! sweep, a wireframe core in gyro rings, wireframe planets, and agents as
//! glowing orbs whose ring says their state. Beams carry pulses outward and
//! related agents are joined by arcs with a running light. Every motion is a
//! pure function of the clock, so a stopped clock is a clean still frame.
//! On the matrix map the same links carry packets between desks.

use std::f32::consts::{PI, TAU};

use gpui::{Bounds, Hsla, Pixels, Point, point, px};

use super::galaxy::{self, Mood, RING_GAP, SHELL};
use super::plan::{self, Pt};
use super::{Camera, Paint, ellipse, fill};

/// One planet: where, how many rings, whether its machine answers.
pub(super) struct Planet {
    pub centre: Pt,
    pub rings: usize,
    pub online: bool,
}

/// One agent: its planet, where it is, its state colour and mood.
pub(super) struct Node {
    pub planet: Pt,
    pub at: Pt,
    pub color: Hsla,
    pub mood: Mood,
}

/// The galaxy at this moment, ready to paint.
pub(super) struct Sky {
    pub core: Pt,
    pub orbit: f32,
    pub rim: f32,
    pub planets: Vec<Planet>,
    pub nodes: Vec<Node>,
    pub links: Vec<(Pt, Pt)>,
    /// The theme's light: every structure is drawn in it.
    pub light: Hsla,
    /// The needs-you colour while anything waits: the ping and the core's heart.
    pub ping: Option<Hsla>,
    /// The selected or hovered agent, for the reticle.
    pub focus: Option<Pt>,
    /// A light theme: glows stay low so the state colour reads.
    pub soft: bool,
}

impl Paint {
    pub(super) fn sky(
        &self,
        sky: &Sky,
        camera: &Camera,
        bounds: Bounds<Pixels>,
        time: f32,
        window: &mut gpui::Window,
    ) {
        let at = |world: Pt| -> Point<Pixels> {
            let s = camera.screen(world);
            point(bounds.origin.x + px(s.x), bounds.origin.y + px(s.y))
        };
        let k = camera.scale;
        let glow = if sky.soft { 0.5 } else { 1. };
        let light = sky.light;
        let core = at(sky.core);
        let rim = sky.rim * k;

        // Dust inside the bezel, each mote from its own seed.
        for index in 0..120 {
            let (u, v) = plan::seed(&format!("dust:{index}"));
            let (r, a) = (u.sqrt() * sky.rim, v * TAU);
            let twinkle = 0.25 + 0.35 * (1. + (time * (0.6 + v) + u * 40.).sin()) / 2.;
            let mote = at(Pt {
                x: sky.core.x + r * a.cos(),
                y: sky.core.y + r * a.sin(),
            });
            fill(
                window,
                &ellipse(mote, 1. + v, 1. + v),
                light.opacity(twinkle * glow),
            );
        }
        self.bezel(core, rim, light, glow, sky.ping, time, window);
        if !sky.planets.is_empty() {
            dashed(window, core, sky.orbit * k, 64, 0., self.faint, 1.);
        }
        for planet in &sky.planets {
            let alpha = if planet.online { 0.3 } else { 0.1 };
            flow(
                window,
                core,
                at(planet.centre),
                light,
                alpha,
                planet.online,
                time,
            );
            for ring in 0..planet.rings {
                let r = (SHELL + RING_GAP * ring as f32) * k;
                dashed(
                    window,
                    at(planet.centre),
                    r,
                    40,
                    0.,
                    self.faint.opacity(0.6),
                    1.,
                );
            }
        }
        self.arcs(&sky.links, sky.core, light, &at, time, window);
        if let Some(focus) = sky.focus {
            line(window, core, at(focus), light.opacity(0.4), 1.);
        }
        for node in &sky.nodes {
            let live = node.mood != Mood::Idle;
            flow(
                window,
                at(node.planet),
                at(node.at),
                node.color,
                0.25,
                live,
                time,
            );
        }
        for planet in &sky.planets {
            self.planet(
                at(planet.centre),
                planet.online,
                light,
                glow,
                k,
                time,
                window,
            );
        }
        for node in &sky.nodes {
            self.node(at(node.at), node, glow, k, time, window);
        }
        if let Some(focus) = sky.focus {
            reticle(window, at(focus), 22. * k.max(0.7), light, time);
        }
        self.core(
            core,
            light,
            sky.ping.unwrap_or(light),
            glow,
            k,
            time,
            window,
        );
        brackets(window, bounds, light.opacity(0.5));
    }

    #[allow(clippy::too_many_arguments)]
    fn bezel(
        &self,
        core: Point<Pixels>,
        rim: f32,
        light: Hsla,
        glow: f32,
        ping: Option<Hsla>,
        time: f32,
        window: &mut gpui::Window,
    ) {
        ring(window, core, rim, light.opacity(0.6 * glow.max(0.7)), 1.5);
        ring(
            window,
            core,
            rim * 0.88,
            light.opacity(0.3 * glow.max(0.7)),
            1.,
        );
        for degree in (0..360).step_by(5) {
            let a = degree as f32 * PI / 180.;
            let long = if degree % 30 == 0 { 14. } else { 6. };
            let (c, s) = (a.cos(), a.sin());
            line(
                window,
                point(core.x + px(c * rim), core.y + px(s * rim)),
                point(core.x + px(c * (rim + long)), core.y + px(s * (rim + long))),
                light.opacity(0.5),
                1.,
            );
        }
        let turn = time / 40. * TAU;
        for half in [0., PI] {
            arc_stroke(
                window,
                core,
                rim * 0.94,
                turn + half,
                0.55 * PI,
                light.opacity(0.5),
                3.,
            );
        }
        // The radar: a sweeping line with a fading wedge behind it.
        let sweep = time / 8. * TAU;
        let wedge = 25f32.to_radians();
        for slice in 0..6 {
            let a0 = sweep - wedge * slice as f32 / 6.;
            let a1 = sweep - wedge * (slice + 1) as f32 / 6.;
            let alpha = 0.18 * glow * (1. - slice as f32 / 6.);
            let tip = |a: f32| point(core.x + px(a.cos() * rim), core.y + px(a.sin() * rim));
            fill(window, &[core, tip(a0), tip(a1)], light.opacity(alpha));
        }
        let tip = point(
            core.x + px(sweep.cos() * rim),
            core.y + px(sweep.sin() * rim),
        );
        line(window, core, tip, light.opacity(0.6), 1.);
        if let Some(needs) = ping {
            let t = time.rem_euclid(4.) / 4.;
            ring(window, core, rim * t, needs.opacity(0.7 * (1. - t)), 2.);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn core(
        &self,
        at: Point<Pixels>,
        light: Hsla,
        heart_color: Hsla,
        glow: f32,
        k: f32,
        time: f32,
        window: &mut gpui::Window,
    ) {
        halo(window, at, 60. * k, light, 0.25 * glow);
        // A hexagon and its two inscribed triangles: an icosahedron's outline.
        let spin = time / 30. * TAU;
        let hex = (0..6)
            .map(|i| {
                let a = spin + i as f32 * TAU / 6.;
                point(at.x + px(a.cos() * 26. * k), at.y + px(a.sin() * 26. * k))
            })
            .collect::<Vec<_>>();
        stroke(window, &hex, light.opacity(0.6), 1.2);
        stroke(window, &[hex[0], hex[2], hex[4]], light.opacity(0.45), 1.);
        stroke(window, &[hex[1], hex[3], hex[5]], light.opacity(0.45), 1.);
        let heart = (9. + 1.5 * (time * PI).sin()) * k;
        halo(window, at, 20. * k, heart_color, 0.35 * glow);
        fill(window, &ellipse(at, heart, heart), heart_color);
        // Two gyros whose minor axes breathe, as if tilting in depth.
        for (r, rate, phase, dir) in [(38., 0.7, 0., 1.), (46., 0.5, 1., -1.)] {
            let minor = (time * rate + phase).cos().abs().max(0.15);
            let points = tilted(at, r * k, r * k * minor, dir * time * 0.4);
            stroke(window, &points, light.opacity(0.7), 1.);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn planet(
        &self,
        at: Point<Pixels>,
        online: bool,
        light: Hsla,
        glow: f32,
        k: f32,
        time: f32,
        window: &mut gpui::Window,
    ) {
        let edge = if online { light } else { self.faint };
        halo(
            window,
            at,
            34. * k,
            edge,
            if online { 0.18 } else { 0.08 } * glow,
        );
        // An octahedron's outline, turning by narrowing.
        let w = 16. * k * (0.6 + 0.4 * (time * 0.4).cos().abs());
        let h = 20. * k;
        let diamond = [
            point(at.x, at.y - px(h)),
            point(at.x + px(w), at.y),
            point(at.x, at.y + px(h)),
            point(at.x - px(w), at.y),
        ];
        if online {
            stroke(window, &diamond, edge, 1.5);
            line(window, diamond[0], diamond[2], edge.opacity(0.5), 1.);
            line(window, diamond[1], diamond[3], edge.opacity(0.5), 1.);
        } else {
            for i in 0..4 {
                dash_line(window, diamond[i], diamond[(i + 1) % 4], edge, 1.5);
            }
        }
        let (iw, ih) = (px(5. * k), px(7. * k));
        let inner = [
            point(at.x, at.y - ih),
            point(at.x + iw, at.y),
            point(at.x, at.y + ih),
            point(at.x - iw, at.y),
        ];
        fill(window, &inner, edge.opacity(0.9));
    }

    fn node(
        &self,
        at: Point<Pixels>,
        node: &Node,
        glow: f32,
        k: f32,
        time: f32,
        window: &mut gpui::Window,
    ) {
        let c = node.color;
        let k = k.max(0.6);
        let idle = node.mood == Mood::Idle;
        halo(window, at, 24. * k, c, if idle { 0.1 } else { 0.22 } * glow);
        fill(
            window,
            &ellipse(at, 7. * k, 7. * k),
            c.opacity(if idle { 0.6 } else { 1. }),
        );
        ring(
            window,
            at,
            7. * k,
            Hsla {
                l: (c.l + 0.2).min(0.95),
                ..c
            }
            .opacity(0.8),
            1.,
        );
        match node.mood {
            Mood::Working => {
                let spin = time / 1.6 * TAU;
                arc_stroke(window, at, 12. * k, spin, 100f32.to_radians(), c, 1.5);
            }
            Mood::Waiting => {
                let breathe = 0.75 + 0.25 * (time * TAU).sin();
                ring(window, at, 14. * k, c.opacity(breathe), 1.5);
                let swell = time.rem_euclid(1.5) / 1.5;
                ring(
                    window,
                    at,
                    (30. + 26. * swell) * k,
                    c.opacity(0.6 * (1. - swell)),
                    2.,
                );
            }
            Mood::Done => ring(window, at, 12. * k, c, 1.),
            Mood::Idle => {}
        }
    }

    /// Links on the galaxy: arcs bowing away from the core, each with a pulse
    /// of light running along it and a short tail behind.
    fn arcs(
        &self,
        links: &[(Pt, Pt)],
        core: Pt,
        light: Hsla,
        at: &dyn Fn(Pt) -> Point<Pixels>,
        time: f32,
        window: &mut gpui::Window,
    ) {
        for (index, &(a, b)) in links.iter().enumerate() {
            let curve = (0..=24)
                .map(|step| at(galaxy::arc(a, b, core, step as f32 / 24.)))
                .collect::<Vec<_>>();
            polyline(window, &curve, light.opacity(0.3), 1.5);
            let head = (time * 0.35 + index as f32 * 0.37).rem_euclid(1.);
            let tail = (0..=6)
                .map(|step| {
                    let t = (head - 0.12 + step as f32 * 0.02).clamp(0., 1.);
                    at(galaxy::arc(a, b, core, t))
                })
                .collect::<Vec<_>>();
            polyline(window, &tail, light.opacity(0.8), 2.5);
            let dot = at(galaxy::arc(a, b, core, head));
            fill(window, &ellipse(dot, 3.5, 3.5), light);
        }
    }

    /// Links on the matrix map: a faint wire between the two desks with
    /// three packets streaming along it.
    pub(super) fn packets(
        &self,
        links: &[(Pt, Pt)],
        camera: &Camera,
        bounds: Bounds<Pixels>,
        time: f32,
        window: &mut gpui::Window,
    ) {
        let at = |world: Pt| -> Point<Pixels> {
            let s = camera.screen(world);
            point(bounds.origin.x + px(s.x), bounds.origin.y + px(s.y))
        };
        let side = px(4. * camera.scale.max(0.6));
        for (index, &(a, b)) in links.iter().enumerate() {
            line(window, at(a), at(b), self.edge.opacity(0.25), 1.);
            for packet in 0..3 {
                let t = (time * 0.5 + packet as f32 / 3. + index as f32 * 0.21).rem_euclid(1.);
                let p = at(Pt {
                    x: a.x + (b.x - a.x) * t,
                    y: a.y + (b.y - a.y) * t,
                });
                let square = [
                    point(p.x - side / 2., p.y - side / 2.),
                    point(p.x + side / 2., p.y - side / 2.),
                    point(p.x + side / 2., p.y + side / 2.),
                    point(p.x - side / 2., p.y + side / 2.),
                ];
                fill(window, &square, self.edge.opacity(0.9));
            }
        }
    }
}

/// A beam with three bright dashes travelling from `a` to `b`; a dead beam is
/// a still dashed line.
fn flow(
    window: &mut gpui::Window,
    a: Point<Pixels>,
    b: Point<Pixels>,
    color: Hsla,
    alpha: f32,
    live: bool,
    time: f32,
) {
    if !live {
        dash_line(window, a, b, color.opacity(alpha), 1.);
        return;
    }
    line(window, a, b, color.opacity(alpha), 1.);
    let along = |t: f32| point(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
    for dash in 0..3 {
        let t = (time * 0.6 + dash as f32 / 3.).rem_euclid(1.);
        line(
            window,
            along(t),
            along((t + 0.06).min(1.)),
            color.opacity(0.9),
            1.5,
        );
    }
}

/// A soft glow: stacked discs fading outward, standing in for bloom.
fn halo(window: &mut gpui::Window, at: Point<Pixels>, r: f32, color: Hsla, alpha: f32) {
    for step in 1..=4 {
        let f = step as f32 / 4.;
        fill(
            window,
            &ellipse(at, r * f, r * f),
            color.opacity(alpha * 0.35),
        );
    }
}

/// Four corner brackets turning round `at`.
fn reticle(window: &mut gpui::Window, at: Point<Pixels>, r: f32, color: Hsla, time: f32) {
    let spin = time / 6. * TAU;
    for corner in 0..4 {
        let a = spin + corner as f32 * TAU / 4. + PI / 4.;
        arc_stroke(window, at, r, a - 0.3, 0.6, color, 1.5);
    }
}

/// The view's four corners, bracketed: the HUD's frame.
fn brackets(window: &mut gpui::Window, bounds: Bounds<Pixels>, color: Hsla) {
    let (inset, leg) = (px(10.), px(18.));
    let (l, t) = (bounds.origin.x + inset, bounds.origin.y + inset);
    let (r, b) = (
        bounds.origin.x + bounds.size.width - inset,
        bounds.origin.y + bounds.size.height - inset,
    );
    for (x, y, dx, dy) in [
        (l, t, 1., 1.),
        (r, t, -1., 1.),
        (l, b, 1., -1.),
        (r, b, -1., -1.),
    ] {
        polyline(
            window,
            &[point(x, y + leg * dy), point(x, y), point(x + leg * dx, y)],
            color,
            1.5,
        );
    }
}

fn tilted(at: Point<Pixels>, rx: f32, ry: f32, rot: f32) -> Vec<Point<Pixels>> {
    let (c, s) = (rot.cos(), rot.sin());
    (0..40)
        .map(|i| {
            let a = i as f32 / 40. * TAU;
            let (x, y) = (a.cos() * rx, a.sin() * ry);
            point(at.x + px(x * c - y * s), at.y + px(x * s + y * c))
        })
        .collect()
}

fn arc_stroke(
    window: &mut gpui::Window,
    at: Point<Pixels>,
    r: f32,
    start: f32,
    sweep: f32,
    color: Hsla,
    width: f32,
) {
    let steps = ((sweep / TAU * 64.).ceil() as usize).max(2);
    let points = (0..=steps)
        .map(|i| {
            let a = start + sweep * i as f32 / steps as f32;
            point(at.x + px(a.cos() * r), at.y + px(a.sin() * r))
        })
        .collect::<Vec<_>>();
    polyline(window, &points, color, width);
}

fn dash_line(
    window: &mut gpui::Window,
    a: Point<Pixels>,
    b: Point<Pixels>,
    color: Hsla,
    width: f32,
) {
    let along = |t: f32| point(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
    for dash in 0..12 {
        let t = dash as f32 / 12.;
        line(window, along(t), along(t + 0.5 / 12.), color, width);
    }
}

fn line(window: &mut gpui::Window, a: Point<Pixels>, b: Point<Pixels>, color: Hsla, width: f32) {
    polyline(window, &[a, b], color, width);
}

fn polyline(window: &mut gpui::Window, points: &[Point<Pixels>], color: Hsla, width: f32) {
    let mut path = gpui::PathBuilder::stroke(px(width));
    path.add_polygon(points, false);
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn stroke(window: &mut gpui::Window, points: &[Point<Pixels>], color: Hsla, width: f32) {
    let mut path = gpui::PathBuilder::stroke(px(width));
    path.add_polygon(points, true);
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn ring(window: &mut gpui::Window, centre: Point<Pixels>, r: f32, color: Hsla, width: f32) {
    let steps = (r / 3.).clamp(20., 160.) as usize;
    let points = (0..steps)
        .map(|i| {
            let a = i as f32 / steps as f32 * TAU;
            point(centre.x + px(a.cos() * r), centre.y + px(a.sin() * r))
        })
        .collect::<Vec<_>>();
    stroke(window, &points, color, width);
}

/// A circle as `dashes` dashes, each half its share of the way round, turned
/// by `turn` revolutions.
fn dashed(
    window: &mut gpui::Window,
    centre: Point<Pixels>,
    r: f32,
    dashes: usize,
    turn: f32,
    color: Hsla,
    width: f32,
) {
    let share = TAU / dashes as f32;
    for dash in 0..dashes {
        arc_stroke(
            window,
            centre,
            r,
            turn * TAU + dash as f32 * share,
            share * 0.5,
            color,
            width,
        );
    }
}
