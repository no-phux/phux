//! Box drawing (U+2500–U+257F) drawn from the cell's edges, not a face's
//! ink. A face's lines stop at its own line height, so with taller cells
//! (`adjust-cell-height`) the borders of a TUI break between rows; drawn
//! here, every arm reaches its cell edge on whole device pixels.
//!
//! Each character is four arms (up, right, down, left), each absent, light,
//! heavy or double. A double arm is a triple-width bar with its middle third
//! cut away, which joins doubles to doubles and singles without a table of
//! special cases.

use gpui::{Bounds, Pixels, point, px, size};

/// A quarter-circle line: the two borders of `bounds` that meet at
/// `corner`, rounded there with `radius`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Bend {
    /// 0 top-left, 1 top-right, 2 bottom-right, 3 bottom-left.
    pub(super) corner: u8,
    pub(super) thickness: Pixels,
    pub(super) radius: Pixels,
}

/// One quad of a drawn character: where, whether it is a rounded corner, and
/// its coverage (antialiased edges and shades are partial).
pub(super) type Piece = (Bounds<Pixels>, Option<Bend>, f32);

/// The quads that draw `ch` in `cell`, or `None` when the face draws it.
pub(super) fn box_quads(ch: char, cell: Bounds<Pixels>, scale: f32) -> Option<Vec<Piece>> {
    let shape = shape(ch)?;
    let device = |logical: Pixels| (f32::from(logical) * scale).round() as i32;
    let grid = Grid {
        left: device(cell.left()),
        top: device(cell.top()),
        right: device(cell.right()),
        bottom: device(cell.bottom()),
    };
    // Paper Mono's light rule is 0.086 em, 0.14 of its 0.6 em advance.
    let light = ((grid.right - grid.left) as f32 * 0.14).round().max(1.) as i32;
    let rects = match shape {
        Shape::Arms(arms) => arms_rects(&grid, light, arms),
        Shape::Dashes {
            vertical,
            weight,
            count,
        } => dash_rects(&grid, light, vertical, weight, count),
        Shape::Bend(corner) => {
            let (quad, bend) = bend(&grid, light, corner);
            return Some(vec![(
                logical(quad, scale),
                Some(Bend {
                    corner,
                    thickness: px(bend.0 as f32 / scale),
                    radius: px(bend.1 as f32 / scale),
                }),
                1.,
            )]);
        }
        Shape::Shade(alpha) => {
            let whole = (grid.left, grid.top, grid.right, grid.bottom);
            return Some(vec![(logical(whole, scale), None, alpha)]);
        }
        Shape::Sextant(bits) => sextant_rects(&grid, bits),
        Shape::Braille(bits) => braille_rects(&grid, bits),
        Shape::Outline(outline) => {
            return Some(
                raster(&grid, light, outline)
                    .into_iter()
                    .map(|(rect, alpha)| (logical(rect, scale), None, alpha))
                    .collect(),
            );
        }
    };
    Some(
        rects
            .into_iter()
            .filter(|rect| rect.2 > rect.0 && rect.3 > rect.1)
            .map(|rect| (logical(rect, scale), None, 1.))
            .collect(),
    )
}

