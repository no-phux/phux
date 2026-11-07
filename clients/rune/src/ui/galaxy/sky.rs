//! The galaxy's drawing, in world points carried to the screen by the camera:
//! the bezel with its ticks, arcs, radar sweep and ping, the wireframe core
//! and planets, the agents' state nodes, beams with pulses flowing outward,
//! the kin beams and the reticle on the picked agent. Every motion is a pure
//! function of the clock, so a stopped clock is a clean still frame. An
//! agent's tier ([`crate::creature`]) sizes its node and adds a halo ring,
//! moons, a corona and a trail as it grows.

use std::f32::consts::{PI, TAU};

use rune::{Color, LineCap, Path, Shape};

use crate::activity::Mark;
use crate::creature::TIERS;
use crate::galaxy::{self, Camera, Plan, Pt, System};

const GOLD: Color = Color {
    r: 1.0,
    g: 0.76,
    b: 0.26,
    a: 1.0,
};

/// What an agent has grown into: its tier, its harness's colour, and how
/// much of its last level-up's burst is left (1 to 0).
pub(super) struct Grown {
    pub tier: usize,
    pub harness: Color,
    pub level_up: f32,
}

/// What a node shows for its mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum State {
    Needs,
    Working,
    Ok,
    Failed,
    Idle,
}

impl State {
    pub(super) fn of(mark: Mark) -> State {
        match mark {
            Mark::Blocked | Mark::Bell => State::Needs,
            Mark::Working | Mark::Running => State::Working,
            Mark::Done | Mark::Succeeded => State::Ok,
            Mark::Failed => State::Failed,
            Mark::Background | Mark::Idle | Mark::None => State::Idle,
        }
    }
}

pub(super) struct Sky {
    pub(super) camera: Camera,
    pub(super) shape: Shape,
    /// A light ground: halos stay faint so the state colour still reads.
    pub(super) light: bool,
}

impl Sky {
    fn at(&self, world: Pt) -> [f32; 2] {
        self.camera.screen(world)
    }

    fn arc(&self, centre: Pt, r: f32, from: f32, to: f32, n: usize) -> Vec<[f32; 2]> {
        let c = self.at(centre);
        let r = r * self.camera.scale;
        (0..=n)
            .map(|k| {
                let a = from + (to - from) * k as f32 / n as f32;
                [c[0] + r * a.cos(), c[1] + r * a.sin()]
            })
            .collect()
    }

    fn line(mut self, a: Pt, b: Pt, width: f32, color: Color) -> Sky {
        let (a, b) = (self.at(a), self.at(b));
        self.shape = self.shape.line(a, b, width, LineCap::Round, color);
        self
    }

    fn stroke(mut self, points: Vec<[f32; 2]>, closed: bool, width: f32, color: Color) -> Sky {
        self.shape = self
            .shape
            .stroke_with(points, closed, width, LineCap::Round, color);
        self
    }

    fn ring(self, centre: Pt, r: f32, width: f32, color: Color) -> Sky {
        let points = self.arc(centre, r, 0.0, TAU, 48);
        self.stroke(points, true, width, color)
    }

    fn disc(mut self, centre: Pt, r: f32, color: Color) -> Sky {
        let mut points = self.arc(centre, r, 0.0, TAU, 32);
        points.pop();
        self.shape = self.shape.fill(points, color);
        self
    }

    /// A soft glow: discs stacked from wide and faint to small and bright.
    fn halo(self, centre: Pt, r: f32, color: Color, strength: f32) -> Sky {
        let strength = if self.light { strength * 0.5 } else { strength };
        [1.0, 0.7, 0.45].into_iter().fold(self, |sky, k| {
            sky.disc(centre, r * k, color.alpha(strength * (1.1 - k) * 0.9))
        })
    }

    /// `dashes` dashes round a circle, each `share` of its step.
    fn dashed(
        mut self,
        centre: Pt,
        r: f32,
        (dashes, share): (usize, f32),
        width: f32,
        color: Color,
    ) -> Sky {
        let step = TAU / dashes as f32;
        for k in 0..dashes {
            let from = k as f32 * step;
            let points = self.arc(centre, r, from, from + step * share, 3);
            self.shape = self.shape.stroke(points, width, color);
        }
        self
    }

