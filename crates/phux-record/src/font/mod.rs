//! Vendored 1-bit bitmap font and procedural glyph coverage.
//!
//! The exported animation is drawn with a **1-bit** font, not an antialiasing
//! TTF rasterizer, and that is a load-bearing decision rather than a shortcut
//! (ADR-0060). Because every pixel is either the cell's resolved foreground
//! or its resolved background, rendering introduces *no new colors*: the
//! distinct-color count of a whole recording stays under 256 for essentially
//! every real session, so GIF quantization degenerates to an exact-palette
//! hash map with no dithering and no quality loss. An antialiased rasterizer
//! blends fg into bg at 256 coverage levels and spawns up to 256 intermediate
//! colors *per (fg, bg) pair*, which forces real quantization, dithering, and
//! far worse LZW compression.
//!
//! The accepted consequence is narrow coverage: Latin, Greek, Cyrillic, box
//! drawing, Braille, and Powerline. CJK and other wide glyphs, and color
//! emoji, render as tofu boxes.
//!
//! # Layout
//!
//! * `spleen_8x16` — the generated glyph table, committed, produced from
//!   `crates/phux-record/assets/spleen-8x16.bdf` by
//!   `scripts/gen-bitmap-font.py`. The script is reproducibility
//!   documentation; there is no `build.rs` and no codegen at build time.
//! * [`boxdraw`] — procedural glyphs, in two tiers. Line, block, and shade
//!   glyphs for `U+2500..=U+259F` are tried *before* the bitmap table because
//!   they are pixel-exact at any cell size, and because those codepoints are
//!   the bulk of what a TUI (phux's own dividers and status bar included)
//!   paints. A second, curated tier — prompt chevrons and arrows, check
//!   marks and crosses, the sideways arrowheads — is tried *after* the table,
//!   and only fills in what Spleen genuinely lacks.

pub mod boxdraw;
mod spleen_8x16;

/// A monospaced 1-bit bitmap face.
///
/// `ranges` is sorted by `start` and non-overlapping: each entry maps an
/// inclusive codepoint range onto a dense slice of glyph bitmaps, one byte
/// per pixel row with the most significant bit at the leftmost of the eight
/// columns.
#[derive(Debug)]
pub struct BitmapFont {
    /// Glyph width in pixels.
    pub cell_w: u32,
    /// Glyph height in pixels.
    pub cell_h: u32,
    /// Sorted, non-overlapping `(start, end, bitmaps)` groups.
    pub ranges: &'static [(u32, u32, &'static [[u8; 16]])],
}

