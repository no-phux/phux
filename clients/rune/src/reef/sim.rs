//! The reef in motion: the prototype's `step`, the camera, and keeping the
//! fish in step with the fleet.

use std::f32::consts::TAU;

use phux_protocol::ids::ResourceId;

use super::{Bubble, Fish, Mood, Pt, Reef, Ripple, Rng, Species, hash};

/// One fixed step of scene time: the same scene whatever the frame rate.
pub const STEP: f32 = 1.0 / 60.0;
const MAX_BUBBLES: usize = 320;

/// An agent as the reef needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Swimmer {
    pub id: ResourceId,
    pub session: String,
    pub name: String,
    /// Its machine's zone.
    pub host: usize,
    pub mood: Mood,
    pub species: &'static Species,
    pub project: Option<String>,
    /// Fish with one school swim together: the galaxy's related agents.
    pub school: usize,
}

fn ease(dt: f32, rate: f32) -> f32 {
    1.0 - (-dt * rate).exp()
}

impl Reef {
    /// Fish for exactly `swimmers`, keeping each one's place and motion.
    pub fn sync(&mut self, swimmers: Vec<Swimmer>) {
        let mut old: Vec<Fish> = std::mem::take(&mut self.fish);
        let mut fresh = Vec::new();
        for who in swimmers {
            match old.iter().position(|f| f.who.id == who.id) {
                Some(at) => {
                    let mut f = old.swap_remove(at);
                    let mood = who.mood;
                    let burst = f.who.mood != mood && mood == Mood::Waiting;
                    f.who = who;
                    if burst {
                        self.burst(f.x, f.y);
                    }
                    self.fish.push(f);
                }
                None => {
                    fresh.push(self.fish.len());
                    self.fish.push(hatch(who));
                }
            }
        }
        self.assign_slots();
        for i in fresh {
            self.place(i);
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|id| !self.fish.iter().any(|f| &f.who.id == id))
        {
            self.selected = None;
        }
    }

    /// Ten bubbles as a fish turns to need you.
    fn burst(&mut self, x: f32, y: f32) {
        let s = self.s;
        for _ in 0..10 {
            let (a, b, c) = (self.rng.next(), self.rng.next(), self.rng.next());
            self.bubble(
                x + (a - 0.5) * 20.0 * s,
                y,
                (1.5 + b * 3.0) * s,
                (40.0 + c * 40.0) * s,
            );
        }
    }

    fn bubble(&mut self, x: f32, y: f32, r: f32, vy: f32) {
        if self.bubbles.len() < MAX_BUBBLES {
            let s = self.rng.next() * TAU;
            self.bubbles.push(Bubble { x, y, r, vy, s });
        }
    }

