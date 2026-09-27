//! Animated PNG output: truecolor with millisecond delays, the reference
//! backend when a GIF looks wrong.
//!
//! Two `png` constraints shape it: `acTL` needs the frame count before the
//! header (why the driver is two-pass), and the first frame rides in `IDAT`,
//! which must cover the whole canvas.

use std::cell::Cell;
use std::io::Write;
use std::rc::Rc;

use png::{BitDepth, BlendOp, ColorType, DisposeOp};

use super::{CountingWriter, Rect};
use crate::error::RecordError;
use crate::raster::Surface;

/// `fcTL` delay denominator, so the numerator is literally milliseconds.
const DELAY_DEN: u16 = 1000;

/// Wraps `png`'s animated writer with byte accounting and frame clamping.
pub(crate) struct ApngWriter<W: Write> {
    writer: png::Writer<CountingWriter<W>>,
    pub(super) count: Rc<Cell<u64>>,
    /// What `acTL` promised.
    declared: u32,
    written: u32,
    /// Scratch RGB buffer reused across frames.
    rgb: Vec<u8>,
}

impl<W: Write> ApngWriter<W> {
    /// Open an APNG over `sink` for exactly `frames` frames.
    pub(super) fn new(sink: W, width: u32, height: u32, frames: u32) -> Result<Self, RecordError> {
        if width == 0 || height == 0 {
            return Err(RecordError::Encode(format!(
                "apng canvas must be non-empty, got {width}x{height}"
            )));
        }
        // `set_animated` rejects zero.
        let declared = frames.max(1);
        let count = Rc::new(Cell::new(0));
        let mut encoder =
            png::Encoder::new(CountingWriter::new(sink, Rc::clone(&count)), width, height);
        encoder.set_color(ColorType::Rgb);
        encoder.set_depth(BitDepth::Eight);
        // 0 plays loops forever, matching GIF.
        encoder
            .set_animated(declared, 0)
            .map_err(|err| encode_err("acTL", &err))?;
        let writer = encoder
            .write_header()
            .map_err(|err| encode_err("png header", &err))?;
        Ok(Self {
            writer,
            count,
            declared,
            written: 0,
            rgb: Vec::new(),
        })
    }

    /// Append one frame covering `rect`, held for `delay_ms`.
    pub(super) fn add_frame(
        &mut self,
        surface: &Surface,
        rect: Rect,
        delay_ms: u32,
    ) -> Result<(), RecordError> {
        if self.written >= self.declared {
            // Past `acTL`'s count `png` falls back to plain `IDAT`, producing a
            // still image with garbage appended; drop the frame instead.
            return Ok(());
        }
        let rect = if self.written == 0 {
            Rect::whole(surface)
        } else {
            rect
        };
        let writer = &mut self.writer;
        // Reset first: position and dimension are each validated against the
        // other's current value, so a shrinking rectangle would reject itself.
        writer
            .reset_frame_position()
            .map_err(|err| encode_err("frame reset", &err))?;
        writer
            .set_frame_dimension(rect.w, rect.h)
            .map_err(|err| encode_err("frame dimension", &err))?;
        writer
            .set_frame_position(rect.x, rect.y)
            .map_err(|err| encode_err("frame position", &err))?;
        writer
            .set_frame_delay(u16::try_from(delay_ms).unwrap_or(u16::MAX), DELAY_DEN)
            .map_err(|err| encode_err("frame delay", &err))?;
        // Each frame overwrites its rectangle and leaves the rest standing.
        writer
            .set_dispose_op(DisposeOp::None)
            .map_err(|err| encode_err("dispose op", &err))?;
        writer
            .set_blend_op(BlendOp::Source)
            .map_err(|err| encode_err("blend op", &err))?;

        self.rgb.clear();
        self.rgb.extend(rect.pixels(surface).flatten());
        writer
            .write_image_data(&self.rgb)
            .map_err(|err| encode_err("frame data", &err))?;
        self.written = self.written.saturating_add(1);
        Ok(())
    }

    /// Write `IEND` and return the total bytes. A `--max-bytes` truncation
    /// writes fewer frames than `acTL` promised; `png` does not validate that.
    pub(super) fn finish(self) -> Result<u64, RecordError> {
        self.writer
            .finish()
            .map_err(|err| encode_err("png finish", &err))?;
        Ok(self.count.get())
    }
}

/// Wrap a `png` failure with the step that produced it.
fn encode_err(what: &str, err: &png::EncodingError) -> RecordError {
    RecordError::Encode(format!("apng {what}: {err}"))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    fn flat(width: u32, height: u32, color: [u8; 3]) -> Surface {
        Surface {
            width,
            height,
            pixels: vec![color; (width * height) as usize],
        }
    }

    /// Every `fcTL` payload as `(width, height, x, y, delay_num, delay_den)`.
    fn fctls(bytes: &[u8]) -> Vec<(u32, u32, u32, u32, u16, u16)> {
        let be32 = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
        let be16 = |at: usize| u16::from_be_bytes(bytes[at..at + 2].try_into().unwrap());
        bytes
            .windows(4)
            .enumerate()
            .filter(|(_, tag)| *tag == b"fcTL")
            .map(|(at, _)| {
                // Payload: sequence(4) width(4) height(4) x(4) y(4) num(2) den(2).
                let p = at + 4;
                (
                    be32(p + 4),
                    be32(p + 8),
                    be32(p + 12),
                    be32(p + 16),
                    be16(p + 20),
                    be16(p + 22),
                )
            })
            .collect()
    }

    #[test]
    fn frames_carry_geometry_and_millisecond_delays() {
        let surface = flat(16, 32, [1, 2, 3]);
        let band = Rect {
            x: 0,
            y: 16,
            w: 16,
            h: 16,
        };
        let mut sink: Vec<u8> = Vec::new();
        let mut enc = ApngWriter::new(&mut sink, 16, 32, 2).expect("encoder opens");
        // The first frame is forced full-canvas: an IDAT smaller than IHDR is
        // a corrupt PNG.
        enc.add_frame(&surface, band, 250).unwrap();
        enc.add_frame(&surface, band, 50).unwrap();
        // Frames beyond the declared count are dropped, not appended.
        let before = enc.count.get();
        enc.add_frame(&surface, band, 50).unwrap();
        assert_eq!(enc.count.get(), before);
        enc.finish().unwrap();

        let actl = sink
            .windows(4)
            .position(|tag| tag == b"acTL")
            .expect("acTL");
        let first_fctl = sink
            .windows(4)
            .position(|tag| tag == b"fcTL")
            .expect("fcTL");
        assert!(actl < first_fctl, "acTL must precede the first fcTL");
        assert_eq!(
            fctls(&sink),
            [(16, 32, 0, 0, 250, 1000), (16, 16, 0, 16, 50, 1000)]
        );
    }
}
