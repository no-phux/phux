//! Procedural box drawing, block elements, and shades: `U+2500..=U+259F`.
//!
//! These ~160 codepoints are the bulk of what a TUI actually paints — every
//! ratatui border, every `tmux`/phux divider, every progress bar, every
//! sparkline — so they get drawn from geometry rather than looked up in the
//! bitmap face. Two reasons, in order of weight:
//!
//! 1. **Exactness at any cell size.** A bitmap glyph is a fixed 8x16 stamp; a
//!    line computed from `cell_w`/`cell_h` lands on the mathematical centre
//!    whatever the cell is. Adjacent cells therefore always join seamlessly,
//!    which is the one visual defect a reader notices instantly in a box.
//! 2. **Coverage.** A face may be missing a heavy or double variant and would
//!    then render tofu in the middle of an otherwise clean frame.
//!
//! Two approximations are deliberate and permanent (ADR-0060 sanctions them):
//! **heavy** strokes are two pixels rather than a true weight ramp, and
//! **double** strokes are two one-pixel lines. At 8x16 there is no third
//! option — a "true" heavy line in an 8-pixel-wide cell is 2px.
//!
//! Dashed variants (`U+2504..=U+250B`, `U+254C..=U+254F`) are drawn solid:
//! the dash period is not resolvable at this cell size, and a solid line
//! reads correctly where a 1-on-1-off pattern reads as noise. Arcs
//! (`U+256D..=U+2570`) are drawn as square corners for the same reason.
//!
//! A second, much smaller tier lives at the bottom of this file: symbols the
//! vendored face simply does not have, drawn only when the face returns
//! nothing. See [`covers_fallback`] for the lookup order, which is the part
//! that must not drift.

/// Whether [`draw`] renders `ch` *in preference to the bitmap face*.
///
/// Tier one of two — see [`covers_fallback`] for the full lookup order.
///
/// Kept separate from [`draw`] so the rasterizer's colour-histogram pass can
/// ask "would this cell paint foreground pixels?" without running the
/// geometry — the histogram and the paint must agree, and the cheapest way
/// to guarantee that is for both to consult one predicate.
#[must_use]
pub const fn covers(ch: char) -> bool {
    matches!(ch, '\u{2500}'..='\u{259f}')
}