impl BitmapFont {
    /// The bitmap for `ch`, or `None` when the face has no glyph for it.
    ///
    /// `None` must render as a tofu box, never as a blank: a blank silently
    /// corrupts visual column alignment, while a box tells the reader exactly
    /// what happened.
    #[must_use]
    pub fn glyph(&self, ch: char) -> Option<&'static [u8; 16]> {
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

/// The vendored face: Spleen 8x16, BSD-2-Clause.
///
/// Chosen for its real box drawing *and* Powerline coverage — modern prompts
/// are full of U+E0B0 and friends, and tofu in the first user's demo is the
/// fastest way to make this feature look broken.
pub static SPLEEN_8X16: BitmapFont = BitmapFont {
    cell_w: 8,
    cell_h: 16,
    ranges: &spleen_8x16::RANGES,
};

/// Synthesize bold by OR-ing a pixel row with itself shifted one column.
///
/// One instruction, and it reads correctly at 8x16. Bold must select the
/// heavier bitmap and *nothing else*: libghostty deliberately does not apply
/// bold color handling, so brightening the foreground here would make phux's
/// exports disagree with phux's own glass.
#[must_use]
pub const fn bold_row(row: u8) -> u8 {
    row | (row >> 1)
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
    fn glyph_lookup_finds_ascii_letters() {
        // 'A' must be present and must not be blank: an all-zero bitmap here
        // would mean the BDF parse silently produced empty glyphs.
        let glyph = SPLEEN_8X16.glyph('A').expect("ASCII 'A' is in the face");
        assert!(glyph.iter().any(|row| *row != 0), "'A' rasterizes to blank");
        let space = SPLEEN_8X16.glyph(' ').expect("space is in the face");
        assert!(space.iter().all(|row| *row == 0), "space is not blank");
    }

    #[test]
    fn glyph_lookup_returns_none_for_unmapped_cjk() {
        // The accepted coverage limit. `None` is what makes the rasterizer
        // draw a tofu box instead of silently eating the column.
        assert!(SPLEEN_8X16.glyph('\u{6f22}').is_none());
        assert!(SPLEEN_8X16.glyph('\u{1f600}').is_none());
    }

    #[test]
    fn bold_row_widens_a_single_pixel_run() {
        assert_eq!(bold_row(0b0001_0000), 0b0001_1000);
        assert_eq!(bold_row(0b0000_0000), 0b0000_0000);
        // The rightmost column cannot bleed out of the cell: the shift is
        // toward the low bit, which is the rightmost pixel, so a run already
        // touching column 7 simply stays put.
        assert_eq!(bold_row(0b0000_0001), 0b0000_0001);
    }

    #[test]
    fn powerline_e0b0_is_present() {
        // The whole reason for choosing Spleen over a Latin-only face:
        // starship / powerlevel10k prompts are full of these, and tofu in the
        // first demo GIF is the fastest way to make the feature look broken.
        for ch in ['\u{e0b0}', '\u{e0b1}', '\u{e0b2}', '\u{e0b3}'] {
            let glyph = SPLEEN_8X16
                .glyph(ch)
                .unwrap_or_else(|| panic!("U+{:04X} missing from the face", ch as u32));
            assert!(
                glyph.iter().any(|row| *row != 0),
                "U+{:04X} rasterizes to blank",
                ch as u32
            );
        }
    }

    #[test]
    fn fallback_glyphs_are_exactly_what_the_face_lacks() {
        // The fallback tier is a fallback, not an override: every codepoint
        // it claims must be one Spleen has no bitmap for. Asserted against
        // the real table so that swapping the face — or regenerating it from
        // a newer BDF that gained a check mark — fails here rather than
        // silently preferring a hand-drawn approximation of a real glyph.
        for code in 0..=0xffff_u32 {
            let Some(ch) = char::from_u32(code) else {
                continue;
            };
            if boxdraw::covers_fallback(ch) {
                assert!(
                    SPLEEN_8X16.glyph(ch).is_none(),
                    "U+{code:04X} is in the face; drop it from the fallback tier"
                );
            }
        }
    }

    #[test]
    fn the_symbols_the_fallback_tier_exists_for_are_still_missing_from_the_face() {
        // The evidence the tier was built on, kept executable. These turn up
        // in ordinary sessions — the starship prompt chevron, and the status
        // marks every test runner and linter prints — and each rendered as a
        // tofu box before the tier existed.
        for ch in ['\u{276f}', '\u{276e}', '\u{279c}', '\u{2713}', '\u{2717}'] {
            assert!(
                SPLEEN_8X16.glyph(ch).is_none(),
                "U+{:04X} is now in the face",
                ch as u32
            );
            assert!(
                boxdraw::covers_fallback(ch),
                "U+{:04X} would render as tofu",
                ch as u32
            );
        }
    }

    #[test]
    fn ranges_are_sorted_and_non_overlapping() {
        // `glyph` binary-searches, so a generator that ever emitted an
        // unsorted or overlapping table would return wrong glyphs rather than
        // failing loudly. Assert the invariant the search depends on.
        let mut prev_end = None;
        for (start, end, bitmaps) in SPLEEN_8X16.ranges {
            assert!(start <= end);
            if let Some(prev) = prev_end {
                assert!(
                    *start > prev,
                    "range at U+{start:04X} overlaps its predecessor"
                );
            }
            assert_eq!(
                bitmaps.len(),
                (end - start + 1) as usize,
                "range U+{start:04X}..=U+{end:04X} has the wrong bitmap count"
            );
            prev_end = Some(*end);
        }
    }
}
