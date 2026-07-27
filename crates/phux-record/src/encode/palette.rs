//! The GIF global color table.
//!
//! Two paths, and the first one is the one that matters. When a recording has
//! 256 or fewer distinct colors — which the 1-bit bitmap font makes the
//! overwhelmingly common case, because every output pixel is exactly some
//! cell's resolved foreground or background and rendering invents no
//! in-between shades (ADR-0060) — the set of colors *is* the table and the
//! GIF is bit-exact with the RGB surface. Only when that fails does anything
//! resembling quantization happen, and even then it is a nearest-fit into the
//! terminal's own 256-entry palette rather than a general-purpose quantizer.
//!
//! **Never dither.** Terminal content is flat blocks of solid color.
//! Floyd-Steinberg (or any error diffusion) sprays per-pixel noise across
//! those blocks, which looks wrong on a screenshot-like artifact *and*
//! destroys the two things that keep these files small: LZW's run matching
//! within a frame, and the inter-frame similarity that lets sub-rectangle
//! frames stay tiny. If someone one day records sixel or photographic
//! content, median cut is the upgrade to reach for — not dithering, and not
//! today.

use std::collections::{HashMap, HashSet};

use crate::raster::Theme;

/// Rec. 709 luminance weights, x1000, used to order the table.
const LUMA_R: u32 = 2126;
const LUMA_G: u32 = 7152;
const LUMA_B: u32 = 722;

/// The largest table GIF's global color table can hold.
const MAX_ENTRIES: usize = 256;

/// Perceived brightness of `color`, scaled by 1000 so it stays integer.
pub(super) fn luminance(color: [u8; 3]) -> u32 {
    LUMA_R * u32::from(color[0]) + LUMA_G * u32::from(color[1]) + LUMA_B * u32::from(color[2])
}

/// Squared distance under the standard cheap perceptual weighting
/// `2*dR^2 + 4*dG^2 + 3*dB^2`. One line, no LAB crate, and entirely good
/// enough for deciding which of 256 terminal colors a stray blend is nearest.
fn distance(a: [u8; 3], b: [u8; 3]) -> i32 {
    let dr = i32::from(a[0]) - i32::from(b[0]);
    let dg = i32::from(a[1]) - i32::from(b[1]);
    let db = i32::from(a[2]) - i32::from(b[2]);
    2 * dr * dr + 4 * dg * dg + 3 * db * db
}

/// The GIF global color table plus a memoized source-color to index map.
#[derive(Debug, Clone)]
pub struct Palette {
    table: Vec<[u8; 3]>,
    memo: HashMap<[u8; 3], u8>,
}

impl Palette {
    /// Build a table for `colors`, falling back to `fallback`'s own palette
    /// when the recording does not fit in 256 entries.
    ///
    /// The table is sorted by luminance so that similar colors get adjacent
    /// indices. That costs one `sort_unstable_by_key` and buys a little LZW
    /// compression, because adjacent indices in a gradient or a shaded block
    /// then form longer repeating runs.
    #[must_use]
    pub fn build(colors: &HashSet<[u8; 3]>, fallback: &Theme) -> Self {
        let mut table: Vec<[u8; 3]> = if colors.len() <= MAX_ENTRIES {
            colors.iter().copied().collect()
        } else {
            // The terminal's own 256-entry palette is already a well-spread
            // grid over the colors terminal content actually uses, so it beats
            // anything a cheap quantizer would invent from this recording. The
            // two entries that get displaced are the least useful ones at the
            // tail of the table; the two that replace them are the colors that
            // dominate every single frame, and losing either to a nearest-fit
            // would tint the whole animation.
            let mut spread = fallback.palette.to_vec();
            spread.truncate(MAX_ENTRIES - 2);
            spread.push(fallback.bg);
            spread.push(fallback.fg);
            spread
        };
        table.sort_unstable_by_key(|color| luminance(*color));
        // Two distinct colors can share a luminance, so dedup by value after
        // sorting rather than trusting the sort key.
        table.dedup();
        if table.is_empty() {
            // A GIF with an empty global color table is malformed. An empty
            // color set means an empty recording, and one background entry is
            // the honest table for it.
            table.push(fallback.bg);
        }
        Self {
            table,
            memo: HashMap::new(),
        }
    }

