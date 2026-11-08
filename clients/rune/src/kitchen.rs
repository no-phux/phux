//! The kitchen: the fleet as a restaurant line. Each machine runs a line
//! across an even share of the width, each agent a chef in its harness's
//! colours acting out its state: a working chef cooks at a station, one that
//! needs you rings the bell at the pass, a finished one plates up under the
//! heat lamps and an idle one takes a break by the crates.
//! A port of the playground prototype (`scenes/kitchen.js`): its constants,
//! rng and draw order. Pure, in view points, so the painter and the tests
//! read one source. Chefs walk the aisles round the island and the pass,
//! where the prototype cut straight through them.

use phux_protocol::ids::ResourceId;

use crate::creature::Work;
use crate::reef::{Mood, Pt, Rng, Species, hash};

mod sim;
pub use sim::{Part, PartKind};

/// The toque's height by rank, prototype `HAT`.
pub const HAT: [f32; 5] = [7.0, 10.0, 13.0, 17.0, 21.0];
/// The rank a chef holds at each creature tier.
pub const RANK: [&str; 5] = [
    "Commis",
    "Cook",
    "Sous chef",
    "Chef de cuisine",
    "Star chef",
];

/// An agent as the kitchen needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Cook {
    pub id: ResourceId,
    pub name: String,
    /// Its machine's line.
    pub host: usize,
    pub mood: Mood,
    pub species: &'static Species,
    pub work: Option<Work>,
}

/// A station on a counter: where its chef stands (`x + sx`, `y`), and its
/// burner or board at (`x`, `iy`).
#[derive(Debug, Clone, PartialEq)]
pub struct Station {
    pub x: f32,
    pub y: f32,
    pub sx: f32,
    /// 0 a pot on a burner, 1 a chopping board, 2 a wok, 3 plating.
    pub kind: u8,
    pub iy: f32,
    /// A chopping board's grain, from the line's rng.
    pub grain: [f32; 5],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    /// Whether the segment `a`–`b` passes through the inside (edges free).
    pub fn cuts(&self, a: Pt, b: Pt) -> bool {
        let (mut t0, mut t1) = (0.0f32, 1.0f32);
        let d = [b[0] - a[0], b[1] - a[1]];
        for (p, q) in [
            (-d[0], a[0] - self.x0),
            (d[0], self.x1 - a[0]),
            (-d[1], a[1] - self.y0),
            (d[1], self.y1 - a[1]),
        ] {
            if p == 0.0 {
                if q <= 0.0 {
                    return false;
                }
            } else {
                let r = q / p;
                if p < 0.0 {
                    t0 = t0.max(r);
                } else {
                    t1 = t1.min(r);
                }
            }
        }
        t1 - t0 > 1e-4
    }
}

/// One machine's line, prototype `build`.
#[derive(Debug, Clone)]
pub struct Line {
    pub host: String,
    /// Whether this window can list its agents.
    pub known: bool,
    pub x0: f32,
    pub x1: f32,
    pub y0: f32,
    pub y1: f32,
    pub w: f32,
    pub h: f32,
    /// The back counter's depth and top.
    pub ch: f32,
    pub cy0: f32,
    pub cy1: f32,
    /// The pass's depth and top; the break corner is the `brk` left of it.
    pub pd: f32,
    pub py0: f32,
    pub brk: f32,
    pub work: Vec<Station>,
    pub island: Option<Rect>,
    pub pass: Vec<Pt>,
    pub idle: Vec<Pt>,
    pub tickets: Vec<bool>,
    pub lamps: Vec<Pt>,
    pub drain: Pt,
    /// What chefs walk round: the island and the pass.
    pub blocks: Vec<Rect>,
}

/// Where a chef's mood sends it in its line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Goal {
    Station(usize),
    Pass(usize),
    Break(usize),
}

impl Line {
    pub fn at(&self, g: Goal) -> Pt {
        match g {
            Goal::Station(k) => [self.work[k].x + self.work[k].sx, self.work[k].y],
            Goal::Pass(k) => self.pass[k],
            Goal::Break(k) => self.idle[k],
        }
    }
}

