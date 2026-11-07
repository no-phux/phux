//! A reef's coral, seeded from its host's name in the prototype's rng
//! order, and the caustic tile the sand's light is drawn with.

use std::f32::consts::{PI, TAU};

use super::{Pt, Rgb, Rng, hash};

pub const CORALS: [Rgb; 6] = [
    [255.0, 108.0, 150.0],
    [186.0, 118.0, 255.0],
    [255.0, 146.0, 78.0],
    [110.0, 222.0, 206.0],
    [255.0, 202.0, 88.0],
    [255.0, 92.0, 110.0],
];

#[derive(Debug, Clone)]
pub struct Rock {
    pub x: f32,
    pub rx: f32,
    pub ry: f32,
}

#[derive(Debug, Clone)]
pub struct Fan {
    pub x: f32,
    pub h: f32,
    pub col: Rgb,
    pub seed: f32,
}

#[derive(Debug, Clone)]
pub struct Seg {
    pub a: Pt,
    pub b: Pt,
    pub w: f32,
}

#[derive(Debug, Clone)]
pub struct Coral {
    pub x: f32,
    /// Segments by depth from the trunk.
    pub by_depth: Vec<Vec<Seg>>,
    pub tips: Vec<Seg>,
    pub hmax: f32,
    pub col: Rgb,
    pub seed: f32,
}

#[derive(Debug, Clone)]
pub struct Brain {
    pub x: f32,
    pub r: f32,
    pub col: Rgb,
}

#[derive(Debug, Clone)]
pub struct Anemone {
    pub x: f32,
    pub r: f32,
    pub len: f32,
    pub col: Rgb,
    pub seed: f32,
}

/// One machine's reef, its coral seeded from the host's name.
#[derive(Debug, Clone)]
pub struct Zone {
    pub host: String,
    /// Whether this window lists the machine's agents (the connected one).
    pub known: bool,
    pub cx: f32,
    pub l: f32,
    pub r: f32,
    pub w: f32,
    pub home_y: f32,
    pub rocks: Vec<Rock>,
    pub fans: Vec<Fan>,
    pub corals: Vec<Coral>,
    pub brains: Vec<Brain>,
    pub anemones: Vec<Anemone>,
    pub nooks: Vec<Pt>,
    pub vent_x: f32,
    pub vent_next: f32,
    /// Vent bubbles still to come, one every 0.14 s.
    pub vent_left: u8,
    pub vent_gap: f32,
}

/// A segment at `at` heading `ang`, `len` long and `w` wide, at depth `d`,
/// and its branches down to depth `max`.
fn branch(
    segs: &mut Vec<(Seg, usize)>,
    at: Pt,
    [ang, len, w]: [f32; 3],
    d: usize,
    max: usize,
    r: &mut Rng,
) {
    let to = [at[0] + ang.cos() * len, at[1] + ang.sin() * len];
    segs.push((Seg { a: at, b: to, w }, d));
    if d == max {
        return;
    }
    let n = if r.next() < 0.35 { 3 } else { 2 };
    for i in 0..n {
        let spread = (i as f32 - (n - 1) as f32 / 2.0) * (0.42 + r.next() * 0.3);
        let a = ang + spread + (r.next() - 0.5) * 0.25;
        let l = len * (0.66 + r.next() * 0.16);
        branch(segs, to, [a, l, w * 0.72], d + 1, max, r);
    }
}

