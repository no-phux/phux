//! The pit lane in motion: the prototype's `step`, less its demo's random
//! state changes, and keeping the cars in step with the fleet.
//!
//! Two of the prototype's rough spots are smoothed here. A car that comes
//! to need you pushes on to the pit entry, faster and overtaking, instead of
//! taking up to a full lap at race pace. And no car drives the pit lane
//! backwards: one whose spot is behind it (a finished car going back to its
//! garage) rejoins the circuit and boxes again next time round.

use std::f32::consts::{PI, TAU};

use super::{Bit, Car, Driver, Ember, GOLD, MINT, Mode, PitLane, at};
use crate::creature;
use crate::reef::{self, Mood, Rng, hash};

/// One fixed step of scene time: the same race whatever the frame rate.
pub const DT: f32 = 1.0 / 60.0;
const MAX_EMBERS: usize = 600;

fn ease(dt: f32, rate: f32) -> f32 {
    1.0 - (-dt * rate).exp()
}

/// `x` moved toward `to` by at most `by`.
fn toward(x: f32, to: f32, by: f32) -> f32 {
    x + (to - x).clamp(-by, by)
}

impl PitLane {
    /// Cars for exactly `drivers`, keeping each one's place and motion.
    pub fn sync(&mut self, drivers: Vec<Driver>) {
        let mut old = std::mem::take(&mut self.cars);
        for who in drivers {
            let car = match old.iter().position(|c| c.who.id == who.id) {
                Some(at) => {
                    let mut c = old.swap_remove(at);
                    c.tier = creature::tier(who.work.as_ref());
                    c.who = who;
                    c
                }
                None => fresh(who),
            };
            self.cars.push(car);
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|id| !self.cars.iter().any(|c| &c.who.id == id))
        {
            self.selected = None;
        }
    }

    /// Advance `dt` real seconds in fixed steps, slowed for reduced motion.
    pub fn advance(&mut self, dt: f32) {
        if self.trk.is_empty() {
            return;
        }
        let dt = dt.clamp(0.0, 0.05) * if self.reduce { 0.4 } else { 1.0 };
        self.carry += dt;
        while self.carry >= DT {
            self.carry -= DT;
            self.step(DT);
        }
    }

    pub fn step(&mut self, dt: f32) {
        if self.trk.is_empty() {
            return;
        }
        self.t += dt;
        self.nt += (self.nt_target - self.nt) * ease(dt, 2.0);
        if (self.nt_target - self.nt).abs() < 0.002 {
            // Settled: a canvas at full or no opacity paints without a layer.
            self.nt = self.nt_target;
        }
        for i in 0..self.cars.len() {
            if !self.cars[i].placed {
                self.start(i);
            }
            self.drive(i, dt);
        }
        self.bits.retain_mut(|p| {
            p.life -= dt;
            p.vy += 200.0 * dt;
            p.vx *= (-dt * 1.5).exp();
            p.x += p.vx * dt;
            p.y += p.vy * dt;
            p.rot += p.vr * dt;
            p.life > 0.0
        });
        self.fireworks(dt);
    }

    /// A new car: somewhere on the circuit, or already at its spot.
    fn start(&mut self, i: usize) {
        let d = self.rng.next() * self.lap();
        let k = self.k;
        let goal = self.goal(i);
        let c = &mut self.cars[i];
        c.placed = true;
        c.d = d;
        c.v = 150.0 * k;
        if let Some([p, q]) = goal {
            c.mode = Mode::Pit;
            c.p = p;
            c.q = q;
            c.landed = true;
        }
        let (at, ang) = self.place(&self.cars[i]);
        let c = &mut self.cars[i];
        c.h = if goal.is_some() {
            self.nrm[1].atan2(self.nrm[0])
        } else {
            ang
        };
        c.h_t = c.h;
        [c.x, c.y] = at;
    }