/// Paint `ch` into a `cell_w` x `cell_h` cell, calling `put(x, y)` for every
/// foreground pixel.
///
/// Coordinates are cell-local: `(0, 0)` is the top-left pixel of the cell.
/// Returns `false` — having called `put` not at all — for any codepoint
/// neither [`covers`] nor [`covers_fallback`] claims, which is the caller's
/// signal to fall back to the bitmap face and then to tofu.
pub fn draw(ch: char, cell_w: u32, cell_h: u32, put: &mut impl FnMut(u32, u32)) -> bool {
    let boxed = covers(ch);
    if !boxed && !covers_fallback(ch) {
        return false;
    }
    // A degenerate cell has no pixels to paint, but the codepoint is still
    // "covered": answering `false` here would send the caller down the tofu
    // path and change which glyph a zero-sized cell claims to be.
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
/// Transcribed from the Unicode names, not guessed: `U+251E BOX DRAWINGS UP
/// HEAVY AND RIGHT DOWN LIGHT` is `up = H, down = L, right = L`. The three
/// diagonals (`U+2571..=U+2573`) carry no arms and are special-cased in
/// [`draw_line`].
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
/// `size` is the *perpendicular* extent: a horizontal arm strokes rows out of
/// `cell_h`, a vertical arm strokes columns out of `cell_w`. Light lands on
/// the lower/right of the two centre positions so that a light cross in an
/// even-sized cell is at the same place a heavy cross's second stroke is,
/// which keeps `─` and `━` visually aligned when they meet.
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
            // An odd-sized cell collapses lo == hi; nudge outward so heavy is
            // still two pixels rather than silently degrading to light.
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

    // Where the arms meet. An arm runs all the way to the *far* stroke of the
    // perpendicular pair so the junction is closed rather than showing a
    // one-pixel notch; with no perpendicular arm at all it runs to the cell
    // centre, where the opposite arm picks it up.
    let (vx_lo, vx_hi) = span(&[up_cols, down_cols], cell_w / 2);
    let (hy_lo, hy_hi) = span(&[left_rows, right_rows], cell_h / 2);

    // Double-vs-double corners are the one case the uniform rule renders as a
    // filled blob instead of a nested corner, because both parallel strokes
    // would run to the same junction. Pair them up instead: the outer stroke
    // of one arm meets the outer stroke of the other. `matched` says whether
    // stroke 0 of the horizontal arm pairs with stroke 0 of the vertical one.
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

/// The paired junction coordinate for stroke `idx` of a double-double corner.
///
/// `horizontal` selects which family of strokes is asking: a horizontal
/// stroke joins a vertical column, and vice versa.
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
        // Full block. The one glyph that covers every pixel of its cell —
        // `raster` special-cases it when deciding whether a cell still shows
        // any background at all.
        0x2588 => rect(0, 0, cell_w, cell_h, put),
        // Left seven-eighths (2589) down to left one-eighth (258F).
        0x2589..=0x258f => {
            let eighths = 0x2590 - code;
            rect(0, 0, cell_w * eighths / 8, cell_h, put);
        }
        // Right half.
        0x2590 => rect(cell_w / 2, 0, cell_w, cell_h, put),
        // Shades. Ordered dither, never error diffusion: terminal content is
        // flat blocks, and a regular pattern both looks right and compresses,
        // where dithered noise destroys inter-frame and LZW compression.
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
            // A 4x2 ordered cell: one pixel in four on alternating phases is
            // 25%, the checkerboard is 50%, and dark is light's complement.
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

/// Whether [`draw`] renders `ch` *only when the bitmap face has no glyph for
/// it*.
///
/// Tier two of two. The distinction from [`covers`] is load-bearing, so the
/// lookup order is written down once, here, and `raster::Rasterizer::glyph_of`
/// implements exactly it:
///
/// 1. [`covers`] — `U+2500..=U+259F` — beats the face, because a line
///    computed from the cell size joins its neighbours exactly and a stamped
///    one does not.
/// 2. The bitmap face.
/// 3. `covers_fallback` — this set — which paints only where the face
///    returned `None`.
/// 4. Tofu.
///
/// So this is a fallback and never an override: a codepoint with a real
/// bitmap keeps its bitmap. `fallback_glyphs_are_exactly_what_the_face_lacks`
/// in [`super`] asserts the two sets are disjoint against the real table, so
/// a later font swap that *gains* one of these fails the build rather than
/// silently preferring a hand-drawn approximation.
///
/// The set is curated on evidence — a glyph earns its place by turning up in
/// ordinary terminal output, not by being nice to have — and each group
/// carries that evidence below.
#[must_use]
#[rustfmt::skip]
pub const fn covers_fallback(ch: char) -> bool {
    matches!(
        ch,
        // Status marks: check, heavy check, and the four crosses. What test
        // runners, linters, and phux's own tooling print all day.
        '\u{2713}'..='\u{2718}'
        // Prompt characters: the angle-quotation ornaments starship and pure
        // use, and the heavy arrows oh-my-zsh's default theme uses. First
        // thing on screen, so tofu here is the fastest way to make an export
        // look broken.
        | '\u{276e}' | '\u{276f}' | '\u{2794}' | '\u{279c}'
        // The other two thirds of a `log-symbols`-style status line:
        // information source and warning sign.
        | '\u{2139}' | '\u{26a0}'
        // Sideways arrowheads, large and small. The face already has the up
        // and down ones (`U+25B2`, `U+25BC`), and half a set of arrowheads
        // reads as a bug in a way that none of them would not.
        | '\u{25b6}' | '\u{25b8}' | '\u{25c0}' | '\u{25c2}'
    )
}

fn draw_symbol(code: u32, cell_w: u32, cell_h: u32, put: &mut impl FnMut(u32, u32)) {
    // Only the all-diagonal symbols ask for weight: [`Pen`] thickens by one
    // column, which reads as heavier on a diagonal and merely lengthens a
    // horizontal shaft.
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
        // The small variants are the same shape drawn inside a tighter box;
        // scaling the whole frame is what keeps them recognisably the *same*
        // arrowhead rather than a different one.
        0x25b8 => triangle(band.inset(1, 2), pen, true, put),
        0x25c2 => triangle(band.inset(1, 2), pen, false, put),
        _ => {}
    }
}