/// Lay out line `i` of `n` in the band `top`..`bot` of a `w`-wide view.
pub fn line(
    host: &str,
    known: bool,
    [i, n]: [usize; 2],
    [top, bot, w]: [f32; 3],
    portrait: bool,
    cs: f32,
) -> Line {
    let gap = if portrait { 14.0 } else { 22.0 };
    let (fi, fnn) = (i as f32, n as f32);
    let (x0, x1, y0, y1) = if portrait {
        let zh = (bot - top - gap * (fnn - 1.0)) / fnn;
        let y0 = top + fi * (zh + gap);
        (12.0, w - 12.0, y0, y0 + zh)
    } else {
        let m = (w * 0.035).max(24.0);
        let zw = (w - 2.0 * m - gap * (fnn - 1.0)) / fnn;
        let x0 = m + fi * (zw + gap);
        (x0, x0 + zw, top, bot)
    };
    let (lw, h) = (x1 - x0, y1 - y0);
    let r = &mut Rng::new(hash(host));
    let ch = (h * 0.14).clamp(26.0, 44.0);
    let (cy0, pd) = (y0 + 24.0, 28.0 * cs);
    let (cy1, py0, brk) = (cy0 + ch, y1 - pd, lw * 0.3);
    let station = |x: f32, y: f32, kind: u8, iy: f32| Station {
        x,
        y,
        sx: 11.0 * cs,
        kind,
        iy,
        grain: [0.0; 5],
    };
    let n_c = ((lw - 24.0) / (40.0 * cs)).floor().max(4.0) as usize;
    let mut work: Vec<Station> = (0..n_c)
        .map(|k| {
            let x = x0 + 12.0 + (k as f32 + 0.5) * (lw - 24.0) / n_c as f32;
            station(x, cy1 + 30.0 * cs, ((k + i) % 3) as u8, cy0 + ch * 0.48)
        })
        .collect();
    let island = (h > 330.0).then(|| {
        let iy0 = (cy1 + py0) / 2.0 - 22.0 * cs;
        Rect {
            x0: x0 + lw * 0.16,
            x1: x1 - lw * 0.16,
            y0: iy0,
            y1: iy0 + 34.0 * cs,
        }
    });
    if let Some(is) = island {
        let n_i = ((is.x1 - is.x0) / (40.0 * cs)).floor().max(3.0) as usize;
        for k in 0..n_i {
            let x = is.x0 + (k as f32 + 0.5) * (is.x1 - is.x0) / n_i as f32;
            let kind = if k % 2 == 1 { 1 } else { 3 };
            work.push(station(x, is.y1 + 30.0 * cs, kind, (is.y0 + is.y1) / 2.0));
        }
    }
    let n_p = ((lw - brk - 16.0) / (36.0 * cs)).floor().max(4.0) as usize;
    let pass: Vec<Pt> = (0..n_p)
        .map(|k| {
            [
                x0 + brk + 8.0 + (k as f32 + 0.5) * (lw - brk - 16.0) / n_p as f32,
                py0 - 2.0,
            ]
        })
        .collect();
    let idle = (0..6)
        .map(|k| {
            let x = x0 + 18.0 * cs + (k % 3) as f32 * (brk - 26.0 * cs) / 2.4;
            [x, y1 - 6.0 - (k / 3) as f32 * 34.0 * cs]
        })
        .collect();
    let tickets = pass.iter().map(|_| r.next() < 0.45).collect();
    let nl = ((lw / 170.0).round() as usize).max(2);
    let mut lamps: Vec<Pt> = (0..nl)
        .map(|k| [x0 + (k as f32 + 0.5) * lw / nl as f32, cy1 + 22.0 * cs])
        .collect();
    if let Some(is) = island {
        lamps.push([(is.x0 + is.x1) / 2.0, is.y1 + 8.0]);
    }
    // The painter's rng: board grain in station order, then the drain.
    let r = &mut Rng::new(hash(host) ^ 77);
    for s in work.iter_mut().filter(|s| s.kind == 1) {
        s.grain = [r.next(), r.next(), r.next(), r.next(), r.next()];
    }
    let below = island.map_or(8.0, |is| is.y1 - is.y0 + 40.0 * cs);
    let drain = [x0 + lw * (0.35 + r.next() * 0.3), (cy1 + py0) / 2.0 + below];
    // Steel plus a body's half width; the pass from just under where chefs
    // stand at it, the island leaving its stations' spots clear.
    let m = 10.0 * cs;
    let mut blocks = vec![Rect {
        x0: x0 + brk - m,
        y0: py0 + 2.0,
        x1: x1 + m,
        y1: y1 + m,
    }];
    if let Some(is) = island {
        blocks.push(Rect {
            x0: is.x0 - m,
            y0: is.y0 - 6.0 * cs,
            x1: is.x1 + m,
            y1: is.y1 + 7.0 + m,
        });
    }
    Line {
        host: host.to_owned(),
        known,
        x0,
        x1,
        y0,
        y1,
        w: lw,
        h,
        ch,
        cy0,
        cy1,
        pd,
        py0,
        brk,
        work,
        island,
        pass,
        idle,
        tickets,
        lamps,
        drain,
        blocks,
    }
}