/// The reef for `host`, the `i`th of zones `zw` wide; the rng is drawn in
/// the prototype's order so a host keeps its coral.
pub fn zone(host: &str, known: bool, i: usize, zw: f32, s: f32, floor_y: f32) -> Zone {
    let mut r = Rng::new(hash(host));
    let cx = zw * (i as f32 + 0.5);
    let spread = (zw * 0.42).min(230.0 * s);
    let at = |r: &mut Rng| cx + (r.next() - 0.5) * spread * 1.7;
    let pick = |r: &mut Rng| CORALS[(r.next() * CORALS.len() as f32) as usize % CORALS.len()];
    let vent_x = at(&mut r);
    let vent_next = r.next() * 3.0;
    let rocks = (0..3)
        .map(|_| Rock {
            x: at(&mut r),
            rx: (26.0 + r.next() * 30.0) * s,
            ry: (14.0 + r.next() * 16.0) * s,
        })
        .collect();
    let fans_n = 1 + usize::from(r.next() < 0.6);
    let fans = (0..fans_n)
        .map(|_| Fan {
            x: at(&mut r),
            h: (70.0 + r.next() * 40.0) * s,
            col: pick(&mut r),
            seed: r.next() * TAU,
        })
        .collect();
    let corals = (0..4)
        .map(|_| {
            let mut segs = Vec::new();
            let max = 4 + usize::from(r.next() < 0.5);
            let ang = -PI / 2.0 + (r.next() - 0.5) * 0.3;
            let len = (20.0 + r.next() * 12.0) * s;
            branch(&mut segs, [0.0, 0.0], [ang, len, 6.0 * s], 0, max, &mut r);
            let hmax = segs
                .iter()
                .map(|(g, _)| -g.a[1].min(g.b[1]))
                .fold(0.0, f32::max);
            let mut by_depth = vec![Vec::new(); max + 1];
            for (g, d) in &segs {
                by_depth[*d].push(g.clone());
            }
            let tips = by_depth[max].clone();
            Coral {
                x: at(&mut r),
                by_depth,
                tips,
                hmax: if hmax > 0.0 { hmax } else { 1.0 },
                col: pick(&mut r),
                seed: r.next() * TAU,
            }
        })
        .collect();
    let brains = (0..2)
        .map(|_| Brain {
            x: at(&mut r),
            r: (16.0 + r.next() * 14.0) * s,
            col: pick(&mut r),
        })
        .collect();
    let anemones_n = 2 + usize::from(r.next() < 0.5);
    let anemones = (0..anemones_n)
        .map(|_| Anemone {
            x: at(&mut r),
            r: (10.0 + r.next() * 6.0) * s,
            len: (20.0 + r.next() * 10.0) * s,
            col: pick(&mut r),
            seed: r.next() * TAU,
        })
        .collect();
    let nooks = (0..6)
        .map(|_| {
            [
                at(&mut r) * 0.92 + cx * 0.08,
                floor_y - (24.0 + r.next() * 34.0) * s,
            ]
        })
        .collect();
    Zone {
        host: host.to_owned(),
        known,
        cx,
        l: zw * i as f32,
        r: zw * (i as f32 + 1.0),
        w: zw,
        home_y: floor_y - 95.0 * s,
        rocks,
        fans,
        corals,
        brains,
        anemones,
        nooks,
        vent_x,
        vent_next,
        vent_left: 0,
        vent_gap: 0.0,
    }
}

/// The caustic tile's side, in texels.
pub const TILE: u32 = 256;

/// A tile of Worley F2-F1 noise as straight RGBA: bright cell edges that
/// read as light on the sand. It wraps, so it tiles.
pub fn caustic_tile() -> Vec<u8> {
    let n = TILE as f32;
    let mut r = Rng::new(7);
    let pts: Vec<Pt> = (0..24).map(|_| [r.next() * n, r.next() * n]).collect();
    let mut out = Vec::with_capacity((TILE * TILE * 4) as usize);
    for y in 0..TILE {
        for x in 0..TILE {
            let (mut f1, mut f2) = (f32::MAX, f32::MAX);
            for p in &pts {
                let dx = (x as f32 - p[0]).abs();
                let dy = (y as f32 - p[1]).abs();
                let (dx, dy) = (dx.min(n - dx), dy.min(n - dy));
                let d = dx * dx + dy * dy;
                if d < f1 {
                    f2 = f1;
                    f1 = d;
                } else if d < f2 {
                    f2 = d;
                }
            }
            let v = (1.0 - (f2.sqrt() - f1.sqrt()) / 9.0)
                .clamp(0.0, 1.0)
                .powf(2.4);
            out.extend_from_slice(&[255, 250, 225, (v * 255.0) as u8]);
        }
    }
    out
}
