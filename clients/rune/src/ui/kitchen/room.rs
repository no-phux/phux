//! The room, painted once per layout as the prototype's `paintBg`: the
//! tiled floor, the back wall, and each line's mats, counters, stations,
//! drain, pass and break corner. Gradients are bands.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use crate::kitchen::{Kitchen, Line, Station};
use crate::reef::{Pt, Rgb, hash, mix};
use crate::ui::reef::pen::Pen;

pub const BLACK: Rgb = [0.0, 0.0, 0.0];
pub const WHITE: Rgb = [255.0, 255.0, 255.0];

/// `#rrggbb` as the prototype writes its colours.
pub const fn hex(v: u32) -> Rgb {
    [
        ((v >> 16) & 255) as f32,
        ((v >> 8) & 255) as f32,
        (v & 255) as f32,
    ]
}

/// A rounded rectangle's outline, the canvas's `roundRect`.
pub fn rr(x: f32, y: f32, w: f32, h: f32, r: f32) -> Vec<Pt> {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let mut pts = Vec::with_capacity(20);
    for (cx, cy, a0) in [
        (x + w - r, y + r, -FRAC_PI_2),
        (x + w - r, y + h - r, 0.0),
        (x + r, y + h - r, FRAC_PI_2),
        (x + r, y + r, PI),
    ] {
        for k in 0..=4 {
            let a = a0 + FRAC_PI_2 * k as f32 / 4.0;
            pts.push([cx + r * a.cos(), cy + r * a.sin()]);
        }
    }
    pts
}

pub fn rrect(pen: &mut Pen, [x, y, w, h]: [f32; 4], r: f32, c: Rgb, a: f32) {
    pen.fill(&rr(x, y, w, h, r), c, a);
}

/// Brushed steel with a shadow and a front face `face` deep.
fn steel(pen: &mut Pen, x: f32, y: f32, w: f32, h: f32, face: f32) {
    rrect(pen, [x + 2.0, y + 4.0, w, h + face], 4.0, BLACK, 0.22);
    rrect(
        pen,
        [x, y + h - 2.0, w, face + 2.0],
        3.0,
        hex(0x7d8790),
        1.0,
    );
    rrect(pen, [x, y, w, h], 4.0, hex(0xa3acb5), 1.0);
    let (c0, c1, c2) = (hex(0xdfe4e8), hex(0xb9c1c8), hex(0xa3acb5));
    pen.vgrad(x + 1.0, y + 1.0, w - 2.0, y + h - 1.0, 6, |k| {
        (
            if k < 0.5 {
                mix(c0, c1, k * 2.0)
            } else {
                mix(c1, c2, k * 2.0 - 1.0)
            },
            1.0,
        )
    });
    pen.line([x + 4.0, y + 1.5], [x + w - 4.0, y + 1.5], 1.0, WHITE, 0.6);
    let mut k = 0.0;
    while k < w {
        pen.line(
            [x + k, y + 3.0],
            [x + k + 2.0, y + h - 3.0],
            1.0,
            WHITE,
            0.08,
        );
        k += 3.0;
    }
}

