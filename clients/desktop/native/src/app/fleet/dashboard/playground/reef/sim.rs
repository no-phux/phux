//! The fish, the scenery and the reef's state: layout, fleet sync, places.

use super::super::super::cards::tile_id;
use super::super::galaxy;
use super::*;

#[derive(Clone)]
pub(crate) struct Fish {
    /// The card's selector: the click target and the identity across frames.
    pub id: String,
    pub agent: Agent,
    pub zone: usize,
    pub species: Species,
    pub mood: Mood,
    /// Related fish (indices this frame): the school it keeps with.
    pub mates: Vec<usize>,
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub phase: f32,
    pub(super) lb: f32,
    pub(super) cruise: f32,
    pub(super) face: f32,
    pub face_s: f32,
    pub tilt: f32,
    pub(super) wa: f32,
    pub(super) bt: f32,
    pub(super) rt: f32,
    pub seed: f32,
    pub pulse: f32,
    pub(super) slot: usize,
    pub(super) slot_n: usize,
    pub(super) crowd: f32,
    /// The school's preferred depth, 0 (high) to 1 (low) in the working band.
    pub(super) lane: f32,
    /// Its tier (`tier::TIERS`), which sets its size, speed and form.
    pub tier: usize,
    /// A level-up's glow, 1 at the rise, fading to 0.
    pub level_up: f32,
    /// Seconds to a legendary fish's next trail mote.
    pub(super) mt: f32,
}

impl Fish {
    pub(super) fn new(id: String, agent: Agent) -> Fish {
        let mut r = Rng::new(hash(&id));
        let harness = Harness::from_slug(agent.info.kind.as_deref().unwrap_or_default());
        let vx = (r.next() - 0.5) * 40.;
        let vy = (r.next() - 0.5) * 10.;
        let phase = r.next() * TAU;
        let lb = 30. + r.next() * 12.;
        let cruise = r.next();
        let face = if r.next() < 0.5 { -1. } else { 1. };
        let mood = Mood::of(&agent.info);
        Fish {
            id,
            agent,
            zone: 0,
            species: species(harness),
            mood,
            mates: Vec::new(),
            x: 0.,
            y: 0.,
            vx,
            vy,
            phase,
            lb,
            cruise,
            face,
            face_s: face,
            tilt: 0.,
            wa: r.next() * TAU,
            bt: r.next() * 2.,
            rt: r.next() * 1.6,
            seed: r.next() * TAU,
            pulse: r.next(),
            slot: 0,
            slot_n: 1,
            crowd: 1.,
            lane: 0.,
            tier: REEF_FISH,
            level_up: 0.,
            mt: 0.,
        }
    }

    /// Body length in scene points at scale `s`.
    pub fn len(&self, s: f32) -> f32 {
        self.lb * s * self.crowd * TIERS[self.tier].size
    }
}

#[derive(Clone)]
pub(crate) struct Bubble {
    pub x: f32,
    pub y: f32,
    pub r: f32,
    pub(super) vy: f32,
    pub(super) s: f32,
}

#[derive(Clone)]
pub(crate) struct Ripple {
    pub x: f32,
    pub a: f32,
    pub amber: bool,
}

#[derive(Clone)]
pub(crate) struct Snow {
    pub x: f32,
    pub y: f32,
    pub r: f32,
    pub(super) s: f32,
    pub g: f32,
}

#[derive(Clone)]
pub(crate) struct Amb {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub(super) vy: f32,
}

#[derive(Clone)]
pub(crate) struct Jelly {
    pub x: f32,
    pub by: f32,
    pub s: f32,
    pub ph: f32,
    pub(super) vx: f32,
}

#[derive(Clone)]
pub(crate) struct Ray {
    pub x: f32,
    pub w: f32,
    pub skew: f32,
    pub ph: f32,
    pub sp: f32,
}

#[derive(Clone)]
pub(crate) struct Kelp {
    pub x: f32,
    pub h: f32,
    pub seed: f32,
    pub w: f32,
    pub front: bool,
    pub back: bool,
}