    /// One car's step, the prototype's `step`.
    fn drive(&mut self, i: usize, dt: f32) {
        let (k, l) = (self.k, self.lap());
        let goal = self.goal(i);
        let (prev, _) = self.place(&self.cars[i]);
        if self.cars[i].acting != self.cars[i].who.mood {
            let c = &mut self.cars[i];
            c.acting = c.who.mood;
            c.landed = false;
        }
        let mut burst = None;
        if self.cars[i].mode == Mode::Track {
            let c = &self.cars[i];
            let at_i = at(&self.trk, c.d, true).2;
            let slow = self.slow[at_i];
            // A car with somewhere to be pushes to the pit entry.
            let hurry = goal.is_some();
            let mut tv = 250.0 * k * c.f * if hurry { 1.5 * slow.max(0.7) } else { slow };
            let (mut lane_t, mut lane_hold) = (c.lane_t, c.lane_hold - dt);
            if lane_hold < 0.0 {
                lane_t = 0.0;
            }
            for (j, o) in self.cars.iter().enumerate() {
                if j == i || o.mode != Mode::Track {
                    continue;
                }
                let gap = (o.d - c.d + l) % l;
                if gap > 0.0 && gap < 40.0 * k && (o.lane - c.lane).abs() < 1.0 {
                    if c.v > o.v * 1.02 && (slow > 0.75 || hurry) {
                        lane_t = if o.lane > 0.0 { -1.0 } else { 1.0 };
                        lane_hold = 2.5;
                    } else {
                        tv = tv.min(o.v * 0.98);
                    }
                }
            }
            let entry = self.entry;
            let c = &mut self.cars[i];
            c.lane_t = lane_t;
            c.lane_hold = lane_hold;
            c.v += (tv - c.v) * ease(dt, 2.5);
            c.lane += (c.lane_t - c.lane) * ease(dt, 2.4);
            let nd = c.d + c.v * dt;
            let rel = |d: f32| (d - entry + l) % l;
            if goal.is_some() && rel(c.d) > rel(nd) {
                c.mode = Mode::Pit;
                c.p = 0.0;
                c.q = 0.0;
                c.v = c.v.min(120.0 * k);
            } else {
                c.d = nd % l;
            }
        } else {
            let pl = self.pit_len();
            let exit = self.exit;
            let c = &mut self.cars[i];
            // A spot well behind is next time round: the lane runs one way.
            let ahead = goal.filter(|g| g[0] >= c.p - 30.0 * k);
            let [gp, gq] = ahead.unwrap_or([pl, 0.0]);
            let (lim, side) = (85.0 * k, 30.0 * k);
            if (c.p - gp).abs() > 1.0 {
                if c.q.abs() > 0.5 {
                    c.q = toward(c.q, 0.0, side * dt);
                } else {
                    c.p = toward(c.p, gp, lim * dt);
                }
            } else {
                c.q = toward(c.q, gq, side * dt);
            }
            if ahead.is_none() && c.p >= pl - 0.5 && c.q.abs() < 0.5 {
                c.mode = Mode::Track;
                c.d = exit;
                c.v = 110.0 * k;
                c.lane = 0.0;
                c.lane_t = 0.0;
            }
            if ahead.is_some() && !c.landed && (c.p - gp).abs() < 1.0 && (c.q - gq).abs() < 0.6 {
                c.landed = true;
                if c.who.mood == Mood::Done {
                    burst = Some(c.who.species.body);
                }
            }
        }
        let (now, ang) = self.place(&self.cars[i]);
        let nrm = self.nrm;
        let c = &mut self.cars[i];
        let (dx, dy) = (now[0] - prev[0], now[1] - prev[1]);
        if c.mode == Mode::Track {
            c.h_t = ang + (c.lane_t - c.lane) * 0.15;
        } else if dx.hypot(dy) > 0.02 {
            let out = dx * nrm[0] + dy * nrm[1];
            if out.abs() < 0.01 {
                // Along the lane it faces the way the lane runs, even creeping back.
                c.h_t = ang;
            } else if out > 0.0 {
                // Into its box nose first; it backs out the same way round.
                c.h_t = dy.atan2(dx);
            }
        }
        let mut dh = c.h_t - c.h;
        while dh > PI {
            dh -= TAU;
        }
        while dh < -PI {
            dh += TAU;
        }
        c.h += dh * ease(dt, 9.0);
        [c.x, c.y] = now;
        c.speed = dx.hypot(dy) / dt.max(1e-4);
        let tail = [now[0] - c.h.cos() * 12.0 * k, now[1] - c.h.sin() * 12.0 * k];
        c.trail.push(tail);
        let keep = if c.mode == Mode::Track { 16 } else { 2 };
        if c.trail.len() > keep {
            let extra = c.trail.len() - keep;
            c.trail.drain(..extra);
        }
        if let Some(kit) = burst {
            self.burst(now, kit);
        }
    }

