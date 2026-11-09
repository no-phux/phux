//! The pitch in motion: the prototype's `think`, `ballStep`, separation,
//! pigeons and confetti, at a fixed step, and keeping the players in step
//! with the fleet.

use std::f32::consts::{PI, TAU};

use super::{Agent, Flash, Flight, GOLD, MINT, Mood, Part, Pitch, Player, Rng, hash, lerp, post};
use crate::creature;

/// One fixed step of scene time: the same scene whatever the frame rate.
pub const STEP: f32 = 1.0 / 60.0;

fn ease(dt: f32, rate: f32) -> f32 {
    1.0 - (-dt * rate).exp()
}

impl Pitch {
    /// Players for exactly `agents`, keeping each one's place and motion.
    pub fn sync(&mut self, agents: Vec<Agent>) {
        let mut old: Vec<Player> = std::mem::take(&mut self.players);
        for who in agents {
            let tier = creature::tier(who.work.as_ref());
            let p = match old.iter().position(|p| p.who.id == who.id) {
                Some(at) => {
                    let mut p = old.swap_remove(at);
                    p.who = who;
                    p.tier = tier;
                    p
                }
                None => kick_off(who, tier),
            };
            self.players.push(p);
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|id| !self.players.iter().any(|p| &p.who.id == id))
        {
            self.selected = None;
        }
    }

    /// Advance `dt` real seconds in fixed steps, slowed for reduced motion.
    pub fn advance(&mut self, dt: f32) {
        if self.zones.is_empty() {
            return;
        }
        let dt = dt.clamp(0.0, 0.05) * if self.reduce { 0.4 } else { 1.0 };
        self.carry += dt;
        while self.carry >= STEP {
            self.carry -= STEP;
            self.step(STEP);
        }
    }

    /// The simulation half of the prototype's `draw`.
    pub fn step(&mut self, dt: f32) {
        if self.zones.is_empty() {
            return;
        }
        self.t += dt;
        self.nt += (self.nt_target - self.nt) * ease(dt, 2.5);
        for z in &mut self.zones {
            z.wait = 0;
            z.idle = 0;
            z.work.clear();
        }
        for i in 0..self.players.len() {
            let zi = self.zone_at(self.players[i].who.host);
            let (z, p) = (&mut self.zones[zi], &mut self.players[i]);
            match p.who.mood {
                Mood::Waiting => {
                    p.slot = z.wait;
                    z.wait += 1;
                }
                Mood::Idle => {
                    p.slot = z.idle;
                    z.idle += 1;
                }
                Mood::Working => z.work.push(i),
                Mood::Done => {}
            }
        }
        for zi in 0..self.zones.len() {
            self.ball_step(zi, dt);
        }
        self.separate();
        for i in 0..self.players.len() {
            if self.players[i].fresh {
                let z = self.zone_of(&self.players[i]);
                let (l, s) = (z.z0 + z.zw / 2.0, self.sp / 2.0);
                let p = &mut self.players[i];
                (p.l, p.s, p.fresh) = (l, s, false);
                self.think(i, dt);
                // The prototype's warm-up: a new player walks on already settled.
                for _ in 0..400 {
                    self.think(i, 1.0 / 30.0);
                }
                self.players[i].cel = 0.0;
            } else {
                self.think(i, dt);
            }
        }
        self.confetti(dt);
        self.stands(dt);
        self.pigeons(dt);
    }

    /// Players keep apart on the pitch; seated ones don't count.
    fn separate(&mut self) {
        let m = 11.0 * self.k;
        let n = self.players.len();
        for i in 0..n {
            for j in i + 1..n {
                let (a, b) = (&self.players[i], &self.players[j]);
                if a.sit > 0.5 || b.sit > 0.5 {
                    continue;
                }
                let (dl, ds) = (b.l - a.l, b.s - a.s);
                let d = dl.hypot(ds);
                if d < m && d > 0.01 {
                    let f = (m - d) / d * 0.25;
                    self.players[i].l -= dl * f;
                    self.players[i].s -= ds * f;
                    self.players[j].l += dl * f;
                    self.players[j].s += ds * f;
                }
            }
        }
    }

