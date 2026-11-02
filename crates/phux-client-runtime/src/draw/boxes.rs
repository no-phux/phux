//! Box drawing, shades, sextants, braille and Powerline separators.
//!
//! All of them are drawn from the cell's edges, not a face's ink. A face's
//! lines stop at its own line height, so a tall cell breaks a TUI's borders
//! between rows; drawn here, every arm reaches its cell edge on whole device
//! pixels.
//!
//! Each line character is four arms (up, right, down, left), each absent,
//! light, heavy or double. A double arm is a triple-width bar with its middle
//! third cut away, which joins doubles to doubles and singles without a table
//! of special cases.
//!
//! All of it is integer device pixels, for any toolkit. The GPUI painter
//! keeps its own copy (`phux-gpui`'s `terminal/paint/boxes.rs`) until it
//! adopts this one.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::suboptimal_flops,
    reason = "a cell is a few hundred device pixels, and this keeps the shapes readable"
)]

mod outline;

use outline::{Outline, raster};

/// A cell's edges in device pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    /// Left edge.
    pub left: i32,
    /// Top edge.
    pub top: i32,
    /// Right edge.
    pub right: i32,
    /// Bottom edge.
    pub bottom: i32,
}

/// A quarter-circle line: the two borders of its piece that meet at `corner`,
/// rounded there with `radius`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bend {
    /// 0 top-left, 1 top-right, 2 bottom-right, 3 bottom-left.
    pub corner: u8,
    /// Line width in device pixels.
    pub thickness: i32,
    /// Corner radius in device pixels.
    pub radius: i32,
}

/// One rectangle of a drawn character.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Piece {
    /// Where, in device pixels.
    pub rect: Rect,
    /// A rounded corner to border, rather than a rectangle to fill.
    pub bend: Option<Bend>,
    /// Coverage from 0 to 1: antialiased edges and shades are partial.
    pub coverage: f32,
}

impl Piece {
    const fn solid(rect: Rect) -> Self {
        Self {
            rect,
            bend: None,
            coverage: 1.,
        }
    }

    const fn partial((rect, coverage): (Rect, f32)) -> Self {
        Self {
            rect,
            bend: None,
            coverage,
        }
    }
}

/// Whether [`pieces`] draws `ch`.
#[must_use]
pub fn draws(ch: char) -> bool {
    shape(ch).is_some()
}

/// The rectangles that draw `ch` in `cell`, or `None` when the face draws it.
#[must_use]
pub fn pieces(ch: char, cell: Cell) -> Option<Vec<Piece>> {
    let shape = shape(ch)?;
    let grid = &cell;
    // A light rule of 0.14 of the cell's width, Paper Mono's own.
    let light = ((grid.right - grid.left) as f32 * 0.14).round().max(1.) as i32;
    let rects = match shape {
        Shape::Arms(arms) => arms_rects(grid, light, arms),
        Shape::Dashes {
            vertical,
            weight,
            count,
        } => dash_rects(grid, light, vertical, weight, count),
        Shape::Bend(corner) => {
            let (rect, (thickness, radius)) = bend(grid, light, corner);
            return Some(vec![Piece {
                rect,
                bend: Some(Bend {
                    corner,
                    thickness,
                    radius,
                }),
                coverage: 1.,
            }]);
        }
        Shape::Shade(coverage) => {
            let rect = (grid.left, grid.top, grid.right, grid.bottom);
            return Some(vec![Piece::partial((rect, coverage))]);
        }
        Shape::Sextant(bits) => sextant_rects(grid, bits),
        Shape::Braille(bits) => braille_rects(grid, bits),
        Shape::Outline(outline) => {
            let pieces = raster(grid, light, outline);
            return Some(pieces.into_iter().map(Piece::partial).collect());
        }
    };
    let drawn = rects.into_iter().filter(|r| r.2 > r.0 && r.3 > r.1);
    Some(drawn.map(Piece::solid).collect())
}

