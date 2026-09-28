//! `GIF89a` output: one global colour table, sub-rectangle frames, infinite
//! loop. GIF is the default because it renders inline everywhere.
//!
//! One global table (built by the driver's first pass), never per-frame local
//! tables, which cost bytes and make some viewers flicker on table changes.
//!
//! Delays are centiseconds clamped to `2..=500`: browsers rewrite 0 or 1 cs
//! to 10 cs, so asking for 1 cs yields a ten times slower animation.

use std::borrow::Cow;
use std::cell::Cell;
use std::io::Write;
use std::rc::Rc;

use super::palette::Palette;
use super::{CountingWriter, Rect};
use crate::error::RecordError;
use crate::raster::Surface;

/// Smallest delay a browser will honour rather than rewrite, in centiseconds.
const MIN_DELAY_CS: u16 = 2;
/// Largest delay worth emitting, in centiseconds.
const MAX_DELAY_CS: u16 = 500;

/// Wraps `gif`'s encoder with byte accounting and index-buffer reuse.
pub(crate) struct GifWriter<W: Write> {
    encoder: ::gif::Encoder<CountingWriter<W>>,
    palette: Palette,
    pub(super) count: Rc<Cell<u64>>,
    /// Scratch index buffer reused across frames.
    indices: Vec<u8>,
}

impl<W: Write> GifWriter<W> {
    /// Open a `GIF89a` over `sink` with `palette` as the global colour table.
    pub(super) fn new(
        sink: W,
        width: u32,
        height: u32,
        palette: Palette,
    ) -> Result<Self, RecordError> {
        let (screen_w, screen_h) = match (u16::try_from(width), u16::try_from(height)) {
            (Ok(w), Ok(h)) if w != 0 && h != 0 => (w, h),
            _ => {
                return Err(RecordError::Encode(format!(
                    "gif canvas must be non-empty and fit u16, got {width}x{height}"
                )));
            }
        };
        let count = Rc::new(Cell::new(0));
        let mut encoder = ::gif::Encoder::new(
            CountingWriter::new(sink, Rc::clone(&count)),
            screen_w,
            screen_h,
            &palette.to_gif_bytes(),
        )
        .map_err(|err| encode_err("header", &err))?;
        // The Netscape 2.0 loop extension; without it a GIF plays once.
        encoder
            .set_repeat(::gif::Repeat::Infinite)
            .map_err(|err| encode_err("loop extension", &err))?;
        Ok(Self {
            encoder,
            palette,
            count,
            indices: Vec::new(),
        })
    }

    /// Append one frame covering `rect`, held for `delay_ms`.
    pub(super) fn add_frame(
        &mut self,
        surface: &Surface,
        rect: Rect,
        delay_ms: u32,
    ) -> Result<(), RecordError> {
        let too_big = |_| RecordError::Encode(format!("gif frame {rect:?} exceeds 65535"));
        let (left, top) = (
            u16::try_from(rect.x).map_err(too_big)?,
            u16::try_from(rect.y).map_err(too_big)?,
        );
        let (width, height) = (
            u16::try_from(rect.w).map_err(too_big)?,
            u16::try_from(rect.h).map_err(too_big)?,
        );
        self.indices.clear();
        self.indices
            .extend(rect.pixels(surface).map(|pixel| self.palette.index(pixel)));
        let frame = ::gif::Frame {
            buffer: Cow::Borrowed(&self.indices),
            width,
            height,
            left,
            top,
            delay: delay_cs(delay_ms),
            // `Keep` leaves the previous frame standing, so a sub-rectangle
            // means "these rows changed".
            dispose: ::gif::DisposalMethod::Keep,
            ..::gif::Frame::default()
        };
        self.encoder
            .write_frame(&frame)
            .map_err(|err| encode_err("frame", &err))
    }