    /// Player `i`'s target for its mood, and its walk toward it.
    fn think(&mut self, i: usize, dt: f32) {
        let (k, strip, sp, portrait) = (self.k, self.strip, self.sp, self.portrait);
        let zi = self.zone_at(self.players[i].who.host);
        let mood = self.players[i].who.mood;
        if self.players[i].mood != Some(mood) {
            let p = &mut self.players[i];
            let was = p.mood.replace(mood);
            p.mt = 0.0;
            if mood == Mood::Working {
                p.tl = None;
            }
            if mood == Mood::Done && was.is_some() {
                p.cel = 2.6;
                let (l, s, body) = (p.l, p.s, p.who.species.body);
                let at = self.to_s(l, s);
                self.burst(at[0], at[1] - 16.0 * k, body);
            }
        }
        let z = &self.zones[zi];
        let (ball, z0, zw) = ([z.ball.l, z.ball.s], z.z0, z.zw);
        let slots = match mood {
            Mood::Waiting => z.wait,
            _ => z.idle,
        };
        let posted = post(mood, z, [self.players[i].slot, slots], k, strip);
        let r = &mut self.rng;
        let p = &mut self.players[i];
        p.mt += dt;
        let (tl, ts, max) = match mood {
            Mood::Waiting | Mood::Idle => {
                let [l, s] = posted.unwrap_or([p.l, p.s]);
                (l, s, if mood == Mood::Waiting { 90.0 } else { 36.0 } * k)
            }
            Mood::Done if p.cel > 0.0 => {
                p.cel -= dt;
                (p.l, p.s, 0.0)
            }
            Mood::Done => {
                let ang = p.ang.unwrap_or_else(|| r.next() * TAU) + dt * 0.22;
                p.ang = Some(ang);
                (
                    z0 + zw / 2.0 + ang.cos() * zw * 0.34,
                    sp / 2.0 + ang.sin() * sp * 0.34,
                    38.0 * k,
                )
            }
            Mood::Working => {
                let close = p.tl.is_some_and(|t| (t[0] - p.l).hypot(t[1] - p.s) < 5.0);
                if p.tl.is_none()
                    || (close && {
                        p.pause -= dt;
                        p.pause < 0.0
                    })
                {
                    let cones = &self.zones[zi].cones;
                    let q = &cones[((r.next() * cones.len() as f32) as usize).min(cones.len() - 1)];
                    let (a, b) = (r.next(), r.next());
                    p.tl = Some([
                        (q.l + (a - 0.5) * 70.0 * k).clamp(z0 + zw * 0.1, z0 + zw * 0.9),
                        (q.s + (b - 0.5) * 70.0 * k).clamp(sp * 0.1, sp * 0.9),
                    ]);
                    p.pause = 0.2 + r.next() * 0.9;
                }
                let [tl, ts] = p.tl.unwrap_or([p.l, p.s]);
                if p.recv {
                    (p.l, p.s, 0.0)
                } else if p.chase {
                    (ball[0], ball[1], 110.0 * k)
                } else {
                    (tl, ts, if p.has { 42.0 } else { 78.0 } * k)
                }
            }
        };
        let (dl, ds) = (tl - p.l, ts - p.s);
        let d = dl.hypot(ds);
        let speed = max.min(d * 3.0);
        let q = ease(dt, 6.0);
        let (gl, gs) = if d > 0.5 {
            (dl / d * speed, ds / d * speed)
        } else {
            (0.0, 0.0)
        };
        p.vl += (gl - p.vl) * q;
        p.vs += (gs - p.vs) * q;
        p.l += p.vl * dt;
        p.s += p.vs * dt;
        p.arrived = d < 4.0;
        let vx = if portrait { p.vs } else { p.vl };
        if vx.abs() > 4.0 {
            p.ft = vx.signum();
        }
        if p.recv {
            let bx = if portrait {
                ball[1] - p.s
            } else {
                ball[0] - p.l
            };
            if bx.abs() > 2.0 {
                p.ft = bx.signum();
            }
        }
        p.face += (p.ft - p.face) * ease(dt, 10.0);
        p.run = (p.vl.hypot(p.vs) / (70.0 * k)).clamp(0.0, 1.2);
        p.phase += dt * 15.0 * p.run;
        let sit = if mood == Mood::Idle && p.arrived {
            1.0
        } else {
            0.0
        };
        p.sit += (sit - p.sit) * ease(dt, 5.0);
    }