pub fn room(k: &Kitchen) -> rune::Shape {
    let mut pen = Pen::new();
    let (w, h, cs) = (k.w, k.h, k.cs);
    // The floor: soft check tiles with grout.
    let t = 24.0 * cs;
    let (nx, ny) = ((w / t).ceil() as i32 + 1, (h / t).ceil() as i32 + 1);
    for j in 0..ny {
        for i in 0..nx {
            let v = (hash(&format!("{i}|{j}")) % 9) as f32 - 4.0;
            let c = if (i + j) % 2 == 1 {
                [178.0 + v, 184.0 + v, 190.0 + v]
            } else {
                [228.0 + v, 226.0 + v, 218.0 + v]
            };
            pen.rect(i as f32 * t, j as f32 * t, t + 0.5, t + 0.5, c, 1.0);
        }
    }
    let grout = [80.0, 80.0, 90.0];
    for i in 0..nx {
        let x = i as f32 * t + 0.5;
        pen.line([x, 0.0], [x, h], 1.0, grout, 0.18);
    }
    for j in 0..ny {
        let y = j as f32 * t + 0.5;
        pen.line([0.0, y], [w, y], 1.0, grout, 0.18);
    }
    // The back wall: subway tile under the neon.
    let wh = k.wall;
    pen.rect(0.0, 0.0, w, wh, hex(0x26363a), 1.0);
    let joint = [160.0, 200.0, 200.0];
    let mut row = 0;
    while (row as f32) * 10.0 < wh {
        let y = row as f32 * 10.0;
        pen.line([0.0, y], [w, y], 1.0, joint, 0.12);
        let mut x = (row % 2) as f32 * 11.0;
        while x < w {
            pen.line([x, y], [x, (y + 10.0).min(wh)], 1.0, joint, 0.12);
            x += 22.0;
        }
        row += 1;
    }
    pen.vgrad(0.0, wh - 18.0, w, wh + 6.0, 6, |k| (BLACK, 0.25 * k));
    pen.rect(0.0, wh - 3.0, w, 3.0, hex(0x6d757d), 1.0);
    for l in &k.lines {
        line(&mut pen, l, cs);
    }
    let n = k.neon;
    rrect(
        &mut pen,
        [n[0] - 34.0, n[1] - 13.0, 68.0, 26.0],
        5.0,
        [20.0, 22.0, 28.0],
        0.85,
    );
    pen.into_shape()
}

fn mat(pen: &mut Pen, x0: f32, y0: f32, w: f32, cs: f32) {
    rrect(pen, [x0, y0, w, 30.0 * cs], 5.0, [28.0, 36.0, 34.0], 0.82);
    let mut x = x0 + 6.0;
    while x < x0 + w - 4.0 {
        let mut y = y0 + 5.0;
        while y < y0 - 2.0 + 30.0 * cs {
            pen.rect(x - 1.3, y - 1.3, 2.6, 2.6, WHITE, 0.05);
            y += 8.0;
        }
        x += 8.0;
    }
}

fn line(pen: &mut Pen, l: &Line, cs: f32) {
    // Anti-fatigue mats under the line, the counter and its stations.
    mat(pen, l.x0 + 4.0, l.cy1 + 8.0, l.w - 8.0, cs);
    steel(pen, l.x0, l.cy0, l.w, l.ch, 7.0);
    for s in l.work.iter().filter(|s| s.iy <= l.cy1) {
        station(pen, s, cs);
    }
    if let Some(is) = l.island {
        mat(pen, is.x0 - 6.0, is.y1 + 8.0, is.x1 - is.x0 + 12.0, cs);
        steel(pen, is.x0, is.y0, is.x1 - is.x0, is.y1 - is.y0, 7.0);
        for s in l.work.iter().filter(|s| s.iy > l.cy1) {
            station(pen, s, cs);
        }
    }
    // A floor drain.
    let [dx, dy] = l.drain;
    rrect(
        pen,
        [dx - 8.0, dy - 8.0, 16.0, 16.0],
        3.0,
        hex(0x8e969d),
        1.0,
    );
    for k in 0..4 {
        let x = dx - 5.0 + k as f32 * 3.3;
        pen.line([x, dy - 6.0], [x, dy + 6.0], 1.5, hex(0x5c646b), 1.0);
    }
    // The pass: a steel shelf with its heat lamps' housing.
    let px0 = l.x0 + l.brk;
    let pw = l.x1 - px0;
    steel(pen, px0, l.py0, pw, l.pd - 6.0, 6.0);
    rrect(
        pen,
        [px0 + 4.0, l.py0 + 2.0, pw - 8.0, 4.0],
        2.0,
        hex(0x3b4249),
        1.0,
    );
    // The break corner: the back door, crates and sacks.
    let by = l.y1;
    pen.rect(
        l.x0 - 2.0,
        by - 62.0 * cs,
        8.0,
        62.0 * cs,
        hex(0x5b3d2a),
        1.0,
    );
    pen.rect(
        l.x0 - 1.0,
        by - 60.0 * cs,
        6.0,
        58.0 * cs,
        hex(0x7e5a3e),
        1.0,
    );
    pen.rect(
        l.x0 + 1.0,
        by - 38.0 * cs,
        3.0,
        12.0 * cs,
        hex(0xc9c4b8),
        1.0,
    );
    for p in &l.idle[..3] {
        crate_(pen, p[0], p[1] + 2.0, cs);
    }
    let sx = l.x0 + l.brk;
    pen.disc(
        [sx - 14.0 * cs, l.py0 - 34.0 * cs],
        9.0 * cs,
        hex(0xd8c8a2),
        1.0,
    );
    pen.disc(
        [sx - 24.0 * cs, l.py0 - 30.0 * cs],
        8.0 * cs,
        hex(0xc4b28a),
        1.0,
    );
    pen.line(
        [sx - 18.0 * cs, l.py0 - 40.0 * cs],
        [sx - 10.0 * cs, l.py0 - 38.0 * cs],
        1.0,
        hex(0x9b875f),
        1.0,
    );
}

