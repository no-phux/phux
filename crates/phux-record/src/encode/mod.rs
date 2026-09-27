//! Animated-image containers, produced in-process with the `png` and `gif`
//! crates. No external binaries (ADR-0060): `agg`/`gifski` are GPL/AGPL, and
//! shelling out to a tool the user may lack is a support burden.
//!
//! APNG is the reference backend: truecolor, no palette, millisecond delays.
//! If the two containers ever disagree about a recording, APNG is right.
//!
//! Both containers own their sink, so each sink is wrapped in a
//! [`CountingWriter`] sharing a `Cell<u64>` with the encoder; that is how
//! `--max-bytes` is enforced without buffering the animation.

mod apng;
mod gif;
mod palette;

use std::cell::Cell;
use std::io::Write;
use std::rc::Rc;

use crate::error::RecordError;
use crate::raster::Surface;

pub(crate) use self::palette::Palette;

use self::apng::ApngWriter;
use self::gif::GifWriter;

/// A pixel rectangle, used to emit sub-frames covering only changed rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rect {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) w: u32,
    pub(crate) h: u32,
}

impl Rect {
    /// The rectangle covering all of `surface`.
    pub(crate) const fn whole(surface: &Surface) -> Self {
        Self {
            x: 0,
            y: 0,
            w: surface.width,
            h: surface.height,
        }
    }

    /// Clip to `surface`, or `None` when nothing is left (a stale dirty band
    /// can point past a canvas that shrank).
    fn clipped(self, surface: &Surface) -> Option<Self> {
        let x = self.x.min(surface.width);
        let y = self.y.min(surface.height);
        let w = self.w.min(surface.width.saturating_sub(x));
        let h = self.h.min(surface.height.saturating_sub(y));
        (w != 0 && h != 0).then_some(Self { x, y, w, h })
    }

    /// The pixels of `surface` under this rectangle, row-major. Out-of-range
    /// pixels read as black rather than panicking mid-export.
    fn pixels(self, surface: &Surface) -> impl Iterator<Item = [u8; 3]> + '_ {
        let width = surface.width as usize;
        (self.y..self.y.saturating_add(self.h)).flat_map(move |y| {
            let row = (y as usize).saturating_mul(width);
            (self.x..self.x.saturating_add(self.w)).map(move |x| {
                surface
                    .pixels
                    .get(row.saturating_add(x as usize))
                    .copied()
                    .unwrap_or([0, 0, 0])
            })
        })
    }
}

/// A `Write` that tallies accepted bytes into a shared cell.
pub(crate) struct CountingWriter<W: Write> {
    inner: W,
    count: Rc<Cell<u64>>,
}

impl<W: Write> CountingWriter<W> {
    pub(crate) const fn new(inner: W, count: Rc<Cell<u64>>) -> Self {
        Self { inner, count }
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        // Count what the sink took, not what was offered.
        self.count.set(self.count.get().saturating_add(n as u64));
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// An animated-image encoder.
pub(crate) enum AnimEncoder<W: Write> {
    Gif(GifWriter<W>),
    Apng(ApngWriter<W>),
}

impl<W: Write> AnimEncoder<W> {
    /// Open a `GIF89a` encoder over `sink`.
    pub(crate) fn gif(
        sink: W,
        width: u32,
        height: u32,
        palette: Palette,
    ) -> Result<Self, RecordError> {
        GifWriter::new(sink, width, height, palette).map(Self::Gif)
    }

    /// Open an APNG encoder over `sink` for exactly `frames` frames.
    pub(crate) fn apng(sink: W, width: u32, height: u32, frames: u32) -> Result<Self, RecordError> {
        ApngWriter::new(sink, width, height, frames).map(Self::Apng)
    }

    fn bytes(&self) -> u64 {
        match self {
            Self::Gif(inner) => inner.count.get(),
            Self::Apng(inner) => inner.count.get(),
        }
    }

    /// Append one frame covering `rect`, held for `delay_ms`, and return the
    /// total bytes written so far. A rectangle that clips away contributes
    /// nothing.
    pub(crate) fn add_frame(
        &mut self,
        surface: &Surface,
        rect: Rect,
        delay_ms: u32,
    ) -> Result<u64, RecordError> {
        if let Some(rect) = rect.clipped(surface) {
            match self {
                Self::Gif(inner) => inner.add_frame(surface, rect, delay_ms)?,
                Self::Apng(inner) => inner.add_frame(surface, rect, delay_ms)?,
            }
        }
        Ok(self.bytes())
    }

    /// Close the container and return the total bytes written.
    pub(crate) fn finish(self) -> Result<u64, RecordError> {
        match self {
            Self::Gif(inner) => inner.finish(),
            Self::Apng(inner) => inner.finish(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn clipping_and_pixel_walk() {
        let surface = Surface {
            width: 4,
            height: 4,
            pixels: (0..16_u8).map(|i| [i, i, i]).collect(),
        };
        let rect = |x, y, w, h| Rect { x, y, w, h };
        assert!(rect(0, 16, 4, 4).clipped(&surface).is_none());
        assert_eq!(rect(0, 2, 4, 99).clipped(&surface), Some(rect(0, 2, 4, 2)));
        // Row 1 columns 1..3 are pixels 5 and 6; row 2 columns 1..3 are 9, 10.
        let got: Vec<u8> = rect(1, 1, 2, 2).pixels(&surface).map(|p| p[0]).collect();
        assert_eq!(got, [5, 6, 9, 10]);
    }
}
