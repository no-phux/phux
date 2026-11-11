//! The pit lane: the fleet as a race circuit. Each machine is a team garage
//! on the pit lane, each agent a car in its harness's livery acting out its
//! state: a working car laps the circuit, one that needs you boxes at its
//! garage with the lollipop up, a finished one parks in parc fermé under a
//! chequered flag and an idle one sits covered in its garage.
//! A port of the playground prototype (`scenes/pitlane.js`): its circuit,
//! constants, rng and draw order. Pure, in view points, so the painter and
//! the tests read one source.

use phux_protocol::ids::ResourceId;

use crate::creature::Work;
use crate::reef::{Mood, Pt, Rgb, Rng, Species, hash, lerp};

mod sim;

pub const AMBER: Rgb = [255.0, 181.0, 71.0];
pub const MINT: Rgb = [98.0, 230.0, 180.0];
pub const GOLD: Rgb = [255.0, 206.0, 84.0];
/// The prototype's ranks, by creature tier.
pub const RANKS: [&str; 5] = ["Rookie", "Regular", "Veteran", "Star", "Legend"];

/// Points between the circuit's resampled points.
pub const SPACING: f32 = 3.0;
/// The view's top and foot the circuit keeps clear of: the titlebar, and
/// the readout along the foot.
const TOP: f32 = 44.0;
const FOOT: f32 = 56.0;

/// The circuit's control points in (along, across) units; portrait swaps the
/// axes. The main straight runs along across = 0.1.
const CTRL: [Pt; 18] = [
    [0.06, 0.1],
    [0.25, 0.1],
    [0.5, 0.1],
    [0.75, 0.1],
    [0.92, 0.1],
    [0.975, 0.2],
    [0.965, 0.5],
    [0.95, 0.78],
    [0.86, 0.92],
    [0.7, 0.9],
    [0.6, 0.76],
    [0.5, 0.62],
    [0.4, 0.74],
    [0.3, 0.9],
    [0.12, 0.9],
    [0.035, 0.72],
    [0.025, 0.3],
    [0.03, 0.16],
];
const PIT: [Pt; 7] = [
    [0.065, 0.112],
    [0.1, 0.17],
    [0.13, 0.2],
    [0.5, 0.2],
    [0.87, 0.2],
    [0.9, 0.17],
    [0.935, 0.112],
];
/// Parc fermé: along from, to, across from, to.
pub const PARC: [f32; 4] = [0.745, 0.875, 0.25, 0.42];

/// A machine as laid out: its name, whether its agents are known, and theirs.
type Host = (String, bool, Vec<ResourceId>);

/// An agent as the pit lane needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Driver {
    pub id: ResourceId,
    pub name: String,
    /// Its machine's garage.
    pub host: usize,
    pub mood: Mood,
    pub species: &'static Species,
    /// Its work from its machine's agent stats, when published.
    pub work: Option<Work>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Track,
    Pit,
}

/// An agent as a car: who it is, and where it is on the circuit or the pit lane.
#[derive(Debug, Clone)]
pub struct Car {
    pub who: Driver,
    /// Its [`crate::creature`] tier, 0 to 4.
    pub tier: usize,
    pub mode: Mode,
    /// Distance round the circuit, its speed and lane (-1 to 1) on it.
    pub d: f32,
    pub v: f32,
    pub lane: f32,
    pub lane_t: f32,
    pub lane_hold: f32,
    /// Distance down the pit lane, and out from it toward the garages.
    pub p: f32,
    pub q: f32,
    /// Heading, easing toward `h_t`.
    pub h: f32,
    pub h_t: f32,
    /// Its pace, 0.84 to 1.06, from its name.
    pub f: f32,
    /// A fraction from who it is, for its rhythm.
    pub seed: f32,
    pub trail: Vec<Pt>,
    /// The mood it last acted on, and whether it reached its spot since.
    pub(crate) acting: Mood,
    pub landed: bool,
    pub speed: f32,
    pub x: f32,
    pub y: f32,
    pub(crate) placed: bool,
}

impl Car {
    /// Its scale on top of the scene's: higher tiers are a little bigger.
    pub fn u(&self, k: f32) -> f32 {
        k * (1.0 + 0.05 * self.tier as f32)
    }

