//! Immutable pixels and upstream-resolved image placements from one render snapshot.

use crate::publication::{ImageData, ImagePlacement};
use libghostty_vt::{
    Terminal,
    kitty::graphics::{Graphics, Image, ImageFormat, PlacementIterator},
    render::{ImageIteration, ImageIterator, ImageProtocol, KittyPlacementKind, Snapshot},
};
use std::{collections::HashMap, sync::Arc};

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(super) struct Images {
    // Image generations are process-wide, including screen/epoch replacement.
    pixels: HashMap<u64, (u32, Arc<ImageData>)>,
    iterator: Option<ImageIterator<'static>>,
    crop_iterator: Option<PlacementIterator<'static>>,
    crops: HashMap<(u32, u32), [f64; 4]>,
}

impl Images {
    fn image(&mut self, image: &Image<'_>) -> Result<Option<Arc<ImageData>>, libghostty_vt::Error> {
        let generation = image.generation()?;
        if let Some((_, data)) = self.pixels.get(&generation) {
            return Ok(Some(Arc::clone(data)));
        }
        let Some(data) = decode_image(image)? else {
            return Ok(None);
        };
        self.pixels
            .insert(generation, (image.id()?, Arc::clone(&data)));
        Ok(Some(data))
    }

    pub(super) fn project(
        &mut self,
        terminal: &Terminal<'_, '_>,
        snapshot: &Snapshot<'static, '_>,
    ) -> Result<Vec<ImagePlacement>, libghostty_vt::Error> {
        let Some(graphics) = terminal.kitty_graphics()? else {
            self.pixels.clear();
            return Ok(Vec::new());
        };
        let cell = cell_pixels(terminal)?;
        self.read_crops(&graphics)?;
        let mut iterator = match self.iterator.take() {
            Some(iterator) => iterator,
            None => ImageIterator::new()?,
        };
        let output = self.placements(&graphics, &mut iterator, snapshot, cell);
        self.iterator = Some(iterator);
        self.pixels.retain(|generation, (id, _)| {
            graphics
                .image(*id)
                .is_some_and(|image| image.generation().ok() == Some(*generation))
        });
        output
    }

    fn read_crops(&mut self, graphics: &Graphics<'_>) -> Result<(), libghostty_vt::Error> {
        let mut iterator = match self.crop_iterator.take() {
            Some(iterator) => iterator,
            None => PlacementIterator::new()?,
        };
        let result = collect_crops(graphics, &mut iterator, &mut self.crops);
        self.crop_iterator = Some(iterator);
        result
    }

    fn sample_source(
        &self,
        placement: &ImageIteration<'_, '_>,
        id: u32,
    ) -> Result<Option<[f64; 4]>, libghostty_vt::Error> {
        if !matches!(placement.kitty_kind()?, Some(KittyPlacementKind::Virtual)) {
            return Ok(None);
        }
        Ok(self
            .crops
            .get(&(id, placement.kitty_placement_id()?.unwrap_or(0)))
            .copied())
    }

    fn placements(
        &mut self,
        graphics: &Graphics<'_>,
        iterator: &mut ImageIterator<'static>,
        snapshot: &Snapshot<'static, '_>,
        cell: (f64, f64),
    ) -> Result<Vec<ImagePlacement>, libghostty_vt::Error> {
        let mut output = Vec::new();
        let mut placements = iterator.update(snapshot)?;
        while let Some(placement) = placements.next() {
            // Future image protocols are not Kitty storage entries.
            if !matches!(placement.protocol(), Ok(ImageProtocol::Kitty)) {
                continue;
            }
            let id = placement.image_id()?;
            let Some(image) = graphics.image(id) else {
                continue;
            };
            let Some(pixels) = self.image(&image)? else {
                continue;
            };
            let key = paint_order(placement, id)?;
            let crop = self.sample_source(placement, id)?;
            output.push((key, project_placement(placement, pixels, cell, crop)?));
        }
        output.sort_by_key(|(key, _)| *key);
        Ok(output.into_iter().map(|(_, placement)| placement).collect())
    }
}

fn paint_order(
    placement: &ImageIteration<'_, '_>,
    id: u32,
) -> Result<(i32, i32, u32, Option<u32>), libghostty_vt::Error> {
    Ok((
        placement.layer()? as i32,
        placement.z()?,
        id,
        placement.kitty_placement_id()?,
    ))
}

