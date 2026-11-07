//! The reef in motion: the prototype's `step()` and the camera.

use super::*;

impl Sim {
    /// One step of `dt` seconds: the prototype's `step()`.
    pub fn step(&mut self, dt: f32) {
        self.t += dt;
        self.nt += (self.nt_target - self.nt) * (1. - (-dt * 2.).exp());
        let (s, t) = (self.s, self.t);
        let n = self.fish.len();
        for i in 0..n {
            let (fx, fy, fvx, fvy) = {
                let f = &self.fish[i];
                (f.x, f.y, f.vx, f.vy)
            };
            let l = self.fish[i].len(s);
            let (mut ax, mut ay) = (0., 0.);
            let reach = l * 1.6;
            for (j, g) in self.fish.iter().enumerate() {
                if j == i {
                    continue;
                }
                let (dx, dy) = (fx - g.x, fy - g.y);
                let d2 = dx * dx + dy * dy;
                if d2 < reach * reach && d2 > 0.01 {
                    let d = d2.sqrt();
                    let k = (reach - d) / reach;
                    ax += dx / d * k * 260. * s;
                    ay += dy / d * k * 260. * s;
                }
            }
            let mood = self.fish[i].mood;
            let max_sp;
            if mood == Mood::Working {
                let f = &self.fish[i];
                max_sp = (60. + f.cruise * 28.) * s;
                let (mut c, mut cx, mut cy, mut vx, mut vy) = (0., 0., 0., 0., 0.);
                for &j in &f.mates {
                    let g = &self.fish[j];
                    if g.mood != Mood::Working {
                        continue;
                    }
                    if (g.x - fx).hypot(g.y - fy) < 220. * s {
                        c += 1.;
                        cx += g.x;
                        cy += g.y;
                        vx += g.vx;
                        vy += g.vy;
                    }
                }
                if c > 0. {
                    ax += (cx / c - fx) * 0.8 + (vx / c - fvx) * 1.3;
                    ay += (cy / c - fy) * 0.8 + (vy / c - fvy) * 1.3;
                }
                let turn = (self.rand() - 0.5) * 3. * dt;
                let f = &mut self.fish[i];
                f.wa += turn;
                ax += f.wa.cos() * 42. * s;
                ay += f.wa.sin() * 22. * s;
                let (zi, lane) = (f.zone, f.lane);
                let z = &self.zones[zi];
                let m = 34. * s;
                // Open water only; each school prefers its own depth, so
                // schools stack instead of clumping.
                let (top, bottom) = self.band(z);
                ay += (lerp(top, bottom, 0.15 + 0.7 * lane) - fy) * 0.5;
                if fx < z.l + m {
                    ax += (z.l + m - fx) * 4.;
                }
                if fx > z.r - m {
                    ax -= (fx - (z.r - m)) * 4.;
                }
                if fy < top {
                    ay += (top - fy) * 4.;
                }
                if fy > bottom {
                    ay -= (fy - bottom) * 4.;
                }
                let sp = fvx.hypot(fvy);
                let sp = if sp > 0. { sp } else { 1. };
                if sp < max_sp * 0.6 {
                    ax += fvx / sp * 50. * s;
                    ay += fvy / sp * 50. * s;
                }
            } else {
                let (tx, ty) = self.target(&self.fish[i]);
                max_sp = match mood {
                    Mood::Waiting => 125.,
                    Mood::Done => 62.,
                    _ => 42.,
                } * s;
                let (dx, dy) = (tx - fx, ty - fy);
                let d = dx.hypot(dy);
                let d = if d > 0. { d } else { 1. };
                let want = max_sp * (d / (90. * s)).min(1.);
                ax += (dx / d * want - fvx) * 2.6;
                ay += (dy / d * want - fvy) * 2.6;
            }
            let mut emit = None;
            {
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
                f.phase += dt * (3. + sp / s * 0.1);
                if f.vx.abs() > 5. * s {
                    f.face = f.vx.signum();
                }
                f.face_s += (f.face - f.face_s) * (1. - (-dt * 7.).exp());
                let want = f.vy.atan2(f.vx.abs().max(10. * s)).clamp(-0.55, 0.55);
                f.tilt += (want - f.tilt) * (1. - (-dt * 5.).exp());
                f.pulse = (f.pulse + dt * 0.75) % 1.;
                f.bt -= dt;
                if f.bt <= 0. {
                    emit = Some((f.x + f.face * l * 0.48, f.y - l * 0.05));
                }
            }
            if let Some((bx, by)) = emit {
                let next = match mood {
                    Mood::Working => {
                        let r = (1. + self.rand() * 1.6) * s;
                        let vy = (34. + self.rand() * 20.) * s;
                        self.bubble(bx, by, r, vy);
                        0.45 + self.rand() * 0.9
                    }
                    Mood::Idle => {
                        let r = (2.4 + self.rand() * 1.4) * s;
                        self.bubble(bx, by, r, 18. * s);
                        2.5 + self.rand() * 3.
                    }
                    _ => 1.,
                };
                self.fish[i].bt = next;
            }
            let f = &mut self.fish[i];
            if mood == Mood::Waiting && f.y < self.surface_y + 70. * s {
                f.rt -= dt;
                if f.rt <= 0. {
                    f.rt = 1.6;
                    let x = f.x;
                    self.ripples.push(Ripple {
                        x,
                        a: 0.,
                        amber: true,
                    });
                }
            }
        }

        // Vents: six bubbles, 140 ms apart, every few seconds.
        for zi in 0..self.zones.len() {
            self.zones[zi].vent_next -= dt;
            if self.zones[zi].vent_next <= 0. {
                self.zones[zi].vent_left = 6;
                self.zones[zi].vent_gap = 0.;
                self.zones[zi].vent_next = 4. + self.rand() * 5.;
            }
            if self.zones[zi].vent_left > 0 {
                self.zones[zi].vent_gap -= dt;
                if self.zones[zi].vent_gap <= 0. {
                    self.zones[zi].vent_left -= 1;
                    self.zones[zi].vent_gap = 0.14;
                    let vx = self.zones[zi].vent_x;
                    let x = vx + (self.rand() - 0.5) * 6. * s;
                    let y = self.sand_y(vx) - 4. * s;
                    let r = (1.5 + self.rand() * 2.5) * s;
                    let vy = (45. + self.rand() * 30.) * s;
                    self.bubble(x, y, r, vy);
                }
            }
        }
        let mut bubbles = std::mem::take(&mut self.bubbles);
        bubbles.retain_mut(|b| {
            b.vy += 6. * s * dt;
            b.y -= b.vy * dt;
            b.x += (t * 3. + b.s).sin() * 12. * s * dt;
            if b.y < self.wave_y(b.x) + b.r {
                if b.r > 2.6 * s {
                    self.ripples.push(Ripple {
                        x: b.x,
                        a: 0.6,
                        amber: false,
                    });
                }
                return false;
            }
            true
        });
        self.bubbles = bubbles;
        self.ripples.retain_mut(|r| {
            r.a += dt;
            r.a <= 1.8
        });
        let (w, surface_y, floor_y, nt) = (self.w, self.surface_y, self.floor_y, self.nt);
        for p in 0..self.snow.len() {
            let respawn = {
                let q = &mut self.snow[p];
                q.y += (3. + q.r * 3.) * s * dt;
                q.x += (t * 0.3 + q.s).sin() * 4. * s * dt;
                q.y > floor_y + 30. * s
            };
            if respawn {
                let x = self.rand() * w;
                self.snow[p].y = surface_y + 4.;
                self.snow[p].x = x;
            }
            let q = &self.snow[p];
            let lit = nt > 0.05
                && self
                    .fish
                    .iter()
                    .any(|f| (f.x - q.x).abs() < 46. * s && (f.y - q.y).abs() < 46. * s);
            let q = &mut self.snow[p];
            if lit {
                q.g = 1.;
            }
            q.g *= (-dt * 1.4).exp();
        }
        // The background school: plain boids round a drifting Lissajous goal.
        let gx = w * (0.5 + 0.42 * (t * 0.045).sin());
        let gy = surface_y + (floor_y - surface_y) * (0.42 + 0.16 * (t * 0.083).sin());
        let count = self.amb.len().max(1) as f32;
        let mvx = self.amb.iter().map(|a| a.vx).sum::<f32>() / count;
        let mvy = self.amb.iter().map(|a| a.vy).sum::<f32>() / count;
        for i in 0..self.amb.len() {
            let a = &self.amb[i];
            let mut ax = (gx - a.x) * 0.12 + (mvx - a.vx) * 0.6;
            let mut ay = (gy - a.y) * 0.12 + (mvy - a.vy) * 0.6;
            for (j, b) in self.amb.iter().enumerate() {
                if j == i {
                    continue;
                }
                let (dx, dy) = (a.x - b.x, a.y - b.y);
                let d2 = dx * dx + dy * dy;
                if d2 < 140. * s * s && d2 > 0.01 {
                    ax += dx / d2 * 900. * s;
                    ay += dy / d2 * 900. * s;
                }
            }
            let a = &mut self.amb[i];
            a.vx += ax * dt;
            a.vy += ay * dt;
            let sp = a.vx.hypot(a.vy);
            let mx = 46. * s;
            if sp > mx {
                a.vx *= mx / sp;
                a.vy *= mx / sp;
            }
            a.x += a.vx * dt;
            a.y += a.vy * dt;
        }
        for j in &mut self.jellies {
            j.x += j.vx * s * dt;
            if j.x < -60. {
                j.x = w + 60.;
            }
            if j.x > w + 60. {
                j.x = -60.;
            }
        }
    }

    /// Ease the camera toward the fish at `focus` (index), at 1.85 times,
    /// held inside the scene; back to the whole reef without one.
    pub fn camera(&mut self, focus: Option<usize>, dt: f32, snap: bool) {
        let (w, h) = (self.w, self.h);
        let (mut tx, mut ty, mut tz) = (w / 2., h / 2., 1.);
        if let Some(f) = focus.and_then(|i| self.fish.get(i)) {
            tz = if w < 640. { 1.6 } else { 1.85 };
            tx = f.x;
            ty = f.y;
        }
        let (hw, hh) = (w / (2. * tz), h / (2. * tz));
        tx = tx.clamp(hw, (w - hw).max(hw));
        ty = ty.clamp(hh, (h - hh).max(hh));
        let k = if snap { 1. } else { 1. - (-dt * 2.6).exp() };
        self.cam.x += (tx - self.cam.x) * k;
        self.cam.y += (ty - self.cam.y) * k;
        self.cam.z += (tz - self.cam.z) * k;
    }

    /// Without motion the light changes at once.
    pub fn settle_light(&mut self) {
        self.nt = self.nt_target;
    }
}
