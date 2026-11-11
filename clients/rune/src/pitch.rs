//! The pitch: the fleet as football training. Each machine is a squad
//! training in its own share of the pitch, in the galaxy's machine order;
//! each agent a player in its harness's kit acting out its state: working
//! players drill and pass, one that needs you waits on the touchline with a
//! hand up, a finished one celebrates and then cools down, an idle one sits
//! in the dugout. A port of the playground prototype (`scenes/pitch.js`):
//! its constants, rng and draw order. Pure, in view points, so the painter,
//! the background and the tests read one source.
//!
//! The prototype works in pitch coordinates, `l` along the pitch and `s`
//! across it; a portrait view turns the pitch upright ([`Pitch::to_s`]).

use phux_protocol::ids::ResourceId;

use crate::creature::Work;
pub use crate::reef::{Mood, Pt, Rgb, Rng, Species, hash, lerp, mix};

pub mod ground;
mod sim;

pub const AMBER: Rgb = [255.0, 181.0, 71.0];
pub const MINT: Rgb = [98.0, 230.0, 180.0];
pub const GOLD: Rgb = [255.0, 206.0, 84.0];

/// The view's chrome, top and bottom: the title bar and the readout.
pub const INSET: [f32; 2] = [40.0, 50.0];

/// What the card calls each tier ([`crate::creature::TIERS`]).
pub const RANKS: [&str; 5] = ["Rookie", "Regular", "Veteran", "Star", "Legend"];

/// The ground's colours, by day and by night.
#[derive(Debug, Clone, Copy)]
pub struct Pal {
    pub stand: Rgb,
    pub riser: Rgb,
    pub seat: Rgb,
    pub g1: Rgb,
    pub g2: Rgb,
    pub run: Rgb,
    pub line: Rgb,
    pub roof: Rgb,
    pub roof_edge: Rgb,
    pub fan: f32,
    pub dug: Rgb,
    pub board: Rgb,
}

pub const DAY: Pal = Pal {
    stand: [150.0, 158.0, 172.0],
    riser: [126.0, 134.0, 150.0],
    seat: [78.0, 104.0, 150.0],
    g1: [96.0, 176.0, 86.0],
    g2: [82.0, 160.0, 76.0],
    run: [74.0, 144.0, 68.0],
    line: [250.0, 252.0, 246.0],
    roof: [196.0, 202.0, 212.0],
    roof_edge: [120.0, 128.0, 142.0],
    fan: 1.0,
    dug: [214.0, 224.0, 234.0],
    board: [34.0, 40.0, 52.0],
};
pub const NIGHT: Pal = Pal {
    stand: [18.0, 22.0, 32.0],
    riser: [26.0, 31.0, 44.0],
    seat: [28.0, 36.0, 58.0],
    g1: [36.0, 88.0, 62.0],
    g2: [30.0, 77.0, 56.0],
    run: [16.0, 44.0, 36.0],
    line: [214.0, 234.0, 224.0],
    roof: [10.0, 13.0, 20.0],
    roof_edge: [40.0, 48.0, 64.0],
    fan: 0.42,
    dug: [54.0, 66.0, 86.0],
    board: [12.0, 16.0, 24.0],
};

/// The crowd's shirts.
pub const FANS: [Rgb; 10] = [
    [255.0, 118.0, 58.0],
    [44.0, 102.0, 232.0],
    [242.0, 196.0, 50.0],
    [150.0, 98.0, 242.0],
    [112.0, 190.0, 186.0],
    [236.0, 238.0, 242.0],
    [212.0, 62.0, 74.0],
    [38.0, 40.0, 50.0],
    [255.0, 152.0, 190.0],
    [70.0, 170.0, 90.0],
];
pub const SKINS: [Rgb; 6] = [
    [241.0, 200.0, 160.0],
    [198.0, 142.0, 100.0],
    [140.0, 92.0, 62.0],
    [232.0, 180.0, 140.0],
    [96.0, 62.0, 44.0],
    [250.0, 214.0, 180.0],
];
pub const HAIRS: [Rgb; 6] = [
    [40.0, 30.0, 24.0],
    [90.0, 60.0, 30.0],
    [20.0, 20.0, 22.0],
    [200.0, 160.0, 90.0],
    [140.0, 60.0, 30.0],
    [60.0, 40.0, 30.0],
];

