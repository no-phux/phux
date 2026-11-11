//! The galaxy's geometry: the fleet as a core, each machine a planet on one
//! orbit round it, and each agent on a ring round its machine. Pure, in world
//! points, so the view, the camera and the tests read one source. Only a
//! working agent moves round, slowly: one that needs you holds still.

use std::collections::{BTreeMap, BTreeSet};
use std::f32::consts::{PI, TAU};

/// The first ring's radius: an agent's label is about 100 points wide.
pub const SHELL: f32 = 100.0;
pub const RING_GAP: f32 = 95.0;
pub const PER_RING: usize = 6;
/// The core's reach, its rings included.
pub const CORE_R: f32 = 70.0;
const PAD: f32 = 70.0;
/// Half an agent's block, past its ring.
const HALF: f32 = 50.0;
/// Seconds a working agent takes to go once round.
pub const PERIOD: f32 = 90.0;
pub const MIN_SCALE: f32 = 0.3;
pub const MAX_SCALE: f32 = 2.0;

pub type Pt = [f32; 2];

/// One machine's planet and its agents' rings.
#[derive(Debug, Clone, PartialEq)]
pub struct System {
    pub centre: Pt,
    /// The planet's angle about the core, radians.
    pub angle: f32,
    pub count: usize,
}

impl System {
    pub fn rings(&self) -> usize {
        self.count.div_ceil(PER_RING).max(1)
    }

    pub fn ring_radius(ring: usize) -> f32 {
        SHELL + RING_GAP * ring as f32
    }

    /// Agent `index` at `time` seconds; only a `roaming` one moves.
    pub fn point(&self, index: usize, time: f32, roaming: bool) -> Pt {
        let ring = index / PER_RING;
        let on_ring = PER_RING.min(self.count - ring * PER_RING);
        let step = TAU / on_ring as f32;
        // A quarter turn off the planet's own spoke keeps the first agent
        // beside the planet, clear of its name; alternate rings sit half a
        // step round, so agents never line up.
        let mut angle =
            self.angle + PI / 2.0 + step * ((index % PER_RING) as f32 + (ring % 2) as f32 / 2.0);
        if roaming {
            angle += time * TAU / PERIOD;
        }
        let r = Self::ring_radius(ring);
        [
            self.centre[0] + r * angle.cos(),
            self.centre[1] + r * angle.sin(),
        ]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub systems: Vec<System>,
    pub core: Pt,
    /// From the core to every planet.
    pub orbit: f32,
    /// The world is a square this wide, its origin at zero.
    pub side: f32,
}

impl Plan {
    /// A system per machine, with `counts[i]` agents; the first on top, the
    /// rest clockwise.
    pub fn new(counts: &[usize]) -> Plan {
        let reach = counts
            .iter()
            .map(|&n| SHELL + RING_GAP * (n.saturating_sub(1) / PER_RING) as f32 + HALF)
            .fold(0.0, f32::max);
        let n = counts.len();
        // Clear of the core, and neighbours clear of each other round the orbit.
        let apart = if n > 1 {
            (reach + 20.0) / (PI / n as f32).sin()
        } else {
            0.0
        };
        let orbit = (reach + CORE_R + 20.0).max(apart);
        let half = if n == 0 { 160.0 } else { orbit + reach + PAD };
        let core = [half, half];
        let systems = counts
            .iter()
            .enumerate()
            .map(|(i, &count)| {
                let angle = -PI / 2.0 + TAU * i as f32 / n as f32;
                System {
                    centre: [core[0] + orbit * angle.cos(), core[1] + orbit * angle.sin()],
                    angle,
                    count,
                }
            })
            .collect();
        Plan {
            systems,
            core,
            orbit,
            side: 2.0 * half,
        }
    }

    /// The bezel's radius about the core: the outermost agent's reach.
    pub fn bezel(&self) -> f32 {
        self.side / 2.0 - PAD
    }

    /// The scale that shows the whole galaxy in `w` by `h`.
    pub fn fit(&self, w: f32, h: f32) -> f32 {
        if w <= 0.0 || h <= 0.0 {
            return 1.0;
        }
        (w.min(h) / self.side).clamp(MIN_SCALE, 1.0)
    }
}

/// How much an agent says at a camera scale: a phone-sized fit would pile
/// names on names, so words go first, then every calm agent's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// Name and state word.
    Full,
    /// Name only.
    Names,
    /// Only agents that need you are named.
    Urgent,
}

