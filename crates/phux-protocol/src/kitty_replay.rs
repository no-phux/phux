//! Kitty graphics replay helpers for phux's VT-byte snapshot/render paths.
//!
//! ADR-0034 keeps images inside the libghostty terminal state rather than
//! introducing a structured image wire tier. These helpers project that image
//! state back into Kitty graphics APC bytes when a phux renderer or snapshot
//! synthesizer needs to repaint from a libghostty mirror.

use std::io::{self, Write};

use flate2::write::ZlibEncoder;
use libghostty_vt::{
    Terminal as GhosttyTerminal,
    kitty::graphics::{self, Compression, Image, ImageFormat, PlacementIterator},
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
    graphics::set_png_decoder(Some(Box::new(graphics::RustPngDecoder::new())))?;
    terminal.set_kitty_image_storage_limit(KITTY_IMAGE_STORAGE_LIMIT_BYTES)?;
    Ok(())
}

/// Re-emit the delta of `terminal`'s visible Kitty graphics against what
/// `residency` says the outer terminal already holds.
///
/// `origin` is the outer-terminal cell coordinate for pane-local `(0, 0)`.
/// `clip` is the pane's visible cell extent. Only images referenced by
/// collected, clipped placements are kept in the outer terminal: an image
/// whose content generation the outer terminal lacks is transmitted once, an
/// image whose placements moved is re-placed without its payload, and an
/// image that is no longer visibly placed is deleted. Unchanged state writes
/// nothing. Placements keep their original identities, source rectangles,
/// offsets and stacking order. Raw pixel payloads travel zlib-compressed
/// (`o=z`) when that is smaller.
///
/// Returns `true` when a classic (cell-anchored) placement was written or
/// removed: the cursor moved and cells may be overlaid. Payload transmission
/// and Unicode-placeholder placements leave cells and cursor alone.
///
/// The caller calls [`KittyResidency::forget`] whenever the outer terminal
/// may have lost or never received what was emitted (a screen clear, a
/// dropped frame).
pub fn emit_kitty_graphics_replay(
    terminal: &GhosttyTerminal<'_, '_>,
    placement_iter: &mut PlacementIterator<'_>,
    residency: &mut KittyResidency,
    out: &mut impl Write,
    origin: (u16, u16),
    clip: (u16, u16),
) -> Result<bool, KittyReplayError> {
    let graphics = terminal.kitty_graphics()?;
    let replay = sorted_placements(terminal, &graphics, placement_iter, Some(clip))?;
    residency.replay(&graphics, &replay, origin, out)
}

/// Restore graphics into a dedicated terminal replica.
///
/// The receiver clips its own viewport. Preserve right/bottom extents so a
/// later cell-pixel resize can reveal the rest of a natural-size image.
/// All resident payloads, including unplaced images, are restored so later
/// live placement commands can reference them. Raw pixel payloads travel
/// zlib-compressed (`o=z`) when that is smaller.
///
/// Returns `true` when bytes were written.
pub fn emit_kitty_graphics_snapshot(
    terminal: &GhosttyTerminal<'_, '_>,
    placement_iter: &mut PlacementIterator<'_>,
    out: &mut impl Write,
) -> Result<bool, KittyReplayError> {
    let graphics = terminal.kitty_graphics()?;
    let mut image_ids = Vec::new();
    graphics.image_ids_into(&mut image_ids)?;
    let mut replay = sorted_placements(terminal, &graphics, placement_iter, None)?;
    transmit_images(&graphics, out, &mut image_ids)?;
    replay.retain(|p| image_ids.binary_search(&p.image_id).is_ok());
    for placement in replay {
        placement.write(out, (0, 0))?;
    }
    Ok(!image_ids.is_empty())
}

/// Collect `clip`ped placements, grouped by image in a stable order.
fn sorted_placements(
    terminal: &GhosttyTerminal<'_, '_>,
    graphics: &graphics::Graphics<'_>,
    placement_iter: &mut PlacementIterator<'_>,
    clip: Option<(u16, u16)>,
) -> Result<Vec<ReplayPlacement>, KittyReplayError> {
    let mut replay = collect_placements(terminal, graphics, placement_iter, clip)?;
    // Preserve default-placeholder preference without exposing internal IDs.
    replay.sort_by_key(|p| (p.image_id, !p.external, p.placement_id));
    Ok(replay)
}

