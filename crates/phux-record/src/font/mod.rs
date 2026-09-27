//! Vendored 1-bit bitmap font and procedural glyph coverage.
//!
//! A 1-bit font (not an antialiasing rasterizer) is load-bearing (ADR-0060):
//! every pixel is the cell's fg or bg, so rendering adds no colours and GIF
//! palettes stay exact. The accepted cost is narrow coverage; CJK and emoji
//! render as tofu.
//!
//! `spleen_8x16` is generated from `assets/spleen-8x16.bdf` by
//! `scripts/gen-bitmap-font.py` and committed; there is no build-time codegen.
//! [`boxdraw`] draws box/block glyphs before the face and a small set of
//! missing symbols after it.

pub(crate) mod boxdraw;
mod spleen_8x16;

/// A monospaced 1-bit bitmap face.
///
/// `ranges` is sorted and non-overlapping: each entry maps an inclusive
/// codepoint range onto dense glyph bitmaps, one byte per pixel row, MSB
/// leftmost.
#[derive(Debug)]
pub(crate) struct BitmapFont {
    pub(crate) cell_w: u32,
    pub(crate) cell_h: u32,
    ranges: &'static [(u32, u32, &'static [[u8; 16]])],
}

impl BitmapFont {
    /// The bitmap for `ch`, or `None` when the face has no glyph for it.
    pub(crate) fn glyph(&self, ch: char) -> Option<&'static [u8; 16]> {
        let code = ch as u32;
        let idx = self
            .ranges
            .binary_search_by(|(start, end, _)| {
                if code < *start {
                    std::cmp::Ordering::Greater
                } else if code > *end {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .ok()?;
        let (start, _, bitmaps) = self.ranges.get(idx)?;
        bitmaps.get((code - start) as usize)
    }
}

/// The vendored face: Spleen 8x16, BSD-2-Clause, chosen for its box drawing
/// and Powerline coverage.
pub(crate) static SPLEEN_8X16: BitmapFont = BitmapFont {
    cell_w: 8,
    cell_h: 16,
    ranges: &spleen_8x16::RANGES,
};

/// Synthesize bold by OR-ing a pixel row with itself shifted one column.
/// Bold changes weight only, never colour, matching libghostty.
pub(crate) const fn bold_row(row: u8) -> u8 {
    row | (row >> 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_glyphs_are_exactly_what_the_face_lacks() {
        // A face upgrade that gains one of these must fail here rather than
        // silently prefer a hand-drawn approximation of a real glyph.
        for ch in (0..=0xffff_u32).filter_map(char::from_u32) {
            if boxdraw::covers_fallback(ch) {
                assert!(
                    SPLEEN_8X16.glyph(ch).is_none(),
                    "U+{:04X} is in the face; drop it from the fallback tier",
                    ch as u32
                );
            }
        }
    }

    #[test]
    fn ranges_are_sorted_and_non_overlapping() {
        // `glyph` binary-searches.
        let mut prev_end = None;
        for (start, end, bitmaps) in SPLEEN_8X16.ranges {
            assert!(start <= end);
            assert!(
                prev_end.is_none_or(|prev| *start > prev),
                "U+{start:04X} overlaps"
            );
            assert_eq!(bitmaps.len(), (end - start + 1) as usize, "U+{start:04X}");
            prev_end = Some(*end);
        }
    }
}
