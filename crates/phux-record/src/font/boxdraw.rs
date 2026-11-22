//! Procedural glyphs, drawn from cell geometry rather than the bitmap face.
//!
//! Tier one is box drawing, blocks, and shades (`U+2500..=U+259F`): computed
//! lines join their neighbours exactly and cover every heavy/double variant.
//! Heavy is two pixels, double is two one-pixel lines, dashed variants are
//! drawn solid, and arcs are square corners (ADR-0060). Tier two is a small
//! curated set of symbols the face lacks; see [`covers_fallback`] for the
//! lookup order.

/// Whether [`draw`] renders `ch` in preference to the bitmap face (tier one).
pub(crate) const fn covers(ch: char) -> bool {
    matches!(ch, '\u{2500}'..='\u{259f}')
}

/// Paint `ch` into a `cell_w` x `cell_h` cell, calling `put(x, y)` for every
/// foreground pixel.
///
/// Coordinates are cell-local. Returns `false`, painting nothing, for any
/// codepoint neither tier claims.
pub(crate) fn draw(ch: char, cell_w: u32, cell_h: u32, put: &mut impl FnMut(u32, u32)) -> bool {
    let boxed = covers(ch);
    if !boxed && !covers_fallback(ch) {
        return false;
    }
    // A degenerate cell paints nothing but the codepoint is still covered.
    if cell_w == 0 || cell_h == 0 {
        return true;
    }
    let code = ch as u32;
    if !boxed {
        draw_symbol(code, cell_w, cell_h, put);
    } else if code <= 0x257f {
        draw_line(code, cell_w, cell_h, put);
    } else {
        draw_block(code, cell_w, cell_h, put);
    }
    true
}

// ---------------------------------------------------------------------------
// Line drawing: U+2500..=U+257F
// ---------------------------------------------------------------------------

/// Arm weights. `N` none, `L` light (1px), `H` heavy (2px), `D` double (two
/// 1px strokes).
const N: u8 = 0;
const L: u8 = 1;
const H: u8 = 2;
const D: u8 = 3;

/// Pack the four arm weights of one glyph into a byte: up, down, left, right.
const fn arms(up: u8, down: u8, left: u8, right: u8) -> u8 {
    (up << 6) | (down << 4) | (left << 2) | right
}