/// The inclusive box a symbol is inked inside, in cell-local pixels.
///
/// Three shapes of box, because these symbols do not share one aspect ratio
/// and forcing them to would make half of them unreadable at 8x16. All three
/// are derived from the cell size rather than hardcoded, so they still track
/// a larger cell; the 8x16 values are given because that is the size the
/// shapes were tuned at.
#[derive(Clone, Copy)]
struct Frame {
    left: u32,
    right: u32,
    top: u32,
    bottom: u32,
}

impl Frame {
    /// The optical band the face's capitals occupy: `x in 1..=6`,
    /// `y in 3..=12` at 8x16.
    ///
    /// `draw` returns early on a zero-sized cell, so the `- 1` here cannot
    /// wrap and the box is always at least one pixel on each axis.
    const fn band(cell_w: u32, cell_h: u32) -> Self {
        Self {
            left: cell_w / 8,
            right: cell_w - 1 - cell_w / 8,
            top: cell_h * 3 / 16,
            bottom: cell_h - 1 - cell_h * 3 / 16,
        }
    }

    /// Full width and as near square as the cell allows: `x in 0..=7`,
    /// `y in 4..=11` at 8x16.
    ///
    /// The check mark and the crosses need it. Squeezed into the band they
    /// become five columns over nine rows, and a cross at that ratio is a
    /// vertical bar with four whiskers rather than an X.
    const fn square(cell_w: u32, cell_h: u32) -> Self {
        Self {
            left: 0,
            right: cell_w - 1,
            top: cell_h / 4,
            bottom: cell_h - 1 - cell_h / 4,
        }
    }

    /// Every pixel the cell can spare: `x in 0..=7`, `y in 2..=13` at 8x16.
    ///
    /// Only the warning sign asks for it, and it needs all of it: a hollow
    /// triangle with an exclamation inside is three nested shapes in eight
    /// columns.
    const fn full(cell_w: u32, cell_h: u32) -> Self {
        Self {
            left: 0,
            right: cell_w - 1,
            top: cell_h / 8,
            bottom: cell_h - 1 - cell_h / 8,
        }
    }

    /// Give back the column a heavy stroke will grow into, so a heavy symbol
    /// occupies the same box as its light twin instead of losing its right
    /// edge to the clip.
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

    /// Half the vertical extent. `mid_y() - half_h()` is exactly `top` for
    /// every parity, which is what lets the symmetric shapes subtract without
    /// a saturating guard.
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
///
/// One column is the entire weight ramp available at 8x16 — the same
/// approximation ADR-0060 sanctions for heavy box lines. Nothing here is
/// clipped by the caller, so `dot` is the single place cell bounds are
/// enforced for the whole symbol tier.
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

