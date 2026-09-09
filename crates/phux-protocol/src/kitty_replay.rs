//! Kitty graphics replay helpers for phux's VT-byte snapshot/render paths.
//!
//! ADR-0034 keeps images inside the libghostty terminal state rather than
//! introducing a structured image wire tier. These helpers project that image
//! state back into Kitty graphics APC bytes when a phux renderer or snapshot
//! synthesizer needs to repaint from a libghostty mirror.

use std::io::{self, Write};

use libghostty_vt::{
    Terminal as GhosttyTerminal,
    alloc::{Allocator, Bytes},
    kitty::graphics::{
        self, Compression, DecodedImage, Image, ImageFormat, PlacementIterator, PlacementRenderInfo,
    },
};

/// Per-terminal Kitty image storage budget.
///
/// Large enough for real `kitten icat` usage while bounded against accidental
/// unbounded image accumulation in long-running panes.
pub const KITTY_IMAGE_STORAGE_LIMIT_BYTES: u64 = 64 * 1024 * 1024;

/// Error returned while projecting libghostty Kitty image state to APC bytes.
#[derive(Debug, thiserror::Error)]
pub enum KittyReplayError {
    /// libghostty rejected an image/placement query.
    #[error("libghostty: {0}")]
    Ghostty(#[from] libghostty_vt::Error),
    /// The output sink rejected replay bytes.
    #[error("io: {0}")]
    Io(#[from] io::Error),
}

/// Install phux's PNG decoder and enable Kitty image storage on `terminal`.
///
/// The decoder registration is thread-local in libghostty; calling this from
/// each pane constructor is intentional and keeps tests/actors independent.
pub fn configure_terminal_for_kitty_graphics(
    terminal: &mut GhosttyTerminal<'_, '_>,
) -> Result<(), libghostty_vt::Error> {
    graphics::set_png_decoder(Some(Box::<PngDecoder>::default()))?;
    terminal.set_kitty_image_storage_limit(KITTY_IMAGE_STORAGE_LIMIT_BYTES)?;
    Ok(())
}

/// Re-emit Kitty graphics state held by `terminal` as APC bytes.
///
/// `origin` is the outer-terminal cell coordinate for pane-local `(0, 0)`.
/// `clip` is the pane's visible cell extent. Virtual/placeholder placements
/// only need the image payload re-transmitted; classic placements are replayed
/// with `a=T` at their viewport position so they survive cell-renderer paints
/// and server snapshots without introducing a new wire field.
///
/// Returns `true` when bytes were written.
pub fn emit_kitty_graphics_replay(
    terminal: &GhosttyTerminal<'_, '_>,
    placement_iter: &mut PlacementIterator<'_>,
    out: &mut impl Write,
    origin: (u16, u16),
    clip: (u16, u16),
) -> Result<bool, KittyReplayError> {
    let graphics = terminal.kitty_graphics()?;
    let mut placements = placement_iter.update(&graphics)?;
    let mut transmitted: Vec<u32> = Vec::new();
    let mut wrote = false;

    while let Some(placement) = placements.next() {
        let image_id = placement.image_id()?;
        let Some(image) = graphics.image(image_id) else {
            continue;
        };

        if placement.is_virtual()? {
            if transmitted.contains(&image_id) {
                continue;
            }
            write_image_apc(out, &image, ImageAction::TransmitOnly, None)?;
        } else {
            let info = placement.placement_render_info(&image, terminal)?;
            let Some(region) = clip_placement(&info, clip) else {
                continue;
            };
            write_placement(out, &image, region, origin)?;
        }

        transmitted.push(image_id);
        wrote = true;
    }

    Ok(wrote)
}

/// One placement's pane-local cell geometry, already clipped to the pane.
#[derive(Clone, Copy, Debug)]
struct PlacementRegion {
    col: u16,
    row: u16,
    cols: u32,
    rows: u32,
}

/// Whether a placement is on-screen with a non-degenerate cell footprint.
const fn is_renderable(info: &PlacementRenderInfo) -> bool {
    info.viewport_visible
        && info.grid_cols != 0
        && info.grid_rows != 0
        && info.viewport_col >= 0
        && info.viewport_row >= 0
}

/// Clip a classic placement's render info to the pane's visible cell extent.
///
/// Returns `None` for every case the replay must skip: off-viewport,
/// degenerate, or starting past the pane's own `clip` bounds.
fn clip_placement(info: &PlacementRenderInfo, clip: (u16, u16)) -> Option<PlacementRegion> {
    if !is_renderable(info) {
        return None;
    }

    let col = u16::try_from(info.viewport_col)
        .ok()
        .filter(|c| *c < clip.0)?;
    let row = u16::try_from(info.viewport_row)
        .ok()
        .filter(|r| *r < clip.1)?;
    let cols = u32::from(clip.0.saturating_sub(col)).min(info.grid_cols);
    let rows = u32::from(clip.1.saturating_sub(row)).min(info.grid_rows);
    if cols == 0 || rows == 0 {
        return None;
    }

    Some(PlacementRegion {
        col,
        row,
        cols,
        rows,
    })
}

/// Replay one classic placement with `a=T` at its outer-terminal position.
fn write_placement(
    out: &mut impl Write,
    image: &Image<'_>,
    region: PlacementRegion,
    origin: (u16, u16),
) -> Result<(), KittyReplayError> {
    write_cup(
        out,
        region.row.saturating_add(origin.1),
        region.col.saturating_add(origin.0),
    )?;
    write_image_apc(
        out,
        image,
        ImageAction::TransmitAndDisplay,
        Some((region.cols, region.rows)),
    )
}

#[derive(Debug, Default)]
struct PngDecoder {
    buf: Vec<u8>,
}

impl graphics::DecodePng for PngDecoder {
    fn decode_png<'alloc>(
        &mut self,
        alloc: &'alloc Allocator<'_>,
        data: &[u8],
    ) -> Option<DecodedImage<'alloc>> {
        use png::{Decoder, Transformations};
        use std::io::Cursor;

        let mut decoder = Decoder::new(Cursor::new(data));
        decoder.set_transformations(Transformations::ALPHA | Transformations::STRIP_16);

        let mut reader = decoder.read_info().ok()?;
        let needed = reader.output_buffer_size()?;
        if self.buf.len() < needed {
            self.buf.resize(needed, 0);
        }
        let info = reader.next_frame(&mut self.buf).ok()?;

        let mut bytes = Bytes::new_with_alloc(alloc, info.buffer_size()).ok()?;
        bytes.copy_from_slice(&self.buf[..info.buffer_size()]);
        reader.finish().ok()?;

        Some(DecodedImage {
            width: info.width,
            height: info.height,
            data: bytes,
        })
    }
}