/// Arm weights for `U+2500 + index`.
///
/// Transcribed from the Unicode names. The diagonals (`U+2571..=U+2573`)
/// carry no arms and are special-cased in [`draw_line`].
#[rustfmt::skip]
const ARMS: [u8; 0x80] = [
    // 2500 ─ 2501 ━ 2502 │ 2503 ┃
    arms(N,N,L,L), arms(N,N,H,H), arms(L,L,N,N), arms(H,H,N,N),
    // 2504..2507 triple-dash, drawn solid
    arms(N,N,L,L), arms(N,N,H,H), arms(L,L,N,N), arms(H,H,N,N),
    // 2508..250B quadruple-dash, drawn solid
    arms(N,N,L,L), arms(N,N,H,H), arms(L,L,N,N), arms(H,H,N,N),
    // 250C ┌ 250D ┍ 250E ┎ 250F ┏
    arms(N,L,N,L), arms(N,L,N,H), arms(N,H,N,L), arms(N,H,N,H),
    // 2510 ┐ 2511 ┑ 2512 ┒ 2513 ┓
    arms(N,L,L,N), arms(N,L,H,N), arms(N,H,L,N), arms(N,H,H,N),
    // 2514 └ 2515 ┕ 2516 ┖ 2517 ┗
    arms(L,N,N,L), arms(L,N,N,H), arms(H,N,N,L), arms(H,N,N,H),
    // 2518 ┘ 2519 ┙ 251A ┚ 251B ┛
    arms(L,N,L,N), arms(L,N,H,N), arms(H,N,L,N), arms(H,N,H,N),
    // 251C ├ 251D ┝ 251E ┞ 251F ┟
    arms(L,L,N,L), arms(L,L,N,H), arms(H,L,N,L), arms(L,H,N,L),
    // 2520 ┠ 2521 ┡ 2522 ┢ 2523 ┣
    arms(H,H,N,L), arms(H,L,N,H), arms(L,H,N,H), arms(H,H,N,H),
    // 2524 ┤ 2525 ┥ 2526 ┦ 2527 ┧
    arms(L,L,L,N), arms(L,L,H,N), arms(H,L,L,N), arms(L,H,L,N),
    // 2528 ┨ 2529 ┩ 252A ┪ 252B ┫
    arms(H,H,L,N), arms(H,L,H,N), arms(L,H,H,N), arms(H,H,H,N),
    // 252C ┬ 252D ┭ 252E ┮ 252F ┯
    arms(N,L,L,L), arms(N,L,H,L), arms(N,L,L,H), arms(N,L,H,H),
    // 2530 ┰ 2531 ┱ 2532 ┲ 2533 ┳
    arms(N,H,L,L), arms(N,H,H,L), arms(N,H,L,H), arms(N,H,H,H),
    // 2534 ┴ 2535 ┵ 2536 ┶ 2537 ┷
    arms(L,N,L,L), arms(L,N,H,L), arms(L,N,L,H), arms(L,N,H,H),
    // 2538 ┸ 2539 ┹ 253A ┺ 253B ┻
    arms(H,N,L,L), arms(H,N,H,L), arms(H,N,L,H), arms(H,N,H,H),
    // 253C ┼ 253D ┽ 253E ┾ 253F ┿
    arms(L,L,L,L), arms(L,L,H,L), arms(L,L,L,H), arms(L,L,H,H),
    // 2540 ╀ 2541 ╁ 2542 ╂ 2543 ╃
    arms(H,L,L,L), arms(L,H,L,L), arms(H,H,L,L), arms(H,L,H,L),
    // 2544 ╄ 2545 ╅ 2546 ╆ 2547 ╇
    arms(H,L,L,H), arms(L,H,H,L), arms(L,H,L,H), arms(H,L,H,H),
    // 2548 ╈ 2549 ╉ 254A ╊ 254B ╋
    arms(L,H,H,H), arms(H,H,H,L), arms(H,H,L,H), arms(H,H,H,H),
    // 254C..254F double-dash, drawn solid
    arms(N,N,L,L), arms(N,N,H,H), arms(L,L,N,N), arms(H,H,N,N),
    // 2550 ═ 2551 ║ 2552 ╒ 2553 ╓
    arms(N,N,D,D), arms(D,D,N,N), arms(N,L,N,D), arms(N,D,N,L),
    // 2554 ╔ 2555 ╕ 2556 ╖ 2557 ╗
    arms(N,D,N,D), arms(N,L,D,N), arms(N,D,L,N), arms(N,D,D,N),
    // 2558 ╘ 2559 ╙ 255A ╚ 255B ╛
    arms(L,N,N,D), arms(D,N,N,L), arms(D,N,N,D), arms(L,N,D,N),
    // 255C ╜ 255D ╝ 255E ╞ 255F ╟
    arms(D,N,L,N), arms(D,N,D,N), arms(L,L,N,D), arms(D,D,N,L),
    // 2560 ╠ 2561 ╡ 2562 ╢ 2563 ╣
    arms(D,D,N,D), arms(L,L,D,N), arms(D,D,L,N), arms(D,D,D,N),
    // 2564 ╤ 2565 ╥ 2566 ╦ 2567 ╧
    arms(N,L,D,D), arms(N,D,L,L), arms(N,D,D,D), arms(L,N,D,D),
    // 2568 ╨ 2569 ╩ 256A ╪ 256B ╫
    arms(D,N,L,L), arms(D,N,D,D), arms(L,L,D,D), arms(D,D,L,L),
    // 256C ╬, then 256D..2570 arcs drawn as square corners
    arms(D,D,D,D), arms(N,L,N,L), arms(N,L,L,N), arms(L,N,L,N),
    // 2570 ╰ 2571 ╱ 2572 ╲ 2573 ╳  (the three diagonals carry no arms)
    arms(L,N,N,L), arms(N,N,N,N), arms(N,N,N,N), arms(N,N,N,N),
    // 2574 ╴ 2575 ╵ 2576 ╶ 2577 ╷
    arms(N,N,L,N), arms(L,N,N,N), arms(N,N,N,L), arms(N,L,N,N),
    // 2578 ╸ 2579 ╹ 257A ╺ 257B ╻
    arms(N,N,H,N), arms(H,N,N,N), arms(N,N,N,H), arms(N,H,N,N),
    // 257C ╼ 257D ╽ 257E ╾ 257F ╿
    arms(N,N,L,H), arms(L,H,N,N), arms(N,N,H,L), arms(H,L,N,N),
];