    /// Advance `dt` real seconds in fixed steps, slowed for reduced motion,
    /// then ease the camera.
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
        self.update_camera(dt);
    }

    /// The prototype's `step`, less its demo's random state changes.
    pub fn step(&mut self, dt: f32) {
        if self.zones.is_empty() {
            return;
        }
        self.t += dt;
        self.nt += (self.nt_target - self.nt) * ease(dt, 2.0);
        let (s, t) = (self.s, self.t);
        let n = self.fish.len();
        for i in 0..n {
            let (mut ax, mut ay) = (0.0, 0.0);
            let f = &self.fish[i];
            let l = f.len(s);
            let z = (self.zone_of(f).l, self.zone_of(f).r);
            for (j, g) in self.fish.iter().enumerate() {
                if j == i {
                    continue;
                }
                let (dx, dy) = (f.x - g.x, f.y - g.y);
                let (d2, rr) = (dx * dx + dy * dy, l * 1.25);
                if d2 < rr * rr && d2 > 0.01 {
                    let d = d2.sqrt();
                    let k = (rr - d) / rr;
                    ax += dx / d * k * 260.0 * s;
                    ay += dy / d * k * 260.0 * s;
                }
            }
            let max_sp;
            if f.who.mood == Mood::Working {
                max_sp = (60.0 + f.cruise * 28.0) * s;
                let (mut m, mut cx, mut cy, mut vx, mut vy) = (0.0, 0.0, 0.0, 0.0, 0.0);
                for (j, g) in self.fish.iter().enumerate() {
                    if j == i || g.who.mood != Mood::Working || g.who.school != f.who.school {
                        continue;
                    }
                    if (g.x - f.x).hypot(g.y - f.y) < 220.0 * s {
                        m += 1.0;
                        cx += g.x;
                        cy += g.y;
                        vx += g.vx;
                        vy += g.vy;
                    }
                }
                if m > 0.0 {
                    ax += (cx / m - f.x) * 0.8 + (vx / m - f.vx) * 1.3;
                    ay += (cy / m - f.y) * 0.8 + (vy / m - f.vy) * 1.3;
                }
                let wander = (self.rng.next() - 0.5) * 3.0 * dt;
                let f = &mut self.fish[i];
                f.wa += wander;
                ax += f.wa.cos() * 42.0 * s;
                ay += f.wa.sin() * 22.0 * s;
                let (m, top, bot) = (34.0 * s, self.surface_y + 80.0 * s, self.floor_y - 92.0 * s);
                if f.x < z.0 + m {
                    ax += (z.0 + m - f.x) * 4.0;
                }
                if f.x > z.1 - m {
                    ax -= (f.x - (z.1 - m)) * 4.0;
                }
                if f.y < top {
                    ay += (top - f.y) * 4.0;
                }
                if f.y > bot {
                    ay -= (f.y - bot) * 4.0;
                }
                let sp = f.vx.hypot(f.vy);
                let sp = if sp == 0.0 { 1.0 } else { sp };
                if sp < max_sp * 0.6 {
                    ax += f.vx / sp * 50.0 * s;
                    ay += f.vy / sp * 50.0 * s;
                }
            } else {
                let at = self.target_for(f);
                max_sp = match f.who.mood {
                    Mood::Waiting => 125.0,
                    Mood::Done => 62.0,
                    _ => 42.0,
                } * s;
                let (dx, dy) = (at[0] - f.x, at[1] - f.y);
                let d = dx.hypot(dy);
                let d = if d == 0.0 { 1.0 } else { d };
                let want = max_sp * (d / (90.0 * s)).min(1.0);
                ax += (dx / d * want - f.vx) * 2.6;
                ay += (dy / d * want - f.vy) * 2.6;
            }
            let f = &mut self.fish[i];
            f.vx += ax * dt;
            f.vy += ay * dt;
            let sp = f.vx.hypot(f.vy);
            if sp > max_sp {
                f.vx *= max_sp / sp;
                f.vy *= max_sp / sp;
            }
            f.x += f.vx * dt;
            f.y += f.vy * dt;
            f.phase += dt * (3.0 + sp / s * 0.1);
            if f.vx.abs() > 5.0 * s {
                f.face = f.vx.signum();
            }
            f.face_s += (f.face - f.face_s) * ease(dt, 7.0);
            let tilt = f.vy.atan2(f.vx.abs().max(10.0 * s)).clamp(-0.55, 0.55);
            f.tilt += (tilt - f.tilt) * ease(dt, 5.0);
            f.pulse = (f.pulse + dt * 0.75) % 1.0;
            f.bt -= dt;
            let (mood, fx, fy, face) = (f.who.mood, f.x, f.y, f.face);
            if f.bt <= 0.0 {
                let (nx, ny) = (fx + face * l * 0.48, fy - l * 0.05);
                let a = self.rng.next();
                let b = self.rng.next();
                let gap = match mood {
                    Mood::Working => {
                        self.bubble(nx, ny, (1.0 + a * 1.6) * s, (34.0 + b * 20.0) * s);
                        0.45 + self.rng.next() * 0.9
                    }
                    Mood::Idle => {
                        self.bubble(nx, ny, (2.4 + a * 1.4) * s, 18.0 * s);
                        2.5 + self.rng.next() * 3.0
                    }
                    _ => 1.0,
                };
                self.fish[i].bt = gap;
            }
            if mood == Mood::Waiting && fy < self.surface_y + 70.0 * s {
                let f = &mut self.fish[i];
                f.rt -= dt;
                if f.rt <= 0.0 {
                    f.rt = 1.6;
                    self.ripples.push(Ripple {
                        x: fx,
                        a: 0.0,
                        amber: true,
                    });
                }
            }
        }

        for zi in 0..self.zones.len() {
            let z = &mut self.zones[zi];
            z.vent_next -= dt;
            if z.vent_next <= 0.0 {
                z.vent_left = 6;
                z.vent_gap = 0.0;
                z.vent_next = 4.0 + self.rng.next() * 5.0;
            }
            let z = &mut self.zones[zi];
            if z.vent_left > 0 {
                z.vent_gap -= dt;
                if z.vent_gap <= 0.0 {
                    z.vent_left -= 1;
                    z.vent_gap = 0.14;
                    let x = z.vent_x;
                    let (a, b, c) = (self.rng.next(), self.rng.next(), self.rng.next());
                    let y = self.sand_y(x) - 4.0 * s;
                    self.bubble(
                        x + (a - 0.5) * 6.0 * s,
                        y,
                        (1.5 + b * 2.5) * s,
                        (45.0 + c * 30.0) * s,
                    );
                }
            }
        }
        let mut popped = Vec::new();
        let mut bubbles = std::mem::take(&mut self.bubbles);
        bubbles.retain_mut(|b| {
            b.vy += 6.0 * s * dt;
            b.y -= b.vy * dt;
            b.x += (t * 3.0 + b.s).sin() * 12.0 * s * dt;
            let up = b.y < self.wave_y(b.x) + b.r;
            if up && b.r > 2.6 * s {
                popped.push(b.x);
            }
            !up
        });
        self.bubbles = bubbles;
        self.ripples.extend(popped.into_iter().map(|x| Ripple {
            x,
            a: 0.6,
            amber: false,
        }));
        self.ripples.retain_mut(|r| {
            r.a += dt;
            r.a <= 1.8
        });
        let (w, sy, fy, nt) = (self.w, self.surface_y, self.floor_y, self.nt);
        for p in &mut self.snow {
            p.y += (3.0 + p.r * 3.0) * s * dt;
            p.x += (t * 0.3 + p.s).sin() * 4.0 * s * dt;
            if p.y > fy + 30.0 * s {
                p.y = sy + 4.0;
                p.x = self.rng.next() * w;
            }
            if nt > 0.05
                && self
                    .fish
                    .iter()
                    .any(|f| (f.x - p.x).abs() < 46.0 * s && (f.y - p.y).abs() < 46.0 * s)
            {
                p.g = 1.0;
            }
            p.g *= (-dt * 1.4).exp();
        }
        // The background school: plain boids round a drifting Lissajous goal.
        let gx = w * (0.5 + 0.42 * (t * 0.045).sin());
        let gy = sy + (fy - sy) * (0.42 + 0.16 * (t * 0.083).sin());
        let k = self.amb.len().max(1) as f32;
        let mvx = self.amb.iter().map(|a| a.vx).sum::<f32>() / k;
        let mvy = self.amb.iter().map(|a| a.vy).sum::<f32>() / k;
        for i in 0..self.amb.len() {
            let a = &self.amb[i];
            let mut ax = (gx - a.x) * 0.12 + (mvx - a.vx) * 0.6;
            let mut ay = (gy - a.y) * 0.12 + (mvy - a.vy) * 0.6;
            for (j, b) in self.amb.iter().enumerate() {
                let (dx, dy) = (a.x - b.x, a.y - b.y);
                let d2 = dx * dx + dy * dy;
                if j != i && d2 < 140.0 * s * s && d2 > 0.01 {
                    ax += dx / d2 * 900.0 * s;
                    ay += dy / d2 * 900.0 * s;
                }
            }
            let a = &mut self.amb[i];
            a.vx += ax * dt;
            a.vy += ay * dt;
            let (sp, mx) = (a.vx.hypot(a.vy), 46.0 * s);
            if sp > mx {
                a.vx *= mx / sp;
                a.vy *= mx / sp;
            }
            a.x += a.vx * dt;
            a.y += a.vy * dt;
        }
        for j in &mut self.jellies {
            j.x += j.vx * s * dt;
            if j.x < -60.0 {
                j.x = w + 60.0;
            }
            if j.x > w + 60.0 {
                j.x = -60.0;
            }
        }
    }

    pub fn selected_fish(&self) -> Option<&Fish> {
        let id = self.selected.as_ref()?;
        self.fish.iter().find(|f| &f.who.id == id)
    }

    /// The prototype's camera target: the selected fish at about 1.8×, kept
    /// inside the scene; else the whole scene.
    pub fn camera_target(&self) -> super::Cam {
        let (w, h) = (self.w, self.h);
        let (mut x, mut y, mut z) = (w / 2.0, h / 2.0, 1.0);
        if let Some(f) = self.selected_fish() {
            z = if w < 640.0 { 1.6 } else { 1.85 };
            x = f.x;
            y = f.y;
        }
        let (hw, hh) = (w / (2.0 * z), h / (2.0 * z));
        super::Cam {
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

    /// World to screen for a layer with parallax `p` (1 is the scene itself).
    pub fn to_screen(&self, p: f32) -> impl Fn(Pt) -> Pt + Copy {
        let z = 1.0 + (self.cam.z - 1.0) * p;
        let cx = self.w / 2.0 + (self.cam.x - self.w / 2.0) * p;
        let cy = self.h / 2.0 + (self.cam.y - self.h / 2.0) * p;
        let (ox, oy) = (self.w / 2.0 - cx * z, self.h / 2.0 - cy * z);
        move |q: Pt| [q[0] * z + ox, q[1] * z + oy]
    }
}

/// A new fish, its looks and rhythm seeded from who it is.
fn hatch(who: Swimmer) -> Fish {
    let mut r = Rng::new(hash(&format!("{}{:?}", who.name, who.id)));
    let vx = (r.next() - 0.5) * 40.0;
    let vy = (r.next() - 0.5) * 10.0;
    let phase = r.next() * TAU;
    let lb = 30.0 + r.next() * 12.0;
    let cruise = r.next();
    let face = if r.next() < 0.5 { -1.0 } else { 1.0 };
    Fish {
        who,
        x: 0.0,
        y: 0.0,
        vx,
        vy,
        phase,
        lb,
        cruise,
        face,
        face_s: face,
        tilt: 0.0,
        wa: r.next() * TAU,
        bt: r.next() * 2.0,
        rt: r.next() * 1.6,
        seed: r.next() * TAU,
        pulse: r.next(),
        slot: 0,
        slots: 1,
        k: 1.0,
    }
}
