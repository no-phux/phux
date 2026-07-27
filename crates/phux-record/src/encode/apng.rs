//! Animated PNG output.
//!
//! This is the reference backend and the one to reach for when a GIF looks
//! wrong. APNG is truecolor, so there is no palette, no quantization, and no
//! color that can be approximated away; and its per-frame delay is a
//! rational, so the export's millisecond timebase survives to the file
//! instead of being rounded into centiseconds. Everything that can go wrong
//! here is a bug in the replay or raster stages, which is exactly why it was
//! built before the GIF backend rather than after it.
//!
//! # Two constraints the `png` crate imposes
//!
//! 1. `acTL` carries the frame count, and `png` wants it before the header
//!    goes out. That is the reason the render driver is two-pass at all.
//! 2. The first animation frame is written as `IDAT`, not `fdAT`, and an
//!    `IDAT` must cover the whole canvas. A sub-rectangle first frame would
//!    produce a PNG whose pixel data is smaller than its `IHDR` claims, so
//!    the first frame is forced full-canvas here rather than trusting every
//!    future caller to remember.

use std::cell::Cell;
use std::io::Write;
use std::rc::Rc;

use png::{BitDepth, BlendOp, ColorType, DisposeOp};

use super::{CountingWriter, Rect, copy_rect_rgb};
use crate::error::RecordError;
use crate::raster::Surface;

/// APNG's `fcTL` delay denominator. Fixed at 1000 so the numerator is
/// literally milliseconds and the timebase needs no conversion at all.
const DELAY_DEN: u16 = 1000;

/// Wraps `png`'s animated writer with byte accounting and frame clamping.
pub struct ApngWriter<W: Write> {
    /// `Option` only so [`ApngWriter::finish`] can consume the writer out of
    /// `self`; it is `Some` for the encoder's entire useful life.
    writer: Option<png::Writer<CountingWriter<W>>>,
    count: Rc<Cell<u64>>,
    width: u32,
    height: u32,
    /// What `acTL` promised.
    declared: u32,
    /// What has actually been written.
    written: u32,
    /// Scratch RGB buffer, reused across frames so memory stays O(one frame).
    rgb: Vec<u8>,
}

impl<W: Write> std::fmt::Debug for ApngWriter<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApngWriter")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("declared", &self.declared)
            .field("written", &self.written)
            .field("bytes", &self.count.get())
            .finish_non_exhaustive()
    }
}