/// The goal of each of a line's chefs, given as (mood, seed) in a stable
/// order, prototype `plan`: waiting chefs fill the pass from the left, done
/// ones from the right, working ones take their home station or the next
/// free one, idle ones the break corner.
pub fn plan(l: &Line, chefs: &[(Mood, u32)]) -> Vec<Goal> {
    let (np, nw) = (l.pass.len(), l.work.len());
    let mut seen = [0usize; 4];
    let mut used = vec![false; nw];
    chefs
        .iter()
        .map(|&(mood, seed)| {
            let k = &mut seen[mood as usize];
            *k += 1;
            let k = *k - 1;
            match mood {
                Mood::Waiting => Goal::Pass(k.min(np - 1)),
                Mood::Done => Goal::Pass((np - 1).saturating_sub(k)),
                Mood::Idle => Goal::Break(k % l.idle.len()),
                Mood::Working => {
                    let mut at = seed as usize % nw;
                    let mut tries = 0;
                    while used[at] && tries < nw {
                        at = (at + 1) % nw;
                        tries += 1;
                    }
                    used[at] = true;
                    Goal::Station(at)
                }
            }
        })
        .collect()
}

/// The way from `a` to `b` round `blocks`: the fewest-points path through
/// their corners, as waypoints after `a`, ending at `b`.
pub fn route(a: Pt, b: Pt, blocks: &[Rect]) -> Vec<Pt> {
    let clear = |p: Pt, q: Pt| !blocks.iter().any(|r| r.cuts(p, q));
    if clear(a, b) {
        return vec![b];
    }
    let mut pts = vec![a, b];
    for r in blocks {
        for c in [[r.x0, r.y0], [r.x1, r.y0], [r.x1, r.y1], [r.x0, r.y1]] {
            let c = [
                c[0] + (c[0] - (r.x0 + r.x1) / 2.0).signum(),
                c[1] + (c[1] - (r.y0 + r.y1) / 2.0).signum(),
            ];
            pts.push(c);
        }
    }
    // Dijkstra over a dozen points.
    let n = pts.len();
    let mut dist = vec![f32::MAX; n];
    let mut prev = vec![usize::MAX; n];
    let mut done = vec![false; n];
    dist[0] = 0.0;
    while let Some(u) = (0..n)
        .filter(|&i| !done[i] && dist[i] < f32::MAX)
        .min_by(|&i, &j| dist[i].total_cmp(&dist[j]))
    {
        done[u] = true;
        if u == 1 {
            break;
        }
        for v in 0..n {
            if !done[v] && clear(pts[u], pts[v]) {
                let d = dist[u] + (pts[v][0] - pts[u][0]).hypot(pts[v][1] - pts[u][1]);
                if d < dist[v] {
                    dist[v] = d;
                    prev[v] = u;
                }
            }
        }
    }
    if prev[1] == usize::MAX {
        return vec![b];
    }
    let mut way = vec![b];
    let mut at = prev[1];
    while at != 0 {
        way.push(pts[at]);
        at = prev[at];
    }
    way.reverse();
    way
}

/// An agent as a chef: who it is, where it is and where it's going.
#[derive(Debug, Clone)]
pub struct Chef {
    pub who: Cook,
    /// Its looks and rhythm, from who it is.
    pub seed: u32,
    pub x: f32,
    pub y: f32,
    pub ph: f32,
    /// When its bell started ringing.
    pub ring0: f32,
    /// Walking, 0 to 1, easing.
    pub walk: f32,
    /// Facing away, toward a counter.
    pub up: bool,
    pub goal: Goal,
    /// The waypoints left to its goal.
    pub path: Vec<Pt>,
    pub tier: usize,
}

impl Chef {
    pub fn arrived(&self) -> bool {
        self.path.is_empty()
    }
}

/// The whole kitchen: layout, chefs and what's in the air.
#[derive(Debug, Clone)]
pub struct Kitchen {
    pub w: f32,
    pub h: f32,
    /// Scene scale, prototype `cs`.
    pub cs: f32,
    pub portrait: bool,
    /// The back wall's foot.
    pub wall: f32,
    pub lines: Vec<Line>,
    pub neon: Pt,
    pub chefs: Vec<Chef>,
    pub parts: Vec<Part>,
    /// Scene seconds.
    pub t: f32,
    /// Night, 0 to 1, easing toward `nt_target`.
    pub nt: f32,
    pub nt_target: f32,
    pub selected: Option<ResourceId>,
    pub hovered: Option<ResourceId>,
    /// Reduced motion: time at 0.4.
    pub reduce: bool,
    /// Bumped whenever the layout changes: the painter's cue to repaint the room.
    pub generation: u32,
    pub(crate) carry: f32,
    pub(crate) rng: Rng,
    hosts: Vec<(String, bool)>,
}