/// (left, top, right, bottom) in device pixels.
pub type Rect = (i32, i32, i32, i32);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Shape {
    /// Up, right, down, left: 0 none, 1 light, 2 heavy, 3 double.
    Arms([u8; 4]),
    Dashes {
        vertical: bool,
        weight: u8,
        count: u8,
    },
    /// A rounded corner, by where it bends: 0 `╭`, 1 `╮`, 2 `╯`, 3 `╰`.
    Bend(u8),
    /// `░▒▓`: the whole cell at partial coverage.
    Shade(f32),
    /// Legacy computing sextants: bit 0 top-left … bit 5 bottom-right.
    Sextant(u8),
    /// Braille dots 1–8 as Unicode numbers them.
    Braille(u8),
    /// Diagonals, Powerline separators and corner triangles, filled edge to
    /// edge so a prompt's segments meet at any line height.
    Outline(Outline),
}

/// U+2500–U+257F, four digits per character (up, right, down, left);
/// `-` marks the dashes, bends and diagonals [`shape`] handles itself.
const ARMS: [&str; 128] = [
    "0101", "0202", "1010", "2020", "-", "-", "-", "-", "-", "-", "-", "-", // 2500
    "0110", "0210", "0120", "0220", "0011", "0012", "0021", "0022", // 250C
    "1100", "1200", "2100", "2200", "1001", "1002", "2001", "2002", // 2514
    "1110", "1210", "2110", "1120", "2120", "2210", "1220", "2220", // 251C
    "1011", "1012", "2011", "1021", "2021", "2012", "1022", "2022", // 2524
    "0111", "0112", "0211", "0212", "0121", "0122", "0221", "0222", // 252C
    "1101", "1102", "1201", "1202", "2101", "2102", "2201", "2202", // 2534
    "1111", "1112", "1211", "1212", "2111", "1121", "2121", "2112", // 253C
    "2211", "1122", "1221", "2212", "1222", "2122", "2221", "2222", // 2544
    "-", "-", "-", "-", // 254C
    "0303", "3030", "0310", "0130", "0330", "0013", "0031", "0033", // 2550
    "1300", "3100", "3300", "1003", "3001", "3003", "1310", "3130", // 2558
    "3330", "1013", "3031", "3033", "0313", "0131", "0333", "1303", // 2560
    "3101", "3303", "1313", "3131", "3333", // 2568
    "-", "-", "-", "-", "-", "-", "-", // 256D
    "0001", "1000", "0100", "0010", "0002", "2000", "0200", "0020", // 2574
    "0201", "1020", "0102", "2010", // 257C
];

fn shape(ch: char) -> Option<Shape> {
    let code = u32::from(ch);
    let outline = |outline| Some(Shape::Outline(outline));
    match code {
        0x2591 => return Some(Shape::Shade(0.25)),
        0x2592 => return Some(Shape::Shade(0.5)),
        0x2593 => return Some(Shape::Shade(0.75)),
        // Sextants skip the patterns ▌ (21), ▐ (42), empty and full.
        0x1fb00..=0x1fb3b => {
            let mut bits = (code - 0x1fb00 + 1) as u8;
            if bits >= 21 {
                bits += 1;
            }
            if bits >= 42 {
                bits += 1;
            }
            return Some(Shape::Sextant(bits));
        }
        0x2800..=0x28ff => return Some(Shape::Braille((code - 0x2800) as u8)),
        0xe0b0 => return outline(Outline::Arrow { right: true }),
        0xe0b2 => return outline(Outline::Arrow { right: false }),
        0xe0b1 => return outline(Outline::Chevron { right: true }),
        0xe0b3 => return outline(Outline::Chevron { right: false }),
        0xe0b4 => return outline(Outline::Half { right: true }),
        0xe0b6 => return outline(Outline::Half { right: false }),
        0xe0b5 => return outline(Outline::Ring { right: true }),
        0xe0b7 => return outline(Outline::Ring { right: false }),
        0x25e2..=0x25e5 => return outline(Outline::Corner((code - 0x25e2) as u8)),
        0x2571 => {
            return outline(Outline::Diagonal {
                rising: true,
                falling: false,
            });
        }
        0x2572 => {
            return outline(Outline::Diagonal {
                rising: false,
                falling: true,
            });
        }
        0x2573 => {
            return outline(Outline::Diagonal {
                rising: true,
                falling: true,
            });
        }
        _ => {}
    }
    let index = code.checked_sub(0x2500).filter(|index| *index < 128)? as usize;
    let dashes = |vertical, weight, count| {
        Some(Shape::Dashes {
            vertical,
            weight,
            count,
        })
    };
    match code {
        0x2504 => dashes(false, 1, 3),
        0x2505 => dashes(false, 2, 3),
        0x2506 => dashes(true, 1, 3),
        0x2507 => dashes(true, 2, 3),
        0x2508 => dashes(false, 1, 4),
        0x2509 => dashes(false, 2, 4),
        0x250a => dashes(true, 1, 4),
        0x250b => dashes(true, 2, 4),
        0x254c => dashes(false, 1, 2),
        0x254d => dashes(false, 2, 2),
        0x254e => dashes(true, 1, 2),
        0x254f => dashes(true, 2, 2),
        0x256d..=0x2570 => Some(Shape::Bend((code - 0x256d) as u8)),
        _ => {
            let digits = ARMS[index].as_bytes();
            let mut arms = [0; 4];
            for (arm, digit) in arms.iter_mut().zip(digits) {
                *arm = digit - b'0';
            }
            Some(Shape::Arms(arms))
        }
    }
}