    /// Dust that twinkles inside the bezel, each mote at its own place.
    pub(super) fn dust(mut self, core: Pt, r: f32, time: f32, color: Color) -> Sky {
        for i in 0..120 {
            // The R2 low-discrepancy sequence, folded into the disc.
            let u = (0.5 + i as f32 * 0.754_877_7).fract();
            let v = (0.5 + i as f32 * 0.569_840_3).fract();
            let (d, a) = (r * u.sqrt(), v * TAU);
            let twinkle = 0.2 + 0.3 * (1.0 + (time * (0.6 + v) + u * 40.0).sin()) / 2.0;
            let mote = [core[0] + d * a.cos(), core[1] + d * a.sin()];
            self = self.disc(mote, 0.8 + v, color.alpha(twinkle));
        }
        self
    }

    /// The planets' shared orbit and each machine's agent rings, faint.
    pub(super) fn orbits(mut self, plan: &Plan, color: Color) -> Sky {
        if !plan.systems.is_empty() {
            self = self.dashed(plan.core, plan.orbit, (96, 0.5), 1.0, color);
        }
        for system in plan.systems.iter().filter(|s| s.count > 0) {
            for ring in 0..system.rings() {
                let r = System::ring_radius(ring);
                self = self.dashed(system.centre, r, (48, 0.5), 1.0, color.alpha(0.6));
            }
        }
        self
    }

    /// The bezel: rim, inner ring, ticks, two turning arcs, the radar sweep
    /// and, while anything needs you, a ping going out every four seconds.
    pub(super) fn bezel(
        mut self,
        core: Pt,
        r: f32,
        time: f32,
        light: Color,
        ping: Option<Color>,
    ) -> Sky {
        let s = self.camera.scale;
        self =
            self.ring(core, r, 1.5, light.alpha(0.6))
                .ring(core, 0.88 * r, 1.0, light.alpha(0.3));
        let c = self.at(core);
        for d in (0..360).step_by(5) {
            let a = (d as f32).to_radians();
            let long = if d % 30 == 0 { 14.0 } else { 6.0 };
            let (inner, outer) = (r * s, (r + long) * s);
            let from = [c[0] + inner * a.cos(), c[1] + inner * a.sin()];
            let to = [c[0] + outer * a.cos(), c[1] + outer * a.sin()];
            self.shape = self
                .shape
                .line(from, to, 1.0, LineCap::Butt, light.alpha(0.5));
        }
        let turn = time / 40.0 * TAU;
        for half in [0.0, PI] {
            let points = self.arc(core, 0.94 * r, turn + half, turn + half + 0.55 * PI, 40);
            self = self.stroke(points, false, 3.0, light.alpha(0.5));
        }
        // The sweep: its line, and a wedge behind it fading out.
        let sweep = time / 8.0 * TAU;
        let wedge = 25f32.to_radians();
        for k in 0..10 {
            let a = sweep - wedge * k as f32 / 10.0;
            let mut fan = vec![c];
            fan.extend(self.arc(core, r, a - wedge / 10.0, a, 2));
            self.shape = self
                .shape
                .fill(fan, light.alpha(0.18 * (1.0 - k as f32 / 10.0) * 0.6));
        }
        let tip = [core[0] + r * sweep.cos(), core[1] + r * sweep.sin()];
        self = self.line(core, tip, 1.0, light.alpha(0.5));
        if let Some(needs) = ping {
            let t = (time % 4.0) / 4.0;
            self = self.ring(
                core,
                30.0 + (r - 30.0) * t,
                2.0,
                needs.alpha(0.5 * (1.0 - t)),
            );
        }
        self
    }

