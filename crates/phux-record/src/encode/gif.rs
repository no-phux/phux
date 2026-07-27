//! `GIF89a` output: one global color table, sub-rectangle frames, infinite
//! loop.
//!
//! GIF is the shareable default because it renders inline in every browser,
//! every chat client, and GitHub's markdown — which APNG, for all its
//! technical advantages, still does not. The cost is a 256-entry palette and
//! a centisecond clock, and both are handled here rather than upstream.
//!
//! # One global table, never per-frame local tables
//!
//! A local color table per frame costs up to 768 bytes of table per frame and
//! makes viewers that re-map colors on a table change flicker visibly. The
//! palette is built once from the whole recording in the driver's first pass
//! precisely so this file never has to make that trade.
//!
//! # Delays are centiseconds, and the floor is 2
//!
//! GIF's Graphic Control Extension stores the delay as a `u16` in
//! hundredths of a second. Historically, browsers silently rewrite a delay of
//! 0 or 1 cs to 10 cs — a decades-old anti-abuse behaviour that is still
//! present — so asking for 1 cs does not give a fast animation, it gives a
//! *ten times slower* one. The honest floor is therefore 2 cs, and a 50 fps
//! export is silently a 50 fps export only in APNG. The ceiling is 500 cs
//! because a five-second hold already reads as "paused" and anything longer
//! reads as "broken".

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
pub struct GifWriter<W: Write> {
    /// `Option` only so [`GifWriter::finish`] can consume the encoder out of
    /// `self`; it is `Some` for the encoder's entire useful life.
    encoder: Option<::gif::Encoder<CountingWriter<W>>>,
    palette: Palette,
    count: Rc<Cell<u64>>,
    width: u32,
    height: u32,
    /// Scratch index buffer, reused across frames so memory stays O(one
    /// frame) rather than O(the animation).
    indices: Vec<u8>,
}

impl<W: Write> std::fmt::Debug for GifWriter<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GifWriter")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("table", &self.palette.table().len())
            .field("bytes", &self.count.get())
            .finish_non_exhaustive()
    }
}

impl<W: Write> GifWriter<W> {
    /// Open a `GIF89a` over `sink` with `palette` as the global color table.
    pub(super) fn new(
        sink: W,
        width: u32,
        height: u32,
        palette: Palette,
    ) -> Result<Self, RecordError> {
        // GIF's logical screen descriptor is two `u16`s. 65535 px is 8191
        // columns at this cell size, so hitting this means something upstream
        // computed a canvas from garbage dimensions.
        let screen_w = u16::try_from(width)
            .map_err(|_| RecordError::Encode(format!("gif canvas width {width} exceeds 65535")))?;
        let screen_h = u16::try_from(height).map_err(|_| {
            RecordError::Encode(format!("gif canvas height {height} exceeds 65535"))
        })?;
        if screen_w == 0 || screen_h == 0 {
            return Err(RecordError::Encode(format!(
                "gif canvas must be non-empty, got {width}x{height}"
            )));
        }
        let count = Rc::new(Cell::new(0));
        let table = palette.to_gif_bytes();
        let mut encoder = ::gif::Encoder::new(
            CountingWriter::new(sink, Rc::clone(&count)),
            screen_w,
            screen_h,
            &table,
        )
        .map_err(|err| encode_err("header", &err))?;
        // The Netscape 2.0 application extension. Without it a GIF plays once
        // and stops, which for a terminal recording reads as a broken file.
        encoder
            .set_repeat(::gif::Repeat::Infinite)
            .map_err(|err| encode_err("loop extension", &err))?;
        Ok(Self {
            encoder: Some(encoder),
            palette,
            count,
            width,
            height,
            indices: Vec::new(),
        })
    }