/// Transmit each of `ids`, keeping only those that had a payload to send.
fn transmit_images(
    graphics: &graphics::Graphics<'_>,
    out: &mut impl Write,
    ids: &mut Vec<u32>,
) -> Result<(), KittyReplayError> {
    let mut transmitted = 0;
    for index in 0..ids.len() {
        let id = ids[index];
        if let Some(image) = graphics.image(id)
            && write_image_apc(out, &image)?
        {
            ids[transmitted] = id;
            transmitted += 1;
        }
    }
    ids.truncate(transmitted);
    Ok(())
}

fn cell_pixels(terminal: &GhosttyTerminal<'_, '_>) -> Result<(u32, u32), libghostty_vt::Error> {
    Ok((
        terminal.width_px()? / u32::from(terminal.cols()?.max(1)),
        terminal.height_px()? / u32::from(terminal.rows()?.max(1)),
    ))
}

fn collect_placements(
    terminal: &GhosttyTerminal<'_, '_>,
    graphics: &graphics::Graphics<'_>,
    iter: &mut PlacementIterator<'_>,
    clip: Option<(u16, u16)>,
) -> Result<Vec<ReplayPlacement>, KittyReplayError> {
    let cell = cell_pixels(terminal)?;
    let mut placements = iter.update(graphics)?;
    let mut replay = Vec::new();
    while let Some(placement) = placements.next() {
        let id = placement.image_id()?;
        let Some(image) = graphics.image(id) else {
            continue;
        };
        if let Some(placement) = ReplayPlacement::new(id, placement, &image, terminal, cell, clip)?
        {
            replay.push(placement);
        }
    }
    Ok(replay)
}

/// Raw placement controls. Preserve zero dimensions (natural size/aspect ratio)
/// unless clipping requires a smaller destination and source rectangle.
struct PlacementGeometry {
    cols: u32,
    rows: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    x_offset: u32,
    y_offset: u32,
}

struct ReplayPlacement {
    image_id: u32,
    placement_id: u32,
    external: bool,
    virtual_placement: bool,
    geometry: PlacementGeometry,
    position: Option<(u16, u16)>,
    z: i32,
}

impl ReplayPlacement {
    fn new(
        image_id: u32,
        placement: &graphics::PlacementIteration<'_, '_>,
        image: &Image<'_>,
        terminal: &GhosttyTerminal<'_, '_>,
        cell: (u32, u32),
        clip: Option<(u16, u16)>,
    ) -> Result<Option<Self>, libghostty_vt::Error> {
        let virtual_placement = placement.is_virtual()?;
        let mut geometry = PlacementGeometry::from_placement(placement)?;
        let position = if virtual_placement {
            None
        } else {
            let Some(position) = geometry.clip(
                &placement.placement_render_info(image, terminal)?,
                cell,
                clip,
            ) else {
                return Ok(None);
            };
            Some(position)
        };
        Ok(Some(Self {
            image_id,
            virtual_placement,
            geometry,
            position,
            placement_id: placement.placement_id()?,
            external: placement.is_external()?,
            z: placement.z()?,
        }))
    }

    fn write(&self, out: &mut impl Write, origin: (u16, u16)) -> Result<(), KittyReplayError> {
        if let Some((col, row)) = self.position {
            write_cup(
                out,
                row.saturating_add(origin.1),
                col.saturating_add(origin.0),
            )?;
        }
        let geometry = &self.geometry;
        write!(
            out,
            "\x1b_Ga=p,i={},p={},U={},c={},r={},x={},y={},w={},h={},X={},Y={},z={},C=1,q=2;\x1b\\",
            self.image_id,
            if self.external { self.placement_id } else { 0 },
            u8::from(self.virtual_placement),
            geometry.cols,
            geometry.rows,
            geometry.x,
            geometry.y,
            geometry.width,
            geometry.height,
            geometry.x_offset,
            geometry.y_offset,
            self.z,
        )?;
        Ok(())
    }
}