/// The `weight` band across `start..end`, centred: light is `light`, heavy
/// twice that, double three times.
fn band(start: i32, end: i32, light: i32, weight: u8) -> (i32, i32) {
    let thickness = (light * i32::from(weight.clamp(1, 3))).clamp(1, end - start);
    let from = start + (end - start - thickness) / 2;
    (from, from + thickness)
}

fn arms_rects(grid: &Cell, light: i32, [up, right, down, left]: [u8; 4]) -> Vec<Rect> {
    let vband = |weight| band(grid.left, grid.right, light, weight);
    let hband = |weight| band(grid.top, grid.bottom, light, weight);
    let centre_x = grid.left + (grid.right - grid.left) / 2;
    let centre_y = grid.top + (grid.bottom - grid.top) / 2;
    let vertical = up.max(down);
    let horizontal = left.max(right);
    // Horizontal arms reach across the widest vertical bar, and back.
    let (reach_left, reach_right) = if vertical > 0 {
        vband(vertical)
    } else {
        (centre_x, centre_x)
    };
    let (reach_top, reach_bottom) = if horizontal > 0 {
        hband(horizontal)
    } else {
        (centre_y, centre_y)
    };
    let mut rects = Vec::new();
    if up > 0 {
        let (x0, x1) = vband(up);
        rects.push((x0, grid.top, x1, reach_bottom));
    }
    if down > 0 {
        let (x0, x1) = vband(down);
        rects.push((x0, reach_top, x1, grid.bottom));
    }
    if left > 0 {
        let (y0, y1) = hband(left);
        rects.push((grid.left, y0, reach_right, y1));
    }
    if right > 0 {
        let (y0, y1) = hband(right);
        rects.push((reach_left, y0, grid.right, y1));
    }
    // Cut each double arm's middle third, stopping short of a single line
    // that crosses it so that line stays whole.
    let single = |a: u8, b: u8| [a, b].into_iter().filter(|w| *w == 1 || *w == 2).max();
    let mut cuts = Vec::new();
    if up == 3 || down == 3 {
        let (x0, _) = vband(3);
        let gap = (x0 + light, x0 + 2 * light);
        let (stop_top, stop_bottom) = match single(left, right) {
            Some(weight) => hband(weight),
            None if horizontal == 3 => {
                let (y0, _) = hband(3);
                (y0 + 2 * light, y0 + light)
            }
            None => (centre_y, centre_y),
        };
        if up == 3 {
            cuts.push((gap.0, grid.top, gap.1, stop_top));
        }
        if down == 3 {
            cuts.push((gap.0, stop_bottom, gap.1, grid.bottom));
        }
    }
    if left == 3 || right == 3 {
        let (y0, _) = hband(3);
        let gap = (y0 + light, y0 + 2 * light);
        let (stop_left, stop_right) = match single(up, down) {
            Some(weight) => vband(weight),
            None if vertical == 3 => {
                let (x0, _) = vband(3);
                (x0 + 2 * light, x0 + light)
            }
            None => (centre_x, centre_x),
        };
        if left == 3 {
            cuts.push((grid.left, gap.0, stop_left, gap.1));
        }
        if right == 3 {
            cuts.push((stop_right, gap.0, grid.right, gap.1));
        }
    }
    for cut in cuts {
        rects = rects
            .into_iter()
            .flat_map(|rect| subtract(rect, cut))
            .collect();
    }
    rects
}