    /// The index of `color`, memoized.
    ///
    /// Each *distinct* source color pays the 256-entry scan exactly once;
    /// every pixel afterwards is a hash hit. That is what keeps the exact
    /// path — where the scan always terminates on the first zero-distance
    /// match anyway — from dominating the render.
    pub fn index(&mut self, color: [u8; 3]) -> u8 {
        if let Some(found) = self.memo.get(&color) {
            return *found;
        }
        let mut best = 0_usize;
        let mut best_distance = i32::MAX;
        for (idx, candidate) in self.table.iter().enumerate() {
            let d = distance(color, *candidate);
            if d < best_distance {
                best_distance = d;
                best = idx;
                if d == 0 {
                    break;
                }
            }
        }
        // The table is capped at 256 entries, so this conversion cannot
        // actually saturate; `u8::MAX` is a defined answer rather than a
        // panic if that invariant is ever broken by a future edit.
        let slot = u8::try_from(best).unwrap_or(u8::MAX);
        self.memo.insert(color, slot);
        slot
    }

    /// The global color table, in index order.
    #[must_use]
    pub fn table(&self) -> &[[u8; 3]] {
        &self.table
    }

    /// The table flattened to the `[r, g, b, ...]` layout GIF wants.
    ///
    /// No padding to a power of two here: `gif`'s own `check_color_table`
    /// pads and computes the size flag, and duplicating that arithmetic is
    /// how the two disagree.
    #[must_use]
    pub(super) fn to_gif_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.table.len() * 3);
        for color in &self.table {
            out.extend_from_slice(color);
        }
        out
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

    #[test]
    fn exact_path_is_lossless_under_256_colors() {
        let theme = Theme::default();
        let colors: HashSet<[u8; 3]> = [[0, 0, 0], [255, 255, 255], [10, 20, 30]]
            .into_iter()
            .collect();
        let mut palette = Palette::build(&colors, &theme);
        for color in &colors {
            let slot = palette.index(*color);
            assert_eq!(
                palette.table().get(slot as usize),
                Some(color),
                "exact path must round-trip every input color"
            );
        }
    }

    #[test]
    fn exact_table_is_sorted_by_luminance() {
        let theme = Theme::default();
        let colors: HashSet<[u8; 3]> = (0..40_u8).map(|i| [i * 6, 255 - i * 6, i]).collect();
        let palette = Palette::build(&colors, &theme);
        let lums: Vec<u32> = palette.table().iter().map(|c| luminance(*c)).collect();
        assert!(lums.windows(2).all(|pair| pair[0] <= pair[1]), "{lums:?}");
    }

    #[test]
    fn index_is_memoized() {
        let theme = Theme::default();
        let colors: HashSet<[u8; 3]> = [[1, 2, 3], [4, 5, 6]].into_iter().collect();
        let mut palette = Palette::build(&colors, &theme);
        let first = palette.index([1, 2, 3]);
        let second = palette.index([1, 2, 3]);
        assert_eq!(first, second);
    }

    /// A palette built without a color forces the nearest-fit branch, which is
    /// the only place quantization error can be introduced at all.
    #[test]
    fn fallback_path_maps_to_the_nearest_theme_entry() {
        let theme = Theme::default();
        let colors: HashSet<[u8; 3]> = (0..300_u32)
            .map(|i| {
                let hi = u8::try_from(i / 256).unwrap_or(0);
                let lo = u8::try_from(i % 256).unwrap_or(0);
                [hi, lo, 0]
            })
            .collect();
        let mut palette = Palette::build(&colors, &theme);
        // Pure white is in the xterm table (index 15 and 231), so the nearest
        // entry to a near-white must be white itself.
        let slot = palette.index([254, 254, 254]);
        let got = *palette.table().get(slot as usize).expect("index in range");
        let best = palette
            .table()
            .iter()
            .copied()
            .min_by_key(|c| distance([254, 254, 254], *c))
            .expect("non-empty table");
        assert_eq!(got, best, "nearest-fit picked a non-minimal entry");
    }

    #[test]
    fn fallback_table_contains_theme_fg_and_bg() {
        let theme = Theme::default();
        // 300 distinct colors forces the fallback path.
        let colors: HashSet<[u8; 3]> = (0..300_u32)
            .map(|i| {
                let hi = u8::try_from(i / 256).unwrap_or(0);
                let lo = u8::try_from(i % 256).unwrap_or(0);
                [hi, lo, 0]
            })
            .collect();
        let palette = Palette::build(&colors, &theme);
        assert!(palette.table().contains(&theme.bg), "bg missing");
        assert!(palette.table().contains(&theme.fg), "fg missing");
        assert!(palette.table().len() <= MAX_ENTRIES, "table overflowed");
    }

    #[test]
    fn gif_bytes_are_the_table_flattened_in_index_order() {
        let theme = Theme::default();
        let colors: HashSet<[u8; 3]> = [[0, 0, 0], [9, 9, 9]].into_iter().collect();
        let palette = Palette::build(&colors, &theme);
        let bytes = palette.to_gif_bytes();
        assert_eq!(bytes.len(), palette.table().len() * 3);
        assert_eq!(&bytes[0..3], &palette.table()[0][..]);
    }
}