/// The one or two stroke offsets a given arm weight occupies across `size`
/// pixels.
///
/// `size` is the perpendicular extent. Light lands on the lower/right centre
/// so `─` and `━` align where they meet.
fn strokes(weight: u8, size: u32) -> Pair {
    if size == 0 {
        return [None, None];
    }
    let last = size - 1;
    let lo = last / 2;
    let hi = size / 2;
    match weight {
        L => [Some(hi), None],
        H => {
            // An odd-sized cell collapses lo == hi; keep heavy two pixels.
            let second = if lo == hi { (hi + 1).min(last) } else { hi };
            [Some(lo), Some(second)]
        }
        D => [Some(lo.saturating_sub(1)), Some((hi + 1).min(last))],
        _ => [None, None],
    }
}

fn draw_line(code: u32, cell_w: u32, cell_h: u32, put: &mut impl FnMut(u32, u32)) {
    // 2571 / 2572 / 2573 are pure diagonals with no orthogonal arms.
    if matches!(code, 0x2571 | 0x2573) {
        diagonal(cell_w, cell_h, true, put);
    }
    if matches!(code, 0x2572 | 0x2573) {
        diagonal(cell_w, cell_h, false, put);
    }
    let Some(&packed) = ARMS.get((code - 0x2500) as usize) else {
        return;
    };
    let (up, down) = ((packed >> 6) & 3, (packed >> 4) & 3);
    let (left, right) = ((packed >> 2) & 3, packed & 3);

    let up_cols = strokes(up, cell_w);
    let down_cols = strokes(down, cell_w);
    let left_rows = strokes(left, cell_h);
    let right_rows = strokes(right, cell_h);

    // Arms run to the far perpendicular stroke so junctions close without a
    // notch, or to the centre when there is no perpendicular arm.
    let (vx_lo, vx_hi) = span(&[up_cols, down_cols], cell_w / 2);
    let (hy_lo, hy_hi) = span(&[left_rows, right_rows], cell_h / 2);

    // Double-double corners would fill into a blob under the uniform rule,
    // so pair outer stroke with outer stroke. `matched` says whether stroke 0
    // of the horizontal arm pairs with stroke 0 of the vertical one.
    let is_corner = ((up == D) ^ (down == D)) && ((left == D) ^ (right == D));
    let matched = (down == D && right == D) || (up == D && left == D);
    let corner = is_corner.then(|| {
        let v = if up == D { up_cols } else { down_cols };
        let h = if left == D { left_rows } else { right_rows };
        (v, h, matched)
    });

    for (idx, row) in left_rows.into_iter().enumerate() {
        let Some(row) = row else { continue };
        let end = corner_join(corner, idx, true).unwrap_or(vx_hi);
        hline(0, end, row, cell_w, put);
    }
    for (idx, row) in right_rows.into_iter().enumerate() {
        let Some(row) = row else { continue };
        let start = corner_join(corner, idx, true).unwrap_or(vx_lo);
        hline(start, cell_w - 1, row, cell_w, put);
    }
    for (idx, col) in up_cols.into_iter().enumerate() {
        let Some(col) = col else { continue };
        let end = corner_join(corner, idx, false).unwrap_or(hy_hi);
        vline(0, end, col, cell_h, put);
    }
    for (idx, col) in down_cols.into_iter().enumerate() {
        let Some(col) = col else { continue };
        let start = corner_join(corner, idx, false).unwrap_or(hy_lo);
        vline(start, cell_h - 1, col, cell_h, put);
    }
}

/// Stroke pair type: at most two offsets, ordered low then high.
type Pair = [Option<u32>; 2];

/// The paired junction coordinate for stroke `idx` of a double-double
/// corner; `horizontal` says which stroke family is asking.
fn corner_join(corner: Option<(Pair, Pair, bool)>, idx: usize, horizontal: bool) -> Option<u32> {
    let (v, h, matched) = corner?;
    let pick = if matched { idx } else { 1 - idx };
    let pair = if horizontal { v } else { h };
    pair.get(pick).copied().flatten()
}

/// The `(min, max)` of every present stroke offset, or `(fallback, fallback)`
/// when the arm family is absent entirely.
fn span(pairs: &[Pair], fallback: u32) -> (u32, u32) {
    let mut lo = None;
    let mut hi = None;
    for offset in pairs.iter().flatten().flatten().copied() {
        lo = Some(lo.map_or(offset, |v: u32| v.min(offset)));
        hi = Some(hi.map_or(offset, |v: u32| v.max(offset)));
    }
    (lo.unwrap_or(fallback), hi.unwrap_or(fallback))
}