    /// Bresenham from `a` to `b`, inclusive of both ends.
    ///
    /// Integer Bresenham rather than a per-row `x = f(y)` sweep because these
    /// strokes are shallow as often as they are steep — a check mark's foot
    /// runs two columns over three rows — and a sweep along the wrong axis
    /// leaves the stroke in disconnected dashes.
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

/// `U+2713` / `U+2714`: a short arm dropping to a low vertex, and a long arm
/// rising past it to the right.
///
/// The vertex sits at two fifths of the width so the long arm gets the other
/// three: at eight pixels wide there is no room for a 45-degree rise, and a
/// check whose two arms are the same length reads as the letter "v".
fn check(f: Frame, pen: Pen, put: &mut impl FnMut(u32, u32)) {
    let vertex = (f.left + f.width() * 2 / 5, f.bottom);
    pen.line((f.left, f.top + (f.bottom - f.top) * 5 / 8), vertex, put);
    pen.line(vertex, (f.right, f.top), put);
}

/// `U+2715..=U+2718`: two full-box diagonals crossing at the centre.
///
/// The four codepoints differ only in stroke weight and in whether the arms
/// are drawn as a saltire or a rotated cross — distinctions with no pixels to
/// spend at 8x16, so they share one shape and differ only in [`Pen::heavy`].
fn cross(f: Frame, pen: Pen, put: &mut impl FnMut(u32, u32)) {
    pen.line((f.left, f.top), (f.right, f.bottom), put);
    pen.line((f.right, f.top), (f.left, f.bottom), put);
}

/// `U+276E` / `U+276F`: two arms meeting at a point on the vertical middle.
///
/// The apex is on the pointing side and the two open ends are level with each
/// other. That symmetry is the whole glyph — an asymmetric chevron reads as a
/// stray "greater than", which is exactly what a prompt is not.
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
///
/// The two codepoints are a wide-headed and a round-tipped heavy rightwards
/// arrow; the difference is a tip radius, which is below the resolution of an
/// eight-pixel cell. They are deliberately identical here.
fn arrow(f: Frame, pen: Pen, put: &mut impl FnMut(u32, u32)) {
    let mid = f.mid_y();
    let tip = (f.right, mid);
    pen.line((f.left, mid), tip, put);
    // A shallow barb disappears into the shaft at this size, so the head is
    // as tall as the box and swept back over half the width.
    let back = f.right - f.width() / 2;
    let rise = f.half_h();
    pen.line((back, mid - rise), tip, put);
    pen.line((back, mid + rise), tip, put);
}

/// `U+2139`: a dot over a stem with a foot.
///
/// The gap between dot and stem is the entire glyph. Close it and this is a
/// vertical bar, which is the one thing an "information" marker beside a log
/// line must not be mistaken for.
fn info(f: Frame, pen: Pen, put: &mut impl FnMut(u32, u32)) {
    let h = f.bottom - f.top;
    let (x, x1) = (f.mid_x(), f.mid_x() + 1);
    pen.fill((x, x1), (f.top, f.top + h / 8), put);
    pen.fill((x, x1), (f.top + h / 3, f.bottom), put);
    pen.fill((x.saturating_sub(1), x1 + 1), (f.bottom, f.bottom), put);
}

/// `U+26A0`: a hollow triangle with an exclamation inside it.
///
/// The apex is two columns wide so the two sides are mirror images in an
/// even-width cell; with a single-column apex the left side is one pixel
/// shorter than the right and the triangle visibly leans.
fn warning(f: Frame, pen: Pen, put: &mut impl FnMut(u32, u32)) {
    let (lx, rx) = (f.mid_x(), f.mid_x() + 1);
    pen.line((lx, f.top), (f.left, f.bottom), put);
    pen.line((rx, f.top), (f.right, f.bottom), put);
    pen.fill((lx, rx), (f.top, f.top), put);
    pen.fill((f.left, f.right), (f.bottom, f.bottom), put);
    // The bang lives entirely below the vertical middle. Higher up the sides
    // have not diverged far enough to leave a lit-free column either side of
    // it, and a bang fused to the triangle reads as a filled triangle.
    let unit = (f.half_h() / 4).max(1);
    let dot = f.bottom.saturating_sub(unit);
    pen.fill(
        (lx, rx),
        (f.mid_y() + unit, f.bottom.saturating_sub(3 * unit)),
        put,
    );
    pen.fill((lx, rx), (dot, dot), put);
}

/// `U+25B6` / `U+25C0` and, on an inset frame, `U+25B8` / `U+25C2`.
///
/// Filled a row at a time rather than as an outline plus a flood, so the
/// shape has no interior seam at any cell size.
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
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
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

    #[test]
    fn u2500_draws_a_full_width_horizontal_line_at_mid_row() {
        let (covered, pixels) = bitmap('\u{2500}');
        assert!(covered);
        let mid = H / 2;
        for x in 0..W {
            assert!(at(&pixels, x, mid), "gap at x={x} on the mid row");
        }
        // And nothing anywhere else: a horizontal rule that also paints a
        // vertical stub joins wrongly with its neighbours.
        for y in 0..H {
            if y == mid {
                continue;
            }
            for x in 0..W {
                assert!(!at(&pixels, x, y), "stray pixel at ({x}, {y})");
            }
        }
    }

    #[test]
    fn u2502_draws_a_full_height_vertical_line() {
        let (covered, pixels) = bitmap('\u{2502}');
        assert!(covered);
        let mid = W / 2;
        for y in 0..H {
            assert!(at(&pixels, mid, y), "gap at y={y} on the mid column");
        }
        for x in 0..W {
            if x == mid {
                continue;
            }
            for y in 0..H {
                assert!(!at(&pixels, x, y), "stray pixel at ({x}, {y})");
            }
        }
    }

