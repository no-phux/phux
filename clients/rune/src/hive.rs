//! The hive: the fleet as beehives. Each machine is a comb frame, each agent
//! a bee in its harness's colours that acts out its state: a working bee
//! forages between the meadow and its cell, one that needs you waggle-dances
//! an amber figure eight on the comb, a finished one caps its cell in wax and
//! an idle one sleeps in the frame's cluster. A port of the playground
//! prototype (`scenes/hive.js`): its constants, rng and draw order. Pure, in
//! world points (the view's size), so the painter, the camera and the tests
//! read one source.

use std::f32::consts::TAU;

use phux_protocol::ids::ResourceId;

use crate::creature::Work;
use crate::reef::{Mood, Pt, Rgb, Rng, Species, hash, lerp};

mod sim;
pub use sim::STEP;

pub const AMBER: Rgb = [255.0, 181.0, 71.0];
pub const MINT: Rgb = [98.0, 230.0, 180.0];
pub const DARK: Rgb = [34.0, 22.0, 12.0];

/// The band clear of the window's chrome: the title bar over it, the
/// readout along its foot. Backgrounds still run full bleed.
pub const SAFE_TOP: f32 = 42.0;
pub const SAFE_BOTTOM: f32 = 56.0;
/// The narrowest frame before frames wrap to another row (wide), and the
/// shortest before they wrap to another column (tall).
const MIN_W: f32 = 200.0;
const MIN_H: f32 = 150.0;

/// What a comb cell holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Honey,
    Brood,
    Empty,
}

#[derive(Debug, Clone)]
pub struct Cell {
    pub at: Pt,
    pub kind: Kind,
    /// How full a honey cell is, 0.45 to 1.
    pub lvl: f32,
    /// A bee's own cell.
    pub home: bool,
}

/// A machine's comb frame: its wooden frame, top bar, comb and cells.
#[derive(Debug, Clone)]
pub struct Frame {
    pub host: String,
    /// Whether this window can list its agents.
    pub known: bool,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// The top bar's height; the comb starts under it.
    pub bar: f32,
    /// The comb inside the wood.
    pub ix: f32,
    pub iy: f32,
    pub iw: f32,
    pub ih: f32,
    pub cells: Vec<Cell>,
    /// Where idle bees cluster.
    pub idle: Pt,
}

#[derive(Debug, Clone)]
pub struct Flower {
    pub bx: f32,
    pub by: f32,
    pub ph: f32,
    pub pr: f32,
    pub n: usize,
    pub c: Rgb,
    /// Where it has swayed to.
    pub x: f32,
    pub y: f32,
}

/// Drifting pollen.
#[derive(Debug, Clone)]
pub struct Mote {
    pub x: f32,
    pub y: f32,
    pub s: f32,
    pub ph: f32,
    pub v: f32,
}

#[derive(Debug, Clone)]
pub struct Firefly {
    pub x: f32,
    pub y: f32,
    pub ph: f32,
    pub sp: f32,
}

/// An agent as the hive needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Forager {
    pub id: ResourceId,
    pub name: String,
    /// Its machine's frame.
    pub host: usize,
    pub mood: Mood,
    pub species: &'static Species,
    /// Its [`crate::creature`] tier, 0 to 4.
    pub tier: usize,
    pub work: Option<Work>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    ToFlower,
    AtFlower,
    ToCell,
    AtCell,
}

/// An agent as a bee: who it is, and how it moves.
#[derive(Debug, Clone)]
pub struct Bee {
    pub who: Forager,
    /// Its cell in its frame, once laid out.
    pub home: usize,
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub ang: f32,
    pub task: Task,
    pub timer: f32,
    /// The flower it is foraging at.
    pub fi: usize,
    pub pollen: bool,
    pub trail: Vec<Pt>,
    /// Its place round the waggle dance.
    pub wp: f32,
    /// Its cell's wax cap and honey, 0 to 1.
    pub cap: f32,
    pub honey: f32,
    pub ph: f32,
    /// The amber ring as it turns to need you, 1 to 0.
    pub burst: f32,
    pub flap: f32,
    pub rest_ang: f32,
}

impl Bee {
    /// Its drawn length at scene scale `s`: each tier a little bigger.
    pub fn len(&self, s: f32) -> f32 {
        22.0 * s * (1.0 + 0.14 * self.who.tier as f32)
    }
}

