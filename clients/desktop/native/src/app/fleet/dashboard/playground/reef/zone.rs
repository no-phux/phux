//! One machine's reef, its coral grown from the machine's name.

use super::*;

#[derive(Clone)]
pub(crate) struct Rock {
    pub x: f32,
    pub rx: f32,
    pub ry: f32,
}

#[derive(Clone)]
pub(crate) struct Fan {
    pub x: f32,
    pub h: f32,
    pub col: Rgb,
    pub seed: f32,
}

#[derive(Clone, Copy)]
pub(crate) struct Seg {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub w: f32,
}

#[derive(Clone)]
pub(crate) struct Coral {
    pub x: f32,
    /// Segments by depth, trunk first.
    pub by_depth: Vec<Vec<Seg>>,
    pub tips: Vec<Seg>,
    pub hmax: f32,
    pub col: Rgb,
    pub seed: f32,
}

#[derive(Clone)]
pub(crate) struct Brain {
    pub x: f32,
    pub r: f32,
    pub col: Rgb,
}

#[derive(Clone)]
pub(crate) struct Anemone {
    pub x: f32,
    pub r: f32,
    pub n: usize,
    pub len: f32,
    pub col: Rgb,
    pub seed: f32,
}

/// One machine's reef: its slice of the width and the coral grown from its name.
#[derive(Clone)]
pub(crate) struct Zone {
    pub name: String,
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
    pub nooks: Vec<(f32, f32)>,
    pub vent_x: f32,
    pub(super) vent_next: f32,
    pub(super) vent_left: u8,
    pub(super) vent_gap: f32,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn branch(
    segs: &mut Vec<(Seg, usize)>,
    x: f32,
    y: f32,
    ang: f32,
    len: f32,
    w: f32,
    d: usize,
    max_d: usize,
    r: &mut Rng,
) {
    let (x2, y2) = (x + ang.cos() * len, y + ang.sin() * len);
    segs.push((
        Seg {
            x1: x,
            y1: y,
            x2,
            y2,
            w,
        },
        d,
    ));
    if d == max_d {
        return;
    }
    let n = if r.next() < 0.35 { 3 } else { 2 };
    for i in 0..n {
        let spread = (i as f32 - (n - 1) as f32 / 2.) * (0.42 + r.next() * 0.3);
        let jitter = (r.next() - 0.5) * 0.25;
        let next_len = len * (0.66 + r.next() * 0.16);
        branch(
            segs,
            x2,
            y2,
            ang + spread + jitter,
            next_len,
            w * 0.72,
            d + 1,
            max_d,
            r,
        );
    }
}

pub(super) fn zone(host: Option<&str>, i: usize, zw: f32, s: f32, floor_y: f32) -> Zone {
    let name = host.unwrap_or("Local").to_owned();
    let mut r = Rng::new(hash(&name));
    let cx = zw * (i as f32 + 0.5);
    let spread = (zw * 0.42).min(230. * s);
    let at = |r: &mut Rng| cx + (r.next() - 0.5) * spread * 1.7;
    let pick = |r: &mut Rng| CORALS[((r.next() * CORALS.len() as f32) as usize).min(5)];
    let vent_x = at(&mut r);
    let vent_next = r.next() * 3.;
    let rocks = (0..3)
        .map(|_| Rock {
            x: at(&mut r),
            rx: (26. + r.next() * 30.) * s,
            ry: (14. + r.next() * 16.) * s,
        })
        .collect();
    let fans = (0..1 + usize::from(r.next() < 0.6))
        .map(|_| Fan {
            x: at(&mut r),
            h: (70. + r.next() * 40.) * s,
            col: pick(&mut r),
            seed: r.next() * TAU,
        })
        .collect();
    let corals = (0..4)
        .map(|_| {
            let mut segs = Vec::new();
            let max_d = 4 + usize::from(r.next() < 0.5);
            let ang = -PI / 2. + (r.next() - 0.5) * 0.3;
            let len = (20. + r.next() * 12.) * s;
            branch(&mut segs, 0., 0., ang, len, 6. * s, 0, max_d, &mut r);
            let hmax = segs
                .iter()
                .map(|(seg, _)| -seg.y1.min(seg.y2))
                .fold(0., f32::max);
            let mut by_depth = vec![Vec::new(); max_d + 1];
            for (seg, d) in &segs {
                by_depth[*d].push(*seg);
            }
            let tips = segs
                .iter()
                .filter(|(_, d)| *d == max_d)
                .map(|(seg, _)| *seg)
                .collect();
            Coral {
                x: at(&mut r),
                by_depth,
                tips,
                hmax: if hmax > 0. { hmax } else { 1. },
                col: pick(&mut r),
                seed: r.next() * TAU,
            }
        })
        .collect();
    let brains = (0..2)
        .map(|_| Brain {
            x: at(&mut r),
            r: (16. + r.next() * 14.) * s,
            col: pick(&mut r),
        })
        .collect();
    let anemones = (0..2 + usize::from(r.next() < 0.5))
        .map(|_| Anemone {
            x: at(&mut r),
            r: (10. + r.next() * 6.) * s,
            n: 13,
            len: (20. + r.next() * 10.) * s,
            col: pick(&mut r),
            seed: r.next() * TAU,
        })
        .collect();
    let nooks = (0..6)
        .map(|_| {
            let x = at(&mut r) * 0.92 + cx * 0.08;
            (x, floor_y - (24. + r.next() * 34.) * s)
        })
        .collect();
    Zone {
        name,
        cx,
        l: zw * i as f32,
        r: zw * (i as f32 + 1.),
        w: zw,
        home_y: floor_y - 95. * s,
        rocks,
        fans,
        corals,
        brains,
        anemones,
        nooks,
        vent_x,
        vent_next,
        vent_left: 0,
        vent_gap: 0.,
    }
}