#[derive(Clone)]
pub(crate) struct Pebble {
    pub x: f32,
    pub dy: f32,
    pub rx: f32,
    pub sh: f32,
}

#[derive(Clone)]
pub(crate) struct Star {
    pub x: f32,
    pub y: f32,
    pub r: f32,
    pub tw: f32,
}

/// Where the reef camera looks: a centre in scene points and a zoom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Cam {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Clone)]
pub(crate) struct Sim {
    pub w: f32,
    pub h: f32,
    /// The scene scale every length is multiplied by.
    pub s: f32,
    pub surface_y: f32,
    pub floor_y: f32,
    pub t: f32,
    /// Night, eased: 0 is day, 1 is night.
    pub nt: f32,
    pub(super) nt_target: f32,
    /// The panel clock at the last step.
    pub clock: f32,
    pub zones: Vec<Zone>,
    pub rays: Vec<Ray>,
    pub kelp: Vec<Kelp>,
    pub far_a: Vec<(f32, f32)>,
    pub far_b: Vec<(f32, f32)>,
    pub pebbles: Vec<Pebble>,
    pub stars: Vec<Star>,
    pub fish: Vec<Fish>,
    pub bubbles: Vec<Bubble>,
    pub motes: Vec<Mote>,
    pub ripples: Vec<Ripple>,
    pub snow: Vec<Snow>,
    pub amb: Vec<Amb>,
    pub jellies: Vec<Jelly>,
    pub cam: Cam,
    /// The fish under the pointer, by id.
    pub hovered: Option<String>,
    pub(super) hosts: Vec<Option<String>>,
    pub(super) rng: Rng,
}

/// Steps a new reef runs before it is first drawn, as the prototype boots.
const WARM_UP: usize = 150;

impl Sim {
    /// A settled reef: laid out, every fish at its place, then a few
    /// seconds of swimming. Seeded throughout, so the same fleet draws the
    /// same frame.
    pub fn new(w: f32, h: f32, hosts: &[Option<String>], agents: &[Agent], night: bool) -> Sim {
        let nt = if night { 1. } else { 0. };
        let mut sim = Sim {
            w: 0.,
            h: 0.,
            s: 1.,
            surface_y: 0.,
            floor_y: 0.,
            t: 0.,
            nt,
            nt_target: nt,
            clock: 0.,
            zones: Vec::new(),
            rays: Vec::new(),
            kelp: Vec::new(),
            far_a: Vec::new(),
            far_b: Vec::new(),
            pebbles: Vec::new(),
            stars: Vec::new(),
            fish: Vec::new(),
            bubbles: Vec::new(),
            motes: Vec::new(),
            ripples: Vec::new(),
            snow: Vec::new(),
            amb: Vec::new(),
            jellies: Vec::new(),
            cam: Cam {
                x: w / 2.,
                y: h / 2.,
                z: 1.,
            },
            hovered: None,
            hosts: hosts.to_vec(),
            rng: Rng::new(0x5EEF),
        };
        sim.layout(w, h);
        sim.sync(w, h, hosts, agents, night);
        for _ in 0..WARM_UP {
            sim.step(1. / 60.);
        }
        sim
    }

    pub fn wave_y(&self, x: f32) -> f32 {
        let (t, s) = (self.t, self.s);
        self.surface_y
            + (x * 0.011 + t * 1.1).sin() * 3. * s
            + (x * 0.027 - t * 0.8).sin() * 1.6 * s
    }

    pub fn sand_y(&self, x: f32) -> f32 {
        sand_y(self.floor_y, self.s, x)
    }

    pub(super) fn rand(&mut self) -> f32 {
        self.rng.next()
    }