    /// The radius its press target and selection ring go round.
    pub fn r(&self, k: f32) -> f32 {
        13.0 * self.u(k)
    }
}

/// One car's box in a garage.
#[derive(Debug, Clone, PartialEq)]
pub struct Bay {
    pub id: ResourceId,
    /// Along the circuit, and down the pit lane.
    pub u: f32,
    pub p: f32,
}

/// A machine's garage: a stretch of the pit lane, a box per agent.
#[derive(Debug, Clone)]
pub struct Garage {
    pub host: String,
    /// Whether its agents are known to this window.
    pub known: bool,
    pub u0: f32,
    pub u1: f32,
    pub c: Rgb,
    pub bays: Vec<Bay>,
}

#[derive(Debug, Clone)]
pub struct Tree {
    pub at: Pt,
    pub r: f32,
}

/// Confetti from a car parking in parc fermé.
#[derive(Debug, Clone)]
pub struct Bit {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub life: f32,
    pub rot: f32,
    pub vr: f32,
    pub c: Rgb,
}

/// A spark of a firework over the grandstand at night.
#[derive(Debug, Clone)]
pub struct Ember {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub life: f32,
    pub c: Rgb,
}

/// The whole circuit: layout, cars and effects.
#[derive(Debug, Clone)]
pub struct PitLane {
    pub w: f32,
    pub h: f32,
    pub portrait: bool,
    /// The circuit's frame: its corner, and its length along and across.
    pub at: Pt,
    pub lp: f32,
    pub sp: f32,
    /// Scene scale, from how many boxes the pit lane holds.
    pub k: f32,
    pub trk: Vec<Pt>,
    pub pit: Vec<Pt>,
    /// How much of full speed each circuit point allows, braking ahead of bends.
    pub slow: Vec<f32>,
    /// Where on the circuit the pit lane leaves and rejoins it.
    pub entry: f32,
    pub exit: f32,
    /// Out from the pit lane toward the garages.
    pub nrm: Pt,
    pub garages: Vec<Garage>,
    /// Parc fermé's spots, down the pit lane and out from it.
    pub parc: Vec<[f32; 2]>,
    pub trees: Vec<Tree>,
    pub poles: Vec<Pt>,
    pub cars: Vec<Car>,
    pub bits: Vec<Bit>,
    pub embers: Vec<Ember>,
    pub(crate) next_firework: f32,
    /// Scene seconds.
    pub t: f32,
    /// Night, 0 to 1, easing toward `nt_target`.
    pub nt: f32,
    pub nt_target: f32,
    pub selected: Option<ResourceId>,
    pub hovered: Option<ResourceId>,
    /// Reduced motion: time at 0.4, no fireworks.
    pub reduce: bool,
    /// Bumped whenever the layout changes: the painter's cue to redraw the ground.
    pub rev: u64,
    pub(crate) carry: f32,
    pub(crate) rng: Rng,
    key: (f32, f32, Vec<Host>),
}

impl Default for PitLane {
    fn default() -> Self {
        PitLane {
            w: 0.0,
            h: 0.0,
            portrait: false,
            at: [0.0, 0.0],
            lp: 0.0,
            sp: 0.0,
            k: 1.0,
            trk: Vec::new(),
            pit: Vec::new(),
            slow: Vec::new(),
            entry: 0.0,
            exit: 0.0,
            nrm: [0.0, 1.0],
            garages: Vec::new(),
            parc: Vec::new(),
            trees: Vec::new(),
            poles: Vec::new(),
            cars: Vec::new(),
            bits: Vec::new(),
            embers: Vec::new(),
            next_firework: 3.0,
            t: 0.0,
            nt: 0.0,
            nt_target: 0.0,
            selected: None,
            hovered: None,
            reduce: false,
            rev: 0,
            carry: 0.0,
            rng: Rng::new(0x9175),
            key: (0.0, 0.0, Vec::new()),
        }
    }
}

