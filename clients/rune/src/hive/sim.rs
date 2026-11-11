//! The hive in motion: the prototype's per-frame motion, the camera, and
//! keeping the bees in step with the fleet.

use std::f32::consts::{PI, TAU};

use phux_protocol::ids::ResourceId;

use super::{Bee, Cam, Forager, Hive, Pt, Task};
use crate::reef::{Mood, Rng, hash};

/// One fixed step of scene time: the same scene whatever the frame rate.
pub const STEP: f32 = 1.0 / 60.0;

fn ease(dt: f32, rate: f32) -> f32 {
    1.0 - (-dt * rate).exp()
}

/// Turn angle `a` toward `b` by `k`, the short way round.
fn ang_to(a: f32, b: f32, k: f32) -> f32 {
    let mut d = (b - a) % TAU;
    if d > PI {
        d -= TAU;
    }
    if d < -PI {
        d += TAU;
    }
    a + d * k
}

/// Steer `b` toward `to`, the prototype's `steer`; the distance left.
fn steer(b: &mut Bee, to: Pt, max_sp: f32, gain: f32, dt: f32, resp: f32) -> f32 {
    let (dx, dy) = (to[0] - b.x, to[1] - b.y);
    let d = dx.hypot(dy).max(1e-6);
    let sp = max_sp.min(d * gain);
    let k = ease(dt, resp);
    b.vx += (dx / d * sp - b.vx) * k;
    b.vy += (dy / d * sp - b.vy) * k;
    b.x += b.vx * dt;
    b.y += b.vy * dt;
    d
}

impl Hive {
    /// Bees for exactly `foragers`, keeping each one's place and motion.
    pub fn sync(&mut self, foragers: Vec<Forager>) {
        let mut old: Vec<Bee> = std::mem::take(&mut self.bees);
        let mut fresh = Vec::new();
        for who in foragers {
            match old.iter().position(|b| b.who.id == who.id) {
                Some(at) if old[at].who.host == who.host => {
                    let mut b = old.swap_remove(at);
                    if b.who.mood != who.mood {
                        if who.mood == Mood::Waiting {
                            b.burst = 1.0;
                        }
                        if who.mood == Mood::Working {
                            b.task = Task::ToFlower;
                            b.fi = self.pick_flower();
                        }
                        b.trail.clear();
                    }
                    b.who = who;
                    self.bees.push(b);
                }
                _ => {
                    fresh.push(self.bees.len());
                    self.bees.push(hatch(who, self.flowers.len()));
                }
            }
        }
        for gone in old {
            let cell = self
                .frames
                .get_mut(gone.who.host)
                .and_then(|f| f.cells.get_mut(gone.home));
            if let Some(c) = cell {
                c.home = false;
            }
        }
        for i in fresh {
            self.home(i);
            self.place(i);
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|id| !self.bees.iter().any(|b| &b.who.id == id))
        {
            self.selected = None;
        }
    }

    fn pick_flower(&mut self) -> usize {
        (self.rng.next() * self.flowers.len() as f32) as usize
    }

    /// Advance `dt` real seconds in fixed steps, slowed for reduced motion,
    /// then ease the camera.
    pub fn advance(&mut self, dt: f32) {
        if self.frames.is_empty() {
            return;
        }
        let dt = dt.clamp(0.0, 0.05) * if self.reduce { 0.4 } else { 1.0 };
        self.carry += dt;
        while self.carry >= STEP {
            self.carry -= STEP;
            self.step(STEP);
        }
        self.update_camera(dt);
    }