    /// The scene for a `w` by `h` panel: scale, surface, floor, the reefs
    /// and the scenery. What already swims is stretched to the new size.
    pub(super) fn layout(&mut self, w: f32, h: f32) {
        let (old_w, old_h) = (self.w, self.h);
        self.w = w;
        self.h = h;
        let s = ((w / 1200.).min(h / 780.) * 1.1).clamp(0.55, 1.25);
        self.s = s;
        self.surface_y = if w < 640. {
            150f32.max(h * 0.19)
        } else {
            64f32.max(h * 0.12)
        };
        self.floor_y = h * 0.83;
        let count = self.hosts.len().max(1);
        let zw = w / count as f32;
        self.zones = self
            .hosts
            .iter()
            .enumerate()
            .map(|(i, host)| zone(host.as_deref(), i, zw, s, self.floor_y))
            .collect();
        let mut r = Rng::new(99);
        self.rays = (0..8)
            .map(|k| Ray {
                x: (k as f32 + 0.5) / 8. * w + (r.next() - 0.5) * w * 0.08,
                w: (36. + r.next() * 70.) * s,
                skew: (50. + r.next() * 90.) * s,
                ph: r.next() * TAU,
                sp: 0.2 + r.next() * 0.3,
            })
            .collect();
        let depth = self.floor_y - self.surface_y;
        self.kelp = (0..=count)
            .flat_map(|i| (0..3).map(move |k| (i, k)))
            .map(|(i, k)| Kelp {
                x: zw * i as f32 + (r.next() - 0.5) * 70. * s,
                h: depth * (0.42 + r.next() * 0.42),
                seed: r.next() * TAU,
                w: (2.2 + r.next() * 1.6) * s,
                front: (i == 0 || i == count) && k == 0,
                back: k == 2,
            })
            .collect();
        let floor_y = self.floor_y;
        let ridge = |p: [f32; 3], amp: f32, base: f32| {
            let mut pts = Vec::new();
            let mut x = -60.;
            while x <= w + 60. {
                let wobble = (x * 0.013 + p[0]).sin() * 0.5
                    + (x * 0.031 + p[1]).sin() * 0.32
                    + (x * 0.071 + p[2]).sin() * 0.18;
                pts.push((x, floor_y - s * (base + amp * wobble)));
                x += 18.;
            }
            pts
        };
        let pb = [r.next() * 6., r.next() * 6., r.next() * 6.];
        self.far_b = ridge(pb, 70., 120.);
        let pa = [r.next() * 6., r.next() * 6., r.next() * 6.];
        self.far_a = ridge(pa, 50., 60.);
        self.pebbles = (0..(w / 22.).round() as usize)
            .map(|_| Pebble {
                x: r.next() * w,
                dy: r.next(),
                rx: (1.5 + r.next() * 3.) * s,
                sh: r.next(),
            })
            .collect();
        let surface_y = self.surface_y;
        self.stars = (0..70)
            .map(|_| Star {
                x: r.next() * w,
                y: r.next() * surface_y * 0.85,
                r: 0.4 + r.next() * 1.1,
                tw: r.next() * TAU,
            })
            .collect();
        if self.snow.is_empty() {
            for _ in 0..150 {
                let snow = Snow {
                    x: self.rand() * w,
                    y: surface_y + self.rand() * (h - surface_y),
                    r: 0.4 + self.rand() * 1.6,
                    s: self.rand() * TAU,
                    g: 0.,
                };
                self.snow.push(snow);
            }
        }
        if self.amb.is_empty() {
            for _ in 0..34 {
                let amb = Amb {
                    x: w * 0.3 + self.rand() * 120.,
                    y: h * 0.45 + self.rand() * 60.,
                    vx: 20.,
                    vy: 0.,
                };
                self.amb.push(amb);
            }
        }
        if self.jellies.is_empty() {
            for i in 0..3 {
                let jelly = Jelly {
                    x: (i as f32 + 0.3) / 3. * w,
                    by: surface_y + depth * (0.3 + self.rand() * 0.25),
                    s: 0.7 + self.rand() * 0.5,
                    ph: self.rand() * TAU,
                    vx: if self.rand() < 0.5 { -1. } else { 1. } * (4. + self.rand() * 4.),
                };
                self.jellies.push(jelly);
            }
        }
        if old_w > 0. && old_h > 0. {
            let (kx, ky) = (w / old_w, h / old_h);
            let points = self
                .fish
                .iter_mut()
                .map(|f| (&mut f.x, &mut f.y))
                .chain(self.snow.iter_mut().map(|p| (&mut p.x, &mut p.y)))
                .chain(self.amb.iter_mut().map(|a| (&mut a.x, &mut a.y)))
                .chain(self.bubbles.iter_mut().map(|b| (&mut b.x, &mut b.y)));
            for (x, y) in points {
                *x *= kx;
                *y *= ky;
            }
            self.cam.x *= kx;
            self.cam.y *= ky;
        }
    }

