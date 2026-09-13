//! Animated-image containers: `GIF89a` and APNG, both produced in-process.
//!
//! No external binaries, ever (ADR-0060). `agg`, `vhs`, and `ffmpeg` are not
//! runtime dependencies of this feature — asciinema's own `agg` is
//! GPL-3.0-or-later with `gifski` (AGPL-3.0-or-later) underneath, which is
//! incompatible with phux's `MIT OR Apache-2.0` even as a vendored fork, and
//! shelling out to a tool the user may not have installed is not a feature,
//! it is a support burden.
//!
//! Two audited permissive crates do the container work: `png` (already in the
//! lockfile) for APNG, and `gif` for `GIF89a`. Hand-rolling that writer is
//! genuinely feasible at around 250 lines, and the 255-byte sub-block
//! chunking, LZW code-width growth, and clear-code reset are precisely the
//! details that produce a file which plays in one viewer and breaks in
//! another — a bug class we would own forever for no user-visible gain.
//!
//! # Layout
//!
//! * [`palette`] — the global color table, exact when it can be.
//! * `gif` — `GIF89a`: one global table, sub-rectangle frames, centisecond
//!   delays.
//! * `apng` — animated PNG: truecolor, no palette at all, millisecond delays.
//!
//! APNG is the reference backend, deliberately. It needs no palette and no
//! quantization, so it exercises the whole replay -> rasterize -> encode
//! pipeline with none of GIF's failure modes in the way; if the two ever
//! disagree about a recording, APNG is the one that is right.
//!
//! # Byte accounting
//!
//! Both containers own their sink, and `--max-bytes` has to be enforced
//! without buffering the animation. Every sink is therefore wrapped in a
//! `CountingWriter` that shares a `Cell<u64>` with the encoder, so
//! [`AnimEncoder::add_frame`] can report the running total after the
//! container has already consumed the writer.

pub mod apng;
pub mod gif;
pub mod palette;

use std::cell::Cell;
use std::fmt;
use std::io::Write;
use std::rc::Rc;

use crate::error::RecordError;
use crate::raster::Surface;

pub use self::palette::Palette;

use self::apng::ApngWriter;
use self::gif::GifWriter;

/// A pixel rectangle, used to emit sub-frames covering only changed rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Left edge in pixels.
    pub x: u32,
    /// Top edge in pixels.
    pub y: u32,
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
}

impl Rect {
    /// The rectangle covering all of `surface`.
    #[must_use]
    pub const fn whole(surface: &Surface) -> Self {
        Self {
            x: 0,
            y: 0,
            w: surface.width,
            h: surface.height,
        }
    }

    /// Clip to `surface`, returning `None` when nothing is left.
    ///
    /// A sub-rectangle is derived from a dirty row band, and a resize can make
    /// yesterday's band point past the end of today's canvas. Both containers
    /// reject an out-of-bounds frame, so the clip happens once, here, rather
    /// than as a surprise error three layers down.
    #[must_use]
    fn clipped(self, surface: &Surface) -> Option<Self> {
        let x = self.x.min(surface.width);
        let y = self.y.min(surface.height);
        let w = self.w.min(surface.width.saturating_sub(x));
        let h = self.h.min(surface.height.saturating_sub(y));
        if w == 0 || h == 0 {
            None
        } else {
            Some(Self { x, y, w, h })
        }
    }
}

/// A `Write` that tallies bytes into a shared cell.
///
/// The counter is shared rather than owned because both containers take the
/// writer by value and never give it back until they are finished; the
/// render driver needs the running total on every frame.
pub(crate) struct CountingWriter<W: Write> {
    inner: W,
    count: Rc<Cell<u64>>,
}

impl<W: Write> fmt::Debug for CountingWriter<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CountingWriter")
            .field("count", &self.count.get())
            .finish_non_exhaustive()
    }
}

impl<W: Write> CountingWriter<W> {
    /// Wrap `inner`, reporting its running byte total through `count`.
    pub(crate) const fn new(inner: W, count: Rc<Cell<u64>>) -> Self {
        Self { inner, count }
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        // Count what the sink actually took, not what was offered: a short
        // write must not inflate the total and trip `--max-bytes` early.
        self.count.set(self.count.get().saturating_add(n as u64));
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// An animated-image encoder.
///
/// An enum rather than a trait object on purpose: there are exactly two
/// containers, they are both compiled in, and the render driver needs to hand
/// each one a different construction (a palette for GIF, a frame count for
/// APNG) that no single trait signature expresses honestly.
pub enum AnimEncoder<W: Write> {
    /// `GIF89a` with one global color table and infinite looping.
    Gif(GifWriter<W>),
    /// APNG: truecolor, no quantization, 1 ms timing resolution.
    Apng(ApngWriter<W>),
}

impl<W: Write> fmt::Debug for AnimEncoder<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (width, height) = self.canvas();
        f.debug_struct("AnimEncoder")
            .field("format", &self.format_name())
            .field("width", &width)
            .field("height", &height)
            .field("bytes", &self.bytes())
            .finish_non_exhaustive()
    }
}