    /// The prototype's motion for one step: flowers sway, each bee acts out
    /// its mood, its cell fills and caps, pollen drifts.
    pub fn step(&mut self, dt: f32) {
        if self.frames.is_empty() || self.flowers.is_empty() {
            return;
        }
        self.t += dt;
        self.nt += (self.nt_target - self.nt) * ease(dt, 2.5);
        let (s, t, h) = (self.s, self.t, self.h);
        for fl in &mut self.flowers {
            let sw = (t * 0.9 + fl.ph).sin() * 4.0 * s;
            fl.x = fl.bx + sw;
            fl.y = fl.by + (t * 0.7 + fl.ph).cos() * 1.5;
        }
        let mut idle_k = vec![0usize; self.frames.len()];
        for i in 0..self.bees.len() {
            let c = self.home_of(&self.bees[i]);
            let (idle, rad) = (self.frame_of(&self.bees[i]).idle, self.r);
            let (r0, r1) = (self.rng.next(), self.rng.next());
            let nf = self.flowers.len();
            let b = &mut self.bees[i];
            let mood = b.who.mood;
            let l = b.len(s);
            b.flap += dt * if mood == Mood::Idle { 0.0 } else { 52.0 };
            b.burst = (b.burst - dt * 0.8).max(0.0);
            b.cap = (b.cap + dt * if mood == Mood::Done { 1.2 } else { -0.8 }).clamp(0.0, 1.0);
            if mood == Mood::Working {
                b.honey = (b.honey + dt * 0.025).min(1.0);
            }
            if mood != Mood::Done && b.cap == 0.0 && b.honey >= 1.0 {
                b.honey = 0.15;
            }
            match mood {
                Mood::Working => {
                    let fl = &self.flowers[b.fi.min(nf - 1)];
                    let wob = (t * 3.0 + b.ph).sin() * 5.0 * s;
                    if matches!(b.task, Task::ToFlower | Task::AtFlower) {
                        let to = [
                            fl.x + wob * 0.4,
                            fl.y - fl.pr * 0.4 + (t * 4.0 + b.ph).cos() * 3.0,
                        ];
                        let d = steer(b, to, 170.0 * s, 3.0, dt, 5.0);
                        if b.task == Task::ToFlower && d < 6.0 {
                            b.task = Task::AtFlower;
                            b.timer = 0.8 + r0 * 1.2;
                        }
                        b.timer -= dt;
                        if b.task == Task::AtFlower && b.timer < 0.0 {
                            b.task = Task::ToCell;
                            b.pollen = true;
                        }
                    } else {
                        let to = [c[0] + wob * 0.3, c[1] + (t * 5.0 + b.ph).cos() * 2.0];
                        let d = steer(b, to, 170.0 * s, 3.0, dt, 5.0);
                        if b.task == Task::ToCell && d < 5.0 {
                            b.task = Task::AtCell;
                            b.timer = 0.7 + r0;
                            b.pollen = false;
                            b.honey = (b.honey + 0.08).min(1.0);
                        }
                        b.timer -= dt;
                        if b.task == Task::AtCell && b.timer < 0.0 {
                            b.task = Task::ToFlower;
                            b.fi = (r1 * nf as f32) as usize;
                        }
                    }
                }
                Mood::Waiting => {
                    // The waggle dance: a figure eight over its cell, the
                    // waggle run along its middle.
                    b.wp += dt * 2.3;
                    let p = b.wp;
                    let den = 1.0 + p.sin().powi(2);
                    let a = (l * 1.5).max(rad * 2.4);
                    let wag = (t * 40.0).sin() * l * 0.14 * p.sin().abs().powi(4);
                    let to = [
                        c[0] + a * p.cos() / den,
                        c[1] + a * p.sin() * p.cos() / den + wag,
                    ];
                    steer(b, to, 300.0 * s, 9.0, dt, 10.0);
                }
                Mood::Done => {
                    let to = [
                        c[0] + (t * 1.3 + b.ph).sin() * 3.0,
                        c[1] - l * 0.6 + (t * 2.6 + b.ph).sin() * 2.5 * s,
                    ];
                    steer(b, to, 140.0 * s, 3.0, dt, 5.0);
                }
                Mood::Idle => {
                    let at = b.who.host.min(idle_k.len() - 1);
                    let k = idle_k[at];
                    idle_k[at] += 1;
                    let (rr, th) = (l * 0.62 * (k as f32).sqrt(), k as f32 * 2.4);
                    let to = [idle[0] + th.cos() * rr * 1.3, idle[1] + th.sin() * rr * 0.6];
                    steer(b, to, 120.0 * s, 2.5, dt, 5.0);
                }
            }
            b.y = b.y.clamp(
                super::SAFE_TOP + l * 0.5,
                (h - super::SAFE_BOTTOM - l * 0.3).max(0.0),
            );
            let sp = b.vx.hypot(b.vy);
            let want = match mood {
                Mood::Idle => b.rest_ang,
                Mood::Done if sp < 30.0 * s => -PI / 2.0,
                _ if sp > 12.0 * s => b.vy.atan2(b.vx),
                _ => b.ang,
            };
            let rate = if mood == Mood::Waiting { 14.0 } else { 6.0 };
            b.ang = ang_to(b.ang, want, ease(dt, rate));
            let far = b
                .trail
                .last()
                .is_none_or(|q| (q[0] - b.x).hypot(q[1] - b.y) > 2.0);
            if far {
                b.trail.push([b.x, b.y]);
                let keep = if mood == Mood::Waiting { 90 } else { 26 };
                if b.trail.len() > keep {
                    let extra = b.trail.len() - keep;
                    b.trail.drain(..extra);
                }
            }
        }
        let (w, nt) = (self.w, self.nt);
        for m in &mut self.motes {
            m.x += (6.0 + (t * 0.5 + m.ph).sin() * 6.0) * dt;
            m.y += (-m.v + (t * 0.8 + m.ph).cos() * 4.0) * dt;
            if m.y < -4.0 {
                m.y = h + 4.0;
                m.x = self.rng.next() * w;
            }
            if m.x > w + 4.0 {
                m.x = -4.0;
            }
        }
        if nt > 0.02 {
            for f in &mut self.flies {
                f.x += (t * f.sp + f.ph).cos() * 10.0 * dt;
                f.y += (t * f.sp * 1.3 + f.ph).sin() * 8.0 * dt;
            }
        }
    }