fn hline(x0: u32, x1: u32, y: u32, cell_w: u32, put: &mut impl FnMut(u32, u32)) {
    for x in x0..=x1.min(cell_w.saturating_sub(1)) {
        put(x, y);
    }
}

fn vline(y0: u32, y1: u32, x: u32, cell_h: u32, put: &mut impl FnMut(u32, u32)) {
    for y in y0..=y1.min(cell_h.saturating_sub(1)) {
        put(x, y);
    }
}

/// One cell-crossing diagonal. `rising` is `U+2571 ╱` (bottom-left to
/// top-right); otherwise `U+2572 ╲`.
fn diagonal(cell_w: u32, cell_h: u32, rising: bool, put: &mut impl FnMut(u32, u32)) {
    // Step along the taller axis so the line has no gaps when the cell is
    // taller than it is wide, which is the usual 8x16 case.
    for y in 0..cell_h {
        let x = y * cell_w / cell_h;
        let x = if rising {
            cell_w - 1 - x.min(cell_w - 1)
        } else {
            x.min(cell_w - 1)
        };
        put(x, y);
    }
}

// ---------------------------------------------------------------------------
// Block elements and shades: U+2580..=U+259F
// ---------------------------------------------------------------------------

fn draw_block(code: u32, cell_w: u32, cell_h: u32, put: &mut impl FnMut(u32, u32)) {
    match code {
        // Upper half.
        0x2580 => rect(0, 0, cell_w, cell_h / 2, put),
        // Lower one-eighth through lower seven-eighths.
        0x2581..=0x2587 => {
            let eighths = code - 0x2580;
            let top = cell_h - cell_h * eighths / 8;
            rect(0, top, cell_w, cell_h, put);
        }
        // Full block: the one glyph `raster` treats as hiding the background.
        0x2588 => rect(0, 0, cell_w, cell_h, put),
        // Left seven-eighths (2589) down to left one-eighth (258F).
        0x2589..=0x258f => {
            let eighths = 0x2590 - code;
            rect(0, 0, cell_w * eighths / 8, cell_h, put);
        }
        // Right half.
        0x2590 => rect(cell_w / 2, 0, cell_w, cell_h, put),
        // Shades: ordered patterns, which compress, never error diffusion.
        0x2591 => shade(cell_w, cell_h, Shade::Light, put),
        0x2592 => shade(cell_w, cell_h, Shade::Medium, put),
        0x2593 => shade(cell_w, cell_h, Shade::Dark, put),
        // Upper one-eighth.
        0x2594 => rect(0, 0, cell_w, cell_h / 8, put),
        // Right one-eighth.
        0x2595 => rect(cell_w - cell_w / 8, 0, cell_w, cell_h, put),
        // Quadrants, as a bitset: 1 upper-left, 2 upper-right, 4 lower-left,
        // 8 lower-right. Transcribed from the Unicode names.
        0x2596..=0x259f => {
            const QUADRANTS: [u8; 10] = [
                0b0100, // 2596 lower left
                0b1000, // 2597 lower right
                0b0001, // 2598 upper left
                0b1101, // 2599 upper left, lower left, lower right
                0b1001, // 259A upper left, lower right
                0b0111, // 259B upper left, upper right, lower left
                0b1011, // 259C upper left, upper right, lower right
                0b0010, // 259D upper right
                0b0110, // 259E upper right, lower left
                0b1110, // 259F upper right, lower left, lower right
            ];
            let Some(&mask) = QUADRANTS.get((code - 0x2596) as usize) else {
                return;
            };
            let (mx, my) = (cell_w / 2, cell_h / 2);
            if mask & 0b0001 != 0 {
                rect(0, 0, mx, my, put);
            }
            if mask & 0b0010 != 0 {
                rect(mx, 0, cell_w, my, put);
            }
            if mask & 0b0100 != 0 {
                rect(0, my, mx, cell_h, put);
            }
            if mask & 0b1000 != 0 {
                rect(mx, my, cell_w, cell_h, put);
            }
        }
        _ => {}
    }
}

#[derive(Clone, Copy)]
enum Shade {
    Light,
    Medium,
    Dark,
}