    /// Confetti as a finished car parks.
    fn burst(&mut self, at: [f32; 2], kit: reef::Rgb) {
        let cols = [MINT, MINT, [255.0; 3], kit, GOLD, [20.0, 20.0, 24.0]];
        for i in 0..30 {
            let r = &mut self.rng;
            self.bits.push(Bit {
                x: at[0],
                y: at[1],
                vx: (r.next() - 0.5) * 180.0,
                vy: -50.0 - r.next() * 130.0,
                life: 1.6 + r.next() * 0.6,
                rot: r.next() * 6.0,
                vr: (r.next() - 0.5) * 14.0,
                c: cols[i % cols.len()],
            });
        }
    }

    /// Fireworks over the grandstand at night, unless motion is reduced.
    fn fireworks(&mut self, dt: f32) {
        if self.nt > 0.5 && !self.reduce {
            self.next_firework -= dt;
            if self.next_firework < 0.0 && self.embers.len() < MAX_EMBERS {
                let gs = self.rect_s(0.1, -0.3, 0.9, 0.05);
                let r = &mut self.rng;
                self.next_firework = 2.5 + r.next() * 4.0;
                let (x, y) = (gs[0] + r.next() * gs[2], gs[1] + r.next() * gs[3]);
                let kinds = [
                    reef::CLAUDE,
                    reef::CODEX,
                    reef::OPENCODE,
                    reef::GEMINI,
                    reef::OTHER,
                ];
                let col = kinds[((r.next() * 5.0) as usize).min(4)].body;
                for i in 0..46 {
                    let an = i as f32 / 46.0 * TAU;
                    let v = 60.0 + r.next() * 50.0;
                    let c = if r.next() < 0.2 { GOLD } else { col };
                    self.embers.push(Ember {
                        x,
                        y,
                        vx: an.cos() * v,
                        vy: an.sin() * v,
                        life: 1.5,
                        c,
                    });
                }
            }
        }
        self.embers.retain_mut(|p| {
            p.life -= dt;
            p.vx *= (-dt * 1.6).exp();
            p.vy = p.vy * (-dt * 1.6).exp() + 30.0 * dt;
            p.x += p.vx * dt;
            p.y += p.vy * dt;
            p.life > 0.0
        });
    }
}

/// A new car, its pace and rhythm seeded from who it is, already at its tier.
fn fresh(who: Driver) -> Car {
    let tier = creature::tier(who.work.as_ref());
    let f = 0.84 + (hash(&who.name) % 1000) as f32 / 1000.0 * 0.22;
    let seed = Rng::new(hash(&format!("{}{:?}", who.name, who.id))).next();
    let acting = who.mood;
    Car {
        who,
        tier,
        mode: Mode::Track,
        d: 0.0,
        v: 0.0,
        lane: 0.0,
        lane_t: 0.0,
        lane_hold: 0.0,
        p: 0.0,
        q: 0.0,
        h: 0.0,
        h_t: 0.0,
        f,
        seed,
        trail: Vec::new(),
        acting,
        landed: false,
        speed: 0.0,
        x: 0.0,
        y: 0.0,
        placed: false,
    }
}
