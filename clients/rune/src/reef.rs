//! The reef: the fleet as an aquarium. Each machine is a reef across an even
//! share of the width, each agent a fish of its harness's species that acts
//! out its state: working fish school, one that needs you rises to the
//! surface, a finished one glides home and an idle one rests in the coral.
//! A port of the reference prototype (`phux-reef.html`): its constants, rng
//! and draw order. Pure, in world points (the view's size), so the painter,
//! the camera and the tests read one source.

use std::f32::consts::TAU;

use phux_protocol::ids::ResourceId;

use crate::activity::Mark;

mod colors;
mod coral;
mod sim;
pub use colors::Pal;
pub use coral::{TILE, Zone, caustic_tile, zone};
pub use sim::Swimmer;

pub type Rgb = [f32; 3];
pub type Pt = [f32; 2];

/// What a fish acts out, from the mark the galaxy colours a star by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mood {
    Working,
    /// Blocked, asking, failed or rang: rises to the surface.
    Waiting,
    Done,
    Idle,
}

impl Mood {
    /// The galaxy's marks in the prototype's four moods: a finished agent
    /// still counts as needing you there, here it glides home.
    pub fn of(mark: Mark) -> Mood {
        match mark {
            Mark::Done | Mark::Succeeded => Mood::Done,
            _ if mark.needs_you() => Mood::Waiting,
            _ if mark.live() => Mood::Working,
            _ => Mood::Idle,
        }
    }

    pub fn color(self) -> Rgb {
        match self {
            Mood::Working => [89.0, 212.0, 255.0],
            Mood::Waiting => [255.0, 181.0, 71.0],
            Mood::Done => [98.0, 230.0, 180.0],
            Mood::Idle => [159.0, 176.0, 189.0],
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Mood::Working => "working",
            Mood::Waiting => "needs you",
            Mood::Done => "done",
            Mood::Idle => "idle",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pattern {
    Bands,
    Mask,
    Lines,
    Spots,
    Plain,
}

#[derive(Debug, PartialEq)]
pub struct Species {
    pub body: Rgb,
    pub belly: Rgb,
    pub fin: Rgb,
    pub pattern: Pattern,
    pub fat: f32,
    pub tall: f32,
}

pub const CLAUDE: Species = Species {
    body: [255.0, 118.0, 58.0],
    belly: [255.0, 216.0, 176.0],
    fin: [255.0, 148.0, 84.0],
    pattern: Pattern::Bands,
    fat: 1.0,
    tall: 0.9,
};
pub const CODEX: Species = Species {
    body: [44.0, 102.0, 232.0],
    belly: [150.0, 202.0, 255.0],
    fin: [255.0, 212.0, 64.0],
    pattern: Pattern::Mask,
    fat: 1.05,
    tall: 0.8,
};
pub const OPENCODE: Species = Species {
    body: [242.0, 196.0, 50.0],
    belly: [255.0, 244.0, 186.0],
    fin: [248.0, 172.0, 36.0],
    pattern: Pattern::Lines,
    fat: 1.3,
    tall: 1.7,
};
pub const GEMINI: Species = Species {
    body: [150.0, 98.0, 242.0],
    belly: [222.0, 204.0, 255.0],
    fin: [104.0, 222.0, 255.0],
    pattern: Pattern::Spots,
    fat: 0.88,
    tall: 0.7,
};
/// Every other harness, and an agent that never said which it is.
pub const OTHER: Species = Species {
    body: [112.0, 190.0, 186.0],
    belly: [222.0, 246.0, 240.0],
    fin: [86.0, 170.0, 204.0],
    pattern: Pattern::Plain,
    fat: 0.95,
    tall: 0.8,
};

/// The species for an agent's declared kind slug (`claude`, `claude-code`,
/// `gemini-cli`), else its name, which is often the harness's.
pub fn species(kind: Option<&str>, name: &str) -> &'static Species {
    let of = |s: &str| {
        let s = s.to_ascii_lowercase();
        [
            ("claude", &CLAUDE),
            ("codex", &CODEX),
            ("opencode", &OPENCODE),
            ("gemini", &GEMINI),
        ]
        .into_iter()
        .find(|(slug, _)| s.starts_with(slug))
        .map(|(_, species)| species)
    };
    kind.and_then(of).or_else(|| of(name)).unwrap_or(&OTHER)
}

/// How much smaller fish are drawn in a reef of `n`, so a crowd stays readable.
pub fn fish_scale(n: usize) -> f32 {
    if n > 8 {
        (8.0 / n as f32).sqrt().max(0.55)
    } else {
        1.0
    }
}

/// The prototype's mulberry32: the same reef from the same seed.
#[derive(Debug, Clone)]
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Rng {
        Rng(seed)
    }

    pub fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x6D2B_79F5);
        let mut x = self.0;
        x = (x ^ (x >> 15)).wrapping_mul(x | 1);
        x ^= x.wrapping_add((x ^ (x >> 7)).wrapping_mul(x | 61));
        ((x ^ (x >> 14)) as f64 / 4_294_967_296.0) as f32
    }
}

