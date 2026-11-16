//! Screen-space edge shading shared by playground scenes.

use super::pen::Pen;
use crate::playground::Rgb;

/// A radial vignette of `c` at `a` round `[w / 2, cy]`, clear inside `r0`
/// and darkest at `r1`, as edge bands in screen points.
pub(crate) fn edges(pen: &mut Pen, [w, h]: [f32; 2], cy: f32, [r0, r1]: [f32; 2], c: Rgb, a: f32) {
    pen.screen();
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
            pen.rect(x, y, rw, rh, c, stacked);
        }
    }
}