fn shade(cell_w: u32, cell_h: u32, level: Shade, put: &mut impl FnMut(u32, u32)) {
    for y in 0..cell_h {
        for x in 0..cell_w {
            // 25% on alternating phases, 50% checkerboard, dark = !light.
            let sparse = (x + 2 * (y % 2)) % 4 == 0;
            let lit = match level {
                Shade::Light => sparse,
                Shade::Medium => (x + y) % 2 == 0,
                Shade::Dark => !sparse,
            };
            if lit {
                put(x, y);
            }
        }
    }
}

/// Fill `[x0, x1) x [y0, y1)`.
fn rect(x0: u32, y0: u32, x1: u32, y1: u32, put: &mut impl FnMut(u32, u32)) {
    for y in y0..y1 {
        for x in x0..x1 {
            put(x, y);
        }
    }
}

// ---------------------------------------------------------------------------
// Fallback symbols: high-frequency glyphs the vendored face lacks
// ---------------------------------------------------------------------------

/// Whether [`draw`] renders `ch` only when the bitmap face lacks it (tier
/// two). The lookup order `raster` implements is: [`covers`], then the face,
/// then this set, then tofu. A codepoint with a real bitmap keeps it; the
/// font tests assert this set and the face are disjoint.
#[rustfmt::skip]
pub(crate) const fn covers_fallback(ch: char) -> bool {
    matches!(
        ch,
        // Status marks printed by test runners and linters.
        '\u{2713}'..='\u{2718}'
        // Prompt characters (starship, pure, oh-my-zsh).
        | '\u{276e}' | '\u{276f}' | '\u{2794}' | '\u{279c}'
        // `log-symbols` info and warning.
        | '\u{2139}' | '\u{26a0}'
        // Sideways arrowheads; the face has only up and down.
        | '\u{25b6}' | '\u{25b8}' | '\u{25c0}' | '\u{25c2}'
    )
}

fn draw_symbol(code: u32, cell_w: u32, cell_h: u32, put: &mut impl FnMut(u32, u32)) {
    // Only the all-diagonal symbols are heavy: a one-column thickening reads
    // as weight on a diagonal but merely lengthens a horizontal shaft.
    let pen = Pen {
        cell_w,
        cell_h,
        heavy: matches!(code, 0x2714 | 0x2716 | 0x2718 | 0x276e | 0x276f),
    };
    let band = Frame::band(cell_w, cell_h).thinned(pen.heavy);
    let square = Frame::square(cell_w, cell_h).thinned(pen.heavy);
    match code {
        0x2713 | 0x2714 => check(square, pen, put),
        0x2715..=0x2718 => cross(square, pen, put),
        0x276e => chevron(band, pen, false, put),
        0x276f => chevron(band, pen, true, put),
        0x2794 | 0x279c => arrow(band, pen, put),
        0x2139 => info(band, pen, put),
        0x26a0 => warning(Frame::full(cell_w, cell_h), pen, put),
        0x25b6 => triangle(band, pen, true, put),
        0x25c0 => triangle(band, pen, false, put),
        // The small variants are the same shape in a tighter box.
        0x25b8 => triangle(band.inset(1, 2), pen, true, put),
        0x25c2 => triangle(band.inset(1, 2), pen, false, put),
        _ => {}
    }
}

/// The inclusive box a symbol is inked inside, in cell-local pixels, derived
/// from the cell size (8x16 values given as tuned).
#[derive(Clone, Copy)]
struct Frame {
    left: u32,
    right: u32,
    top: u32,
    bottom: u32,
}

impl Frame {
    /// The band the face's capitals occupy: `x 1..=6`, `y 3..=12`. `draw`
    /// rejects zero-sized cells, so the `- 1` cannot wrap.
    const fn band(cell_w: u32, cell_h: u32) -> Self {
        Self {
            left: cell_w / 8,
            right: cell_w - 1 - cell_w / 8,
            top: cell_h * 3 / 16,
            bottom: cell_h - 1 - cell_h * 3 / 16,
        }
    }

    /// Full width, near square: `x 0..=7`, `y 4..=11`. Checks and crosses
    /// squeezed into the band read as bars.
    const fn square(cell_w: u32, cell_h: u32) -> Self {
        Self {
            left: 0,
            right: cell_w - 1,
            top: cell_h / 4,
            bottom: cell_h - 1 - cell_h / 4,
        }
    }