impl Detail {
    pub fn at(scale: f32) -> Detail {
        if scale >= 0.6 {
            Detail::Full
        } else if scale >= 0.45 {
            Detail::Names
        } else {
            Detail::Urgent
        }
    }
}

/// Where the person is looking: world to screen is `offset + world * scale`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub offset: Pt,
    pub scale: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            offset: [0.0, 0.0],
            scale: 1.0,
        }
    }
}

impl Camera {
    pub fn screen(&self, world: Pt) -> Pt {
        [
            self.offset[0] + world[0] * self.scale,
            self.offset[1] + world[1] * self.scale,
        ]
    }

    /// Centre `world` in a `w` by `h` view at `scale`.
    pub fn look(world: Pt, w: f32, h: f32, scale: f32) -> Camera {
        Camera {
            offset: [w / 2.0 - world[0] * scale, h / 2.0 - world[1] * scale],
            scale,
        }
    }

    /// Zoom by `factor` about the screen point `at`, inside the scale range.
    pub fn zoom(&mut self, factor: f32, at: Pt) {
        let next = (self.scale * factor).clamp(MIN_SCALE, MAX_SCALE);
        let ratio = next / self.scale;
        self.offset = [
            at[0] - (at[0] - self.offset[0]) * ratio,
            at[1] - (at[1] - self.offset[1]) * ratio,
        ];
        self.scale = next;
    }
}

/// A working directory's project: its last component. A path this shallow
/// (`/`, `/Users/you`) is a home or a root, which says nothing about the work.
pub fn project(cwd: &str) -> Option<&str> {
    let trimmed = cwd.trim_end_matches('/');
    if trimmed.matches('/').count() < 3 {
        return None;
    }
    trimmed.rsplit('/').next().filter(|last| !last.is_empty())
}

/// What makes two agents related: the same session on the same machine, or
/// the same project anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kin<'a> {
    pub host: &'a str,
    pub session: &'a str,
    pub project: Option<&'a str>,
}

/// The pairs to draw, each once, lower index first. Each group of related
/// agents is joined as a chain in index order rather than every pair to
/// every other.
// ponytail: a chain per group keeps a big session to n-1 lines; a hub or full
// mesh if the shape of a group ever matters.
/// The session phux puts work in when nobody names one.
pub const DEFAULT_SESSION: &str = "default";

pub fn related(agents: &[Kin<'_>]) -> Vec<(usize, usize)> {
    let mut groups: BTreeMap<(u8, &str, &str), Vec<usize>> = BTreeMap::new();
    for (i, kin) in agents.iter().enumerate() {
        // `default` is where everything lands unasked: sharing it says nothing.
        if kin.session != DEFAULT_SESSION {
            groups
                .entry((0, kin.host, kin.session))
                .or_default()
                .push(i);
        }
        if let Some(project) = kin.project {
            groups.entry((1, "", project)).or_default().push(i);
        }
    }
    let pairs: BTreeSet<(usize, usize)> = groups
        .values()
        .flat_map(|members| members.windows(2).map(|w| (w[0], w[1])))
        .collect();
    pairs.into_iter().collect()
}

/// The control point that bows the beam from `a` to `b` away from `core`.
pub fn bow(a: Pt, b: Pt, core: Pt) -> Pt {
    let mid = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
    let away = [mid[0] - core[0], mid[1] - core[1]];
    let len = (away[0] * away[0] + away[1] * away[1]).sqrt().max(1.0);
    let span = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
    let lift = 0.3 * span + 20.0;
    [mid[0] + away[0] / len * lift, mid[1] + away[1] / len * lift]
}

/// The point `t` of the way along the quadratic from `a` through `c` to `b`.
pub fn along(a: Pt, c: Pt, b: Pt, t: f32) -> Pt {
    let u = 1.0 - t;
    [
        u * u * a[0] + 2.0 * u * t * c[0] + t * t * b[0],
        u * u * a[1] + 2.0 * u * t * c[1] + t * t * b[1],
    ]
}

#[cfg(test)]
mod tests;