/// `pts` resampled every [`SPACING`] points along their length.
pub fn resample(pts: &[Pt], closed: bool) -> Vec<Pt> {
    let mut src = pts.to_vec();
    if closed {
        src.push(pts[0]);
    }
    let mut out = vec![src[0]];
    let mut acc = 0.0;
    for w in src.windows(2) {
        let (a, b) = (w[0], w[1]);
        let d = (b[0] - a[0]).hypot(b[1] - a[1]);
        let mut t = SPACING - acc;
        while t <= d {
            out.push([a[0] + (b[0] - a[0]) * t / d, a[1] + (b[1] - a[1]) * t / d]);
            t += SPACING;
        }
        acc = d - (t - SPACING);
    }
    out
}

/// A point `d` along the resampled `arr`, its heading and the index before it.
pub fn at(arr: &[Pt], d: f32, closed: bool) -> (Pt, f32, usize) {
    let n = arr.len();
    let f = d / SPACING;
    let (i, j, t) = if closed {
        let i = (f.floor() as i64).rem_euclid(n as i64) as usize;
        (i, (i + 1) % n, f - f.floor())
    } else {
        let i = (f.floor().max(0.0) as usize).min(n - 2);
        (i, i + 1, (f - i as f32).clamp(0.0, 1.0))
    };
    let (a, b) = (arr[i], arr[j]);
    (
        [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t],
        (b[1] - a[1]).atan2(b[0] - a[0]),
        i,
    )
}

/// The distance along `arr` of its point nearest `p`.
pub fn nearest(arr: &[Pt], p: Pt) -> f32 {
    let d2 = |q: &Pt| (q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2);
    let best = (0..arr.len())
        .min_by(|&a, &b| d2(&arr[a]).total_cmp(&d2(&arr[b])))
        .unwrap_or(0);
    best as f32 * SPACING
}

/// How much of full speed each point allows: the bend six points ahead
/// slows it, and braking starts up to 45 points before a bend.
pub fn braking(trk: &[Pt], k: f32) -> Vec<f32> {
    let n = trk.len();
    let cf: Vec<f32> = (0..n)
        .map(|i| {
            let a = at(trk, i as f32 * SPACING, true).1;
            let b = at(trk, (i + 6) as f32 * SPACING, true).1;
            let mut d = (b - a).abs();
            if d > std::f32::consts::PI {
                d = std::f32::consts::TAU - d;
            }
            (1.0 - d / (6.0 * SPACING) * 34.0 / k).clamp(0.42, 1.0)
        })
        .collect();
    (0..n)
        .map(|i| (0..45).fold(1.0f32, |m, j| m.min(cf[(i + j) % n] + j as f32 * 0.008)))
        .collect()
}

impl PitLane {
    /// Circuit units to view points.
    pub fn to_s(&self, u: f32, v: f32) -> Pt {
        if self.portrait {
            [self.at[0] + v * self.sp, self.at[1] + u * self.lp]
        } else {
            [self.at[0] + u * self.lp, self.at[1] + v * self.sp]
        }
    }

    /// A rectangle in circuit units as x, y, width, height in points.
    pub fn rect_s(&self, u0: f32, v0: f32, u1: f32, v1: f32) -> [f32; 4] {
        let (a, b) = (self.to_s(u0, v0), self.to_s(u1, v1));
        [
            a[0].min(b[0]),
            a[1].min(b[1]),
            (b[0] - a[0]).abs(),
            (b[1] - a[1]).abs(),
        ]
    }

    /// A Catmull-Rom spline through `ctrl`, resampled.
    fn spline(&self, ctrl: &[Pt], closed: bool) -> Vec<Pt> {
        let n = ctrl.len() as i64;
        let get = |i: i64| {
            if closed {
                ctrl[i.rem_euclid(n) as usize]
            } else {
                ctrl[i.clamp(0, n - 1) as usize]
            }
        };
        let mut pts = Vec::new();
        for i in 0..(if closed { n } else { n - 1 }) {
            let (p0, p1, p2, p3) = (get(i - 1), get(i), get(i + 1), get(i + 2));
            for j in 0..30 {
                let t = j as f32 / 30.0;
                let (t2, t3) = (t * t, t * t * t);
                let f = |a: f32, b: f32, c: f32, d: f32| {
                    0.5 * (2.0 * b
                        + (-a + c) * t
                        + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2
                        + (-a + 3.0 * b - 3.0 * c + d) * t3)
                };
                pts.push(self.to_s(f(p0[0], p1[0], p2[0], p3[0]), f(p0[1], p1[1], p2[1], p3[1])));
            }
        }
        if !closed {
            let last = ctrl[ctrl.len() - 1];
            pts.push(self.to_s(last[0], last[1]));
        }
        resample(&pts, closed)
    }