    #[test]
    fn u250c_draws_only_the_lower_right_quadrant_arms() {
        let (covered, pixels) = bitmap('\u{250c}');
        assert!(covered);
        let (mx, my) = (W / 2, H / 2);
        // The two arms are present, meeting at the centre pixel.
        for x in mx..W {
            assert!(at(&pixels, x, my), "missing right arm at x={x}");
        }
        for y in my..H {
            assert!(at(&pixels, mx, y), "missing down arm at y={y}");
        }
        // And the other two quadrants are untouched: a top-left corner that
        // leaks up or left renders as a cross when tiled.
        for x in 0..mx {
            assert!(!at(&pixels, x, my), "left arm leaked at x={x}");
        }
        for y in 0..my {
            assert!(!at(&pixels, mx, y), "up arm leaked at y={y}");
        }
    }

    #[test]
    fn u2554_double_corner_nests_rather_than_filling_the_junction() {
        // The one case the uniform join rule gets wrong; the paired-stroke
        // path exists solely for it. The inner corner pixel must stay empty.
        let (covered, pixels) = bitmap('\u{2554}');
        assert!(covered);
        let (upper, lower) = (H / 2 - 2, H / 2 + 1);
        let (leftc, rightc) = (W / 2 - 2, W / 2 + 1);
        assert!(at(&pixels, leftc, upper), "outer corner missing");
        assert!(at(&pixels, rightc, lower), "inner corner missing");
        // Between the two strokes, above the inner corner, is interior: it is
        // the gap that makes a double line read as two lines.
        assert!(
            !at(&pixels, rightc, upper + 1),
            "junction filled instead of nested"
        );
    }

    #[test]
    fn u2588_full_block_fills_every_pixel() {
        let (covered, pixels) = bitmap('\u{2588}');
        assert!(covered);
        assert!(pixels.iter().all(|lit| *lit));
    }

    #[test]
    fn u2591_light_shade_fills_between_a_fifth_and_a_third_of_the_cell() {
        let (covered, pixels) = bitmap('\u{2591}');
        assert!(covered);
        let lit = pixels.iter().filter(|p| **p).count();
        let total = pixels.len();
        assert!(
            lit * 5 >= total && lit * 3 <= total,
            "light shade lit {lit}/{total} pixels"
        );
    }

    #[test]
    fn shades_are_ordered_light_medium_dark() {
        let count = |ch| bitmap(ch).1.iter().filter(|p| **p).count();
        let (light, medium, dark) = (count('\u{2591}'), count('\u{2592}'), count('\u{2593}'));
        assert!(light < medium, "light {light} not below medium {medium}");
        assert!(medium < dark, "medium {medium} not below dark {dark}");
    }

    #[test]
    fn draw_returns_false_for_anything_neither_tier_claims() {
        let mut painted = 0_u32;
        {
            let mut put = |_x, _y| painted += 1;
            assert!(!draw('A', W, H, &mut put));
            assert!(!draw('\u{24ff}', W, H, &mut put));
            assert!(!draw('\u{25a0}', W, H, &mut put));
            // The codepoints either side of the check/cross run. The face has
            // neither, so both are tofu on purpose: the fallback set is
            // curated on what sessions actually print, not widened to a
            // whole Dingbats block because the neighbours were free.
            assert!(!draw('\u{2712}', W, H, &mut put));
            assert!(!draw('\u{2719}', W, H, &mut put));
        }
        assert_eq!(painted, 0, "a rejected codepoint must paint nothing");
        let mut put = |_x, _y| painted += 1;
        assert!(draw('\u{2500}', W, H, &mut put));
        assert!(draw('\u{259f}', W, H, &mut put));
        assert!(draw('\u{276f}', W, H, &mut put));
    }