    /// Nearly the whole cell: `x 0..=7`, `y 2..=13`, for the warning sign.
    const fn full(cell_w: u32, cell_h: u32) -> Self {
        Self {
            left: 0,
            right: cell_w - 1,
            top: cell_h / 8,
            bottom: cell_h - 1 - cell_h / 8,
        }
    }

    /// Give back the column a heavy stroke grows into, so heavy and light
    /// twins occupy the same box.
    fn thinned(self, heavy: bool) -> Self {
        Self {
            right: self
                .right
                .saturating_sub(u32::from(heavy))
                .max(self.mid_x()),
            ..self
        }
    }

    const fn width(self) -> u32 {
        self.right - self.left
    }

    const fn mid_x(self) -> u32 {
        u32::midpoint(self.left, self.right)
    }

    const fn mid_y(self) -> u32 {
        u32::midpoint(self.top, self.bottom)
    }

    /// Half the vertical extent; `mid_y() - half_h()` is exactly `top`.
    const fn half_h(self) -> u32 {
        (self.bottom - self.top) / 2
    }

    /// Shrink by `dx`, `dy` on every side, never collapsing past the centre.
    fn inset(self, dx: u32, dy: u32) -> Self {
        Self {
            left: (self.left + dx).min(self.mid_x()),
            right: self.right.saturating_sub(dx).max(self.mid_x()),
            top: (self.top + dy).min(self.mid_y()),
            bottom: self.bottom.saturating_sub(dy).max(self.mid_y()),
        }
    }
}

/// A clipping stamp that, when `heavy`, doubles every pixel one column right.
/// `dot` is the one place cell bounds are enforced for the symbol tier.
#[derive(Clone, Copy)]
struct Pen {
    cell_w: u32,
    cell_h: u32,
    heavy: bool,
}

impl Pen {
    fn dot(self, x: u32, y: u32, put: &mut impl FnMut(u32, u32)) {
        if y >= self.cell_h {
            return;
        }
        for x in x..=x.saturating_add(u32::from(self.heavy)) {
            if x < self.cell_w {
                put(x, y);
            }
        }
    }

    /// Fill the inclusive rectangle `xs` x `ys`. An inverted range is empty,
    /// which is what lets the callers subtract without guarding.
    fn fill(self, xs: (u32, u32), ys: (u32, u32), put: &mut impl FnMut(u32, u32)) {
        let (x1, y1) = (
            xs.1.min(self.cell_w.saturating_sub(1)),
            ys.1.min(self.cell_h.saturating_sub(1)),
        );
        for y in ys.0..=y1 {
            for x in xs.0..=x1 {
                put(x, y);
            }
        }
    }