/// `c` washed `q` of the way to its own grey: an idle player's faded kit.
pub fn grey(c: Rgb, q: f32) -> Rgb {
    let g = c[0] * 0.3 + c[1] * 0.59 + c[2] * 0.11;
    mix(c, [g, g, g], q)
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// An agent as the pitch needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Agent {
    pub id: ResourceId,
    pub name: String,
    /// Its machine's zone.
    pub host: usize,
    pub mood: Mood,
    pub species: &'static Species,
    /// The shirt number: its terminal's `@N`.
    pub number: u32,
    /// Its work from its machine's agent stats, when published.
    pub work: Option<Work>,
}

/// What a tier adds to the kit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kit {
    pub boots: Boots,
    /// The captain's armband on the front sleeve.
    pub armband: bool,
    /// A gold edge on the shirt.
    pub trim: bool,
    /// A spinning star overhead.
    pub star: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boots {
    Black,
    /// The kit's sleeve colour.
    Sleeve,
    Gold,
}

/// The prototype's tier flourishes, rookie to legend.
pub fn kit(tier: usize) -> Kit {
    Kit {
        boots: match tier {
            0 => Boots::Black,
            1 | 2 => Boots::Sleeve,
            _ => Boots::Gold,
        },
        armband: tier >= 2,
        trim: tier >= 3,
        star: tier >= 4,
    }
}

/// Where a player who needs you (`Waiting`) or is idle stands: the
/// touchline in front of its third, or its dugout. Slot `slot` of `slots`.
/// Pitch coordinates; `strip` is the band beyond the touchline.
pub fn post(mood: Mood, z: &Zone, [slot, slots]: [usize; 2], k: f32, strip: f32) -> Option<Pt> {
    let off = slot as f32 - (slots as f32 - 1.0) / 2.0;
    match mood {
        Mood::Waiting => Some([
            (z.z0 + z.zw * 0.74 + off * 17.0 * k)
                .clamp(z.z0 + 10.0, (z.z0 + z.zw - 10.0).max(z.z0 + 10.0)),
            -strip * 0.3,
        ]),
        Mood::Idle => Some([z.z0 + z.zw * 0.3 + off * 14.0 * k, -strip * 0.56]),
        _ => None,
    }
}

/// An agent as a player: who it is, and how it moves.
#[derive(Debug, Clone)]
pub struct Player {
    pub who: Agent,
    pub tier: usize,
    pub l: f32,
    pub s: f32,
    pub vl: f32,
    pub vs: f32,
    /// Facing, -1 to 1, easing toward `ft`.
    pub face: f32,
    pub ft: f32,
    pub phase: f32,
    pub run: f32,
    pub sit: f32,
    /// Celebration seconds left.
    pub cel: f32,
    /// The mood it last acted on; `None` before its first step.
    pub mood: Option<Mood>,
    pub mt: f32,
    pub slot: usize,
    /// A working player's drill target.
    pub tl: Option<Pt>,
    pub pause: f32,
    /// A cooling-down player's way round its third.
    pub ang: Option<f32>,
    pub arrived: bool,
    pub chase: bool,
    pub recv: bool,
    pub has: bool,
    /// Skin and hair, from who it is.
    pub look: usize,
    /// Not placed yet: the next step puts it in its third and warms it up.
    pub fresh: bool,
}

#[derive(Debug, Clone)]
pub struct Cone {
    pub l: f32,
    pub s: f32,
    pub c: Rgb,
}

/// A pass in the air.
#[derive(Debug, Clone)]
pub struct Flight {
    pub to: ResourceId,
    pub l0: f32,
    pub s0: f32,
    pub p: f32,
    pub dur: f32,
    pub loft: f32,
}

#[derive(Debug, Clone)]
pub struct Ball {
    pub l: f32,
    pub s: f32,
    /// Height off the grass.
    pub h: f32,
    pub owner: Option<ResourceId>,
    pub fl: Option<Flight>,
    pub hold: f32,
}

/// A machine's share of the pitch.
#[derive(Debug, Clone)]
pub struct Zone {
    pub host: String,
    /// Whether this window can list its agents.
    pub known: bool,
    pub i: usize,
    pub z0: f32,
    pub zw: f32,
    pub cones: Vec<Cone>,
    pub ball: Ball,
    /// This step's counts of players waiting and idle, and its working ones.
    pub wait: usize,
    pub idle: usize,
    pub work: Vec<usize>,
}