    pub fn lap(&self) -> f32 {
        self.trk.len() as f32 * SPACING
    }

    pub fn pit_len(&self) -> f32 {
        (self.pit.len().max(1) - 1) as f32 * SPACING
    }

    /// Lay the circuit out at `w` by `h` for `hosts` (name, agents known) and
    /// the cars as synced, the prototype's `layout`: a garage per machine in
    /// order, a box per agent. Cars keep their place on a new layout.
    pub fn layout(&mut self, w: f32, h: f32, hosts: &[(String, bool)]) {
        if w <= 0.0 || h <= 0.0 || hosts.is_empty() {
            return;
        }
        let per_host: Vec<Vec<ResourceId>> = (0..hosts.len())
            .map(|i| {
                let last = hosts.len() - 1;
                let ids = self.cars.iter().filter(|c| c.who.host.min(last) == i);
                ids.map(|c| c.who.id.clone()).collect()
            })
            .collect();
        let key = (
            w,
            h,
            hosts
                .iter()
                .zip(&per_host)
                .map(|((n, k), ids)| (n.clone(), *k, ids.clone()))
                .collect(),
        );
        if key == self.key {
            return;
        }
        self.key = key;
        let (old_l, old_pl, old_sp) = (self.lap(), self.pit_len(), self.sp);
        self.w = w;
        self.h = h;
        self.portrait = h > w;
        let (y0, y1) = (TOP, (h - FOOT).max(TOP + 1.0));
        if self.portrait {
            self.at = [14.0, y0 + 6.0];
            self.lp = (y1 - y0 - 12.0).max(1.0);
            self.sp = w - 28.0;
        } else {
            // The grandstand stands above the frame; its roof may tuck under
            // the titlebar as the prototype's does under its strip.
            self.sp = (y1 - y0) / 1.1;
            self.at = [30.0, y0 + 0.1 * self.sp];
            self.lp = w - 60.0;
        }
        // A machine whose agents aren't known still gets a box of garage.
        let slots: usize = per_host.iter().map(|ids| ids.len().max(1)).sum();
        let per = 0.59 / (slots as f32 + hosts.len() as f32 * 0.5);
        self.k = (per * self.lp / 34.0).clamp(0.6, 1.15);
        self.nrm = if self.portrait {
            [1.0, 0.0]
        } else {
            [0.0, 1.0]
        };
        self.trk = self.spline(&CTRL, true);
        self.pit = self.spline(&PIT, false);
        self.slow = braking(&self.trk, self.k);
        self.entry = nearest(&self.trk, self.to_s(0.065, 0.1));
        self.exit = nearest(&self.trk, self.to_s(0.935, 0.1));
        let mut u = 0.13;
        self.garages = hosts
            .iter()
            .zip(&per_host)
            .map(|((host, known), ids)| {
                let bays = ids
                    .iter()
                    .enumerate()
                    .map(|(j, id)| {
                        let bu = u + per * (j as f32 + 0.5);
                        let p = nearest(&self.pit, self.to_s(bu, 0.2));
                        Bay {
                            id: id.clone(),
                            u: bu,
                            p,
                        }
                    })
                    .collect();
                let g = Garage {
                    host: host.clone(),
                    known: *known,
                    u0: u,
                    u1: u + per * ids.len().max(1) as f32,
                    // The team colour is the machine's own.
                    c: crate::host_color::channels(host),
                    bays,
                };
                u = g.u1 + per * 0.5;
                g
            })
            .collect();
        // Four across, two rows, then a third row between them.
        self.parc = (0..12)
            .map(|i| {
                let [u0, u1, v0, v1] = PARC;
                let uu = lerp(u0, u1, ((i % 4) as f32 + 0.5) / 4.0);
                let row = if i >= 8 {
                    0.5
                } else if i / 4 == 1 {
                    0.74
                } else {
                    0.28
                };
                let p = nearest(&self.pit, self.to_s(uu, 0.2));
                [p, (lerp(v0, v1, row) - 0.2) * self.sp]
            })
            .collect();
        self.plant();
        let (l, pl) = (self.lap(), self.pit_len());
        if old_l == 0.0 {
            // Opens at the theme's time of day; later changes ease.
            self.nt = self.nt_target;
        } else {
            for c in &mut self.cars {
                c.d = c.d * l / old_l;
                c.p = c.p * pl / old_pl.max(1.0);
                c.q = c.q * self.sp / old_sp.max(1.0);
            }
        }
        self.rev += 1;
    }

