//! What drifts over the reef: bubbles, marine snow (plankton lit by a
//! passing fish at night), and the vignette over it all.

use super::pen::Pen;
use crate::reef::{Reef, Rgb};

const WHITE: Rgb = [255.0, 255.0, 255.0];

pub fn bubbles(pen: &mut Pen, r: &Reef) {
    for b in &r.bubbles {
        pen.disc([b.x, b.y], b.r, WHITE, 0.12);
        pen.ring(
            [b.x, b.y],
            b.r,
            r.s,
            [230.0, 250.0, 255.0],
            0.55 - r.nt * 0.15,
        );
        pen.rect(
            b.x - b.r * 0.45,
            b.y - b.r * 0.5,
            b.r * 0.35,
            b.r * 0.35,
            WHITE,
            0.7,
        );
    }
}

pub fn snow(pen: &mut Pen, r: &Reef) {
    let (s, nt) = (r.s, r.nt);
    for p in &r.snow {
        if p.y < r.wave_y(p.x) {
            continue;
        }
        let (c, a) = if nt > 0.05 {
            if p.g > 0.05 {
                pen.glow(
                    [p.x, p.y],
                    (2.0 + p.g * 7.0) * s,
                    [110.0, 240.0, 255.0],
                    nt * p.g * 0.9,
                );
            }
            (
                [150.0, 240.0, 255.0],
                nt * (0.15 + p.r * 0.1) + (1.0 - nt) * (0.18 + p.r * 0.12),
            )
        } else {
            ([240.0, 250.0, 250.0], 0.18 + p.r * 0.12)
        };
        pen.rect(p.x, p.y, p.r * s, p.r * s, c, a);
    }
}

/// The prototype's radial vignette as a darkening in from each edge, in
/// screen points: the corners take two edges, as a radial one darkens them most.
/// Each edge is `N` bands, the outer as dark as `N` stacked ones and each
/// further in one fewer, cut on whole points so no seam shows between them.
pub fn vignette(pen: &mut Pen, r: &Reef) {
    pen.screen();
    let (w, h) = (r.w, r.h);
    let a = 0.35 + r.nt * 0.25;
    let (r0, r1) = (w.min(h) * 0.35, w.max(h) * 0.8);
    let cy = h * 0.475;
    const N: usize = 8;
    for (d, side) in [(w / 2.0, 0), (w / 2.0, 1), (cy, 2), (h - cy, 3)] {
        let depth = d - r0;
        if depth <= 0.0 {
            continue;
        }
        let edge = a * depth / (r1 - r0) / N as f32;
        for b in 0..N {
            let from = (depth * b as f32 / N as f32).round();
            let to = (depth * (b + 1) as f32 / N as f32).round();
            if to <= from {
                continue;
            }
            let (x, y, rw, rh) = match side {
                0 => (from, 0.0, to - from, h),
                1 => (w - to, 0.0, to - from, h),
                2 => (0.0, from, w, to - from),
                _ => (0.0, h - to, w, to - from),
            };
            let stacked = 1.0 - (1.0 - edge).powi((N - b) as i32);
            pen.rect(x, y, rw, rh, [0.0, 8.0, 16.0], stacked);
        }
    }
}