    /// Canvas size in pixels.
    pub(super) const fn canvas(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Bytes written to the sink so far.
    pub(super) fn bytes(&self) -> u64 {
        self.count.get()
    }

    /// Append one frame covering `rect`, held for `delay_ms`.
    pub(super) fn add_frame(
        &mut self,
        surface: &Surface,
        rect: Rect,
        delay_ms: u32,
    ) -> Result<u64, RecordError> {
        let (left, top, frame_w, frame_h) = geometry(rect)?;
        self.fill_indices(surface, rect);
        let encoder = self
            .encoder
            .as_mut()
            .ok_or_else(|| RecordError::Encode("gif encoder already finished".into()))?;
        let frame = ::gif::Frame {
            buffer: Cow::Borrowed(&self.indices),
            width: frame_w,
            height: frame_h,
            left,
            top,
            delay: delay_cs(delay_ms),
            // `Keep` leaves the previous frame standing, which is what makes a
            // sub-rectangle frame mean "these rows changed" rather than "the
            // rest of the screen is now background".
            dispose: ::gif::DisposalMethod::Keep,
            ..::gif::Frame::default()
        };
        encoder
            .write_frame(&frame)
            .map_err(|err| encode_err("frame", &err))?;
        Ok(self.bytes())
    }

    /// Write the trailer and return the total bytes.
    pub(super) fn finish(mut self) -> Result<u64, RecordError> {
        if let Some(encoder) = self.encoder.take() {
            // `into_inner` writes the `0x3b` trailer and hands the sink back,
            // which is the only way to flush it: the encoder's own `Drop`
            // would write the trailer but never flush a `BufWriter`.
            let mut sink = encoder
                .into_inner()
                .map_err(|err| RecordError::Encode(format!("gif trailer: {err}")))?;
            sink.flush()?;
        }
        Ok(self.bytes())
    }

    /// Translate `rect` of `surface` into palette indices.
    fn fill_indices(&mut self, surface: &Surface, rect: Rect) {
        self.indices.clear();
        self.indices
            .reserve((rect.w as usize).saturating_mul(rect.h as usize));
        for y in rect.y..rect.y.saturating_add(rect.h) {
            let row = (y as usize).saturating_mul(surface.width as usize);
            for x in rect.x..rect.x.saturating_add(rect.w) {
                // Bounds-checked rather than indexed: a rectangle that does
                // not fit must produce a black pixel, not a panic in the
                // middle of a user's export.
                let pixel = surface
                    .pixels
                    .get(row.saturating_add(x as usize))
                    .copied()
                    .unwrap_or([0, 0, 0]);
                self.indices.push(self.palette.index(pixel));
            }
        }
    }
}

/// GIF frame geometry as the four `u16`s the container stores.
fn geometry(rect: Rect) -> Result<(u16, u16, u16, u16), RecordError> {
    let too_big = |what: &str, value: u32| {
        RecordError::Encode(format!("gif frame {what} {value} exceeds 65535"))
    };
    Ok((
        u16::try_from(rect.x).map_err(|_| too_big("left", rect.x))?,
        u16::try_from(rect.y).map_err(|_| too_big("top", rect.y))?,
        u16::try_from(rect.w).map_err(|_| too_big("width", rect.w))?,
        u16::try_from(rect.h).map_err(|_| too_big("height", rect.h))?,
    ))
}

/// Milliseconds to GIF's centisecond delay, rounded to nearest and clamped.
///
/// See the module docs for why the floor is 2 and not 0 or 1.
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
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::raster::Theme;

    fn flat(width: u32, height: u32, color: [u8; 3]) -> Surface {
        Surface {
            width,
            height,
            pixels: vec![color; (width * height) as usize],
        }
    }