impl Default for Kitchen {
    fn default() -> Self {
        Kitchen {
            w: 0.0,
            h: 0.0,
            cs: 1.0,
            portrait: false,
            wall: 0.0,
            lines: Vec::new(),
            neon: [0.0, 0.0],
            chefs: Vec::new(),
            parts: Vec::new(),
            t: 0.0,
            nt: 0.0,
            nt_target: 0.0,
            selected: None,
            hovered: None,
            reduce: false,
            generation: 0,
            carry: 0.0,
            rng: Rng::new(0x0C00C),
            hosts: Vec::new(),
        }
    }
}

/// Rune's chrome over the scene: the title bar and the wall under it at
/// the top, the readout along the foot.
const SAFE_TOP: f32 = 84.0;
const SAFE_BOTTOM: f32 = 52.0;

impl Kitchen {
    /// Lay the scene out at `w` by `h` for `hosts` (name, agents known), the
    /// prototype's `build`: lines split the width (portrait: the height) in order.
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
        self.generation = self.generation.wrapping_add(1);
        let s = (w.min(h) / 720.0).clamp(0.55, 1.6);
        let cs = (s * 1.5).clamp(0.8, 1.35);
        let portrait = h > w;
        self.cs = cs;
        self.portrait = portrait;
        self.wall = SAFE_TOP - 4.0;
        let top = SAFE_TOP + if portrait { 6.0 } else { 26.0 };
        let bot = h - SAFE_BOTTOM - if portrait { 4.0 } else { 26.0 };
        let n = hosts.len();
        self.lines = hosts
            .iter()
            .enumerate()
            .map(|(i, (host, known))| line(host, *known, [i, n], [top, bot, w], portrait, cs))
            .collect();
        self.neon = if portrait {
            [w * 0.43, SAFE_TOP - 16.0]
        } else {
            [w * 0.5, SAFE_TOP - 28.0]
        };
        // As the prototype's setup: everyone at their spot, the air clear.
        self.parts.clear();
        self.replan(true);
    }

    pub fn line_of(&self, c: &Chef) -> &Line {
        &self.lines[c.who.host.min(self.lines.len().saturating_sub(1))]
    }

    /// Give every chef its goal; one whose goal moved walks there round the
    /// counters, or is put there at once when `snap`.
    pub fn replan(&mut self, snap: bool) {
        for (li, l) in self.lines.iter().enumerate() {
            let mine: Vec<usize> = (0..self.chefs.len())
                .filter(|&i| self.chefs[i].who.host.min(self.lines.len() - 1) == li)
                .collect();
            let asks: Vec<(Mood, u32)> = mine
                .iter()
                .map(|&i| (self.chefs[i].who.mood, self.chefs[i].seed))
                .collect();
            for (&i, goal) in mine.iter().zip(plan(l, &asks)) {
                let c = &mut self.chefs[i];
                let to = l.at(goal);
                // A chef new to the kitchen starts at its spot, as the prototype's.
                if snap || c.x.is_nan() {
                    (c.x, c.y) = (to[0], to[1]);
                    c.path.clear();
                } else if goal != c.goal {
                    c.path = route([c.x, c.y], to, &l.blocks);
                }
                c.goal = goal;
            }
        }
    }

    pub fn selected_chef(&self) -> Option<&Chef> {
        let id = self.selected.as_ref()?;
        self.chefs.iter().find(|c| &c.who.id == id)
    }

    /// A chef's drawn scale: the scene's, and a little more by rank.
    pub fn chef_scale(&self, c: &Chef) -> f32 {
        self.cs * (0.9 + c.tier as f32 * 0.045)
    }

    /// The centre and radius a press finds a chef by, prototype `a.x/a.y/a.r`.
    pub fn hit(&self, c: &Chef) -> (Pt, f32) {
        ([c.x, c.y - 20.0 * self.cs], 15.0 * self.cs)
    }

    pub fn waiting_in(&self, line: usize) -> usize {
        self.chefs
            .iter()
            .filter(|c| c.who.host == line && c.who.mood == Mood::Waiting)
            .count()
    }
}

#[cfg(test)]
mod tests;