/// Confetti, in view points.
#[derive(Debug, Clone)]
pub struct Part {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub life: f32,
    pub rot: f32,
    pub vr: f32,
    pub c: Rgb,
}

/// A camera flash in the stands at night.
#[derive(Debug, Clone)]
pub struct Flash {
    pub x: f32,
    pub y: f32,
    pub life: f32,
}

#[derive(Debug, Clone)]
pub struct Pigeon {
    pub l: f32,
    pub s: f32,
    pub fly: f32,
    pub vl: f32,
    pub vs: f32,
    pub a: f32,
    pub peck: f32,
}

/// The whole ground: layout, players and balls.
#[derive(Debug, Clone)]
pub struct Pitch {
    pub w: f32,
    pub h: f32,
    pub portrait: bool,
    /// The dugout band beyond the near touchline (the prototype's `T`).
    pub strip: f32,
    /// The pitch, and the ground round it, in view points.
    pub p: Rect,
    pub g: Rect,
    /// The pitch's length and width in its own coordinates.
    pub lp: f32,
    pub sp: f32,
    /// Scene scale.
    pub k: f32,
    pub zones: Vec<Zone>,
    pub towers: Vec<Pt>,
    pub players: Vec<Player>,
    pub parts: Vec<Part>,
    pub flashes: Vec<Flash>,
    pub pigeons: Vec<Pigeon>,
    /// Scene seconds.
    pub t: f32,
    /// Night, 0 to 1, easing toward `nt_target`.
    pub nt: f32,
    pub nt_target: f32,
    pub selected: Option<ResourceId>,
    pub hovered: Option<ResourceId>,
    /// Reduced motion: time at 0.4, no flashes.
    pub reduce: bool,
    /// Bumped by every new layout: the background is drawn once per one.
    pub laid: u64,
    pub(crate) carry: f32,
    pub(crate) rng: Rng,
    hosts: Vec<(String, bool)>,
}

impl Default for Pitch {
    fn default() -> Self {
        Pitch {
            w: 0.0,
            h: 0.0,
            portrait: false,
            strip: 0.0,
            p: Rect::default(),
            g: Rect::default(),
            lp: 0.0,
            sp: 0.0,
            k: 1.0,
            zones: Vec::new(),
            towers: Vec::new(),
            players: Vec::new(),
            parts: Vec::new(),
            flashes: Vec::new(),
            pigeons: Vec::new(),
            t: 0.0,
            nt: 0.0,
            nt_target: 0.0,
            selected: None,
            hovered: None,
            reduce: false,
            laid: 0,
            carry: 0.0,
            rng: Rng::new(0x9177C4),
            hosts: Vec::new(),
        }
    }
}

impl Pitch {
    /// Pitch coordinates to view points.
    pub fn to_s(&self, l: f32, s: f32) -> Pt {
        if self.portrait {
            [self.p.x + s, self.p.y + l]
        } else {
            [self.p.x + l, self.p.y + s]
        }
    }

    pub fn zone_at(&self, host: usize) -> usize {
        host.min(self.zones.len().saturating_sub(1))
    }

    pub fn zone_of(&self, p: &Player) -> &Zone {
        &self.zones[self.zone_at(p.who.host)]
    }