/// A cell's edges in device pixels.
struct Grid {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// (left, top, right, bottom) in device pixels.
type Rect = (i32, i32, i32, i32);

fn logical((left, top, right, bottom): Rect, scale: f32) -> Bounds<Pixels> {
    Bounds::new(
        point(px(left as f32 / scale), px(top as f32 / scale)),
        size(
            px((right - left) as f32 / scale),
            px((bottom - top) as f32 / scale),
        ),
    )
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outline {
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

fn arms_rects(grid: &Grid, light: i32, [up, right, down, left]: [u8; 4]) -> Vec<Rect> {
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

fn dash_rects(grid: &Grid, light: i32, vertical: bool, weight: u8, count: u8) -> Vec<Rect> {
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
fn bend(grid: &Grid, light: i32, corner: u8) -> (Rect, (i32, i32)) {
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
fn sextant_rects(grid: &Grid, bits: u8) -> Vec<Rect> {
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
fn braille_rects(grid: &Grid, bits: u8) -> Vec<Rect> {
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

/// `outline` filled one device row at a time: whole pixels inside each span,
/// and the span's fractional ends as partially covered pixels.
fn raster(grid: &Grid, light: i32, outline: Outline) -> Vec<(Rect, f32)> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Grid {
        Grid {
            left: 0,
            top: 0,
            right: 20,
            bottom: 44,
        }
    }

    fn covered(rects: &[Rect], x: i32, y: i32) -> bool {
        rects
            .iter()
            .any(|(l, t, r, b)| x >= *l && x < *r && y >= *t && y < *b)
    }

    #[test]
    fn the_table_covers_every_line_character() {
        for code in 0x2500..=0x257f {
            let ch = char::from_u32(code).unwrap();
            assert!(shape(ch).is_some(), "U+{code:04X}");
        }
    }

    #[test]
    fn arms_reach_the_cell_edges_and_meet() {
        // └: the stem leaves the top edge and the arm reaches the right edge.
        let rects = arms_rects(&grid(), 2, [1, 1, 0, 0]);
        assert!(covered(&rects, 10, 0), "stem touches the top");
        assert!(covered(&rects, 19, 22), "arm touches the right");
        assert!(covered(&rects, 9, 22), "the corner is filled");
        assert!(!covered(&rects, 10, 43), "nothing below the corner");
    }

    #[test]
    fn doubles_join_as_two_lines() {
        // ╔: outer and inner L, with the gap between them open at the corner.
        let rects = arms_rects(&grid(), 2, [0, 3, 3, 0]);
        let (x0, _) = band(0, 20, 2, 3);
        let (y0, _) = band(0, 44, 2, 3);
        assert!(covered(&rects, 19, y0), "outer line reaches the right");
        assert!(!covered(&rects, 19, y0 + 2), "gap between the lines");
        assert!(covered(&rects, 19, y0 + 4), "inner line reaches the right");
        assert!(covered(&rects, x0, y0), "outer corner");
        assert!(!covered(&rects, x0 + 2, y0 + 2), "the gaps meet inside");
        assert!(covered(&rects, x0 + 4, y0 + 4), "inner corner");
        // ╞: the single vertical stays whole across the double arm.
        let rects = arms_rects(&grid(), 2, [1, 3, 1, 0]);
        let (v0, _) = band(0, 20, 2, 1);
        for y in 0..44 {
            assert!(covered(&rects, v0, y), "╞ stem row {y}");
        }
    }

    #[test]
    fn sextants_skip_the_half_blocks() {
        assert_eq!(shape('\u{1fb00}'), Some(Shape::Sextant(1)));
        // U+1FB13 would be 20; 21 is ▌, so U+1FB14 is 22.
        assert_eq!(shape('\u{1fb14}'), Some(Shape::Sextant(22)));
        assert_eq!(shape('\u{1fb3b}'), Some(Shape::Sextant(62)));
    }

    #[test]
    fn powerline_arrows_fill_to_both_edges() {
        let pieces = raster(&grid(), 2, Outline::Arrow { right: true });
        let first_row: f32 = pieces
            .iter()
            .filter(|(r, _)| r.1 == 0)
            .map(|(r, a)| (r.2 - r.0) as f32 * a)
            .sum();
        let middle: f32 = pieces
            .iter()
            .filter(|(r, _)| r.1 == 22)
            .map(|(r, a)| (r.2 - r.0) as f32 * a)
            .sum();
        assert!(first_row > 0., "the tip row still has ink");
        assert!(
            middle > 19.,
            "the middle row reaches the right edge: {middle}"
        );
        assert!(
            pieces.iter().any(|(r, _)| r.3 == 44),
            "reaches the bottom edge"
        );
    }

    #[test]
    fn dashes_are_even() {
        let rects = dash_rects(&grid(), 2, false, 1, 4);
        let widths: Vec<_> = rects.iter().map(|r| r.2 - r.0).collect();
        assert_eq!(widths, [4, 4, 4, 4]);
    }
}