#[derive(Clone, Copy, Debug)]
enum ImageAction {
    TransmitOnly,
    TransmitAndDisplay,
}

fn write_image_apc(
    out: &mut impl Write,
    image: &Image<'_>,
    action: ImageAction,
    placement_size: Option<(u32, u32)>,
) -> Result<(), KittyReplayError> {
    let Some(format) = kitty_format_code(image.format()?) else {
        return Ok(());
    };
    if !matches!(image.compression()?, Compression::None) {
        return Ok(());
    }

    let action = match action {
        ImageAction::TransmitOnly => b't',
        ImageAction::TransmitAndDisplay => b'T',
    };
    let data = image.data()?;
    if data.is_empty() {
        return Ok(());
    }

    let mut first = true;
    let mut offset = 0usize;
    while offset < data.len() {
        let end = (offset + 3_072).min(data.len());
        let more = end < data.len();
        if first {
            write!(
                out,
                "\x1b_Ga={},f={},s={},v={},i={},q=2",
                char::from(action),
                format,
                image.width()?,
                image.height()?,
                image.id()?
            )?;
            if let Some((cols, rows)) = placement_size {
                write!(out, ",c={cols},r={rows}")?;
            }
            write!(out, ",m={};", u8::from(more))?;
            first = false;
        } else {
            write!(out, "\x1b_Gm={};", u8::from(more))?;
        }
        write_base64(out, &data[offset..end])?;
        out.write_all(b"\x1b\\")?;
        offset = end;
    }

    Ok(())
}

const fn kitty_format_code(format: ImageFormat) -> Option<u32> {
    match format {
        ImageFormat::Rgb => Some(24),
        ImageFormat::Rgba => Some(32),
        ImageFormat::Png => Some(100),
        _ => None,
    }
}

fn write_base64(out: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let (chunks, rem) = bytes.as_chunks::<3>();
    for chunk in chunks {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        out.write_all(&[
            TABLE[((n >> 18) & 0x3f) as usize],
            TABLE[((n >> 12) & 0x3f) as usize],
            TABLE[((n >> 6) & 0x3f) as usize],
            TABLE[(n & 0x3f) as usize],
        ])?;
    }

    if !rem.is_empty() {
        let b0 = rem[0];
        let b1 = rem.get(1).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8);
        let pad = if rem.len() == 1 {
            b'='
        } else {
            TABLE[((n >> 6) & 0x3f) as usize]
        };
        out.write_all(&[
            TABLE[((n >> 18) & 0x3f) as usize],
            TABLE[((n >> 12) & 0x3f) as usize],
            pad,
            b'=',
        ])?;
    }

    Ok(())
}

fn write_cup(out: &mut impl Write, row: u16, col: u16) -> io::Result<()> {
    write!(
        out,
        "\x1b[{};{}H",
        row.saturating_add(1),
        col.saturating_add(1)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_encodes_padding_cases() {
        let mut out = Vec::new();
        write_base64(&mut out, b"f").expect("encode one");
        assert_eq!(out, b"Zg==");

        out.clear();
        write_base64(&mut out, b"fo").expect("encode two");
        assert_eq!(out, b"Zm8=");

        out.clear();
        write_base64(&mut out, b"foo").expect("encode three");
        assert_eq!(out, b"Zm9v");
    }

    #[test]
    fn replay_reemits_classic_rgba_placement() {
        let mut terminal = GhosttyTerminal::new(libghostty_vt::TerminalOptions {
            cols: 10,
            rows: 5,
            max_scrollback: 0,
        })
        .expect("terminal");
        configure_terminal_for_kitty_graphics(&mut terminal).expect("kitty config");
        terminal.resize(10, 5, 8, 16).expect("cell geometry");
        terminal.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,c=1,r=1,i=7,q=2;/wAA/w==\x1b\\");

        let mut iter = PlacementIterator::new().expect("placement iterator");
        let mut out = Vec::new();
        assert!(
            emit_kitty_graphics_replay(&terminal, &mut iter, &mut out, (2, 3), (10, 5))
                .expect("replay"),
            "stored placement must emit replay bytes"
        );

        let replay = String::from_utf8_lossy(&out);
        assert!(
            replay.contains("\x1b[4;3H"),
            "placement must be shifted by pane origin; got {replay:?}"
        );
        assert!(
            replay.contains("\x1b_Ga=T,f=32,s=1,v=1,i=7,q=2,c=1,r=1,m=0;/wAA/w==\x1b\\"),
            "replay must transmit and display the stored RGBA image; got {replay:?}"
        );
    }
}