    /// Bresenham from `a` to `b`, inclusive; strokes are shallow as often as
    /// steep, so a per-row sweep would leave gaps.
    fn line(self, a: (u32, u32), b: (u32, u32), put: &mut impl FnMut(u32, u32)) {
        let (mut x, mut y) = (i64::from(a.0), i64::from(a.1));
        let (x1, y1) = (i64::from(b.0), i64::from(b.1));
        let (dx, dy) = ((x1 - x).abs(), -(y1 - y).abs());
        let (sx, sy) = (if x < x1 { 1 } else { -1 }, if y < y1 { 1 } else { -1 });
        let mut err = dx + dy;
        loop {
            if let (Ok(px), Ok(py)) = (u32::try_from(x), u32::try_from(y)) {
                self.dot(px, py, put);
            }
            if x == x1 && y == y1 {
                return;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }
}

/// `U+2713` / `U+2714`: a short arm down to a vertex at two fifths of the
/// width, then a long arm up to the right (equal arms read as "v").
fn check(f: Frame, pen: Pen, put: &mut impl FnMut(u32, u32)) {
    let vertex = (f.left + f.width() * 2 / 5, f.bottom);
    pen.line((f.left, f.top + (f.bottom - f.top) * 5 / 8), vertex, put);
    pen.line(vertex, (f.right, f.top), put);
}

/// `U+2715..=U+2718`: two full-box diagonals, differing only in weight.
fn cross(f: Frame, pen: Pen, put: &mut impl FnMut(u32, u32)) {
    pen.line((f.left, f.top), (f.right, f.bottom), put);
    pen.line((f.right, f.top), (f.left, f.bottom), put);
}

/// `U+276E` / `U+276F`: two symmetric arms meeting at the vertical middle.
fn chevron(f: Frame, pen: Pen, pointing_right: bool, put: &mut impl FnMut(u32, u32)) {
    let (apex_x, base_x) = if pointing_right {
        (f.right, f.left)
    } else {
        (f.left, f.right)
    };
    let (mid, half) = (f.mid_y(), f.half_h());
    let apex = (apex_x, mid);
    pen.line((base_x, mid - half), apex, put);
    pen.line(apex, (base_x, mid + half), put);
}

/// `U+2794` / `U+279C`: a shaft on the vertical middle with an open head.
/// Their tip-radius difference is below 8x16 resolution.
fn arrow(f: Frame, pen: Pen, put: &mut impl FnMut(u32, u32)) {
    let mid = f.mid_y();
    let tip = (f.right, mid);
    pen.line((f.left, mid), tip, put);
    // A full-height head swept back over half the width.
    let back = f.right - f.width() / 2;
    let rise = f.half_h();
    pen.line((back, mid - rise), tip, put);
    pen.line((back, mid + rise), tip, put);
}

/// `U+2139`: a dot, a one-row gap, and a stem with a foot.
fn info(f: Frame, pen: Pen, put: &mut impl FnMut(u32, u32)) {
    let h = f.bottom - f.top;
    let (x, x1) = (f.mid_x(), f.mid_x() + 1);
    pen.fill((x, x1), (f.top, f.top + h / 8), put);
    pen.fill((x, x1), (f.top + h / 3, f.bottom), put);
    pen.fill((x.saturating_sub(1), x1 + 1), (f.bottom, f.bottom), put);
}

/// `U+26A0`: a hollow triangle (two-column apex, so it does not lean) with an
/// exclamation inside.
fn warning(f: Frame, pen: Pen, put: &mut impl FnMut(u32, u32)) {
    let (lx, rx) = (f.mid_x(), f.mid_x() + 1);
    pen.line((lx, f.top), (f.left, f.bottom), put);
    pen.line((rx, f.top), (f.right, f.bottom), put);
    pen.fill((lx, rx), (f.top, f.top), put);
    pen.fill((f.left, f.right), (f.bottom, f.bottom), put);
    // The bang sits below the middle, where it clears the sides.
    let unit = (f.half_h() / 4).max(1);
    let dot = f.bottom.saturating_sub(unit);
    pen.fill(
        (lx, rx),
        (f.mid_y() + unit, f.bottom.saturating_sub(3 * unit)),
        put,
    );
    pen.fill((lx, rx), (dot, dot), put);
}

/// `U+25B6` / `U+25C0` and, on an inset frame, `U+25B8` / `U+25C2`, filled
/// a row at a time.
fn triangle(f: Frame, pen: Pen, pointing_right: bool, put: &mut impl FnMut(u32, u32)) {
    let (mid, half) = (f.mid_y(), f.half_h());
    if half == 0 {
        pen.fill((f.left, f.right), (mid, mid), put);
        return;
    }
    for y in (mid - half)..=(mid + half) {
        let run = f.width() * y.abs_diff(mid) / half;
        if pointing_right {
            pen.fill((f.left, f.right - run), (y, y), put);
        } else {
            pen.fill((f.left + run, f.right), (y, y), put);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 8;
    const H: u32 = 16;

    /// Rasterize one glyph into a `W x H` boolean bitmap.
    fn bitmap(ch: char) -> (bool, Vec<bool>) {
        let mut pixels = vec![false; (W * H) as usize];
        let mut put = |x: u32, y: u32| {
            if x < W && y < H {
                pixels[(y * W + x) as usize] = true;
            }
        };
        let covered = draw(ch, W, H, &mut put);
        (covered, pixels)
    }

    fn at(pixels: &[bool], x: u32, y: u32) -> bool {
        pixels[(y * W + x) as usize]
    }

    fn lit(pixels: &[bool]) -> usize {
        pixels.iter().filter(|p| **p).count()
    }

    #[test]
    fn u2500_is_exactly_the_mid_row() {
        let (covered, pixels) = bitmap('\u{2500}');
        assert!(covered);
        for y in 0..H {
            for x in 0..W {
                assert_eq!(at(&pixels, x, y), y == H / 2, "({x}, {y})");
            }
        }
    }

    #[test]
    fn u250c_draws_only_the_lower_right_arms() {
        let (_, pixels) = bitmap('\u{250c}');
        let (mx, my) = (W / 2, H / 2);
        for x in 0..W {
            assert_eq!(at(&pixels, x, my), x >= mx, "right arm at x={x}");
        }
        for y in 0..H {
            assert_eq!(at(&pixels, mx, y), y >= my, "down arm at y={y}");
        }
    }

    #[test]
    fn u2554_double_corner_nests_rather_than_filling_the_junction() {
        let (_, pixels) = bitmap('\u{2554}');
        let (upper, lower) = (H / 2 - 2, H / 2 + 1);
        let (leftc, rightc) = (W / 2 - 2, W / 2 + 1);
        assert!(at(&pixels, leftc, upper), "outer corner missing");
        assert!(at(&pixels, rightc, lower), "inner corner missing");
        assert!(!at(&pixels, rightc, upper + 1), "junction filled");
    }

    #[test]
    fn blocks_and_shades() {
        assert_eq!(lit(&bitmap('\u{2588}').1), (W * H) as usize);
        let (light, medium, dark) = (
            lit(&bitmap('\u{2591}').1),
            lit(&bitmap('\u{2592}').1),
            lit(&bitmap('\u{2593}').1),
        );
        assert!(light < medium && medium < dark, "{light} {medium} {dark}");
    }

    #[test]
    fn coverage_is_exactly_the_two_disjoint_tiers_and_every_glyph_paints() {
        // `raster::emits_ink` assumes every covered glyph paints something.
        for ch in (0..=0xffff_u32).filter_map(char::from_u32) {
            let (box_tier, fallback) = (covers(ch), covers_fallback(ch));
            assert!(!(box_tier && fallback), "{ch:?} is in both tiers");
            let (covered, pixels) = bitmap(ch);
            assert_eq!(covered, box_tier || fallback, "{ch:?}");
            assert_eq!(lit(&pixels) > 0, covered, "{ch:?}");
        }
    }

    #[test]
    fn a_zero_sized_cell_is_still_covered_and_paints_nothing() {
        let mut painted = 0_u32;
        let mut put = |_x, _y| painted += 1;
        assert!(draw('\u{2500}', 0, 0, &mut put));
        assert!(draw('\u{276f}', 0, 0, &mut put));
        assert_eq!(painted, 0);
    }

    /// The tight box around every lit pixel: `(x0, x1, y0, y1)`, inclusive.
    fn ink(pixels: &[bool]) -> (u32, u32, u32, u32) {
        let lit: Vec<(u32, u32)> = (0..H)
            .flat_map(|y| (0..W).map(move |x| (x, y)))
            .filter(|(x, y)| at(pixels, *x, *y))
            .collect();
        let xs = lit.iter().map(|p| p.0);
        let ys = lit.iter().map(|p| p.1);
        (
            xs.clone().min().unwrap_or(0),
            xs.max().unwrap_or(0),
            ys.clone().min().unwrap_or(0),
            ys.max().unwrap_or(0),
        )
    }

    fn assert_mirrored(left: char, right: char) {
        let (l, r) = (bitmap(left).1, bitmap(right).1);
        let (x0, x1, ..) = ink(&r);
        assert_eq!(
            ink(&l),
            ink(&r),
            "{left:?} and {right:?} occupy different boxes"
        );
        for y in 0..H {
            for x in x0..=x1 {
                assert_eq!(at(&l, x0 + x1 - x, y), at(&r, x, y), "({x}, {y})");
            }
        }
    }

    #[test]
    fn u276f_prompt_chevron_is_symmetric_and_converges_on_its_apex() {
        let (_, pixels) = bitmap('\u{276f}');
        let (x0, x1, y0, y1) = ink(&pixels);
        let mid = u32::midpoint(y0, y1);
        assert!(at(&pixels, x1, mid), "no apex at the middle");
        assert!(
            at(&pixels, x0, y0) && at(&pixels, x0, y1),
            "open ends not level"
        );
        assert_eq!(mid - y0, y1 - mid, "arms are not the same length");
        let rightmost = |y: u32| (0..W).rev().find(|x| at(&pixels, *x, y));
        for y in y0..mid {
            assert!(rightmost(y) < rightmost(y + 1), "upper arm at y={y}");
        }
        for y in mid..y1 {
            assert!(rightmost(y) > rightmost(y + 1), "lower arm at y={y}");
        }
        assert_mirrored('\u{276e}', '\u{276f}');
        assert_mirrored('\u{25c0}', '\u{25b6}');
    }
}