/// `rect` without `cut`: up to four rectangles.
fn subtract(rect: Rect, cut: Rect) -> Vec<Rect> {
    let (l, t, r, b) = rect;
    let (cl, ct, cr, cb) = (cut.0.max(l), cut.1.max(t), cut.2.min(r), cut.3.min(b));
    if cl >= cr || ct >= cb {
        return vec![rect];
    }
    [
        (l, t, r, ct),
        (l, cb, r, b),
        (l, ct, cl, cb),
        (cr, ct, r, cb),
    ]
    .into_iter()
    .filter(|piece| piece.2 > piece.0 && piece.3 > piece.1)
    .collect()
}

fn dash_rects(grid: &Cell, light: i32, vertical: bool, weight: u8, count: u8) -> Vec<Rect> {
    let count = i32::from(count);
    let (start, end) = if vertical {
        (grid.top, grid.bottom)
    } else {
        (grid.left, grid.right)
    };
    let (across0, across1) = if vertical {
        band(grid.left, grid.right, light, weight)
    } else {
        band(grid.top, grid.bottom, light, weight)
    };
    let length = end - start;
    // Half a gap at each end, so dashes in neighbouring cells are evenly spaced.
    let gap = (length / count / 3).max(1);
    (0..count)
        .map(|index| {
            let from = start + length * index / count + gap / 2;
            let to = start + length * (index + 1) / count - (gap - gap / 2);
            if vertical {
                (across0, from, across1, to)
            } else {
                (from, across0, to, across1)
            }
        })
        .collect()
}

/// The quad a rounded corner borders, and its (thickness, radius).
fn bend(grid: &Cell, light: i32, corner: u8) -> (Rect, (i32, i32)) {
    let (x0, x1) = band(grid.left, grid.right, light, 1);
    let (y0, y1) = band(grid.top, grid.bottom, light, 1);
    let quad = match corner {
        0 => (x0, y0, grid.right, grid.bottom),
        1 => (grid.left, y0, x1, grid.bottom),
        2 => (grid.left, grid.top, x1, y1),
        _ => (x0, grid.top, grid.right, y1),
    };
    let radius = (quad.2 - quad.0).min(quad.3 - quad.1);
    (quad, (light, radius))
}

/// A 2×3 grid split on whole device pixels.
fn sextant_rects(grid: &Cell, bits: u8) -> Vec<Rect> {
    let middle = grid.left + (grid.right - grid.left) / 2;
    let height = grid.bottom - grid.top;
    let row = |index: i32| grid.top + height * index / 3;
    (0..6)
        .filter(|bit| bits & (1 << bit) != 0)
        .map(|bit| {
            let (x0, x1) = if bit % 2 == 0 {
                (grid.left, middle)
            } else {
                (middle, grid.right)
            };
            (x0, row(bit / 2), x1, row(bit / 2 + 1))
        })
        .collect()
}

/// Square dots on an even 2×4 grid. Unicode numbers dots 1–3 down the left,
/// 4–6 down the right, then 7 and 8 along the bottom.
fn braille_rects(grid: &Cell, bits: u8) -> Vec<Rect> {
    const PLACES: [(i32, i32); 8] = [
        (0, 0),
        (0, 1),
        (0, 2),
        (1, 0),
        (1, 1),
        (1, 2),
        (0, 3),
        (1, 3),
    ];
    let (width, height) = (grid.right - grid.left, grid.bottom - grid.top);
    let dot = ((width as f32 * 0.22).round() as i32).clamp(1, (height / 4).max(1));
    PLACES
        .iter()
        .enumerate()
        .filter(|(bit, _)| bits & (1 << bit) != 0)
        .map(|(_, &(column, row))| {
            let x = grid.left + width * (2 * column + 1) / 4 - dot / 2;
            let y = grid.top + height * (2 * row + 1) / 8 - dot / 2;
            (x, y, x + dot, y + dot)
        })
        .collect()
}

#[cfg(test)]
mod tests;
