//! The GIF global colour table.
//!
//! The 1-bit font means every pixel is some cell's resolved fg or bg
//! (ADR-0060), so a recording almost always has at most 256 colours and the
//! table is exact. Beyond that, colours map nearest-fit into the terminal's
//! own 256-entry palette. Never dither: error diffusion on flat terminal
//! content looks wrong and destroys LZW and inter-frame compression.

use std::collections::{HashMap, HashSet};

use crate::raster::Theme;

/// The largest table GIF's global colour table can hold.
const MAX_ENTRIES: usize = 256;

/// Rec. 709 luminance, x10000, used to order the table.
fn luminance(color: [u8; 3]) -> u32 {
    2126 * u32::from(color[0]) + 7152 * u32::from(color[1]) + 722 * u32::from(color[2])
}

/// Squared distance under the cheap perceptual weighting
/// `2*dR^2 + 4*dG^2 + 3*dB^2`.
fn distance(a: [u8; 3], b: [u8; 3]) -> i32 {
    let dr = i32::from(a[0]) - i32::from(b[0]);
    let dg = i32::from(a[1]) - i32::from(b[1]);
    let db = i32::from(a[2]) - i32::from(b[2]);
    2 * dr * dr + 4 * dg * dg + 3 * db * db
}

/// The GIF global colour table plus a memoized colour-to-index map.
#[derive(Debug, Clone)]
pub(crate) struct Palette {
    table: Vec<[u8; 3]>,
    memo: HashMap<[u8; 3], u8>,
}

impl Palette {
    /// Build a table for `colors`, falling back to `fallback`'s palette when
    /// the recording does not fit in 256 entries. Sorted by luminance so
    /// similar colours get adjacent indices, which helps LZW a little.
    pub(crate) fn build(colors: &HashSet<[u8; 3]>, fallback: &Theme) -> Self {
        let mut table: Vec<[u8; 3]> = if colors.len() <= MAX_ENTRIES {
            colors.iter().copied().collect()
        } else {
            // The theme palette with its last two entries swapped for the
            // default bg and fg, which dominate every frame.
            let mut spread = fallback.palette.to_vec();
            spread.truncate(MAX_ENTRIES - 2);
            spread.push(fallback.bg);
            spread.push(fallback.fg);
            spread
        };
        table.sort_unstable_by_key(|color| luminance(*color));
        // Distinct colours can share a luminance, so dedup by value.
        table.dedup();
        if table.is_empty() {
            // An empty global table is malformed GIF.
            table.push(fallback.bg);
        }
        Self {
            table,
            memo: HashMap::new(),
        }
    }

    /// The index of `color`, memoized per distinct colour.
    pub(crate) fn index(&mut self, color: [u8; 3]) -> u8 {
        if let Some(found) = self.memo.get(&color) {
            return *found;
        }
        let best = self
            .table
            .iter()
            .enumerate()
            .min_by_key(|(_, candidate)| distance(color, **candidate))
            .map_or(0, |(idx, _)| idx);
        // The table is capped at 256 entries, so this cannot saturate.
        let slot = u8::try_from(best).unwrap_or(u8::MAX);
        self.memo.insert(color, slot);
        slot
    }

    /// The table flattened to `[r, g, b, ...]`; `gif` pads it itself.
    pub(super) fn to_gif_bytes(&self) -> Vec<u8> {
        self.table.iter().flatten().copied().collect()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn exact_path_is_lossless_and_sorted_by_luminance() {
        let colors: HashSet<[u8; 3]> = (0..40_u8).map(|i| [i * 6, 255 - i * 6, i]).collect();
        let mut palette = Palette::build(&colors, &Theme::default());
        for color in &colors {
            let slot = palette.index(*color);
            assert_eq!(palette.table.get(slot as usize), Some(color));
        }
        let lums: Vec<u32> = palette.table.iter().map(|c| luminance(*c)).collect();
        assert!(lums.windows(2).all(|pair| pair[0] <= pair[1]), "{lums:?}");
    }

    #[test]
    fn fallback_path_keeps_fg_bg_and_maps_nearest() {
        let theme = Theme::default();
        // 300 distinct colours forces the fallback path.
        let colors: HashSet<[u8; 3]> = (0..300_u32)
            .map(|i| {
                [
                    u8::try_from(i / 256).unwrap_or(0),
                    u8::try_from(i % 256).unwrap_or(0),
                    0,
                ]
            })
            .collect();
        let mut palette = Palette::build(&colors, &theme);
        assert!(palette.table.contains(&theme.bg), "bg missing");
        assert!(palette.table.contains(&theme.fg), "fg missing");
        assert!(palette.table.len() <= MAX_ENTRIES, "table overflowed");
        let slot = palette.index([254, 254, 254]);
        let best = palette
            .table
            .iter()
            .copied()
            .min_by_key(|c| distance([254, 254, 254], *c))
            .expect("non-empty table");
        assert_eq!(palette.table[slot as usize], best);
    }
}