impl PlacementGeometry {
    fn from_placement(
        placement: &libghostty_vt::kitty::graphics::PlacementIteration<'_, '_>,
    ) -> Result<Self, libghostty_vt::Error> {
        Ok(Self {
            cols: placement.columns()?,
            rows: placement.rows()?,
            x: placement.source_x()?,
            y: placement.source_y()?,
            width: placement.source_width()?,
            height: placement.source_height()?,
            x_offset: placement.x_offset()?,
            y_offset: placement.y_offset()?,
        })
    }

    fn clip(
        &mut self,
        info: &libghostty_vt::kitty::graphics::PlacementRenderInfo,
        cell: (u32, u32),
        clip: Option<(u16, u16)>,
    ) -> Option<(u16, u16)> {
        if !info.viewport_visible {
            return None;
        }
        let col = clip_axis(
            info.viewport_col,
            info.grid_cols,
            info.pixel_width,
            cell.0,
            clip.map(|clip| clip.0),
            info.source_x,
            info.source_width,
            &mut self.cols,
            &mut self.x,
            &mut self.width,
            &mut self.x_offset,
        )?;
        let row = clip_axis(
            info.viewport_row,
            info.grid_rows,
            info.pixel_height,
            cell.1,
            clip.map(|clip| clip.1),
            info.source_y,
            info.source_height,
            &mut self.rows,
            &mut self.y,
            &mut self.height,
            &mut self.y_offset,
        )?;
        Some((col, row))
    }
}

/// Clip in destination pixels, then map the visible interval into the source.
/// In particular, a negative viewport row is a partially scrolled image, not
/// an invisible image. Integer source bounds round outwards as Kitty requires.
#[allow(
    clippy::too_many_arguments,
    reason = "one axis of Kitty placement geometry"
)]
fn clip_axis(
    position: i32,
    grid_size: u32,
    pixel_size: u32,
    cell_size: u32,
    clip: Option<u16>,
    source_start: u32,
    source_size: u32,
    cells: &mut u32,
    source: &mut u32,
    size: &mut u32,
    offset: &mut u32,
) -> Option<u16> {
    let (start, end, visible_position) =
        visible_cell_interval(position, grid_size, pixel_size, source_size, clip)?;
    if start == i64::from(position) && end == i64::from(position) + i64::from(grid_size) {
        return Some(visible_position);
    }
    let (first, last, visible_offset) =
        visible_pixel_interval(position, start, end, pixel_size, cell_size, *offset)?;
    let (source_first, source_len) =
        mapped_source_interval(first, last, pixel_size, source_start, source_size)?;
    *source = source_first;
    *size = source_len;
    *offset = visible_offset;
    if *cells != 0 {
        *cells = u32::try_from(end - start).ok()?;
    }
    Some(visible_position)
}

fn visible_cell_interval(
    position: i32,
    grid_size: u32,
    pixel_size: u32,
    source_size: u32,
    clip: Option<u16>,
) -> Option<(i64, i64, u16)> {
    let start = i64::from(position).max(0);
    let end = (i64::from(position) + i64::from(grid_size)).min(clip.map_or(i64::MAX, i64::from));
    if start >= end || grid_size == 0 || pixel_size == 0 || source_size == 0 {
        return None;
    }
    Some((start, end, u16::try_from(start).ok()?))
}

fn visible_pixel_interval(
    position: i32,
    start: i64,
    end: i64,
    pixel_size: u32,
    cell_size: u32,
    offset: u32,
) -> Option<(u64, u64, u32)> {
    if cell_size == 0 {
        return None;
    }
    let offset_px = offset.min(cell_size - 1);
    let skipped_cells = u64::try_from(start - i64::from(position)).ok()?;
    let skipped_pixels = skipped_cells * u64::from(cell_size);
    let first = skipped_pixels.saturating_sub(u64::from(offset_px));
    let visible_offset = u32::try_from(u64::from(offset_px).saturating_sub(skipped_pixels)).ok()?;
    let available = u64::try_from(end - start).ok()? * u64::from(cell_size);
    let last = (skipped_pixels + available)
        .saturating_sub(u64::from(offset_px))
        .min(u64::from(pixel_size));
    if first >= last {
        return None;
    }
    Some((first, last, visible_offset))
}