impl<W: Write> AnimEncoder<W> {
    /// Open a `GIF89a` encoder over `sink`.
    ///
    /// # Errors
    ///
    /// Fails when the canvas does not fit GIF's 16-bit dimensions, or when
    /// the header write fails.
    pub fn gif(sink: W, width: u32, height: u32, palette: Palette) -> Result<Self, RecordError> {
        GifWriter::new(sink, width, height, palette).map(Self::Gif)
    }

    /// Open an APNG encoder over `sink` for exactly `frames` frames.
    ///
    /// # Errors
    ///
    /// Fails when the PNG header or `acTL` chunk cannot be written.
    pub fn apng(sink: W, width: u32, height: u32, frames: u32) -> Result<Self, RecordError> {
        ApngWriter::new(sink, width, height, frames).map(Self::Apng)
    }

    /// The container's name, for diagnostics.
    #[must_use]
    pub const fn format_name(&self) -> &'static str {
        match self {
            Self::Gif(_) => "gif",
            Self::Apng(_) => "apng",
        }
    }

    /// Canvas size in pixels.
    #[must_use]
    pub const fn canvas(&self) -> (u32, u32) {
        match self {
            Self::Gif(inner) => inner.canvas(),
            Self::Apng(inner) => inner.canvas(),
        }
    }

    /// Bytes written to the sink so far.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        match self {
            Self::Gif(inner) => inner.bytes(),
            Self::Apng(inner) => inner.bytes(),
        }
    }

    /// Append one frame covering `rect`, held for `delay_ms`.
    ///
    /// Returns the total bytes written so far, which is how the driver
    /// enforces `--max-bytes` without ever buffering the whole animation.
    ///
    /// # Errors
    ///
    /// Propagates container and I/O failures.
    pub fn add_frame(
        &mut self,
        surface: &Surface,
        rect: Rect,
        delay_ms: u32,
    ) -> Result<u64, RecordError> {
        // An empty rectangle is not an error: a dirty band can clip away
        // entirely after a shrink, and the honest response is to contribute
        // nothing rather than to fail the export.
        let Some(rect) = rect.clipped(surface) else {
            return Ok(self.bytes());
        };
        match self {
            Self::Gif(inner) => inner.add_frame(surface, rect, delay_ms),
            Self::Apng(inner) => inner.add_frame(surface, rect, delay_ms),
        }
    }

    /// Close the container and return the total bytes written.
    ///
    /// # Errors
    ///
    /// Propagates the trailer or `IEND` write.
    pub fn finish(self) -> Result<u64, RecordError> {
        match self {
            Self::Gif(inner) => inner.finish(),
            Self::Apng(inner) => inner.finish(),
        }
    }
}

/// Copy `rect` out of `surface` as tightly packed `[r, g, b, ...]`.
///
/// Shared by both containers so they can never disagree about row order or
/// about which pixels a rectangle covers.
fn copy_rect_rgb(surface: &Surface, rect: Rect, out: &mut Vec<u8>) {
    out.clear();
    let want = (rect.w as usize).saturating_mul(rect.h as usize) * 3;
    out.reserve(want);
    for y in rect.y..rect.y.saturating_add(rect.h) {
        let row = (y as usize).saturating_mul(surface.width as usize);
        for x in rect.x..rect.x.saturating_add(rect.w) {
            // Bounds-checked rather than indexed: a malformed rectangle must
            // produce black pixels, not a panic in the middle of an export.
            let pixel = surface
                .pixels
                .get(row.saturating_add(x as usize))
                .copied()
                .unwrap_or([0, 0, 0]);
            out.extend_from_slice(&pixel);
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

    fn surface(width: u32, height: u32) -> Surface {
        Surface {
            width,
            height,
            pixels: (0..width * height)
                .map(|i| {
                    let byte = u8::try_from(i % 256).unwrap();
                    [byte, byte, byte]
                })
                .collect(),
        }
    }

    #[test]
    fn counting_writer_tallies_accepted_bytes() {
        let count = Rc::new(Cell::new(0));
        let mut sink = CountingWriter::new(Vec::new(), Rc::clone(&count));
        sink.write_all(b"hello").unwrap();
        sink.write_all(b"!").unwrap();
        assert_eq!(count.get(), 6);
    }

    #[test]
    fn clipping_drops_a_rectangle_that_falls_off_the_canvas() {
        let surface = surface(8, 8);
        assert!(
            Rect {
                x: 0,
                y: 16,
                w: 8,
                h: 8
            }
            .clipped(&surface)
            .is_none()
        );
        assert_eq!(
            Rect {
                x: 0,
                y: 4,
                w: 8,
                h: 99
            }
            .clipped(&surface),
            Some(Rect {
                x: 0,
                y: 4,
                w: 8,
                h: 4
            })
        );
    }

    #[test]
    fn copy_rect_reads_row_major_from_the_requested_offset() {
        let surface = surface(4, 4);
        let mut out = Vec::new();
        copy_rect_rgb(
            &surface,
            Rect {
                x: 1,
                y: 1,
                w: 2,
                h: 2,
            },
            &mut out,
        );
        // Row 1 columns 1..3 are pixels 5 and 6; row 2 columns 1..3 are 9, 10.
        assert_eq!(out, vec![5, 5, 5, 6, 6, 6, 9, 9, 9, 10, 10, 10]);
    }
}