    /// Trees off the circuit, the grounds and the lake; floodlights round it.
    fn plant(&mut self) {
        let (w, h, k) = (self.w, self.h, self.k);
        let r = &mut Rng::new(hash("pitlane-trees"));
        let gr = self.rect_s(0.02, -0.2, 0.98, 0.45);
        let lake = self.to_s(0.79, 0.7);
        let lr = 0.12 * self.lp.min(self.sp);
        self.trees.clear();
        for _ in 0..90 {
            if self.trees.len() >= 46 {
                break;
            }
            let (x, y) = (r.next() * w, r.next() * h);
            let rr = (6.0 + r.next() * 9.0) * k + 3.0;
            let near_track = self
                .trk
                .iter()
                .step_by(4)
                .any(|q| (q[0] - x).hypot(q[1] - y) < 34.0 * k + rr);
            let in_grounds = x > gr[0] - 10.0
                && x < gr[0] + gr[2] + 10.0
                && y > gr[1] - 10.0
                && y < gr[1] + gr[3] + 10.0;
            let in_lake = (x - lake[0]).hypot(y - lake[1]) < lr + rr;
            if !near_track && !in_grounds && !in_lake {
                self.trees.push(Tree { at: [x, y], r: rr });
            }
        }
        let gap = (150.0 / SPACING).round() as usize;
        self.poles = (0..self.trk.len())
            .step_by(gap)
            .map(|i| {
                let (q, ang, _) = at(&self.trk, i as f32 * SPACING, true);
                let o = 30.0 * k;
                [q[0] - ang.sin() * o, q[1] + ang.cos() * o]
            })
            .collect();
    }

    pub fn host_of(&self, c: &Car) -> usize {
        c.who.host.min(self.key.2.len().max(1) - 1)
    }

    /// Where car `i` wants to stop, down the pit lane and out from it: none
    /// while working (it laps), its box when waiting or idle, a parc fermé
    /// spot when done. The prototype's `goal`.
    pub fn goal(&self, i: usize) -> Option<[f32; 2]> {
        let c = &self.cars[i];
        let bay = |q: f32| {
            let g = self.garages.get(self.host_of(c))?;
            let b = g.bays.iter().find(|b| b.id == c.who.id)?;
            Some([b.p, q * self.sp])
        };
        match c.who.mood {
            Mood::Working => None,
            Mood::Waiting => bay(0.035),
            Mood::Idle => bay(0.13),
            Mood::Done => {
                let n = self.cars[..i]
                    .iter()
                    .filter(|o| o.who.mood == Mood::Done)
                    .count();
                let spot = if n < 8 { n } else { 8 + n % 4 };
                self.parc.get(spot).copied()
            }
        }
    }

    /// Where a car is and its heading along the way it is on.
    pub fn place(&self, c: &Car) -> (Pt, f32) {
        match c.mode {
            Mode::Track => {
                let (q, ang, _) = at(&self.trk, c.d, true);
                let o = c.lane * 7.5 * self.k;
                ([q[0] - ang.sin() * o, q[1] + ang.cos() * o], ang)
            }
            Mode::Pit => {
                let (q, ang, _) = at(&self.pit, c.p, false);
                ([q[0] + self.nrm[0] * c.q, q[1] + self.nrm[1] * c.q], ang)
            }
        }
    }

    pub fn waiting_in(&self, host: usize) -> usize {
        self.cars
            .iter()
            .filter(|c| self.host_of(c) == host && c.who.mood == Mood::Waiting)
            .count()
    }

    pub fn selected_car(&self) -> Option<&Car> {
        let id = self.selected.as_ref()?;
        self.cars.iter().find(|c| &c.who.id == id)
    }
}

#[cfg(test)]
mod tests;