fn mapped_source_interval(
    first: u64,
    last: u64,
    pixel_size: u32,
    source_start: u32,
    source_size: u32,
) -> Option<(u32, u32)> {
    let first = first * u64::from(source_size) / u64::from(pixel_size);
    let last = (last * u64::from(source_size)).div_ceil(u64::from(pixel_size));
    Some((
        source_start.checked_add(u32::try_from(first).ok()?)?,
        u32::try_from(last - first).ok()?,
    ))
}

/// Replace pixels and retire old placements before replaying the current set.
fn write_image_apc(out: &mut impl Write, image: &Image<'_>) -> Result<bool, KittyReplayError> {
    let Some((format, data)) = image_payload(image)? else {
        return Ok(false);
    };
    let compressed = (format != PNG_FORMAT).then(|| zlib(data)).flatten();
    let header = ImageHeader {
        format,
        id: image.id()?,
        width: image.width()?,
        height: image.height()?,
        compressed: compressed.is_some(),
    };
    // Anonymous p=0 placements cannot be overwritten by ID.
    write_delete_image(out, header.id)?;
    write_image_chunks(out, &header, compressed.as_deref().unwrap_or(data))?;
    Ok(true)
}

/// Delete image `id` with its placements and pixels (`d=I`).
fn write_delete_image(out: &mut impl Write, id: u32) -> io::Result<()> {
    write!(out, "\x1b_Ga=d,d=I,i={id},q=2;\x1b\\")
}

/// Delete image `id`'s placements but keep its pixels resident (`d=i`).
fn write_delete_placements(out: &mut impl Write, id: u32) -> io::Result<()> {
    write!(out, "\x1b_Ga=d,d=i,i={id},q=2;\x1b\\")
}

/// `data` deflated into a zlib stream (Kitty's `o=z`), or `None` when that
/// would not shrink it. libghostty stores only decoded pixels, so raw RGB(A)
/// payloads are the norm and usually compress well.
fn zlib(data: &[u8]) -> Option<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(data).ok()?;
    let deflated = encoder.finish().ok()?;
    (deflated.len() < data.len()).then_some(deflated)
}

struct ImageHeader {
    format: u32,
    id: u32,
    width: u32,
    height: u32,
    compressed: bool,
}

fn image_payload<'a>(
    image: &'a Image<'_>,
) -> Result<Option<(u32, &'a [u8])>, libghostty_vt::Error> {
    let Some(format) = kitty_format_code(image.format()?) else {
        return Ok(None);
    };
    if !matches!(image.compression()?, Compression::None) {
        return Ok(None);
    }
    let Some(data) = image.data()? else {
        return Ok(None);
    };
    if data.is_empty() {
        return Ok(None);
    }
    Ok(Some((format, data)))
}

fn write_image_chunks(out: &mut impl Write, header: &ImageHeader, data: &[u8]) -> io::Result<()> {
    let mut first = true;
    let mut offset = 0usize;
    while offset < data.len() {
        let end = (offset + 3_072).min(data.len());
        let more = u8::from(end < data.len());
        if first {
            write!(
                out,
                "\x1b_Ga=t,f={},s={},v={},i={},{}q=2,m={more};",
                header.format,
                header.width,
                header.height,
                header.id,
                if header.compressed { "o=z," } else { "" },
            )?;
            first = false;
        } else {
            write!(out, "\x1b_Gm={more};")?;
        }
        write_base64(out, &data[offset..end])?;
        out.write_all(b"\x1b\\")?;
        offset = end;
    }
    Ok(())
}

const PNG_FORMAT: u32 = 100;

const fn kitty_format_code(format: ImageFormat) -> Option<u32> {
    match format {
        ImageFormat::Rgb => Some(24),
        ImageFormat::Rgba => Some(32),
        ImageFormat::Png => Some(PNG_FORMAT),
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

mod residency;

pub use residency::KittyResidency;

#[cfg(test)]
mod tests;