    fn player(&self, id: &phux_protocol::ids::ResourceId) -> Option<usize> {
        self.players.iter().position(|p| &p.who.id == id)
    }

    /// Zone `zi`'s ball: in flight to a team-mate, at its owner's feet, or
    /// loose with the nearest working player chasing it.
    fn ball_step(&mut self, zi: usize, dt: f32) {
        let k = self.k;
        let work = self.zones[zi].work.clone();
        for &i in &work {
            let p = &mut self.players[i];
            (p.chase, p.recv, p.has) = (false, false, false);
        }
        let working = |p: &Player| p.who.mood == Mood::Working;
        if let Some(fl) = self.zones[zi].ball.fl.clone() {
            match self.player(&fl.to).filter(|&r| working(&self.players[r])) {
                None => {
                    let b = &mut self.zones[zi].ball;
                    (b.fl, b.h) = (None, 0.0);
                }
                Some(r) => {
                    let rp = &mut self.players[r];
                    rp.recv = true;
                    let (to_l, to_s) = (rp.l + rp.face * 4.0 * k, rp.s + 1.0);
                    let b = &mut self.zones[zi].ball;
                    let p = (fl.p + dt / fl.dur).min(1.0);
                    b.l = lerp(fl.l0, to_l, p);
                    b.s = lerp(fl.s0, to_s, p);
                    b.h = fl.loft * (PI * p).sin();
                    if p >= 1.0 {
                        let hold = 0.5 + self.rng.next() * 1.3;
                        (b.fl, b.owner, b.hold, b.h) = (None, Some(fl.to), hold, 0.0);
                    } else if let Some(f) = &mut b.fl {
                        f.p = p;
                    }
                }
            }
        } else if let Some(owner) = self.zones[zi].ball.owner.clone() {
            match self.player(&owner).filter(|&o| working(&self.players[o])) {
                None => self.zones[zi].ball.owner = None,
                Some(o) => {
                    let op = &mut self.players[o];
                    op.has = true;
                    let (l, s) = (op.l + op.face * 4.5 * k, op.s + 1.5 * k);
                    let b = &mut self.zones[zi].ball;
                    (b.l, b.s) = (l, s);
                    b.hold -= dt;
                    if b.hold <= 0.0 && work.len() > 1 {
                        let others: Vec<usize> = work.iter().copied().filter(|&w| w != o).collect();
                        let pick = (self.rng.next() * others.len() as f32) as usize;
                        let rp = &self.players[others[pick.min(others.len() - 1)]];
                        let d = (rp.l - l).hypot(rp.s - s);
                        let loft = if d > 110.0 * k && self.rng.next() < 0.5 {
                            d * 0.16
                        } else {
                            0.0
                        };
                        let b = &mut self.zones[zi].ball;
                        b.fl = Some(Flight {
                            to: rp.who.id.clone(),
                            l0: l,
                            s0: s,
                            p: 0.0,
                            dur: (d / (240.0 * k)).clamp(0.25, 1.3),
                            loft,
                        });
                        b.owner = None;
                    }
                }
            }
        } else {
            let b = &self.zones[zi].ball;
            let best = work
                .iter()
                .map(|&i| (i, (self.players[i].l - b.l).hypot(self.players[i].s - b.s)))
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((i, d)) = best {
                self.players[i].chase = true;
                if d < 6.0 * k {
                    let id = self.players[i].who.id.clone();
                    let b = &mut self.zones[zi].ball;
                    (b.owner, b.hold) = (Some(id), 0.4);
                }
            }
        }
    }