/// What a bee's tier adds: antenna tips (2), big eyes (3), a queen's
/// longer abdomen and crown (4).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Flourish {
    pub tips: bool,
    pub eye: f32,
    pub abdomen: f32,
    pub crown: bool,
}

pub fn flourish(tier: usize) -> Flourish {
    Flourish {
        tips: tier >= 2,
        eye: if tier >= 3 { 0.08 } else { 0.055 },
        abdomen: if tier >= 4 { 1.25 } else { 1.0 },
        crown: tier >= 4,
    }
}

/// The rank a card names, by tier.
pub const RANKS: [&str; 5] = ["Rookie", "Regular", "Veteran", "Star", "Legend"];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cam {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

/// The whole hive: layout, bees and camera.
#[derive(Debug, Clone)]
pub struct Hive {
    pub w: f32,
    pub h: f32,
    /// Scene scale.
    pub s: f32,
    pub portrait: bool,
    /// The meadow's width, left of the hive box.
    pub mw: f32,
    pub top: f32,
    pub bot: f32,
    /// A cell's radius.
    pub r: f32,
    pub frames: Vec<Frame>,
    pub flowers: Vec<Flower>,
    pub bees: Vec<Bee>,
    pub motes: Vec<Mote>,
    pub flies: Vec<Firefly>,
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
    /// Bumped on every new layout: the backdrop is painted once per layout.
    pub generation: u64,
    pub(crate) carry: f32,
    pub(crate) rng: Rng,
    hosts: Vec<(String, bool)>,
}

impl Default for Hive {
    fn default() -> Self {
        Hive {
            w: 0.0,
            h: 0.0,
            s: 1.0,
            portrait: false,
            mw: 0.0,
            top: 0.0,
            bot: 0.0,
            r: 7.0,
            frames: Vec::new(),
            flowers: Vec::new(),
            bees: Vec::new(),
            motes: Vec::new(),
            flies: Vec::new(),
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
            generation: 0,
            carry: 0.0,
            rng: Rng::new(0xB33),
            hosts: Vec::new(),
        }
    }
}

/// Frame rectangles `[x, y, w, h]` for `n` machines between `x0..x1` and
/// `top..bot`: side by side while wide (wrapping to rows when they'd get too
/// narrow), stacked while tall (wrapping to columns), in order. Wide, a
/// frame is at most `max_w` across, the frames centred: one machine is not
/// one giant comb.
pub fn frame_rects(
    n: usize,
    [x0, x1]: [f32; 2],
    [top, bot]: [f32; 2],
    portrait: bool,
    max_w: f32,
) -> Vec<[f32; 4]> {
    if n == 0 {
        return Vec::new();
    }
    let (gap, cols, rows) = if portrait {
        let gap = 14.0;
        let rows = (((bot - top + gap) / (MIN_H + gap)) as usize).clamp(1, n);
        (gap, n.div_ceil(rows), rows)
    } else {
        let gap = 22.0;
        let cols = (((x1 - x0 + gap) / (MIN_W + gap)) as usize).clamp(1, n);
        (gap, cols, n.div_ceil(cols))
    };
    let mut fw = (x1 - x0 - gap * (cols - 1) as f32) / cols as f32;
    if !portrait {
        fw = fw.min(max_w);
    }
    let x0 = x0 + (x1 - x0 - fw * cols as f32 - gap * (cols - 1) as f32) / 2.0;
    let fh = (bot - top - gap * (rows - 1) as f32) / rows as f32;
    (0..n)
        .map(|i| {
            let (c, r) = ((i % cols) as f32, (i / cols) as f32);
            [x0 + c * (fw + gap), top + r * (fh + gap), fw, fh]
        })
        .collect()
}

/// The comb's cells in a frame's inside, typed by `r`: honey along the top,
/// brood in the middle, the rest empty. Prototype `build`.
fn comb(ix: f32, iy: f32, iw: f32, ih: f32, rad: f32, r: &mut Rng) -> Vec<Cell> {
    let (dx, dy) = (3f32.sqrt() * rad, 1.5 * rad);
    let mut cells = Vec::new();
    for j in 0.. {
        let cy = iy + rad + j as f32 * dy;
        if cy + rad > iy + ih {
            break;
        }
        for k in 0.. {
            let cx = ix + dx * (k as f32 + (j % 2) as f32 * 0.5) + dx / 2.0;
            if cx + dx / 2.0 > ix + iw {
                break;
            }
            let (v, q) = ((cy - iy) / ih, r.next());
            let kind = if (v < 0.32 && q < 0.8) || q < 0.12 {
                Kind::Honey
            } else if v > 0.36 && v < 0.82 && q < 0.55 {
                Kind::Brood
            } else {
                Kind::Empty
            };
            let lvl = 0.45 + r.next() * 0.55;
            cells.push(Cell {
                at: [cx, cy],
                kind,
                lvl,
                home: false,
            });
        }
    }
    cells
}

/// A home cell for a bee in `f`: in the comb's middle band, clear of the
/// idle cluster, apart from other homes when it can be. Prototype `build`.
pub fn claim(f: &mut Frame, rad: f32, r: &mut Rng) -> usize {
    let pool: Vec<usize> = (0..f.cells.len())
        .filter(|&i| {
            let c = &f.cells[i];
            let (v, u) = ((c.at[1] - f.iy) / f.ih, (c.at[0] - f.ix) / f.iw);
            v > 0.3 && v < 0.78 && u > 0.12 && u < 0.88
        })
        .collect();
    let dist = |a: Pt, b: Pt| (a[0] - b[0]).hypot(a[1] - b[1]);
    let mut best = None;
    for _ in 0..120 {
        let Some(&i) = pool.get((r.next() * pool.len() as f32) as usize) else {
            continue;
        };
        let c = &f.cells[i];
        if c.home || dist(c.at, f.idle) < rad * 5.0 {
            continue;
        }
        let near = f
            .cells
            .iter()
            .any(|o| o.home && dist(o.at, c.at) < rad * 6.5);
        best = Some(i);
        if !near {
            break;
        }
    }
    let at = best
        .or_else(|| pool.iter().copied().find(|&i| !f.cells[i].home))
        .or_else(|| f.cells.iter().position(|c| !c.home))
        .unwrap_or(0);
    if let Some(c) = f.cells.get_mut(at) {
        c.home = true;
    }
    at
}

/// Pills `(at, chars)` laid out the prototype's way (`pill()`): centred over
/// `at` and kept inside `w`; one that would overlap a pill already placed
/// moves up past it, at most six times. Give them bottom-most first, so
/// stacks grow upward. Each is its left edge, centre y and width.
pub fn place_pills(pills: &[(Pt, usize)], w: f32) -> Vec<[f32; 3]> {
    const H: f32 = 19.0;
    let mut placed: Vec<[f32; 3]> = Vec::new();
    for &(at, chars) in pills {
        // ponytail: an estimated width; measure the text if pills clip.
        let pw = chars as f32 * 6.6 + 14.0;
        let x = (at[0] - pw / 2.0).clamp(6.0, (w - pw - 6.0).max(6.0));
        let mut y = at[1];
        for _ in 0..6 {
            let hit = placed.iter().any(|p| {
                (p[1] - y).abs() < H + 2.0 && x < p[0] + p[2] + 4.0 && p[0] < x + pw + 4.0
            });
            if !hit {
                break;
            }
            y -= H + 3.0;
        }
        placed.push([x, y, pw]);
    }
    placed
}

impl Hive {
    pub fn frame_of(&self, b: &Bee) -> &Frame {
        &self.frames[b.who.host.min(self.frames.len().saturating_sub(1))]
    }

    pub fn home_of(&self, b: &Bee) -> Pt {
        let f = self.frame_of(b);
        f.cells.get(b.home).map_or(f.idle, |c| c.at)
    }

    pub fn waiting_in(&self, frame: usize) -> usize {
        self.bees
            .iter()
            .filter(|b| b.who.host == frame && b.who.mood == Mood::Waiting)
            .count()
    }

    /// Lay the scene out at `w` by `h` for `hosts` (name, agents known), the
    /// prototype's `build`: the meadow on the left, a frame per machine.
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
        self.generation += 1;
        let portrait = h > w;
        let s = (w.min(h) / 720.0).clamp(0.55, 1.6);
        self.s = s;
        self.portrait = portrait;
        let mw = (w * if portrait { 0.2 } else { 0.22 }).round();
        let (top, bot) = (SAFE_TOP + 6.0, h - SAFE_BOTTOM - 4.0);
        (self.mw, self.top, self.bot) = (mw, top, bot);
        let xs = if portrait {
            [mw + 10.0, w - 12.0]
        } else {
            [mw + 28.0, w - 24.0]
        };
        let rad = (10.5 * s).max(7.0);
        self.r = rad;
        let bw = (6.0 + 7.0 * s).round();
        let bar = (bw + 10.0 * s + 6.0).round();
        self.frames = frame_rects(hosts.len(), xs, [top, bot], portrait, 420.0 * s)
            .into_iter()
            .zip(hosts)
            .map(|([x, y, fw, fh], (host, known))| {
                let r = &mut Rng::new(hash(host));
                let (ix, iy, iw, ih) = (x + bw, y + bar, fw - bw * 2.0, fh - bar - bw);
                Frame {
                    host: host.clone(),
                    known: *known,
                    x,
                    y,
                    w: fw,
                    h: fh,
                    bar,
                    ix,
                    iy,
                    iw,
                    ih,
                    cells: comb(ix, iy, iw, ih, rad, r),
                    idle: [ix + iw * 0.22, iy + ih - rad * 1.6],
                }
            })
            .collect();
        let r0 = &mut Rng::new(hash("hive-meadow"));
        let n = ((bot - top) / if portrait { 58.0 } else { 64.0 })
            .floor()
            .max(4.0) as usize;
        const PETALS: [Rgb; 5] = [
            [255.0, 120.0, 160.0],
            [255.0, 248.0, 236.0],
            [196.0, 150.0, 255.0],
            [255.0, 206.0, 74.0],
            [255.0, 150.0, 96.0],
        ];
        self.flowers = (0..n)
            .map(|i| {
                let y = lerp(top + 26.0, bot - 16.0, (i as f32 + 0.5) / n as f32)
                    + (r0.next() - 0.5) * 18.0;
                let bx = mw * (0.2 + r0.next() * 0.6);
                let ph = r0.next() * TAU;
                let pr = if portrait { 7.0 } else { 10.0 } * s.max(0.8) * (0.8 + r0.next() * 0.4);
                let petals = 5 + (r0.next() * 4.0) as usize;
                let c = PETALS[((r0.next() * 5.0) as usize).min(4)];
                Flower {
                    bx,
                    by: y,
                    ph,
                    pr,
                    n: petals,
                    c,
                    x: bx,
                    y,
                }
            })
            .collect();
        self.flowers.sort_by(|a, b| a.by.total_cmp(&b.by));
        let r = &mut Rng::new(11);
        self.motes = (0..46)
            .map(|_| Mote {
                x: r.next() * w,
                y: r.next() * h,
                s: 0.6 + r.next() * 1.6,
                ph: r.next() * TAU,
                v: 4.0 + r.next() * 8.0,
            })
            .collect();
        self.flies = (0..18)
            .map(|_| Firefly {
                x: r.next() * mw * 1.3,
                y: top + r.next() * (bot - top),
                ph: r.next() * TAU,
                sp: 0.4 + r.next() * 0.6,
            })
            .collect();
        for i in 0..self.bees.len() {
            self.home(i);
        }
        if old_w > 0.0 {
            let (kx, ky) = (w / old_w, h / old_h);
            for b in &mut self.bees {
                b.x *= kx;
                b.y *= ky;
                b.trail.clear();
            }
        } else {
            self.cam = Cam {
                x: w / 2.0,
                y: h / 2.0,
                z: 1.0,
            };
            self.nt = self.nt_target;
            for i in 0..self.bees.len() {
                self.place(i);
            }
            // The scene opens already alive.
            for _ in 0..150 {
                self.step(STEP);
            }
        }
    }

    /// Give bee `i` a home cell in its frame, seeded from its frame and name.
    pub(crate) fn home(&mut self, i: usize) {
        let (host, name) = (self.bees[i].who.host, self.bees[i].who.name.clone());
        let rad = self.r;
        let Some(f) = self.frames.get_mut(host) else {
            return;
        };
        let r = &mut Rng::new(hash(&f.host) ^ hash(&name));
        self.bees[i].home = claim(f, rad, r);
    }

    /// Put bee `i` where its mood keeps it: a working one somewhere between
    /// its cell and the meadow, the rest at their cells.
    pub(crate) fn place(&mut self, i: usize) {
        if self.frames.is_empty() {
            return;
        }
        let c = self.home_of(&self.bees[i]);
        let (a, b) = (self.rng.next(), self.rng.next());
        let fl = self.flowers.get(self.bees[i].fi).map_or(c[1], |f| f.by);
        let bee = &mut self.bees[i];
        bee.x = c[0];
        bee.y = c[1];
        if bee.who.mood == Mood::Working {
            bee.x = lerp(c[0], self.mw * 0.6, a);
            bee.y = lerp(c[1], fl, b);
        }
    }
}

#[cfg(test)]
mod tests;
