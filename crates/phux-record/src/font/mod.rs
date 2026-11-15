//! Vendored 1-bit bitmap font and procedural glyph coverage.
//!
//! A 1-bit font (not an antialiasing rasterizer) is load-bearing (ADR-0060):
//! every pixel is the cell's fg or bg, so rendering adds no colours and GIF
//! palettes stay exact. The accepted cost is narrow coverage; CJK and emoji
//! render as tofu.
//!
//! The face is parsed from the vendored `assets/spleen-8x16.bdf` on first
//! use. [`boxdraw`] draws box/block glyphs before the face and a small set
//! of missing symbols after it.

use std::sync::LazyLock;

pub(crate) mod boxdraw;

/// Spleen 2.1.0 (BSD-2-Clause; its notice is in `/THIRD-PARTY-NOTICES.md`),
/// chosen for its box drawing and Powerline coverage. Every glyph is 8x16.
const SPLEEN_BDF: &str = include_str!("../../assets/spleen-8x16.bdf");

/// Holes up to this many codepoints between glyphs read as blank glyphs, not
/// as missing ones.
const MAX_GAP: u32 = 8;

/// A monospaced 1-bit bitmap face: glyphs sorted by codepoint, one byte per
/// pixel row, MSB leftmost.
#[derive(Debug)]
pub(crate) struct BitmapFont {
    pub(crate) cell_w: u32,
    pub(crate) cell_h: u32,
    glyphs: LazyLock<Vec<(u32, [u8; 16])>>,
}

impl BitmapFont {
    /// The bitmap for `ch`, or `None` when the face has no glyph for it.
    pub(crate) fn glyph(&self, ch: char) -> Option<&[u8; 16]> {
        let index = self
            .glyphs
            .binary_search_by_key(&(ch as u32), |(code, _)| *code)
            .ok()?;
        Some(&self.glyphs[index].1)
    }
}

pub(crate) static SPLEEN_8X16: BitmapFont = BitmapFont {
    cell_w: 8,
    cell_h: 16,
    glyphs: LazyLock::new(|| parse_bdf(SPLEEN_BDF)),
};

/// Every encoded glyph's `BITMAP` rows, with small holes padded blank.
fn parse_bdf(bdf: &str) -> Vec<(u32, [u8; 16])> {
    let mut parsed = Vec::new();
    let mut code = None;
    let mut lines = bdf.lines();
    while let Some(line) = lines.next() {
        if let Some(value) = line.strip_prefix("ENCODING ") {
            code = value.trim().parse::<u32>().ok();
        } else if line == "BITMAP" {
            let mut rows = [0; 16];
            for row in &mut rows {
                let hex = lines.next().unwrap_or_default().trim();
                *row = u8::from_str_radix(hex, 16).unwrap_or(0);
            }
            if let Some(code) = code.take() {
                parsed.push((code, rows));
            }
        }
    }
    parsed.sort_unstable_by_key(|(code, _)| *code);
    let mut glyphs: Vec<(u32, [u8; 16])> = Vec::with_capacity(parsed.len());
    for (code, rows) in parsed {
        if let Some(&(last, _)) = glyphs.last()
            && code - last - 1 <= MAX_GAP
        {
            glyphs.extend((last + 1..code).map(|blank| (blank, [0; 16])));
        }
        glyphs.push((code, rows));
    }
    glyphs
}

/// Synthesize bold by OR-ing a pixel row with itself shifted one column.
/// Bold changes weight only, never colour, matching libghostty.
pub(crate) const fn bold_row(row: u8) -> u8 {
    row | (row >> 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsed_face_matches_every_glyph_in_the_retired_generated_table() {
        assert_eq!(SPLEEN_8X16.glyphs.len(), 1_115);
        // FNV-1a over each big-endian codepoint and its 16 bitmap rows,
        // including the old generated table's small-gap blank glyphs.
        let mut digest = 0xcbf2_9ce4_8422_2325_u64;
        for (code, rows) in SPLEEN_8X16.glyphs.iter() {
            for byte in code.to_be_bytes().into_iter().chain(rows.iter().copied()) {
                digest = (digest ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        assert_eq!(digest, 0xe00f_982d_f211_a3dd);
    }

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
    fn the_face_matches_the_published_cell_size() {
        assert_eq!(SPLEEN_8X16.cell_w, crate::FACE_CELL_WIDTH_PX);
        assert_eq!(SPLEEN_8X16.cell_h, crate::FACE_CELL_HEIGHT_PX);
    }

    #[test]
    fn every_glyph_is_one_full_cell() {
        let cells: Vec<&str> = SPLEEN_BDF
            .lines()
            .filter(|line| line.starts_with("BBX ") || line.starts_with("FONTBOUNDINGBOX "))
            .collect();
        assert_eq!(cells.len(), 970);
        assert!(
            cells.iter().all(|line| line.ends_with(" 8 16 0 -4")),
            "{cells:?}"
        );
        assert_eq!(
            SPLEEN_8X16.glyph('A'),
            Some(&[
                0x00, 0x00, 0x7c, 0xc6, 0xc6, 0xc6, 0xfe, 0xc6, 0xc6, 0xc6, 0xc6, 0xc6, 0x00, 0x00,
                0x00, 0x00
            ])
        );
    }
}
