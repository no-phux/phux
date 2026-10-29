//! The playground's geometry: a room per host on a campus in tile space,
//! projected isometrically, and every ghost's loop and spot as pure functions
//! of its agent's state, a stable seed and the clock. Pure, so the painter,
//! the click targets and the tests read one geometry.

use std::f32::consts::TAU;

use crate::app::fleet::Agent;
use crate::connection::{AgentInfo, AgentState};

/// One tile is a 2:1 diamond, 64 by 32 points at scale 1.
pub(crate) const TILE_W: f32 = 64.;
pub(crate) const TILE_H: f32 = 32.;
pub(crate) const WALL_H: f32 = 44.;
/// Ghosts per row of a room; a sprite is about two tiles wide.
pub(crate) const COLUMNS: usize = 3;
const CAMPUS_COLUMNS: usize = 2;
const CORRIDOR: f32 = 2.;
const CELL: f32 = 3.;
const PADDING: f32 = 48.;
/// How long a finished agent celebrates after its last change.
const CELEBRATES_MS: u64 = 4000;

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(crate) struct Pt {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(crate) struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn max_x(self) -> f32 {
        self.x + self.w
    }

    pub fn max_y(self) -> f32 {
        self.y + self.h
    }

    pub fn mid(self) -> Pt {
        Pt {
            x: self.x + self.w / 2.,
            y: self.y + self.h / 2.,
        }
    }

    pub fn inset(self, by: f32) -> Rect {
        Rect {
            x: self.x + by,
            y: self.y + by,
            w: self.w - 2. * by,
            h: self.h - 2. * by,
        }
    }

    #[cfg(test)]
    pub fn contains(self, p: Pt) -> bool {
        p.x >= self.x && p.x <= self.max_x() && p.y >= self.y && p.y <= self.max_y()
    }

    #[cfg(test)]
    fn intersects(self, other: Rect) -> bool {
        self.x < other.max_x()
            && other.x < self.max_x()
            && self.y < other.max_y()
            && other.y < self.max_y()
    }
}

/// Tile space to world points: `across` runs down-right, `down` down-left.
pub(crate) fn project(across: f32, down: f32) -> Pt {
    Pt {
        x: (across - down) * TILE_W / 2.,
        y: (across + down) * TILE_H / 2.,
    }
}

/// The four loops the kit ships, in the atlas's order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Strip {
    Idle,
    Celebrate,
    Dash,
    Sleep,
}

impl Strip {
    pub const ALL: [Strip; 4] = [Strip::Idle, Strip::Celebrate, Strip::Dash, Strip::Sleep];

    pub fn index(self) -> usize {
        match self {
            Strip::Idle => 0,
            Strip::Celebrate => 1,
            Strip::Dash => 2,
            Strip::Sleep => 3,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Strip::Idle => "idle",
            Strip::Celebrate => "celebrate",
            Strip::Dash => "dash",
            Strip::Sleep => "sleep",
        }
    }
}

/// The loop an agent plays: working ghosts dash, waiting ghosts bob where
/// they stand, idle ones sleep, a fresh finish celebrates.
pub(crate) fn strip(info: &AgentInfo, now_ms: u64) -> Strip {
    if info.urgent() {
        return Strip::Idle;
    }
    match info.state {
        AgentState::Working => Strip::Dash,
        AgentState::Blocked => Strip::Idle,
        AgentState::Done if now_ms.saturating_sub(info.changed_at_ms) < CELEBRATES_MS => {
            Strip::Celebrate
        }
        AgentState::Done => Strip::Idle,
        AgentState::Idle | AgentState::Unknown => Strip::Sleep,
    }
}

/// Two stable values in 0..1 from an id (FNV-1a with a final mix, so
/// neighbouring ids land apart and a ghost keeps its spot across launches;
/// the same function mobile uses).
pub(crate) fn seed(id: &str) -> (f32, f32) {
    let mut hash: u64 = 0xCBF2_9CE4_8422_2325;
    for byte in id.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    hash ^= hash >> 33;
    let low = (hash & 0xFFFF) as f32 / 65536.;
    let high = ((hash >> 16) & 0xFFFF) as f32 / 65536.;
    (low, high)
}

/// The ghost's spot in tiles: slot `index` of `count` in a `COLUMNS`-wide
/// grid over `field`, nudged by its seed, plus a slow wander inside its own
/// cell when `roaming`. Never outside the cell.
pub(crate) fn spot(
    slot: (usize, usize),
    seed: (f32, f32),
    time: f32,
    roaming: bool,
    field: Rect,
) -> Pt {
    let (index, count) = slot;
    let across = COLUMNS.min(count).max(1);
    let down = count.div_ceil(COLUMNS).max(1);
    let cell_w = field.w / across as f32;
    let cell_h = field.h / down as f32;
    let mut x = 0.5 + (seed.0 - 0.5) * 0.2;
    let mut y = 0.5 + (seed.1 - 0.5) * 0.2;
    if roaming {
        x += (time / 5.3 + seed.0 * TAU).sin() * 0.16;
        y += (time / 3.7 + seed.1 * TAU).cos() * 0.16;
    }
    Pt {
        x: field.x + ((index % COLUMNS) as f32 + x.clamp(0., 1.)) * cell_w,
        y: field.y + ((index / COLUMNS) as f32 + y.clamp(0., 1.)) * cell_h,
    }
}

/// One host's room on the campus, in tile units.
pub(crate) struct Room {
    pub host: Option<String>,
    pub online: bool,
    pub agents: Vec<Agent>,
    /// The walls.
    pub tiles: Rect,
    /// Where ghosts stand: one tile in from the walls.
    pub field: Rect,
    /// The door's span along the back-right wall, in tiles of `across`.
    pub door: (f32, f32),
}

