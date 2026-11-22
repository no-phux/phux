//! Diagonals and Powerline shapes, filled one device row at a time.

use super::{Cell, Rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Outline {
    /// `╱`, `╲`, `╳`.
    Diagonal { rising: bool, falling: bool },
    /// Powerline solid triangle pointing right (or left).
    Arrow { right: bool },
    /// Powerline thin chevron.
    Chevron { right: bool },
    /// Powerline solid half disc, flat side left (`right`) or right.
    Half { right: bool },
    /// Powerline half-disc outline.
    Ring { right: bool },
    /// `◢◣◤◥`: the half of the cell under a diagonal, by its square corner:
    /// 0 bottom-right, 1 bottom-left, 2 top-left, 3 top-right.
    Corner(u8),
}

/// `outline` filled one device row at a time: whole pixels inside each span,
/// and the span's fractional ends as partially covered pixels.
pub(super) fn raster(grid: &Cell, light: i32, outline: Outline) -> Vec<(Rect, f32)> {
    let (width, height) = (
        (grid.right - grid.left) as f32,
        (grid.bottom - grid.top) as f32,
    );
    let stroke = light as f32;
    // A line's horizontal half-width when it runs `dx` across per `dy` down.
    let half = |dx: f32, dy: f32| stroke / 2. * (1. + (dx / dy).powi(2)).sqrt();
    let mut pieces = Vec::new();
    for row in 0..(grid.bottom - grid.top) {
        let y = row as f32 + 0.5;
        // How far from the middle row, -1 at the top to 1 at the bottom.
        let from_middle = 2. * y / height - 1.;
        let mut spans: Vec<(f32, f32)> = Vec::new();
        let mirror = |right: bool, (a, b): (f32, f32)| {
            if right {
                (a, b)
            } else {
                (width - b, width - a)
            }
        };
        match outline {
            Outline::Diagonal { rising, falling } => {
                let reach = half(width, height);
                if rising {
                    let x = width * (1. - y / height);
                    spans.push((x - reach, x + reach));
                }
                if falling {
                    let x = width * y / height;
                    spans.push((x - reach, x + reach));
                }
            }
            Outline::Arrow { right } => {
                spans.push(mirror(right, (0., width * (1. - from_middle.abs()))));
            }
            Outline::Chevron { right } => {
                let x = width * (1. - from_middle.abs());
                let reach = half(width, height / 2.);
                spans.push(mirror(right, (x - reach, x + reach)));
            }
            Outline::Half { right } => {
                let x = width * (1. - from_middle.powi(2)).max(0.).sqrt();
                spans.push(mirror(right, (0., x)));
            }
            Outline::Ring { right } => {
                let outer = width * (1. - from_middle.powi(2)).max(0.).sqrt();
                // The inner edge is the same curve, `stroke` in from the outer.
                let (inner_width, inner_height) = (width - stroke, height / 2. - stroke);
                let dy = (y - height / 2.).abs();
                let inner = if dy < inner_height {
                    inner_width * (1. - (dy / inner_height).powi(2)).sqrt()
                } else {
                    0.
                };
                spans.push(mirror(right, (inner.max(0.), outer)));
            }
            Outline::Corner(corner) => {
                let x = width * y / height;
                spans.push(match corner {
                    0 => (width - x, width),
                    1 => (0., x),
                    2 => (0., width - x),
                    _ => (x, width),
                });
            }
        }
        let top = grid.top + row;
        for (a, b) in spans {
            let (a, b) = (a.clamp(0., width), b.clamp(0., width));
            if b <= a {
                continue;
            }
            let (full_a, full_b) = (a.ceil(), b.floor());
            let x = |at: f32| grid.left + at as i32;
            if full_b > full_a {
                pieces.push(((x(full_a), top, x(full_b), top + 1), 1.));
            }
            if full_a > a {
                let end = full_a.min(b);
                pieces.push(((x(a.floor()), top, x(a.floor()) + 1, top + 1), end - a));
            }
            if b > full_b && full_b >= full_a {
                pieces.push(((x(full_b), top, x(full_b) + 1, top + 1), b - full_b));
            }
        }
    }
    pieces
}