    pub fn selected_bee(&self) -> Option<&Bee> {
        let id = self.selected.as_ref()?;
        self.bees.iter().find(|b| &b.who.id == id)
    }

    pub fn bee(&self, id: &ResourceId) -> Option<&Bee> {
        self.bees.iter().find(|b| &b.who.id == id)
    }

    /// The selected bee at about 1.8×, kept inside the scene; else the
    /// whole scene. The reef's camera.
    pub fn camera_target(&self) -> Cam {
        let (w, h) = (self.w, self.h);
        let (mut x, mut y, mut z) = (w / 2.0, h / 2.0, 1.0);
        if let Some(b) = self.selected_bee() {
            z = if w < 640.0 { 1.6 } else { 1.85 };
            x = b.x;
            y = b.y;
        }
        let (hw, hh) = (w / (2.0 * z), h / (2.0 * z));
        Cam {
            x: x.clamp(hw, (w - hw).max(hw)),
            y: y.clamp(hh, (h - hh).max(hh)),
            z,
        }
    }

    fn update_camera(&mut self, dt: f32) {
        let to = self.camera_target();
        let k = ease(dt, if self.reduce { 12.0 } else { 2.6 });
        self.cam.x += (to.x - self.cam.x) * k;
        self.cam.y += (to.y - self.cam.y) * k;
        self.cam.z += (to.z - self.cam.z) * k;
    }

    /// World to screen through the camera: `world * z + o`.
    pub fn view(&self) -> (f32, Pt) {
        let z = self.cam.z;
        (
            z,
            [self.w / 2.0 - self.cam.x * z, self.h / 2.0 - self.cam.y * z],
        )
    }

    pub fn to_screen(&self, q: Pt) -> Pt {
        let (z, o) = self.view();
        [q[0] * z + o[0], q[1] * z + o[1]]
    }

    /// Bees in draw order: the idle cluster first, then the rest.
    pub fn draw_order(&self) -> Vec<&Bee> {
        let mut order: Vec<&Bee> = self.bees.iter().collect();
        order.sort_by_key(|b| b.who.mood != Mood::Idle);
        order
    }
}

/// A new bee, its rhythm seeded from who it is.
fn hatch(who: super::Forager, flowers: usize) -> Bee {
    let mut r = Rng::new(hash(&who.name));
    let fi = (r.next() * flowers as f32) as usize;
    let wp = r.next() * TAU;
    let honey = 0.2 + r.next() * 0.5;
    let ph = r.next() * TAU;
    let flap = r.next() * 10.0;
    let rest_ang = -0.3 + r.next() * 0.6 - PI / 2.0;
    let cap = if who.mood == Mood::Done { 1.0 } else { 0.0 };
    Bee {
        who,
        home: 0,
        x: 0.0,
        y: 0.0,
        vx: 0.0,
        vy: 0.0,
        ang: -PI / 2.0,
        task: Task::ToFlower,
        timer: 0.0,
        fi,
        pollen: false,
        trail: Vec::new(),
        wp,
        cap,
        honey,
        ph,
        burst: 0.0,
        flap,
        rest_ang,
    }
}