    /// Confetti in mint, white, gold and the player's kit as it finishes.
    fn burst(&mut self, x: f32, y: f32, kit: [f32; 3]) {
        let cols = [MINT, MINT, [255.0; 3], kit, GOLD];
        let r = &mut self.rng;
        for i in 0..28 {
            let part = Part {
                x,
                y,
                vx: (r.next() - 0.5) * 170.0,
                vy: -60.0 - r.next() * 120.0,
                life: 1.6 + r.next() * 0.6,
                rot: r.next() * 6.0,
                vr: (r.next() - 0.5) * 14.0,
                c: cols[i % cols.len()],
            };
            self.parts.push(part);
        }
    }

    fn confetti(&mut self, dt: f32) {
        for p in &mut self.parts {
            p.life -= dt;
            p.vy += 200.0 * dt;
            p.vx *= (-dt * 1.5).exp();
            p.x += p.vx * dt;
            p.y += p.vy * dt;
            p.rot += p.vr * dt;
        }
        self.parts.retain(|p| p.life > 0.0);
    }

    /// Camera flashes in the stands at night, never on the ground.
    fn stands(&mut self, dt: f32) {
        let (n, g, r) = (self.nt, self.g, &mut self.rng);
        if n > 0.2 && !self.reduce && r.next() < dt * 14.0 * n {
            let on_ground = |x: f32, y: f32| {
                x > g.x - 6.0 && x < g.x + g.w + 6.0 && y > g.y - 6.0 && y < g.y + g.h + 6.0
            };
            // Eight tries for a seat; a tall view's stands are thin, and a
            // flash on the grass would read as a glitch, so none then.
            let seat = (0..8)
                .map(|_| (r.next() * self.w, r.next() * self.h))
                .find(|&(x, y)| !on_ground(x, y));
            if let Some((x, y)) = seat {
                self.flashes.push(Flash { x, y, life: 0.16 });
            }
        }
        for f in &mut self.flashes {
            f.life -= dt;
        }
        self.flashes.retain(|f| f.life > 0.0);
    }

    /// Pigeons peck until a player comes close, then fly off and land again.
    fn pigeons(&mut self, dt: f32) {
        let (k, lp, sp) = (self.k, self.lp, self.sp);
        let r = &mut self.rng;
        for b in &mut self.pigeons {
            if b.fly == 0.0 {
                if let Some(p) = self
                    .players
                    .iter()
                    .find(|p| (p.l - b.l).hypot(p.s - b.s) < 26.0 * k)
                {
                    b.fly = 0.01;
                    let an = (b.s - p.s).atan2(b.l - p.l);
                    (b.vl, b.vs) = (an.cos() * 120.0, an.sin() * 120.0);
                }
                b.peck += dt;
            } else {
                b.fly += dt;
                b.l += b.vl * dt;
                b.s += b.vs * dt;
                b.a = (1.0 - b.fly / 1.6).max(0.0);
                if b.fly > 6.0 {
                    b.fly = 0.0;
                    b.l = lp * (0.1 + 0.8 * r.next());
                    b.s = sp * (0.1 + 0.8 * r.next());
                    b.a = 0.0;
                }
            }
            if b.fly == 0.0 {
                b.a = (b.a + dt).min(1.0);
            }
        }
    }
}

/// A new player, its looks and rhythm seeded from who it is.
fn kick_off(who: Agent, tier: usize) -> Player {
    let mut r = Rng::new(hash(&format!("{}{:?}", who.name, who.id)));
    Player {
        phase: r.next() * 6.0,
        look: (r.next() * 36.0) as usize,
        who,
        tier,
        l: 0.0,
        s: 0.0,
        vl: 0.0,
        vs: 0.0,
        face: 1.0,
        ft: 1.0,
        run: 0.0,
        sit: 0.0,
        cel: 0.0,
        mood: None,
        mt: 0.0,
        slot: 0,
        tl: None,
        pause: 0.0,
        ang: None,
        arrived: false,
        chase: false,
        recv: false,
        has: false,
        fresh: true,
    }
}