    #[test]
    fn every_covered_codepoint_paints_at_least_one_pixel() {
        // `raster::colors_of` assumes a covered glyph always emits foreground
        // ink; a silently blank entry in ARMS would make the colour histogram
        // over-report and the drift guard fail confusingly rather than here.
        for code in 0x2500..=0x259f_u32 {
            let ch = char::from_u32(code).expect("BMP scalar value");
            let (covered, pixels) = bitmap(ch);
            assert!(covered, "U+{code:04X} not covered");
            assert!(pixels.iter().any(|lit| *lit), "U+{code:04X} paints nothing");
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

    // -----------------------------------------------------------------------
    // Fallback symbols
    // -----------------------------------------------------------------------

    /// The tight box around every lit pixel: `(x0, x1, y0, y1)`, inclusive.
    ///
    /// Assertions are written against this rather than against the frame
    /// constants so they describe the *shape* and survive a re-tuning of
    /// where in the cell it sits.
    fn ink(pixels: &[bool]) -> (u32, u32, u32, u32) {
        let mut found: Option<(u32, u32, u32, u32)> = None;
        for y in 0..H {
            for x in 0..W {
                if at(pixels, x, y) {
                    found = Some(found.map_or((x, x, y, y), |(x0, x1, y0, y1)| {
                        (x0.min(x), x1.max(x), y0.min(y), y1.max(y))
                    }));
                }
            }
        }
        found.expect("glyph paints at least one pixel")
    }

    /// The number of horizontal runs of lit pixels on row `y`.
    fn runs(pixels: &[bool], y: u32) -> usize {
        (0..W)
            .filter(|x| at(pixels, *x, y) && (*x == 0 || !at(pixels, x - 1, y)))
            .count()
    }

    /// The rightmost lit column on row `y`.
    fn rightmost(pixels: &[bool], y: u32) -> Option<u32> {
        (0..W).rev().find(|x| at(pixels, *x, y))
    }

    #[test]
    fn u276f_chevron_arms_meet_at_the_vertical_middle() {
        // The starship / pure prompt character, and the reason this tier
        // exists at all.
        let (covered, pixels) = bitmap('\u{276f}');
        assert!(covered);
        let (x0, x1, y0, y1) = ink(&pixels);
        let mid = u32::midpoint(y0, y1);
        assert!(at(&pixels, x1, mid), "no apex on the right at the middle");
        // The open ends are level with each other and equidistant from the
        // apex row: an asymmetric chevron reads as a stray "greater than".
        assert!(at(&pixels, x0, y0), "upper arm does not reach the left");
        assert!(at(&pixels, x0, y1), "lower arm does not reach the left");
        assert_eq!(mid - y0, y1 - mid, "arms are not the same length");
        // And they converge on the apex rather than bowing: each row's
        // rightmost pixel moves right as the row approaches the middle.
        for y in y0..mid {
            let (here, next) = (rightmost(&pixels, y), rightmost(&pixels, y + 1));
            assert!(here < next, "upper arm does not converge at y={y}");
        }
        for y in mid..y1 {
            let (here, next) = (rightmost(&pixels, y), rightmost(&pixels, y + 1));
            assert!(here > next, "lower arm does not converge at y={y}");
        }
    }

    #[test]
    fn u276e_is_the_mirror_image_of_u276f() {
        let left = bitmap('\u{276e}').1;
        let right = bitmap('\u{276f}').1;
        let (x0, x1, ..) = ink(&right);
        assert_eq!(ink(&left), ink(&right), "the two occupy different boxes");
        for y in 0..H {
            for x in x0..=x1 {
                assert_eq!(
                    at(&left, x0 + x1 - x, y),
                    at(&right, x, y),
                    "not mirrored at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn u2713_check_has_a_low_left_arm_and_a_long_arm_rising_to_the_right() {
        let (covered, pixels) = bitmap('\u{2713}');
        assert!(covered);
        let (x0, x1, y0, y1) = ink(&pixels);
        // The long arm ends at the top right corner of the ink.
        assert!(at(&pixels, x1, y0), "long arm does not reach the top right");
        // The vertex — the lowest row — sits in the left half, so the long
        // arm gets the majority of the width.
        let vertex = (0..W)
            .find(|x| at(&pixels, *x, y1))
            .expect("the lowest row is lit");
        assert!(
            vertex < u32::midpoint(x0, x1),
            "vertex at x={vertex} is not left"
        );
        // The short arm's foot is low but above the vertex: level with it and
        // this is a "v", above the middle and it is a tick.
        let foot = (0..H)
            .find(|y| at(&pixels, x0, *y))
            .expect("the leftmost column is lit");
        assert!(foot > u32::midpoint(y0, y1), "short arm starts too high");
        assert!(foot < y1, "short arm starts level with the vertex");
        // The long arm rises monotonically from the vertex to the tip.
        for y in y0..(y1 - 1) {
            let (here, next) = (rightmost(&pixels, y), rightmost(&pixels, y + 1));
            assert!(here >= next, "long arm doubles back at y={y}");
        }
    }

    #[test]
    fn u2714_is_u2713_with_a_column_of_extra_weight() {
        let light = bitmap('\u{2713}').1;
        let heavy = bitmap('\u{2714}').1;
        let count = |px: &[bool]| px.iter().filter(|p| **p).count();
        assert!(
            count(&heavy) > count(&light),
            "the heavy check is not heavier"
        );
        // Every heavy stroke is two columns wide; a lone pixel would mean the
        // frame gave back a column the doubling then lost to the clip.
        for y in 0..H {
            for x in 0..W {
                if !at(&heavy, x, y) {
                    continue;
                }
                let neighbour =
                    (x > 0 && at(&heavy, x - 1, y)) || (x + 1 < W && at(&heavy, x + 1, y));
                assert!(neighbour, "single-pixel stroke at ({x}, {y})");
            }
        }
    }

    #[test]
    fn u2715_through_u2718_cross_two_diagonals_at_the_centre() {
        // One shape for all four: they differ only in weight and in a
        // rotation that has no pixels to spend at 8x16.
        for ch in ['\u{2715}', '\u{2716}', '\u{2717}', '\u{2718}'] {
            let (covered, pixels) = bitmap(ch);
            assert!(covered, "U+{:04X} not covered", ch as u32);
            let (x0, x1, y0, y1) = ink(&pixels);
            for (x, y) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
                assert!(
                    at(&pixels, x, y),
                    "U+{:04X} corner ({x}, {y}) unlit",
                    ch as u32
                );
            }
            // The diagonals must actually meet: a cross whose arms pass on
            // either side of the centre reads as two slashes.
            let (cx, cy) = (u32::midpoint(x0, x1), u32::midpoint(y0, y1));
            assert!(
                at(&pixels, cx, cy),
                "U+{:04X} has no ink at its centre",
                ch as u32
            );
            assert_eq!(
                runs(&pixels, cy),
                1,
                "U+{:04X} centre row is split",
                ch as u32
            );
            for y in y0..=y1 {
                for x in x0..=x1 {
                    assert_eq!(
                        at(&pixels, x0 + x1 - x, y),
                        at(&pixels, x, y),
                        "U+{:04X} is not symmetric at ({x}, {y})",
                        ch as u32
                    );
                }
            }
        }
    }

    #[test]
    fn u2794_and_u279c_draw_a_shaft_on_the_middle_row_with_a_converging_head() {
        for ch in ['\u{2794}', '\u{279c}'] {
            let (covered, pixels) = bitmap(ch);
            assert!(covered, "U+{:04X} not covered", ch as u32);
            let (x0, x1, y0, y1) = ink(&pixels);
            let mid = u32::midpoint(y0, y1);
            for x in x0..=x1 {
                assert!(at(&pixels, x, mid), "gap in the shaft at x={x}");
            }
            // The head is entirely in the right half — an arrow with ink off
            // the shaft on the left is a double-headed arrow.
            for y in y0..=y1 {
                if y == mid {
                    continue;
                }
                for x in x0..u32::midpoint(x0, x1) {
                    assert!(!at(&pixels, x, y), "head ink at ({x}, {y})");
                }
            }
            for y in y0..mid {
                let (here, next) = (rightmost(&pixels, y), rightmost(&pixels, y + 1));
                assert!(here <= next, "upper barb doubles back at y={y}");
            }
            assert!(
                rightmost(&pixels, y0) < rightmost(&pixels, mid),
                "the barb is parallel to the shaft, not swept back"
            );
        }
    }

    #[test]
    fn u2139_keeps_one_clear_row_between_its_dot_and_its_stem() {
        // The gap is the entire glyph: close it and this is a vertical bar,
        // which is the one thing an information marker must not look like.
        let (covered, pixels) = bitmap('\u{2139}');
        assert!(covered);
        let (_, _, y0, y1) = ink(&pixels);
        let blank: Vec<u32> = (y0..=y1).filter(|y| runs(&pixels, *y) == 0).collect();
        assert_eq!(
            blank.len(),
            1,
            "expected exactly one clear row, got {blank:?}"
        );
        let gap = blank[0];
        assert!(gap < u32::midpoint(y0, y1), "the dot is not above the stem");
    }

    #[test]
    fn u26a0_is_a_hollow_triangle_with_a_free_standing_bang() {
        let (covered, pixels) = bitmap('\u{26a0}');
        assert!(covered);
        let (x0, x1, y0, y1) = ink(&pixels);
        for x in x0..=x1 {
            assert!(at(&pixels, x, y1), "gap in the base at x={x}");
        }
        assert_eq!(runs(&pixels, y0), 1, "the apex is split");
        // Three runs means the bang clears both sloping sides; the two-run
        // row between two three-run rows is the gap under the stem, which is
        // what makes it an exclamation mark and not a bar.
        let three: Vec<u32> = (y0..y1).filter(|y| runs(&pixels, *y) == 3).collect();
        assert!(three.len() >= 3, "bang fused to the sides: {three:?}");
        let first = three.first().copied().expect("at least three such rows");
        let last = three.last().copied().expect("at least three such rows");
        assert!(
            (first..last).any(|y| runs(&pixels, y) == 2),
            "no clear row between the stem and the dot"
        );
    }

    #[test]
    fn u25b6_and_u25c0_are_solid_mirrored_arrowheads() {
        let right = bitmap('\u{25b6}').1;
        let left = bitmap('\u{25c0}').1;
        let (x0, x1, y0, y1) = ink(&right);
        assert_eq!(ink(&left), ink(&right), "the two occupy different boxes");
        let mid = u32::midpoint(y0, y1);
        // Solid: one run per row, widest at the middle, narrowing to a point.
        let width = |px: &[bool], y: u32| (x0..=x1).filter(|x| at(px, *x, y)).count();
        for y in y0..=y1 {
            assert_eq!(runs(&right, y), 1, "row {y} is not solid");
        }
        assert_eq!(width(&right, mid), (x1 - x0 + 1) as usize, "no full row");
        for y in y0..mid {
            assert!(width(&right, y) < width(&right, y + 1), "no taper at y={y}");
        }
        for y in 0..H {
            for x in x0..=x1 {
                assert_eq!(
                    at(&left, x0 + x1 - x, y),
                    at(&right, x, y),
                    "not mirrored at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn u25b8_and_u25c2_are_the_small_variants_of_the_same_arrowheads() {
        for (big, small) in [('\u{25b6}', '\u{25b8}'), ('\u{25c0}', '\u{25c2}')] {
            let outer = ink(&bitmap(big).1);
            let inner = ink(&bitmap(small).1);
            assert!(
                outer.0 <= inner.0 && inner.1 <= outer.1,
                "U+{:04X} is wider than U+{:04X}",
                small as u32,
                big as u32
            );
            assert!(
                outer.2 < inner.2 && inner.3 < outer.3,
                "U+{:04X} is not shorter than U+{:04X}",
                small as u32,
                big as u32
            );
        }
    }

    #[test]
    fn the_two_tiers_never_claim_the_same_codepoint() {
        // `draw` dispatches on `covers` first, so an overlap would silently
        // make the fallback unreachable rather than fail anywhere visible.
        for code in 0..=0xffff_u32 {
            let Some(ch) = char::from_u32(code) else {
                continue;
            };
            assert!(
                !(covers(ch) && covers_fallback(ch)),
                "U+{code:04X} is in both tiers"
            );
        }
    }

    #[test]
    fn every_fallback_codepoint_paints_at_least_one_pixel() {
        // Same invariant, same reason as the box range: `raster::emits_ink`
        // answers `true` for anything the procedural renderer claims, so a
        // silently blank symbol would make the colour histogram over-report.
        for code in 0..=0xffff_u32 {
            let Some(ch) = char::from_u32(code) else {
                continue;
            };
            if !covers_fallback(ch) {
                continue;
            }
            let (covered, pixels) = bitmap(ch);
            assert!(covered, "U+{code:04X} not covered");
            assert!(pixels.iter().any(|lit| *lit), "U+{code:04X} paints nothing");
        }
    }
}