    /// Lay the ground out at `w` by `h` for `hosts` (name, agents known), the
    /// prototype's `layout`: a squad's third per machine, in order.
    pub fn layout(&mut self, w: f32, h: f32, hosts: &[(String, bool)]) {
        if w <= 0.0 || h <= 0.0 || hosts.is_empty() {
            return;
        }
        if w == self.w && h == self.h && hosts == self.hosts.as_slice() {
            return;
        }
        self.w = w;
        self.h = h;
        self.hosts = hosts.to_vec();
        self.laid += 1;
        self.portrait = h > w;
        let (b0, b1) = (INSET[0] + 4.0, h - INSET[1] - 4.0);
        let (old_l, old_s) = (self.lp, self.sp);
        if !self.portrait {
            self.strip = 60.0;
            let y = b0 + self.strip;
            let ph = (b1 - y - 16.0).max(160.0);
            let pw = (w - 100.0).min(ph * 1.75);
            self.p = Rect {
                x: (w - pw) / 2.0,
                y,
                w: pw,
                h: ph,
            };
            (self.lp, self.sp) = (pw, ph);
            self.g = Rect {
                x: self.p.x - 20.0,
                y: self.p.y - self.strip,
                w: pw + 40.0,
                h: ph + self.strip + 20.0,
            };
        } else {
            self.strip = 44.0;
            let x = self.strip + 8.0;
            let pw = w - x - 20.0;
            let ph = (b1 - b0).min(pw * 1.8);
            self.p = Rect {
                x,
                y: b0 + (b1 - b0 - ph) / 2.0,
                w: pw,
                h: ph,
            };
            (self.lp, self.sp) = (ph, pw);
            self.g = Rect {
                x: self.p.x - self.strip,
                y: self.p.y - 16.0,
                w: pw + self.strip + 12.0,
                h: ph + 32.0,
            };
        }
        self.k = (self.sp / 420.0).clamp(0.8, 1.3);
        let (kl, ks) = if old_l > 0.0 {
            (self.lp / old_l, self.sp / old_s)
        } else {
            (1.0, 1.0)
        };
        for p in &mut self.players {
            p.l *= kl;
            p.s *= ks;
            if let Some(t) = &mut p.tl {
                *t = [t[0] * kl, t[1] * ks];
            }
        }
        let n = hosts.len();
        let (lp, sp) = (self.lp, self.sp);
        let old = std::mem::take(&mut self.zones);
        self.zones = hosts
            .iter()
            .enumerate()
            .map(|(i, (host, known))| {
                let r = &mut Rng::new(hash(host));
                let zw = lp / n as f32;
                let z0 = i as f32 * zw;
                let cones = (0..6)
                    .map(|_| Cone {
                        l: z0 + zw * (0.16 + 0.68 * r.next()),
                        s: sp * (0.2 + 0.6 * r.next()),
                        c: if r.next() < 0.5 {
                            [255.0, 130.0, 40.0]
                        } else {
                            [250.0, 214.0, 60.0]
                        },
                    })
                    .collect();
                let ball = match old.get(i) {
                    Some(z) => Ball {
                        l: z.ball.l * kl,
                        s: z.ball.s * ks,
                        ..z.ball.clone()
                    },
                    None => Ball {
                        l: z0 + zw / 2.0,
                        s: sp / 2.0,
                        h: 0.0,
                        owner: None,
                        fl: None,
                        hold: 0.0,
                    },
                };
                Zone {
                    host: host.clone(),
                    known: *known,
                    i,
                    z0,
                    zw,
                    cones,
                    ball,
                    wait: 0,
                    idle: 0,
                    work: Vec::new(),
                }
            })
            .collect();
        let g = self.g;
        self.towers = [
            [g.x - 14.0, g.y - 14.0],
            [g.x + g.w + 14.0, g.y - 14.0],
            [g.x - 14.0, g.y + g.h + 14.0],
            [g.x + g.w + 14.0, g.y + g.h + 14.0],
        ]
        .map(|[x, y]| [x.clamp(12.0, w - 12.0), y.clamp(12.0, h - 12.0)])
        .to_vec();
        self.parts.clear();
        self.flashes.clear();
        let r = &mut self.rng;
        self.pigeons = (0..5)
            .map(|_| Pigeon {
                l: lp * (0.1 + 0.8 * r.next()),
                s: sp * r.next(),
                fly: 0.0,
                vl: 0.0,
                vs: 0.0,
                a: 1.0,
                peck: r.next() * 6.0,
            })
            .collect();
        if old_l == 0.0 {
            self.nt = self.nt_target;
        }
    }

    pub fn waiting_in(&self, zone: usize) -> usize {
        self.players
            .iter()
            .filter(|p| self.zone_at(p.who.host) == zone && p.who.mood == Mood::Waiting)
            .count()
    }

    pub fn players_in(&self, zone: usize) -> usize {
        self.players
            .iter()
            .filter(|p| self.zone_at(p.who.host) == zone)
            .count()
    }

    pub fn selected_player(&self) -> Option<&Player> {
        let id = self.selected.as_ref()?;
        self.players.iter().find(|p| &p.who.id == id)
    }
}

#[cfg(test)]
mod tests;