    fn palette_of(colors: &[[u8; 3]]) -> Palette {
        let set: HashSet<[u8; 3]> = colors.iter().copied().collect();
        Palette::build(&set, &Theme::default())
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    /// One decoded block header, for the structural assertions below.
    #[derive(Debug, PartialEq, Eq)]
    enum Block {
        /// A Graphic Control Extension's delay, in centiseconds.
        Delay(u16),
        /// An Image Descriptor's `(left, top, width, height)`.
        Image(u16, u16, u16, u16),
    }

    /// Walk the container's block structure instead of grepping for magic
    /// bytes. `0x2C` and `0x21` occur freely inside LZW payloads and color
    /// tables, so a substring search finds a real descriptor only by luck.
    fn blocks(bytes: &[u8]) -> Vec<Block> {
        let table_len = |packed: u8| {
            if packed & 0x80 == 0 {
                0
            } else {
                3 * (1_usize << ((packed & 0x07) + 1))
            }
        };
        let read16 = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        // Skip signature (6) plus the logical screen descriptor (7), then the
        // global color table its packed byte describes.
        let mut at = 13 + table_len(bytes[10]);
        let mut out = Vec::new();
        // Skip a chain of length-prefixed data sub-blocks, terminated by 0.
        let skip_sub_blocks = |mut at: usize| {
            while at < bytes.len() && bytes[at] != 0 {
                at += 1 + bytes[at] as usize;
            }
            at + 1
        };
        while at < bytes.len() && bytes[at] != 0x3B {
            match bytes[at] {
                0x21 => {
                    let label = bytes[at + 1];
                    if label == 0xF9 {
                        // Graphic Control Extension: one 4-byte sub-block of
                        // flags, delay (LE u16), transparent index.
                        out.push(Block::Delay(read16(at + 4)));
                    }
                    at = skip_sub_blocks(at + 2);
                }
                0x2C => {
                    out.push(Block::Image(
                        read16(at + 1),
                        read16(at + 3),
                        read16(at + 5),
                        read16(at + 7),
                    ));
                    let packed = bytes[at + 9];
                    // 9 descriptor bytes, the optional local table, then the
                    // LZW minimum code size byte.
                    at = skip_sub_blocks(at + 10 + table_len(packed) + 1);
                }
                other => panic!("unexpected GIF block 0x{other:02x} at {at}"),
            }
        }
        out
    }

    fn images(bytes: &[u8]) -> Vec<Block> {
        blocks(bytes)
            .into_iter()
            .filter(|block| matches!(block, Block::Image(..)))
            .collect()
    }

    /// One full-canvas frame at `delay_ms`, plus the raw bytes.
    fn render_one(delay_ms: u32) -> Vec<u8> {
        let surface = flat(8, 16, [10, 20, 30]);
        let mut sink: Vec<u8> = Vec::new();
        {
            let mut enc = GifWriter::new(&mut sink, 8, 16, palette_of(&[[10, 20, 30]]))
                .expect("encoder opens");
            enc.add_frame(&surface, Rect::whole(&surface), delay_ms)
                .expect("frame");
            enc.finish().expect("finish");
        }
        sink
    }

    fn first_delay_cs(bytes: &[u8]) -> u16 {
        blocks(bytes)
            .into_iter()
            .find_map(|block| match block {
                Block::Delay(cs) => Some(cs),
                Block::Image(..) => None,
            })
            .expect("graphic control extension")
    }

    #[test]
    fn output_starts_with_the_gif89a_signature() {
        let out = render_one(100);
        assert_eq!(out.get(..6), Some(&b"GIF89a"[..]));
    }

    #[test]
    fn output_contains_the_netscape_looping_extension() {
        let out = render_one(100);
        assert!(
            find(&out, b"NETSCAPE2.0").is_some(),
            "a GIF without the loop extension plays once and stops"
        );
    }

    #[test]
    fn output_ends_with_the_trailer() {
        let out = render_one(100);
        assert_eq!(out.last(), Some(&0x3b), "missing GIF trailer byte");
    }

    /// The one assertion that actually proves the container is well formed.
    ///
    /// Structural greps confirm the bytes we meant to write are present; only
    /// a decoder confirms the LZW stream, its code-width growth, and the
    /// 255-byte sub-block chunking are all valid — which is precisely the bug
    /// class that yields a file playing in one viewer and breaking in another.
    #[test]
    fn pixels_survive_a_round_trip_through_the_decoder() {
        let ink = [200, 30, 40];
        let paper = [12, 12, 12];
        let mut surface = flat(8, 4, paper);
        // A recognisable pattern, so a decode that silently returns zeroes
        // fails rather than passing on an all-background image.
        for x in 0..8_usize {
            surface.pixels[x] = ink;
            surface.pixels[3 * 8 + x] = ink;
        }
        let mut sink: Vec<u8> = Vec::new();
        {
            let mut enc =
                GifWriter::new(&mut sink, 8, 4, palette_of(&[ink, paper])).expect("opens");
            enc.add_frame(&surface, Rect::whole(&surface), 100)
                .expect("frame");
            enc.finish().expect("finish");
        }

        let mut decoder = ::gif::Decoder::new(sink.as_slice()).expect("decodes");
        assert_eq!(decoder.width(), 8);
        assert_eq!(decoder.height(), 4);
        let table = decoder
            .global_palette()
            .expect("a global color table")
            .to_vec();
        let frame = decoder
            .read_next_frame()
            .expect("frame decodes")
            .expect("one frame present");
        assert_eq!(frame.buffer.len(), 8 * 4);
        let color_at = |i: usize| {
            let slot = frame.buffer[i] as usize * 3;
            [table[slot], table[slot + 1], table[slot + 2]]
        };
        assert_eq!(color_at(0), ink, "top-left pixel");
        assert_eq!(color_at(8), paper, "second row is background");
        assert_eq!(color_at(3 * 8 + 7), ink, "bottom-right pixel");
    }

    #[test]
    fn delay_is_clamped_to_at_least_two_centiseconds() {
        assert_eq!(delay_cs(0), MIN_DELAY_CS);
        assert_eq!(delay_cs(1), MIN_DELAY_CS);
        assert_eq!(delay_cs(19), MIN_DELAY_CS);
        assert_eq!(first_delay_cs(&render_one(5)), MIN_DELAY_CS);
    }

    #[test]
    fn delay_is_clamped_to_at_most_five_hundred_centiseconds() {
        assert_eq!(delay_cs(60_000), MAX_DELAY_CS);
        assert_eq!(delay_cs(u32::MAX), MAX_DELAY_CS);
        assert_eq!(first_delay_cs(&render_one(9_000)), MAX_DELAY_CS);
    }

    #[test]
    fn delay_rounds_to_the_nearest_centisecond() {
        assert_eq!(delay_cs(100), 10);
        assert_eq!(delay_cs(104), 10);
        assert_eq!(delay_cs(105), 11);
        assert_eq!(delay_cs(250), 25);
    }

    #[test]
    fn subrectangle_frame_records_its_left_and_top() {
        let surface = flat(16, 32, [1, 2, 3]);
        let mut sink: Vec<u8> = Vec::new();
        {
            let mut enc =
                GifWriter::new(&mut sink, 16, 32, palette_of(&[[1, 2, 3]])).expect("opens");
            enc.add_frame(&surface, Rect::whole(&surface), 100).unwrap();
            enc.add_frame(
                &surface,
                Rect {
                    x: 8,
                    y: 16,
                    w: 8,
                    h: 16,
                },
                100,
            )
            .unwrap();
            enc.finish().unwrap();
        }
        let descriptors = images(&sink);
        assert_eq!(descriptors.len(), 2, "{descriptors:?}");
        assert_eq!(descriptors[0], Block::Image(0, 0, 16, 32));
        assert_eq!(descriptors[1], Block::Image(8, 16, 8, 16));
    }

    #[test]
    fn a_zero_sized_canvas_is_an_error_not_a_corrupt_file() {
        let mut sink: Vec<u8> = Vec::new();
        let err = GifWriter::new(&mut sink, 0, 16, palette_of(&[[0, 0, 0]]));
        assert!(err.is_err());
    }
}