    /// Bring the reef up to date with the fleet: the panel size, the
    /// machines, each agent's fish and mood, the theme's day or night.
    pub fn sync(
        &mut self,
        w: f32,
        h: f32,
        hosts: &[Option<String>],
        agents: &[Agent],
        night: bool,
    ) {
        self.nt_target = if night { 1. } else { 0. };
        if hosts != self.hosts.as_slice() {
            self.hosts = hosts.to_vec();
            let (keep_w, keep_h) = (self.w, self.h);
            self.layout(keep_w, keep_h);
        }
        if (w - self.w).abs() > 0.5 || (h - self.h).abs() > 0.5 {
            self.layout(w, h);
        }
        let now = crate::connection::now_ms();
        let mut old = std::mem::take(&mut self.fish);
        let mut fresh = Vec::new();
        for agent in agents {
            let id = tile_id(agent.destination.host.as_deref(), &agent.terminal);
            let zone = self
                .hosts
                .iter()
                .position(|host| *host == agent.destination.host)
                .unwrap_or(0);
            let fish = match old.iter().position(|f| f.id == id) {
                Some(at) => {
                    let mut fish = old.swap_remove(at);
                    fish.agent = agent.clone();
                    fish.zone = zone;
                    self.regrade(&mut fish, now);
                    let mood = Mood::of(&agent.info);
                    if mood != fish.mood {
                        fish.mood = mood;
                        if mood == Mood::Waiting {
                            for _ in 0..10 {
                                let (x, y, s) = (fish.x, fish.y, self.s);
                                let dx = (self.rand() - 0.5) * 20. * s;
                                let r = (1.5 + self.rand() * 3.) * s;
                                let vy = (40. + self.rand() * 40.) * s;
                                self.bubble(x + dx, y, r, vy);
                            }
                        }
                    }
                    (fish, false)
                }
                None => {
                    let mut fish = Fish::new(id, agent.clone());
                    fish.zone = zone;
                    fish.tier = tier(agent, now);
                    (fish, true)
                }
            };
            fresh.push(fish);
        }
        let (fish, new): (Vec<Fish>, Vec<bool>) = fresh.into_iter().unzip();
        self.fish = fish;
        self.assign();
        // Newcomers start where they belong: a working fish somewhere in its
        // reef's water, any other at its own place.
        for (index, new) in new.into_iter().enumerate() {
            if !new {
                continue;
            }
            let (x, y) = if self.fish[index].mood == Mood::Working {
                let z = &self.zones[self.fish[index].zone];
                let (l, r, s) = (z.l, z.r, self.s);
                let (top, bottom) = (self.surface_y + 100. * s, z.canopy - 120. * s);
                let (a, b) = (self.rand(), self.rand());
                (lerp(l + 50. * s, r - 50. * s, a), lerp(top, bottom, b))
            } else {
                self.target(&self.fish[index])
            };
            self.fish[index].x = x;
            self.fish[index].y = y;
        }
    }