pub(crate) struct Plan {
    pub rooms: Vec<Room>,
    /// The projected extent with the walls and padding, in world points.
    pub bounds: Rect,
}

impl Plan {
    /// Rooms in campus rows, each row as deep as its deepest room. Agents go
    /// to their host's room in catalog order.
    pub fn new(hosts: Vec<(Option<String>, bool)>, agents: &[Agent]) -> Plan {
        let mut rooms = Vec::new();
        let width = CELL * COLUMNS as f32 + 2.;
        let mut row_depth = 0f32;
        let mut down = 0f32;
        for (index, (host, online)) in hosts.into_iter().enumerate() {
            let column = index % CAMPUS_COLUMNS;
            if column == 0 && index > 0 {
                down += row_depth + CORRIDOR;
                row_depth = 0.;
            }
            let agents: Vec<Agent> = agents
                .iter()
                .filter(|agent| agent.destination.host == host)
                .cloned()
                .collect();
            let rows = agents.len().div_ceil(COLUMNS).max(1);
            let depth = CELL * rows as f32 + 2.;
            row_depth = row_depth.max(depth);
            let across = column as f32 * (width + CORRIDOR);
            let tiles = Rect {
                x: across,
                y: down,
                w: width,
                h: depth,
            };
            let mid = tiles.x + tiles.w / 2.;
            rooms.push(Room {
                host,
                online,
                agents,
                tiles,
                field: tiles.inset(1.),
                door: (mid - 1., mid + 1.),
            });
        }
        let bounds = Self::extent(&rooms);
        Plan { rooms, bounds }
    }

    fn extent(rooms: &[Room]) -> Rect {
        if rooms.is_empty() {
            return Rect {
                x: 0.,
                y: 0.,
                w: 320.,
                h: 240.,
            };
        }
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        );
        for room in rooms {
            let t = room.tiles;
            for corner in [
                project(t.x, t.y),
                project(t.max_x(), t.y),
                project(t.max_x(), t.max_y()),
                project(t.x, t.max_y()),
            ] {
                min_x = min_x.min(corner.x);
                max_x = max_x.max(corner.x);
                min_y = min_y.min(corner.y - WALL_H);
                max_y = max_y.max(corner.y);
            }
        }
        Rect {
            x: min_x - PADDING,
            y: min_y - PADDING,
            w: max_x - min_x + 2. * PADDING,
            h: max_y - min_y + 2. * PADDING,
        }
    }

    /// A tile point as a point in the world's own coordinates.
    pub fn place(&self, tile: Pt) -> Pt {
        let p = project(tile.x, tile.y);
        Pt {
            x: p.x - self.bounds.x,
            y: p.y - self.bounds.y,
        }
    }

    /// Where the camera opens: the first room that needs someone, else the
    /// first room, else the stage's centre.
    pub fn focus(&self) -> Pt {
        let room = self
            .rooms
            .iter()
            .find(|room| room.agents.iter().any(|agent| agent.info.needs_you()))
            .or(self.rooms.first());
        match room {
            Some(room) => self.place(room.tiles.mid()),
            None => Pt {
                x: self.bounds.w / 2.,
                y: self.bounds.h / 2.,
            },
        }
    }

    #[cfg(test)]
    fn overlaps(&self) -> bool {
        self.rooms.iter().enumerate().any(|(i, a)| {
            self.rooms[i + 1..]
                .iter()
                .any(|b| a.tiles.intersects(b.tiles))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_keeps_the_isometric_ratio() {
        assert_eq!(project(0., 0.), Pt { x: 0., y: 0. });
        assert_eq!(project(1., 0.), Pt { x: 32., y: 16. });
        assert_eq!(project(1., 1.), Pt { x: 0., y: 32. });
    }

    #[test]
    fn neighbouring_ids_seed_apart_and_stay_put() {
        let a = seed("mini/@9");
        let b = seed("mini/@10");
        assert_eq!(a, seed("mini/@9"));
        assert!((a.0 - b.0).abs() + (a.1 - b.1).abs() > 0.1);
    }

    #[test]
    fn a_roaming_ghost_keeps_its_own_cell() {
        let field = Rect {
            x: 1.,
            y: 1.,
            w: 9.,
            h: 6.,
        };
        for index in 0..5 {
            let s = seed(&format!("h/@{index}"));
            let cell = Rect {
                x: 1. + (index % 3) as f32 * 3.,
                y: 1. + (index / 3) as f32 * 3.,
                w: 3.,
                h: 3.,
            };
            for step in 0..90 {
                let p = spot((index, 5), s, step as f32 * 0.7, true, field);
                assert!(cell.contains(p), "{index} at {step}: {p:?}");
            }
            assert_eq!(
                spot((index, 5), s, 9., false, field),
                spot((index, 5), s, 30., false, field)
            );
        }
    }

    #[test]
    fn rooms_tile_the_campus_without_overlap() {
        let plan = Plan::new(
            vec![
                (None, true),
                (Some("mini".into()), true),
                (Some("nas".into()), false),
            ],
            &[],
        );
        assert_eq!(plan.rooms.len(), 3);
        assert!(!plan.overlaps());
        assert_eq!(
            plan.rooms[2].tiles.x, 0.,
            "the third room starts the second row"
        );
        for room in &plan.rooms {
            assert!(room.tiles.contains(room.field.mid()));
            let top = plan.place(Pt {
                x: room.tiles.x,
                y: room.tiles.y,
            });
            assert!(
                top.y - WALL_H >= 0. && top.x >= 0.,
                "{top:?} inside the stage"
            );
        }
        assert_eq!(plan.focus(), plan.place(plan.rooms[0].tiles.mid()));
    }
}