fn cell_pixels(terminal: &Terminal<'_, '_>) -> Result<(f64, f64), libghostty_vt::Error> {
    Ok((
        f64::from((terminal.width_px()? / u32::from(terminal.cols()?.max(1))).max(1)),
        f64::from((terminal.height_px()? / u32::from(terminal.rows()?.max(1))).max(1)),
    ))
}

fn project_placement(
    placement: &ImageIteration<'_, '_>,
    image: Arc<ImageData>,
    cell: (f64, f64),
    sample_source: Option<[f64; 4]>,
) -> Result<ImagePlacement, libghostty_vt::Error> {
    let source = source_rect(placement)?;
    Ok(ImagePlacement {
        image,
        col: f64::from(placement.viewport_col()?) + f64::from(placement.cell_offset_x()?) / cell.0,
        row: f64::from(placement.viewport_row()?) + f64::from(placement.cell_offset_y()?) / cell.1,
        cols: f64::from(placement.pixel_width()?) / cell.0,
        rows: f64::from(placement.pixel_height()?) / cell.1,
        source,
        sample_source: sample_source.unwrap_or(source),
        z: placement.z()?,
    })
}

/// Preserve the original filtering crop without recomputing placement geometry.
/// A virtual run's resolved source may be only a slice of this crop.
fn collect_crops(
    graphics: &Graphics<'_>,
    iterator: &mut PlacementIterator<'_>,
    crops: &mut HashMap<(u32, u32), [f64; 4]>,
) -> Result<(), libghostty_vt::Error> {
    crops.clear();
    let mut placements = iterator.update(graphics)?;
    while let Some(placement) = placements.next() {
        let id = placement.image_id()?;
        record_crop(graphics, placement, id, crops)?;
    }
    Ok(())
}

fn record_crop(
    graphics: &Graphics<'_>,
    placement: &libghostty_vt::kitty::graphics::PlacementIteration<'_, '_>,
    id: u32,
    crops: &mut HashMap<(u32, u32), [f64; 4]>,
) -> Result<(), libghostty_vt::Error> {
    if !placement.is_virtual()? {
        return Ok(());
    }
    let Some(image) = graphics.image(id) else {
        return Ok(());
    };
    let source = placement.source_rect(&image)?;
    let source = [source.x, source.y, source.width, source.height].map(f64::from);
    if placement.is_external()? {
        crops.insert((id, placement.placement_id()?), source);
    }
    if placement.is_placeholder_default()? {
        crops.insert((id, 0), source);
    }
    Ok(())
}

fn source_rect(placement: &ImageIteration<'_, '_>) -> Result<[f64; 4], libghostty_vt::Error> {
    Ok([
        placement.source_x()?,
        placement.source_y()?,
        placement.source_width()?,
        placement.source_height()?,
    ]
    .map(f64::from))
}

fn decode_image(image: &Image<'_>) -> Result<Option<Arc<ImageData>>, libghostty_vt::Error> {
    let Some(bytes) = image.data()? else {
        return Ok(None);
    };
    let (width, height) = (image.width()?, image.height()?);
    let Some(count) = (width as usize).checked_mul(height as usize) else {
        return Ok(None);
    };
    // RGB storage may use the full engine budget before adding alpha.
    let limit = usize::try_from(phux_protocol::kitty_replay::KITTY_IMAGE_STORAGE_LIMIT_BYTES)
        .map_err(|_| libghostty_vt::Error::OutOfMemory)?;
    if count > limit / 3 {
        return Ok(None);
    }
    let Some(rgba) = rgba_pixels(image.format()?, bytes, count) else {
        return Ok(None);
    };
    Ok(Some(Arc::new(ImageData {
        width,
        height,
        rgba,
    })))
}

fn rgba_pixels(format: ImageFormat, bytes: &[u8], count: usize) -> Option<Arc<[u8]>> {
    match format {
        ImageFormat::Rgba if bytes.len() == count * 4 => Some(Arc::from(bytes)),
        ImageFormat::Rgb if bytes.len() == count * 3 => {
            let mut rgba = Vec::with_capacity(count * 4);
            for rgb in bytes.as_chunks::<3>().0 {
                rgba.extend_from_slice(rgb);
                rgba.push(255);
            }
            Some(rgba.into())
        }
        _ => None,
    }
}