    /// The core: a halo, a turning wireframe shell, a pulsing heart and two
    /// gyro rings whose tilt breathes.
    pub(super) fn core(mut self, core: Pt, time: f32, light: Color) -> Sky {
        self = self.halo(core, 60.0, light, 0.25);
        let turn = time / 30.0 * TAU;
        let corner = |k: usize, r: f32, off: f32| {
            let a = turn + off + k as f32 * TAU / 6.0;
            [core[0] + r * a.cos(), core[1] + r * a.sin()]
        };
        let hex: Vec<Pt> = (0..6).map(|k| corner(k, 26.0, 0.0)).collect();
        let screen = |sky: &Sky, pts: &[Pt]| pts.iter().map(|&p| sky.at(p)).collect::<Vec<_>>();
        let shell = screen(&self, &hex);
        self = self.stroke(shell, true, 1.2, light.alpha(0.6));
        for start in [0, 1] {
            let tri: Vec<Pt> = (0..3).map(|k| hex[(start + 2 * k) % 6]).collect();
            let tri = screen(&self, &tri);
            self = self.stroke(tri, true, 1.0, light.alpha(0.45));
        }
        for (r, tilt, spin) in [
            (38.0, (time * 0.7).cos(), 1.0),
            (46.0, (time * 0.5 + 1.0).cos(), -1.0),
        ] {
            let rot = spin * time * 0.6;
            let minor = 0.25 + 0.75 * tilt.abs();
            let ellipse: Vec<[f32; 2]> = (0..48)
                .map(|k| {
                    let a = k as f32 / 48.0 * TAU;
                    let (x, y) = (r * a.cos(), r * minor * a.sin());
                    self.at([
                        core[0] + x * rot.cos() - y * rot.sin(),
                        core[1] + x * rot.sin() + y * rot.cos(),
                    ])
                })
                .collect();
            self = self.stroke(ellipse, true, 1.0, light.alpha(0.7));
        }
        let beat = 1.5 * (time * PI).sin();
        self.disc(core, 9.0 + beat, light)
    }

    /// A machine: halo, a wireframe diamond turning by its width, and a solid
    /// diamond inside; dashed and dim when offline.
    pub(super) fn planet(mut self, centre: Pt, time: f32, color: Color, online: bool) -> Sky {
        self = self.halo(centre, 34.0, color, if online { 0.18 } else { 0.08 });
        let width = 0.6 + 0.4 * (time * 0.4).cos().abs();
        let diamond = |hw: f32, hh: f32| -> Vec<Pt> {
            vec![
                [centre[0], centre[1] - hh],
                [centre[0] + hw, centre[1]],
                [centre[0], centre[1] + hh],
                [centre[0] - hw, centre[1]],
            ]
        };
        let outer: Vec<_> = diamond(16.0 * width, 20.0)
            .iter()
            .map(|&p| self.at(p))
            .collect();
        if online {
            self = self.stroke(outer.clone(), true, 1.5, color);
            self = self
                .line(
                    [centre[0], centre[1] - 20.0],
                    [centre[0], centre[1] + 20.0],
                    1.0,
                    color.alpha(0.5),
                )
                .line(
                    [centre[0] - 16.0 * width, centre[1]],
                    [centre[0] + 16.0 * width, centre[1]],
                    1.0,
                    color.alpha(0.5),
                );
        } else {
            for k in 0..4 {
                let (a, b) = (outer[k], outer[(k + 1) % 4]);
                let mid = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
                self = self.stroke(vec![a, mid], false, 1.5, color);
            }
        }
        let inner: Vec<_> = diamond(6.0, 8.0).iter().map(|&p| self.at(p)).collect();
        self.shape = self.shape.fill(inner, color);
        self
    }

