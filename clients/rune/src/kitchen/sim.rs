//! The kitchen in motion: chefs walking to their goals, what the stations
//! throw into the air, and keeping the chefs in step with the fleet. The
//! prototype's `Math.random` is a seeded rng here, so a frame is the same
//! from the same start.

use std::f32::consts::TAU;

use super::{Chef, Cook, Goal, Kitchen};
use crate::creature;
use crate::reef::{Mood, Rgb, Rng, hash};

/// One fixed step of scene time: the same scene whatever the frame rate.
pub const STEP: f32 = 1.0 / 60.0;
const MAX_PARTS: usize = 420;
const MINT: Rgb = [98.0, 230.0, 180.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartKind {
    Steam,
    /// A wok's spark, falling.
    Spark,
    /// A chopped bit, falling.
    Bit,
    /// A flambé's flame, rising and shrinking.
    Flame,
    /// A finished chef's burst.
    Mint,
}

#[derive(Debug, Clone)]
pub struct Part {
    pub kind: PartKind,
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub life: f32,
    pub max: f32,
    pub r: f32,
    pub c: Rgb,
}

fn part(kind: PartKind, x: f32, y: f32, [vx, vy]: [f32; 2], r: f32, max: f32) -> Part {
    Part {
        kind,
        x,
        y,
        vx,
        vy,
        life: 0.0,
        max,
        r,
        c: [255.0; 3],
    }
}

impl Kitchen {
    /// Chefs for exactly `cooks`, keeping each one's place and stride.
    pub fn sync(&mut self, cooks: Vec<Cook>) {
        let mut old = std::mem::take(&mut self.chefs);
        for who in cooks {
            match old.iter().position(|c| c.who.id == who.id) {
                Some(at) => {
                    let mut c = old.swap_remove(at);
                    if c.who.mood != who.mood {
                        match who.mood {
                            Mood::Waiting => c.ring0 = self.t + 0.3,
                            Mood::Done => self.burst(c.x, c.y - 20.0 * self.cs),
                            _ => {}
                        }
                    }
                    c.tier = creature::tier(who.work.as_ref());
                    c.who = who;
                    self.chefs.push(c);
                }
                None => {
                    self.chefs.push(hire(who, self.t));
                }
            }
        }
        if self.lines.is_empty() {
            return;
        }
        self.replan(false);
        if self
            .selected
            .as_ref()
            .is_some_and(|id| !self.chefs.iter().any(|c| &c.who.id == id))
        {
            self.selected = None;
        }
    }

    fn burst(&mut self, x: f32, y: f32) {
        for _ in 0..12 {
            let (a, b) = (self.rng.next(), self.rng.next());
            let mut p = part(
                PartKind::Mint,
                x,
                y,
                [(a - 0.5) * 50.0, -30.0 - b * 40.0],
                2.2,
                0.9,
            );
            p.c = MINT;
            self.puff(p);
        }
    }

    fn puff(&mut self, p: Part) {
        if self.parts.len() <= MAX_PARTS {
            self.parts.push(p);
        }
    }

    /// Advance `dt` real seconds in fixed steps, slowed for reduced motion.
    pub fn advance(&mut self, dt: f32) {
        if self.lines.is_empty() {
            return;
        }
        let dt = dt.clamp(0.0, 0.05) * if self.reduce { 0.4 } else { 1.0 };
        self.carry += dt;
        while self.carry >= STEP {
            self.carry -= STEP;
            self.step(STEP);
        }
    }

    /// The prototype's `draw`, less the drawing: walk, then what the
    /// stations and breaks send up.
    pub fn step(&mut self, dt: f32) {
        self.t += dt;
        self.nt += (self.nt_target - self.nt) * (1.0 - (-dt * 2.5).exp());
        let cs = self.cs;
        for i in 0..self.chefs.len() {
            let c = &mut self.chefs[i];
            let mut sp = 95.0 * cs * dt;
            let moving = !c.path.is_empty();
            while let Some(&to) = c.path.first() {
                let (dx, dy) = (to[0] - c.x, to[1] - c.y);
                let d = dx.hypot(dy);
                c.up = dy < -2.0;
                if d <= sp {
                    (c.x, c.y) = (to[0], to[1]);
                    sp -= d;
                    c.path.remove(0);
                } else {
                    c.x += dx / d * sp;
                    c.y += dy / d * sp;
                    break;
                }
            }
            if moving {
                c.walk = (c.walk + dt * 6.0).min(1.0);
                c.ph += dt * 13.0;
            } else {
                c.walk = (c.walk - dt * 6.0).max(0.0);
            }
            if c.path.is_empty() {
                c.up = c.who.mood == Mood::Working;
            } else {
                continue;
            }
            let (x, y, ph, mood, goal) = (c.x, c.y, c.ph, c.who.mood, c.goal);
            let l = self.line_of(&self.chefs[i]);
            match (mood, goal) {
                (Mood::Working, Goal::Station(k)) => {
                    let s = &l.work[k];
                    let (sx, iy, kind) = (s.x, s.iy, s.kind);
                    self.cook(kind, [sx, iy], ph, dt);
                }
                // A sip of something hot on the break.
                (Mood::Idle, _) if self.rng.next() < dt * 1.5 => {
                    self.puff(part(
                        PartKind::Steam,
                        x + 5.0 * cs,
                        y - 34.0 * cs,
                        [0.0, -10.0],
                        1.5,
                        1.2,
                    ));
                }
                _ => {}
            }
        }
        for p in &mut self.parts {
            p.life += dt;
            p.x += p.vx * dt;
            p.y += p.vy * dt;
            match p.kind {
                PartKind::Steam => {
                    p.vx += (p.life * 3.0 + p.y * 0.05).sin() * 6.0 * dt;
                    p.r += dt * 5.0;
                }
                PartKind::Spark | PartKind::Bit => p.vy += 260.0 * dt,
                PartKind::Flame => {
                    p.r *= 1.0 - dt * 1.6;
                    p.vy -= 20.0 * dt;
                }
                PartKind::Mint => {}
            }
        }
        self.parts.retain(|p| p.life < p.max);
    }

    /// What a station sends up while its chef works it, prototype `prop`.
    fn cook(&mut self, kind: u8, [x, y]: [f32; 2], ph: f32, dt: f32) {
        let (t, cs, r) = (self.t, self.cs, 10.0 * self.cs);
        let rng = &mut self.rng;
        let mut out = Vec::new();
        match kind {
            0 => {
                if rng.next() < dt * 9.0 {
                    let (a, b) = (rng.next(), rng.next());
                    out.push(part(
                        PartKind::Steam,
                        x + (a - 0.5) * r,
                        y - r * 0.3,
                        [0.0, -18.0 - b * 10.0],
                        2.5 * cs,
                        1.6,
                    ));
                }
            }
            1 => {
                let chop = (t * 11.0 + ph).sin().abs();
                if chop > 0.97 && rng.next() < 0.5 {
                    let (a, b) = (rng.next(), rng.next());
                    let mut p = part(
                        PartKind::Bit,
                        x + 2.0,
                        y,
                        [(a - 0.5) * 60.0, -50.0 - b * 30.0],
                        2.0,
                        0.35,
                    );
                    p.c = [124.0, 195.0, 90.0];
                    out.push(p);
                }
            }
            2 => {
                if rng.next() < dt * 14.0 {
                    let (a, b, c) = (rng.next(), rng.next(), rng.next());
                    out.push(part(
                        PartKind::Spark,
                        x + (a - 0.5) * r,
                        y,
                        [(b - 0.5) * 70.0, -40.0 - c * 60.0],
                        2.0,
                        0.5,
                    ));
                }
                let toss = ((t + ph) % 2.4) / 2.4;
                let flambe = if self.nt > 0.5 { 0.35 } else { 0.08 };
                if toss < 0.02 && rng.next() < flambe * 4.0 {
                    for _ in 0..14 {
                        let (a, b, c, d, e) =
                            (rng.next(), rng.next(), rng.next(), rng.next(), rng.next());
                        out.push(part(
                            PartKind::Flame,
                            x + (a - 0.5) * r,
                            y - b * 6.0,
                            [(c - 0.5) * 30.0, -40.0 - d * 50.0],
                            (8.0 + e * 8.0) * cs,
                            0.6 + rng.next() * 0.4,
                        ));
                    }
                }
            }
            _ => {}
        }
        for p in out {
            self.puff(p);
        }
    }
}

/// A new chef, its looks and rhythm seeded from who it is, at no spot yet.
fn hire(who: Cook, t: f32) -> Chef {
    let seed = hash(&format!("{}{:?}", who.name, who.id));
    let ph = Rng::new(seed).next() * TAU;
    Chef {
        tier: creature::tier(who.work.as_ref()),
        who,
        seed,
        x: f32::NAN,
        y: f32::NAN,
        ph,
        ring0: t,
        walk: 0.0,
        up: false,
        goal: Goal::Break(0),
        path: Vec::new(),
    }
}