    /// Slots within each reef and mood, the crowd scale, and the schools.
    pub(super) fn assign(&mut self) {
        let mut counts = vec![0usize; self.zones.len().max(1)];
        let mut groups: Vec<((usize, Mood), usize)> = Vec::new();
        let last = counts.len() - 1;
        for f in &mut self.fish {
            counts[f.zone.min(last)] += 1;
            let key = (f.zone, f.mood);
            let slot = match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, n)) => {
                    *n += 1;
                    *n - 1
                }
                None => {
                    groups.push((key, 1));
                    0
                }
            };
            f.slot = slot;
        }
        for f in &mut self.fish {
            f.slot_n = groups
                .iter()
                .find(|(k, _)| *k == (f.zone, f.mood))
                .map_or(1, |(_, n)| *n);
            f.crowd = crowd(counts[f.zone.min(last)]);
            f.lane = lane(&f.agent);
            f.mates.clear();
        }
        let agents = self
            .fish
            .iter()
            .map(|f| f.agent.clone())
            .collect::<Vec<_>>();
        for (a, b) in galaxy::relations(&agents) {
            self.fish[a].mates.push(b);
            self.fish[b].mates.push(a);
        }
    }

    /// Where a fish that is not working heads: up under the surface when it
    /// needs you (spread across the reef, in two rows when their tags would
    /// touch), well above its reef when done, resting on the reef when
    /// idle. Done and idle fish spread across the reef's width.
    pub fn target(&self, f: &Fish) -> (f32, f32) {
        let (z, s, t) = (&self.zones[f.zone], self.s, self.t);
        let x = z.l + z.w * (0.12 + 0.76 * (f.slot as f32 + 0.5) / f.slot_n as f32);
        let open = z.canopy - self.surface_y;
        match f.mood {
            Mood::Waiting => {
                let (gap, row) = waiting_slot(z.w, f.slot, f.slot_n, s);
                (
                    z.l + z.w * 0.08 + gap * (f.slot as f32 + 0.5),
                    self.surface_y + (30. + row * 50.) * s + (t * 1.6 + f.seed).sin() * 3. * s,
                )
            }
            Mood::Done => (
                x + (t * 0.35 + f.seed).sin() * 10. * s,
                z.canopy - (56. * s).max(open * 0.2) + (t * 0.7 + f.seed).sin() * 5. * s,
            ),
            Mood::Working | Mood::Idle => (
                x + (t * 0.25 + f.seed).sin() * 6. * s,
                z.canopy - 16. * s + (t * 0.5 + f.seed).sin() * 2. * s,
            ),
        }
    }

    /// The open water a working fish keeps to on `z`: below the surface,
    /// well above the canopy.
    pub(crate) fn band(&self, z: &Zone) -> (f32, f32) {
        let s = self.s;
        let open = z.canopy - self.surface_y;
        (
            self.surface_y + 70. * s,
            z.canopy - (70. * s).max(open * 0.28),
        )
    }

    pub(super) fn bubble(&mut self, x: f32, y: f32, r: f32, vy: f32) {
        if self.bubbles.len() < 320 {
            let s = self.rand() * TAU;
            self.bubbles.push(Bubble { x, y, r, vy, s });
        }
    }
}

/// The school's lane, 0..1, so each school settles at its own depth: the
/// first draw of the generator seeded with its key (raw FNV-1a top bits
/// cluster for keys sharing a prefix). Fish school by project, else session.
pub(super) fn lane(agent: &Agent) -> f32 {
    let host = agent.destination.host.as_deref().unwrap_or_default();
    let key = agent.project.as_deref().unwrap_or(&agent.destination.name);
    Rng::new(hash(&format!("{host}/{key}"))).next()
}

/// The gap between waiting fish across a reef `w` wide, and slot's row: when
/// the tags would come closer than 96 points, alternate fish drop a row.
pub(crate) fn waiting_slot(w: f32, slot: usize, slot_n: usize, s: f32) -> (f32, f32) {
    let gap = w * 0.84 / slot_n.max(1) as f32;
    let row = if gap < 96. * s { (slot % 2) as f32 } else { 0. };
    (gap, row)
}