    /// An agent: halo, orb and the mark its state wears, all sized by its
    /// tier. A fry is faint without a halo; an elder wears a thin ring, an
    /// exotic a moon and its harness's glow too, a legendary two moons and a
    /// pulsing corona.
    pub(super) fn node(
        mut self,
        at: Pt,
        (state, color, needs): (State, Color, Color),
        time: f32,
        g: &Grown,
    ) -> Sky {
        let idle = state == State::Idle;
        let k = TIERS[g.tier].size;
        let fade = if g.tier == 0 { 0.6 } else { 1.0 };
        let r = 7.0 * k;
        if g.tier >= 3 {
            self = self.halo(at, 4.5 * r, g.harness, 0.3);
        }
        if g.tier > 0 {
            self = self.halo(at, 24.0 * k, color, if idle { 0.1 } else { 0.22 });
        }
        let ink = color.alpha(fade);
        self = match state {
            State::Working => {
                let a = time / 1.6 * TAU;
                let spinner = self.arc(at, 12.0 * k, a, a + 100f32.to_radians(), 12);
                self.stroke(spinner, false, 2.0, ink)
            }
            State::Needs => {
                let breath = 0.75 + 0.25 * (time * TAU).sin();
                let swell = (time % 1.5) / 1.5;
                self.ring(at, 14.0 * k, 2.0, needs.alpha(breath * fade))
                    .ring(
                        at,
                        (30.0 + 26.0 * swell) * k,
                        2.0,
                        needs.alpha(0.6 * (1.0 - swell) * fade),
                    )
            }
            State::Ok => self.ring(at, 12.0 * k, 1.0, ink),
            State::Failed => self.dashed(at, 12.0 * k, (8, 0.5), 1.5, ink),
            State::Idle => self,
        };
        if g.tier >= 2 {
            self = self.ring(at, 1.6 * r, 1.0, color.alpha(0.5));
        }
        for m in 0..g.tier.saturating_sub(2) {
            let a = time / 6.0 * TAU + m as f32 * PI;
            let moon = [at[0] + 2.2 * r * a.cos(), at[1] + 2.2 * r * a.sin()];
            self = self.disc(moon, 0.3 * r, g.harness);
        }
        if g.tier == 4 {
            let reach = r * (0.35 + 0.25 * (time * 3.0).sin());
            for i in 0..8 {
                let a = i as f32 * TAU / 8.0 + time * 0.2;
                let (c, s) = (a.cos(), a.sin());
                let from = [at[0] + 1.25 * r * c, at[1] + 1.25 * r * s];
                let to = [from[0] + reach * c, from[1] + reach * s];
                self = self.line(from, to, 1.5, g.harness.alpha(0.7));
            }
        }
        if g.level_up > 0.0 {
            let lu = g.level_up;
            self = self.halo(at, 5.6 * r, GOLD, 0.5 * lu).ring(
                at,
                4.0 * r * (0.5 + (1.0 - lu) * 1.8),
                3.0,
                GOLD.alpha(lu),
            );
        }
        let orb = if idle { color.alpha(0.6 * fade) } else { ink };
        self.disc(at, r, orb)
            .ring(at, r, 1.0, Color::WHITE.alpha(0.35 * fade))
    }

    /// A legendary's trail: motes fading behind it along its own orbit
    /// round `centre`.
    pub(super) fn trail(mut self, centre: Pt, at: Pt, color: Color) -> Sky {
        let (dx, dy) = (at[0] - centre[0], at[1] - centre[1]);
        for i in 1..=12 {
            let (s, c) = (-0.035 * i as f32).sin_cos();
            let mote = [centre[0] + dx * c - dy * s, centre[1] + dx * s + dy * c];
            let left = 1.0 - i as f32 / 13.0;
            self = self.disc(mote, 1.0 + 2.5 * left, color.alpha(0.6 * left));
        }
        self
    }

    /// A beam from `a` out to `b`: a faint line with three bright dashes
    /// flowing outward; dashed and still when offline.
    pub(super) fn beam(mut self, a: Pt, b: Pt, time: f32, color: Color, online: bool) -> Sky {
        let point = |t: f32| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
        if !online {
            for k in 0..10 {
                let t = k as f32 / 10.0;
                self = self.line(point(t), point(t + 0.05), 1.0, color.alpha(0.2));
            }
            return self;
        }
        self = self.line(a, b, 1.0, color.alpha(0.25));
        for k in 0..3 {
            let t = (time * 0.6 + k as f32 / 3.0).fract();
            self = self.line(point(t), point((t + 0.06).min(1.0)), 2.0, color.alpha(0.9));
        }
        self
    }

    /// Two related agents: a beam bowed away from the core, and a light
    /// running along it once every three seconds.
    pub(super) fn kin(mut self, a: Pt, b: Pt, core: Pt, time: f32, color: Color) -> Sky {
        let c = galaxy::bow(a, b, core);
        let path = Path::new()
            .move_to(self.at(a))
            .quad_to(self.at(c), self.at(b));
        self.shape = self
            .shape
            .stroke_path(&path, 1.5, LineCap::Round, color.alpha(0.45));
        let glow = galaxy::along(a, c, b, (time / 3.0).fract());
        self.disc(glow, 7.0, color.alpha(0.3))
            .disc(glow, 3.0, color)
    }

    /// The picked agent: four brackets turning round it, and a faint line
    /// from the core.
    pub(super) fn reticle(mut self, core: Pt, at: Pt, time: f32, color: Color) -> Sky {
        self = self.line(core, at, 1.0, color.alpha(0.4));
        let turn = time / 6.0 * TAU;
        for k in 0..4 {
            let a = turn + k as f32 * PI / 2.0;
            let bracket = self.arc(at, 22.0, a - 0.35, a + 0.35, 6);
            self = self.stroke(bracket, false, 2.0, color);
        }
        self
    }
}