fn crate_(pen: &mut Pen, x: f32, y: f32, cs: f32) {
    let s = 13.0 * cs;
    pen.rect(x - s + 2.0, y - s * 1.2 + 4.0, s * 2.0, s * 1.4, BLACK, 0.2);
    pen.rect(x - s, y - s * 1.2, s * 2.0, s * 1.2, hex(0x9b7148), 1.0);
    pen.rect(x - s, y - s * 1.6, s * 2.0, s * 0.45, hex(0xb8895a), 1.0);
    for k in 1..3 {
        let yy = y - s * 1.2 + k as f32 * s * 0.4;
        pen.line([x - s, yy], [x + s, yy], 1.0, hex(0x6e4d2f), 1.0);
    }
}

/// A burner or a board, prototype `drawStation`.
fn station(pen: &mut Pen, s: &Station, cs: f32) {
    let (x, y, r) = (s.x, s.iy, 10.0 * cs);
    if s.kind == 0 || s.kind == 2 {
        rrect(
            pen,
            [x - r * 1.35, y - r * 1.25, r * 2.7, r * 2.5],
            3.0,
            hex(0x2d3136),
            1.0,
        );
        let grate = hex(0x555c63);
        pen.ring([x, y], r * 0.95, 1.6, grate, 1.0);
        pen.ring([x, y], r * 0.45, 1.6, grate, 1.0);
        pen.line([x - r, y], [x + r, y], 1.6, grate, 1.0);
        pen.line([x, y - r], [x, y + r], 1.6, grate, 1.0);
        for k in [-1.0, 1.0] {
            pen.disc([x + k * r * 0.8, y + r * 1.05], 1.6, hex(0x1a1d20), 1.0);
        }
    } else {
        let board = [x - r * 1.3, y - r * 0.85, r * 2.6, r * 1.7];
        rrect(
            pen,
            [board[0] + 1.0, board[1] + 2.0, board[2], board[3]],
            3.0,
            BLACK,
            0.18,
        );
        rrect(
            pen,
            board,
            3.0,
            if s.kind == 3 {
                hex(0xf2f1ec)
            } else {
                hex(0xc99a62)
            },
            1.0,
        );
        if s.kind == 1 {
            for (k, g) in s.grain.iter().enumerate() {
                let yy = y + (k as f32 - 2.0) * 3.0;
                pen.line(
                    [x - r * 1.2, yy + g],
                    [x + r * 1.2, yy],
                    0.8,
                    [110.0, 70.0, 30.0],
                    0.35,
                );
            }
        }
    }
}

/// Points round a circle, for a dashed ring.
pub fn arc(c: Pt, r: f32, from: f32, to: f32) -> Vec<Pt> {
    let n = (((to - from).abs() / TAU) * (r * 0.7).max(16.0))
        .ceil()
        .max(2.0) as usize;
    (0..=n)
        .map(|k| {
            let a = from + (to - from) * k as f32 / n as f32;
            [c[0] + r * a.cos(), c[1] + r * a.sin()]
        })
        .collect()
}