/// FNV-1a over UTF-16 units, as the prototype hashes a host name.
pub fn hash(s: &str) -> u32 {
    s.encode_utf16().fold(2_166_136_261u32, |h, unit| {
        (h ^ unit as u32).wrapping_mul(16_777_619)
    })
}

pub fn lerp(a: f32, b: f32, k: f32) -> f32 {
    a + (b - a) * k
}

pub fn mix(a: Rgb, b: Rgb, k: f32) -> Rgb {
    [
        lerp(a[0], b[0], k),
        lerp(a[1], b[1], k),
        lerp(a[2], b[2], k),
    ]
}

#[derive(Debug, Clone)]
pub struct Ray {
    pub x: f32,
    pub w: f32,
    pub skew: f32,
    pub ph: f32,
    pub sp: f32,
}

#[derive(Debug, Clone)]
pub struct Kelp {
    pub x: f32,
    pub h: f32,
    pub seed: f32,
    pub w: f32,
    /// At the outer edges, over everything.
    pub front: bool,
    pub back: bool,
}

#[derive(Debug, Clone)]
pub struct Pebble {
    pub x: f32,
    pub dy: f32,
    pub rx: f32,
    pub sh: f32,
}

#[derive(Debug, Clone)]
pub struct Star {
    pub at: Pt,
    pub r: f32,
    pub tw: f32,
}

/// Where a fish swims to in its reef: the mood's target, prototype `targetFor`.
/// Slot `slot` of `slots` spreads fish of one mood in one reef; `t` is scene time.
pub fn target(
    mood: Mood,
    z: &Zone,
    [slot, slots]: [usize; 2],
    seed: f32,
    t: f32,
    s: f32,
    surface_y: f32,
) -> Pt {
    let off = slot as f32 - (slots as f32 - 1.0) / 2.0;
    match mood {
        Mood::Waiting => [
            z.cx + 26.0 * s + off * (64.0 * s).min(z.w * 0.7 / slots as f32),
            surface_y + 30.0 * s + (t * 1.6 + seed).sin() * 3.0 * s,
        ],
        Mood::Done => [
            z.cx + off * 54.0 * s + (t * 0.35 + seed).sin() * 16.0 * s,
            z.home_y - 34.0 * s + (t * 0.7 + seed).sin() * 5.0 * s,
        ],
        Mood::Idle | Mood::Working => {
            let n = z.nooks[slot % z.nooks.len()];
            [
                n[0] + (t * 0.25 + seed).sin() * 9.0 * s,
                n[1] + (t * 0.5 + seed).sin() * 2.0 * s,
            ]
        }
    }
}

/// An agent as a fish: who it is, and how it moves.
#[derive(Debug, Clone)]
pub struct Fish {
    pub who: Swimmer,
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub phase: f32,
    /// Base length, before the scene scale and the reef's crowd scale.
    pub lb: f32,
    pub cruise: f32,
    pub face: f32,
    pub face_s: f32,
    pub tilt: f32,
    pub wa: f32,
    pub bt: f32,
    pub rt: f32,
    pub seed: f32,
    pub pulse: f32,
    pub slot: usize,
    pub slots: usize,
    /// The crowd scale of its reef ([`fish_scale`]).
    pub k: f32,
}