    /// Write the trailer, flush, and return the total bytes.
    pub(super) fn finish(self) -> Result<u64, RecordError> {
        // `into_inner` writes the trailer and returns the sink so it can be
        // flushed; the encoder's `Drop` never flushes.
        let mut sink = self
            .encoder
            .into_inner()
            .map_err(|err| RecordError::Encode(format!("gif trailer: {err}")))?;
        sink.flush()?;
        Ok(self.count.get())
    }
}

/// Milliseconds to GIF's centisecond delay, rounded to nearest and clamped.
fn delay_cs(delay_ms: u32) -> u16 {
    let cs = delay_ms.saturating_add(5) / 10;
    u16::try_from(cs)
        .unwrap_or(u16::MAX)
        .clamp(MIN_DELAY_CS, MAX_DELAY_CS)
}

/// Wrap a `gif` failure with the step that produced it.
fn encode_err(what: &str, err: &::gif::EncodingError) -> RecordError {
    RecordError::Encode(format!("gif {what}: {err}"))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::raster::Theme;

    fn encode(surface: &Surface, frames: &[(Rect, u32)], colors: &[[u8; 3]]) -> Vec<u8> {
        let set: HashSet<[u8; 3]> = colors.iter().copied().collect();
        let palette = Palette::build(&set, &Theme::default());
        let mut sink: Vec<u8> = Vec::new();
        let mut enc =
            GifWriter::new(&mut sink, surface.width, surface.height, palette).expect("opens");
        for (rect, delay_ms) in frames {
            enc.add_frame(surface, *rect, *delay_ms).expect("frame");
        }
        enc.finish().expect("finish");
        sink
    }

    /// Only a decoder proves the LZW stream and sub-block chunking are valid.
    #[test]
    fn frames_survive_a_round_trip_through_the_decoder() {
        let (ink, paper) = ([200, 30, 40], [12, 12, 12]);
        let mut surface = Surface {
            width: 8,
            height: 4,
            pixels: vec![paper; 32],
        };
        for x in 0..8_usize {
            surface.pixels[x] = ink;
            surface.pixels[3 * 8 + x] = ink;
        }
        let band = Rect {
            x: 0,
            y: 2,
            w: 8,
            h: 2,
        };
        let bytes = encode(
            &surface,
            &[(Rect::whole(&surface), 100), (band, 9_000)],
            &[ink, paper],
        );

        let mut decoder = ::gif::DecodeOptions::new()
            .read_info(bytes.as_slice())
            .expect("decodes");
        let table = decoder.global_palette().expect("global table").to_vec();
        let first = decoder
            .read_next_frame()
            .expect("decodes")
            .expect("frame 1");
        assert_eq!((first.width, first.height, first.delay), (8, 4, 10));
        let color_at = |i: usize| {
            let slot = first.buffer[i] as usize * 3;
            [table[slot], table[slot + 1], table[slot + 2]]
        };
        assert_eq!(color_at(0), ink);
        assert_eq!(color_at(8), paper);
        assert_eq!(color_at(3 * 8 + 7), ink);
        let second = decoder
            .read_next_frame()
            .expect("decodes")
            .expect("frame 2");
        assert_eq!(
            (second.left, second.top, second.width, second.height),
            (0, 2, 8, 2)
        );
        assert_eq!(second.delay, MAX_DELAY_CS);
    }

    #[test]
    fn delay_rounds_to_centiseconds_within_the_browser_safe_range() {
        let cases = [
            (0, 2),
            (19, 2),
            (100, 10),
            (104, 10),
            (105, 11),
            (60_000, 500),
            (u32::MAX, 500),
        ];
        for (ms, cs) in cases {
            assert_eq!(delay_cs(ms), cs, "{ms} ms");
        }
    }

    #[test]
    fn a_zero_sized_canvas_is_an_error_not_a_corrupt_file() {
        let palette = Palette::build(&HashSet::new(), &Theme::default());
        assert!(GifWriter::new(Vec::new(), 0, 16, palette).is_err());
    }
}