impl<W: Write> ApngWriter<W> {
    /// Open an APNG over `sink` for exactly `frames` frames.
    pub(super) fn new(sink: W, width: u32, height: u32, frames: u32) -> Result<Self, RecordError> {
        if width == 0 || height == 0 {
            return Err(RecordError::Encode(format!(
                "apng canvas must be non-empty, got {width}x{height}"
            )));
        }
        // `set_animated` rejects zero, and a recording always has at least the
        // opening frame (the replayer's first sample never reports clean), so
        // a zero here means a caller miscounted rather than an empty film.
        let declared = frames.max(1);
        let count = Rc::new(Cell::new(0));
        let mut encoder =
            png::Encoder::new(CountingWriter::new(sink, Rc::clone(&count)), width, height);
        encoder.set_color(ColorType::Rgb);
        encoder.set_depth(BitDepth::Eight);
        // 0 plays means loop forever, matching GIF's Netscape extension.
        encoder
            .set_animated(declared, 0)
            .map_err(|err| encode_err("acTL", &err))?;
        let writer = encoder
            .write_header()
            .map_err(|err| encode_err("png header", &err))?;
        Ok(Self {
            writer: Some(writer),
            count,
            width,
            height,
            declared,
            written: 0,
            rgb: Vec::new(),
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
        if self.written >= self.declared {
            // Writing past `acTL`'s count makes `png` silently switch back to
            // plain `IDAT` chunks, which produces a file that decodes as a
            // still image with garbage appended. Refusing the extra frame
            // keeps the animation short but valid; the two-pass driver makes
            // this unreachable, and it stays here as a guard rather than as a
            // behaviour anyone should rely on.
            return Ok(self.bytes());
        }
        // Constraint 2 from the module docs: the first frame rides in `IDAT`
        // and `IDAT` is whole-canvas by definition.
        let rect = if self.written == 0 {
            Rect::whole(surface)
        } else {
            rect
        };
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| RecordError::Encode("apng encoder already finished".into()))?;

        // Reset before setting: `set_frame_position` validates against the
        // *current* dimension and `set_frame_dimension` against the *current*
        // offset, so a shrinking rectangle rejects itself unless the previous
        // frame's geometry is cleared first.
        writer
            .reset_frame_position()
            .map_err(|err| encode_err("frame reset", &err))?;
        writer
            .set_frame_dimension(rect.w, rect.h)
            .map_err(|err| encode_err("frame dimension", &err))?;
        writer
            .set_frame_position(rect.x, rect.y)
            .map_err(|err| encode_err("frame position", &err))?;
        // Milliseconds straight into the numerator; see `DELAY_DEN`. A delay
        // beyond ~65 s is a recording artifact rather than an intention, and
        // saturating is friendlier than wrapping to a few milliseconds.
        let delay = u16::try_from(delay_ms).unwrap_or(u16::MAX);
        writer
            .set_frame_delay(delay, DELAY_DEN)
            .map_err(|err| encode_err("frame delay", &err))?;
        // `Source` over `None` disposal: each frame overwrites its rectangle
        // outright and leaves the rest of the canvas standing, which is the
        // sub-rectangle model the driver emits. `Over` would alpha-composite,
        // and with opaque RGB that is the same picture for more work.
        writer
            .set_dispose_op(DisposeOp::None)
            .map_err(|err| encode_err("dispose op", &err))?;
        writer
            .set_blend_op(BlendOp::Source)
            .map_err(|err| encode_err("blend op", &err))?;

        copy_rect_rgb(surface, rect, &mut self.rgb);
        writer
            .write_image_data(&self.rgb)
            .map_err(|err| encode_err("frame data", &err))?;
        self.written = self.written.saturating_add(1);
        Ok(self.bytes())
    }

    /// Write `IEND` and return the total bytes.
    pub(super) fn finish(mut self) -> Result<u64, RecordError> {
        if let Some(writer) = self.writer.take() {
            // Sequence validation is off by default in `png`, which is what we
            // want: a `--max-bytes` truncation writes fewer frames than `acTL`
            // promised, and closing that file cleanly beats erroring out on a
            // recording the user can still open.
            writer
                .finish()
                .map_err(|err| encode_err("png finish", &err))?;
        }
        Ok(self.bytes())
    }
}

/// Wrap a `png` failure with the step that produced it.
fn encode_err(what: &str, err: &png::EncodingError) -> RecordError {
    RecordError::Encode(format!("apng {what}: {err}"))
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

    const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

    fn flat(width: u32, height: u32, color: [u8; 3]) -> Surface {
        Surface {
            width,
            height,
            pixels: vec![color; (width * height) as usize],
        }
    }

    /// Byte offset of the first occurrence of a chunk type tag.
    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    /// Two full-canvas frames into a buffer the caller can still inspect —
    /// `&mut Vec<u8>` implements `Write`, so the encoder can consume a sink
    /// that outlives it.
    fn render_two_frames(delay_ms: u32) -> Vec<u8> {
        let first = flat(8, 16, [10, 20, 30]);
        let second = flat(8, 16, [40, 50, 60]);
        let mut sink: Vec<u8> = Vec::new();
        {
            let mut enc = ApngWriter::new(&mut sink, 8, 16, 2).expect("encoder opens");
            enc.add_frame(&first, Rect::whole(&first), delay_ms)
                .expect("first frame");
            enc.add_frame(&second, Rect::whole(&second), delay_ms)
                .expect("second frame");
            enc.finish().expect("finish");
        }
        sink
    }

    #[test]
    fn output_is_a_valid_png_signature() {
        let out = render_two_frames(100);
        assert_eq!(out.get(..8), Some(&PNG_SIGNATURE[..]));
    }

    #[test]
    fn actl_chunk_precedes_the_first_fctl() {
        let out = render_two_frames(100);
        let actl = find(&out, b"acTL").expect("acTL present");
        let fctl = find(&out, b"fcTL").expect("fcTL present");
        assert!(
            actl < fctl,
            "acTL at {actl} must precede the first fcTL at {fctl}"
        );
    }

    #[test]
    fn frame_delay_is_written_as_a_rational_over_1000() {
        let out = render_two_frames(250);
        let fctl = find(&out, b"fcTL").expect("fcTL present");
        // fcTL payload: sequence(4) width(4) height(4) x(4) y(4) num(2) den(2)
        let payload = fctl + 4;
        let num = u16::from_be_bytes([out[payload + 20], out[payload + 21]]);
        let den = u16::from_be_bytes([out[payload + 22], out[payload + 23]]);
        assert_eq!(den, 1000, "denominator must be milliseconds");
        assert_eq!(num, 250, "numerator is the delay verbatim");
    }

    #[test]
    fn a_subrectangle_frame_records_its_offset() {
        let surface = flat(16, 32, [1, 2, 3]);
        let mut sink: Vec<u8> = Vec::new();
        {
            let mut enc = ApngWriter::new(&mut sink, 16, 32, 2).expect("encoder opens");
            enc.add_frame(&surface, Rect::whole(&surface), 50).unwrap();
            enc.add_frame(
                &surface,
                Rect {
                    x: 0,
                    y: 16,
                    w: 16,
                    h: 16,
                },
                50,
            )
            .unwrap();
            enc.finish().unwrap();
        }
        // The second fcTL carries the sub-rectangle.
        let first = find(&sink, b"fcTL").expect("first fcTL");
        let rest = &sink[first + 4..];
        let second = find(rest, b"fcTL").expect("second fcTL") + first + 4;
        let payload = second + 4;
        let height = u32::from_be_bytes([
            sink[payload + 8],
            sink[payload + 9],
            sink[payload + 10],
            sink[payload + 11],
        ]);
        let y = u32::from_be_bytes([
            sink[payload + 16],
            sink[payload + 17],
            sink[payload + 18],
            sink[payload + 19],
        ]);
        assert_eq!(height, 16);
        assert_eq!(y, 16);
    }

    #[test]
    fn first_frame_is_forced_full_canvas() {
        // An IDAT smaller than IHDR is a corrupt PNG; the encoder must widen
        // a sub-rectangle first frame rather than honour it.
        let surface = flat(16, 32, [7, 7, 7]);
        let mut sink: Vec<u8> = Vec::new();
        {
            let mut enc = ApngWriter::new(&mut sink, 16, 32, 1).expect("encoder opens");
            enc.add_frame(
                &surface,
                Rect {
                    x: 0,
                    y: 16,
                    w: 16,
                    h: 16,
                },
                50,
            )
            .expect("first frame");
            enc.finish().expect("finish");
        }
        let fctl = find(&sink, b"fcTL").expect("fcTL present");
        let payload = fctl + 4;
        let height = u32::from_be_bytes([
            sink[payload + 8],
            sink[payload + 9],
            sink[payload + 10],
            sink[payload + 11],
        ]);
        assert_eq!(height, 32, "first frame must cover the canvas");
    }

    #[test]
    fn frames_beyond_the_declared_count_are_refused_not_appended() {
        let surface = flat(8, 16, [3, 3, 3]);
        let mut sink: Vec<u8> = Vec::new();
        let before;
        {
            let mut enc = ApngWriter::new(&mut sink, 8, 16, 1).expect("encoder opens");
            enc.add_frame(&surface, Rect::whole(&surface), 100).unwrap();
            before = enc.bytes();
            let after = enc.add_frame(&surface, Rect::whole(&surface), 100).unwrap();
            assert_eq!(before, after, "extra frame must not write bytes");
            enc.finish().unwrap();
        }
        assert!(!sink.is_empty());
    }
}