impl Fish {
    /// Its drawn length at scene scale `s`.
    pub fn len(&self, s: f32) -> f32 {
        self.lb * s * self.k
    }
}

#[derive(Debug, Clone)]
pub struct Bubble {
    pub x: f32,
    pub y: f32,
    pub r: f32,
    pub vy: f32,
    pub s: f32,
}

#[derive(Debug, Clone)]
pub struct Ripple {
    pub x: f32,
    pub a: f32,
    pub amber: bool,
}

#[derive(Debug, Clone)]
pub struct Snow {
    pub x: f32,
    pub y: f32,
    pub r: f32,
    pub s: f32,
    /// Lit by a passing fish at night, fading.
    pub g: f32,
}

/// One of the background school.
#[derive(Debug, Clone)]
pub struct Mote {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
}

#[derive(Debug, Clone)]
pub struct Jelly {
    pub x: f32,
    pub by: f32,
    pub s: f32,
    pub ph: f32,
    pub vx: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cam {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

/// The whole aquarium: layout, creatures and camera.
#[derive(Debug, Clone)]
pub struct Reef {
    pub w: f32,
    pub h: f32,
    /// Scene scale.
    pub s: f32,
    pub surface_y: f32,
    pub floor_y: f32,
    pub zones: Vec<Zone>,
    pub rays: Vec<Ray>,
    pub kelp: Vec<Kelp>,
    pub far_a: Vec<Pt>,
    pub far_b: Vec<Pt>,
    pub pebbles: Vec<Pebble>,
    pub stars: Vec<Star>,
    pub fish: Vec<Fish>,
    pub bubbles: Vec<Bubble>,
    pub ripples: Vec<Ripple>,
    pub snow: Vec<Snow>,
    pub amb: Vec<Mote>,
    pub jellies: Vec<Jelly>,
    /// Scene seconds.
    pub t: f32,
    /// Night, 0 to 1, easing toward `nt_target`.
    pub nt: f32,
    pub nt_target: f32,
    pub cam: Cam,
    pub selected: Option<ResourceId>,
    pub hovered: Option<ResourceId>,
    /// Reduced motion: time at 0.4, the camera snaps.
    pub reduce: bool,
    /// Scene time not yet stepped, for a fixed step.
    pub(crate) carry: f32,
    pub(crate) rng: Rng,
    hosts: Vec<(String, bool)>,
}

impl Default for Reef {
    fn default() -> Self {
        Reef {
            w: 0.0,
            h: 0.0,
            s: 1.0,
            surface_y: 0.0,
            floor_y: 0.0,
            zones: Vec::new(),
            rays: Vec::new(),
            kelp: Vec::new(),
            far_a: Vec::new(),
            far_b: Vec::new(),
            pebbles: Vec::new(),
            stars: Vec::new(),
            fish: Vec::new(),
            bubbles: Vec::new(),
            ripples: Vec::new(),
            snow: Vec::new(),
            amb: Vec::new(),
            jellies: Vec::new(),
            t: 0.0,
            nt: 0.0,
            nt_target: 0.0,
            cam: Cam {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            },
            selected: None,
            hovered: None,
            reduce: false,
            carry: 0.0,
            rng: Rng::new(0x5EEF),
            hosts: Vec::new(),
        }
    }
}

impl Reef {
    /// The surface at `x`, waving.
    pub fn wave_y(&self, x: f32) -> f32 {
        let (t, s) = (self.t, self.s);
        self.surface_y
            + (x * 0.011 + t * 1.1).sin() * 3.0 * s
            + (x * 0.027 - t * 0.8).sin() * 1.6 * s
    }

    /// The sand at `x`.
    pub fn sand_y(&self, x: f32) -> f32 {
        let s = self.s;
        self.floor_y + (x * 0.007 + 1.3).sin() * 7.0 * s + (x * 0.019 + 0.4).sin() * 3.5 * s
    }

    pub fn pal(&self) -> Pal {
        Pal::at(self.nt)
    }

    pub fn zone_of(&self, f: &Fish) -> &Zone {
        &self.zones[f.who.host.min(self.zones.len().saturating_sub(1))]
    }

    /// The fish's target, prototype `targetFor`.
    pub fn target_for(&self, f: &Fish) -> Pt {
        let z = self.zone_of(f);
        target(
            f.who.mood,
            z,
            [f.slot, f.slots],
            f.seed,
            self.t,
            self.s,
            self.surface_y,
        )
    }

    /// Lay the scene out at `w` by `h` for `hosts` (name, agents known), the
    /// prototype's `layout`: zones split the width evenly in order.
    pub fn layout(&mut self, w: f32, h: f32, hosts: &[(String, bool)]) {
        if w <= 0.0 || h <= 0.0 || hosts.is_empty() {
            return;
        }
        if w == self.w && h == self.h && hosts == self.hosts.as_slice() {
            return;
        }
        let (old_w, old_h) = (self.w, self.h);
        self.w = w;
        self.h = h;
        self.hosts = hosts.to_vec();
        let s = ((w / 1200.0).min(h / 780.0) * 1.1).clamp(0.55, 1.25);
        self.s = s;
        self.surface_y = if w < 640.0 {
            (h * 0.19).max(150.0)
        } else {
            (h * 0.12).max(64.0)
        };
        self.floor_y = h * 0.83;
        let n = hosts.len();
        let zw = w / n as f32;
        self.zones = hosts
            .iter()
            .enumerate()
            .map(|(i, (host, known))| zone(host, *known, i, zw, s, self.floor_y))
            .collect();
        let r = &mut Rng::new(99);
        self.rays = (0..8)
            .map(|k| Ray {
                x: (k as f32 + 0.5) / 8.0 * w + (r.next() - 0.5) * w * 0.08,
                w: (36.0 + r.next() * 70.0) * s,
                skew: (50.0 + r.next() * 90.0) * s,
                ph: r.next() * TAU,
                sp: 0.2 + r.next() * 0.3,
            })
            .collect();
        self.kelp.clear();
        for i in 0..=n {
            for k in 0..3 {
                self.kelp.push(Kelp {
                    x: zw * i as f32 + (r.next() - 0.5) * 70.0 * s,
                    h: (self.floor_y - self.surface_y) * (0.42 + r.next() * 0.42),
                    seed: r.next() * TAU,
                    w: (2.2 + r.next() * 1.6) * s,
                    front: (i == 0 || i == n) && k == 0,
                    back: k == 2,
                });
            }
        }
        let floor_y = self.floor_y;
        let mut ridge = |amp: f32, base: f32| {
            let p = [r.next() * 6.0, r.next() * 6.0, r.next() * 6.0];
            (0..)
                .map(|k| -60.0 + 18.0 * k as f32)
                .take_while(|x| *x <= w + 60.0)
                .map(|x| {
                    let wob = (x * 0.013 + p[0]).sin() * 0.5
                        + (x * 0.031 + p[1]).sin() * 0.32
                        + (x * 0.071 + p[2]).sin() * 0.18;
                    [x, floor_y - s * (base + amp * wob)]
                })
                .collect::<Vec<Pt>>()
        };
        self.far_b = ridge(70.0, 120.0);
        self.far_a = ridge(50.0, 60.0);
        self.pebbles = (0..(w / 22.0).round() as usize)
            .map(|_| Pebble {
                x: r.next() * w,
                dy: r.next(),
                rx: (1.5 + r.next() * 3.0) * s,
                sh: r.next(),
            })
            .collect();
        let sy = self.surface_y;
        self.stars = (0..70)
            .map(|_| Star {
                at: [r.next() * w, r.next() * sy * 0.85],
                r: 0.4 + r.next() * 1.1,
                tw: r.next() * TAU,
            })
            .collect();
        self.seed_creatures();
        if old_w > 0.0 {
            let (kx, ky) = (w / old_w, h / old_h);
            for f in &mut self.fish {
                f.x *= kx;
                f.y *= ky;
            }
            for p in &mut self.snow {
                p.x *= kx;
                p.y *= ky;
            }
            for a in &mut self.amb {
                a.x *= kx;
                a.y *= ky;
            }
            for b in &mut self.bubbles {
                b.x *= kx;
                b.y *= ky;
            }
        } else {
            self.cam = Cam {
                x: w / 2.0,
                y: h / 2.0,
                z: 1.0,
            };
            self.nt = self.nt_target;
            for i in 0..self.fish.len() {
                self.place(i);
            }
            // The prototype's warm-up: the scene opens already alive.
            for _ in 0..150 {
                self.step(1.0 / 60.0);
            }
        }
        self.assign_slots();
    }

    /// Snow, the background school and the jellies, once.
    fn seed_creatures(&mut self) {
        let (w, h, sy, fy) = (self.w, self.h, self.surface_y, self.floor_y);
        let r = &mut self.rng;
        if self.snow.is_empty() {
            self.snow = (0..150)
                .map(|_| Snow {
                    x: r.next() * w,
                    y: sy + r.next() * (h - sy),
                    r: 0.4 + r.next() * 1.6,
                    s: r.next() * TAU,
                    g: 0.0,
                })
                .collect();
        }
        if self.amb.is_empty() {
            self.amb = (0..34)
                .map(|_| Mote {
                    x: w * 0.3 + r.next() * 120.0,
                    y: h * 0.45 + r.next() * 60.0,
                    vx: 20.0,
                    vy: 0.0,
                })
                .collect();
        }
        if self.jellies.is_empty() {
            self.jellies = (0..3)
                .map(|i| Jelly {
                    x: (i as f32 + 0.3) / 3.0 * w,
                    by: sy + (fy - sy) * (0.3 + r.next() * 0.25),
                    s: 0.7 + r.next() * 0.5,
                    ph: r.next() * TAU,
                    vx: if r.next() < 0.5 { -1.0 } else { 1.0 } * (4.0 + r.next() * 4.0),
                })
                .collect();
        }
    }

    /// Put fish `i` where its mood keeps it: a working one anywhere in its
    /// reef's water, the rest at their targets.
    pub(crate) fn place(&mut self, i: usize) {
        if self.zones.is_empty() {
            return;
        }
        let s = self.s;
        let (l, r) = {
            let z = self.zone_of(&self.fish[i]);
            (z.l + 50.0 * s, z.r - 50.0 * s)
        };
        if self.fish[i].who.mood == Mood::Working {
            let (a, b) = (self.rng.next(), self.rng.next());
            let (top, bot) = (self.surface_y + 100.0 * s, self.floor_y - 110.0 * s);
            self.fish[i].x = lerp(l, r, a);
            self.fish[i].y = lerp(top, bot, b);
        } else {
            let at = self.target_for(&self.fish[i]);
            self.fish[i].x = at[0];
            self.fish[i].y = at[1];
        }
    }

    /// Spread fish of one mood in one reef over their slots, and scale each
    /// reef's fish by its crowd.
    pub fn assign_slots(&mut self) {
        let mut groups: std::collections::HashMap<(usize, Mood), usize> = Default::default();
        let mut per_reef: std::collections::HashMap<usize, usize> = Default::default();
        for f in &mut self.fish {
            let n = groups.entry((f.who.host, f.who.mood)).or_default();
            f.slot = *n;
            *n += 1;
            *per_reef.entry(f.who.host).or_default() += 1;
        }
        for f in &mut self.fish {
            f.slots = groups[&(f.who.host, f.who.mood)];
            f.k = fish_scale(per_reef[&f.who.host]);
        }
    }

    pub fn waiting_in(&self, zone: usize) -> usize {
        self.fish
            .iter()
            .filter(|f| f.who.host == zone && f.who.mood == Mood::Waiting)
            .count()
    }

    pub fn fish_in(&self, zone: usize) -> usize {
        self.fish.iter().filter(|f| f.who.host == zone).count()
    }
}

#[cfg(test)]
mod tests;
