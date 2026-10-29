//! Immutable image snapshots from the active libghostty screen.

use crate::publication::{GridBuffer, ImageData, ImagePlacement};
use libghostty_vt::{
    Terminal,
    kitty::graphics::{Graphics, Image, ImageFormat, PlacementIteration, PlacementIterator},
};
use std::{collections::HashMap, sync::Arc};

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(super) struct Images {
    // Image generations are process-wide, including screen/epoch replacement.
    pixels: HashMap<u64, (u32, Arc<ImageData>)>,
    iterator: Option<PlacementIterator<'static>>,
}

type OrderedPlacement = (i32, u32, u32, ImagePlacement);

struct Projection<'a> {
    buffer: &'a GridBuffer,
    cols: u16,
    rows: u16,
    cell_width: u32,
    cell_height: u32,
    offset: u32,
    output: Vec<OrderedPlacement>,
}

struct PlacedImage<'a> {
    image: Image<'a>,
    id: u32,
    placement_id: u32,
    z: i32,
    placeholder_default: bool,
    external: bool,
    source: [f64; 4],
    virtual_placement: bool,
}

#[derive(Clone, Copy)]
struct Placeholder {
    row: u32,
    col: u32,
    high: u32,
    image: u32,
    placement: u32,
}

struct VirtualGrid {
    cols: u32,
    rows: u32,
    scale: f64,
    extent: [f64; 2],
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
        buffer: &GridBuffer,
        cols: u16,
        rows: u16,
    ) -> Result<Vec<ImagePlacement>, libghostty_vt::Error> {
        let graphics = terminal.kitty_graphics()?;
        if graphics.generation()? == 0 {
            self.pixels.clear();
            return Ok(Vec::new());
        }
        let mut iter = match self.iterator.take() {
            Some(iter) => iter,
            None => PlacementIterator::new()?,
        };
        let mut grid = Projection::new(terminal, buffer, cols, rows)?;
        let mut placements = iter.update(&graphics)?;
        while let Some(placement) = placements.next() {
            self.project_placement(terminal, &graphics, placement, &mut grid)?;
        }
        self.iterator = Some(iter);
        self.pixels.retain(|generation, (id, _)| {
            graphics
                .image(*id)
                .is_some_and(|image| image.generation().ok() == Some(*generation))
        });
        grid.output.sort_by_key(|entry| (entry.0, entry.1, entry.2));
        Ok(grid.output.into_iter().map(|entry| entry.3).collect())
    }

    fn project_placement(
        &mut self,
        terminal: &Terminal<'_, '_>,
        graphics: &Graphics<'_>,
        placement: &PlacementIteration<'_, '_>,
        grid: &mut Projection<'_>,
    ) -> Result<(), libghostty_vt::Error> {
        let id = placement.image_id()?;
        let Some(image) = graphics.image(id) else {
            return Ok(());
        };
        let image = PlacedImage::new(image, id, placement)?;
        if image.virtual_placement {
            self.project_virtual(terminal, placement, &image, grid)
        } else {
            self.project_classic(terminal, placement, &image, grid)
        }
    }

    fn project_classic(
        &mut self,
        terminal: &Terminal<'_, '_>,
        placement: &PlacementIteration<'_, '_>,
        image: &PlacedImage<'_>,
        grid: &mut Projection<'_>,
    ) -> Result<(), libghostty_vt::Error> {
        let Some([col, row, cols, rows]) = grid.classic_rect(terminal, placement, image)? else {
            return Ok(());
        };
        let Some(data) = self.image(&image.image)? else {
            return Ok(());
        };
        grid.push(
            image,
            ImagePlacement {
                image: data,
                col,
                row,
                cols,
                rows,
                source: image.source,
                sample_source: image.source,
                z: image.z,
            },
        );
        Ok(())
    }

    fn project_virtual(
        &mut self,
        terminal: &Terminal<'_, '_>,
        placement: &PlacementIteration<'_, '_>,
        image: &PlacedImage<'_>,
        grid: &mut Projection<'_>,
    ) -> Result<(), libghostty_vt::Error> {
        let Some(virtual_grid) = VirtualGrid::new(placement, image, grid)? else {
            return Ok(());
        };
        let mut data = None;
        for y in 0..=grid.rows {
            let cells = grid.row(y);
            let history_y = grid.offset.checked_add(u32::from(y));
            let mut previous = None;
            for (x, cell) in (0..grid.cols).zip(cells) {
                let start = cell.utf8_offset as usize;
                let text = &grid.buffer.utf8[start..start + usize::from(cell.utf8_len)];
                let placeholder = Placeholder::read(terminal, x, history_y, text, previous)?;
                previous = placeholder;
                let Some(placeholder) = placeholder else {
                    continue;
                };
                self.project_placeholder(
                    image,
                    &virtual_grid,
                    &placeholder,
                    (x, y),
                    &mut data,
                    grid,
                )?;
            }
        }
        Ok(())
    }

    fn project_placeholder(
        &mut self,
        image: &PlacedImage<'_>,
        virtual_grid: &VirtualGrid,
        placeholder: &Placeholder,
        (x, y): (u16, u16),
        data: &mut Option<Arc<ImageData>>,
        grid: &mut Projection<'_>,
    ) -> Result<(), libghostty_vt::Error> {
        if !placeholder.matches(image, virtual_grid.cols, virtual_grid.rows) {
            return Ok(());
        }
        let width = (virtual_grid.extent[0] - f64::from(placeholder.col)).clamp(0.0, 1.0);
        let height = (virtual_grid.extent[1] - f64::from(placeholder.row)).clamp(0.0, 1.0);
        if width == 0.0 || height == 0.0 {
            return Ok(());
        }
        if data.is_none() {
            *data = self.image(&image.image)?;
        }
        let Some(data) = data else {
            return Ok(());
        };
        let scale = virtual_grid.scale;
        grid.push(
            image,
            ImagePlacement {
                image: Arc::clone(data),
                col: f64::from(x),
                row: f64::from(y),
                cols: width,
                rows: height,
                source: [
                    image.source[0] + grid.width() * f64::from(placeholder.col) / scale,
                    image.source[1] + grid.height() * f64::from(placeholder.row) / scale,
                    grid.width() * width / scale,
                    grid.height() * height / scale,
                ],
                sample_source: image.source,
                z: image.z,
            },
        );
        Ok(())
    }
}

impl<'a> Projection<'a> {
    fn new(
        terminal: &Terminal<'_, '_>,
        buffer: &'a GridBuffer,
        cols: u16,
        rows: u16,
    ) -> Result<Self, libghostty_vt::Error> {
        Ok(Self {
            buffer,
            cols,
            rows,
            cell_width: (terminal.width_px()? / u32::from(cols.max(1))).max(1),
            cell_height: (terminal.height_px()? / u32::from(rows.max(1))).max(1),
            offset: u32::try_from(terminal.scrollbar()?.offset)
                .map_err(|_| libghostty_vt::Error::InvalidValue)?,
            output: Vec::new(),
        })
    }

    fn classic_rect(
        &self,
        terminal: &Terminal<'_, '_>,
        placement: &PlacementIteration<'_, '_>,
        image: &PlacedImage<'_>,
    ) -> Result<Option<[f64; 4]>, libghostty_vt::Error> {
        let rect = placement.rect(&image.image, terminal)?;
        let Some(origin) = terminal
            .point_from_grid_ref(&rect.start(), libghostty_vt::terminal::PointSpace::History)?
        else {
            return Ok(None);
        };
        let x_offset = f64::from(placement.x_offset()?.min(self.cell_width - 1));
        let y_offset = f64::from(placement.y_offset()?.min(self.cell_height - 1));
        let row = f64::from(origin.y) - f64::from(self.offset) + y_offset / self.height();
        let size = placement.pixel_size(&image.image, terminal)?;
        let height = f64::from(size.height) / self.height();
        if row + height <= 0.0
            || row >= f64::from(self.rows) + f64::from(u8::from(!self.buffer.fringe.is_empty()))
        {
            return Ok(None);
        }
        Ok(Some([
            f64::from(origin.x) + x_offset / self.width(),
            row,
            f64::from(size.width) / self.width(),
            height,
        ]))
    }

    fn virtual_dimensions(
        &self,
        placement: &PlacementIteration<'_, '_>,
        image: &Image<'_>,
    ) -> Result<(u32, u32), libghostty_vt::Error> {
        let cols = match placement.columns()? {
            0 => image.width()?.div_ceil(self.cell_width),
            cols => cols,
        };
        let rows = match placement.rows()? {
            0 => image.height()?.div_ceil(self.cell_height),
            rows => rows,
        };
        Ok((cols, rows))
    }

    fn width(&self) -> f64 {
        f64::from(self.cell_width)
    }
    fn height(&self) -> f64 {
        f64::from(self.cell_height)
    }

    fn row(&self, row: u16) -> &'a [crate::publication::Cell] {
        if row == self.rows {
            return &self.buffer.fringe;
        }
        let start = usize::from(row) * usize::from(self.cols);
        &self.buffer.cells[start..start + usize::from(self.cols)]
    }

    fn push(&mut self, image: &PlacedImage<'_>, placement: ImagePlacement) {
        self.output
            .push((image.z, image.id, image.placement_id, placement));
    }
}

impl<'a> PlacedImage<'a> {
    fn new(
        image: Image<'a>,
        id: u32,
        placement: &PlacementIteration<'_, '_>,
    ) -> Result<Self, libghostty_vt::Error> {
        let source = placement.source_rect(&image)?;
        let virtual_placement = placement.is_virtual()?;
        Ok(Self {
            image,
            id,
            virtual_placement,
            placement_id: placement.placement_id()?,
            z: placement.z()?,
            placeholder_default: virtual_placement && placement.is_placeholder_default()?,
            external: placement.is_external()?,
            source: [source.x, source.y, source.width, source.height].map(f64::from),
        })
    }
}

impl VirtualGrid {
    fn new(
        placement: &PlacementIteration<'_, '_>,
        image: &PlacedImage<'_>,
        grid: &Projection<'_>,
    ) -> Result<Option<Self>, libghostty_vt::Error> {
        let (cols, rows) = grid.virtual_dimensions(placement, &image.image)?;
        if cols == 0
            || rows == 0
            || cols > u32::from(u16::MAX)
            || rows > u32::from(u16::MAX)
            || image.source[2] <= 0.0
            || image.source[3] <= 0.0
        {
            return Ok(None);
        }
        // Virtual prototypes preserve aspect ratio within their resolved grid.
        let scale = (f64::from(cols) * grid.width() / image.source[2])
            .min(f64::from(rows) * grid.height() / image.source[3]);
        if scale <= 0.0 {
            return Ok(None);
        }
        Ok(Some(Self {
            cols,
            rows,
            scale,
            extent: [
                image.source[2] * scale / grid.width(),
                image.source[3] * scale / grid.height(),
            ],
        }))
    }
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

impl Placeholder {
    fn read(
        terminal: &Terminal<'_, '_>,
        x: u16,
        history_y: Option<u32>,
        text: &[u8],
        previous: Option<Self>,
    ) -> Result<Option<Self>, libghostty_vt::Error> {
        if !text.starts_with("\u{10eeee}".as_bytes()) {
            return Ok(None);
        }
        let style = terminal
            .grid_ref(libghostty_vt::terminal::Point::History(
                libghostty_vt::terminal::PointCoordinate {
                    x,
                    y: history_y.ok_or(libghostty_vt::Error::InvalidValue)?,
                },
            ))?
            .style()?;
        Ok(Some(Self::resolve(text, style, previous)))
    }

    fn resolve(text: &[u8], style: libghostty_vt::style::Style, previous: Option<Self>) -> Self {
        let image = color_id(style.fg_color);
        let placement = color_id(style.underline_color);
        let previous = previous.filter(|p| p.image == image && p.placement == placement);
        let text = std::str::from_utf8(text).unwrap_or("");
        let mut marks = text.chars().skip(1).take(3).map(diacritic);
        let row = marks
            .next()
            .flatten()
            .unwrap_or_else(|| previous.map_or(0, |p| p.row));
        let col = marks
            .next()
            .flatten()
            .unwrap_or_else(|| previous.filter(|p| p.row == row).map_or(0, |p| p.col + 1));
        let high = marks.next().flatten().unwrap_or_else(|| {
            previous
                .filter(|p| p.row == row && p.col + 1 == col)
                .map_or(0, |p| p.high)
        });
        Self {
            row,
            col,
            high,
            image,
            placement,
        }
    }

    const fn matches(&self, image: &PlacedImage<'_>, cols: u32, rows: u32) -> bool {
        self.high <= 255
            && self.image | (self.high << 24) == image.id
            && (if self.placement == 0 {
                image.placeholder_default
            } else {
                image.external && self.placement == image.placement_id
            })
            && self.col < cols
            && self.row < rows
    }
}

fn color_id(color: libghostty_vt::style::StyleColor) -> u32 {
    match color {
        libghostty_vt::style::StyleColor::None => 0,
        libghostty_vt::style::StyleColor::Palette(index) => u32::from(index.0),
        libghostty_vt::style::StyleColor::Rgb(rgb) => {
            (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b)
        }
    }
}

// Kitty's stable Unicode-6 row/column encoding (not Unicode canonical order).
fn diacritic(c: char) -> Option<u32> {
    DIACRITICS
        .binary_search(&(c as u32))
        .ok()
        .and_then(|i| u32::try_from(i).ok())
}

const DIACRITICS: &[u32] = &[
    0x0305, 0x030d, 0x030e, 0x0310, 0x0312, 0x033d, 0x033e, 0x033f, 0x0346, 0x034a, 0x034b, 0x034c,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035b, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036a, 0x036b, 0x036c, 0x036d, 0x036e, 0x036f, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059c, 0x059d, 0x059e, 0x059f, 0x05a0, 0x05a1,
    0x05a8, 0x05a9, 0x05ab, 0x05ac, 0x05af, 0x05c4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065a, 0x065b, 0x065d, 0x065e, 0x06d6, 0x06d7, 0x06d8,
    0x06d9, 0x06da, 0x06db, 0x06dc, 0x06df, 0x06e0, 0x06e1, 0x06e2, 0x06e4, 0x06e7, 0x06e8, 0x06eb,
    0x06ec, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073a, 0x073d, 0x073f, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074a, 0x07eb, 0x07ec, 0x07ed, 0x07ee, 0x07ef, 0x07f0, 0x07f1, 0x07f3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081b, 0x081c, 0x081d, 0x081e, 0x081f, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082a, 0x082b, 0x082c, 0x082d, 0x0951, 0x0953, 0x0954,
    0x0f82, 0x0f83, 0x0f86, 0x0f87, 0x135d, 0x135e, 0x135f, 0x17dd, 0x193a, 0x1a17, 0x1a75, 0x1a76,
    0x1a77, 0x1a78, 0x1a79, 0x1a7a, 0x1a7b, 0x1a7c, 0x1b6b, 0x1b6d, 0x1b6e, 0x1b6f, 0x1b70, 0x1b71,
    0x1b72, 0x1b73, 0x1cd0, 0x1cd1, 0x1cd2, 0x1cda, 0x1cdb, 0x1ce0, 0x1dc0, 0x1dc1, 0x1dc3, 0x1dc4,
    0x1dc5, 0x1dc6, 0x1dc7, 0x1dc8, 0x1dc9, 0x1dcb, 0x1dcc, 0x1dd1, 0x1dd2, 0x1dd3, 0x1dd4, 0x1dd5,
    0x1dd6, 0x1dd7, 0x1dd8, 0x1dd9, 0x1dda, 0x1ddb, 0x1ddc, 0x1ddd, 0x1dde, 0x1ddf, 0x1de0, 0x1de1,
    0x1de2, 0x1de3, 0x1de4, 0x1de5, 0x1de6, 0x1dfe, 0x20d0, 0x20d1, 0x20d4, 0x20d5, 0x20d6, 0x20d7,
    0x20db, 0x20dc, 0x20e1, 0x20e7, 0x20e9, 0x20f0, 0x2cef, 0x2cf0, 0x2cf1, 0x2de0, 0x2de1, 0x2de2,
    0x2de3, 0x2de4, 0x2de5, 0x2de6, 0x2de7, 0x2de8, 0x2de9, 0x2dea, 0x2deb, 0x2dec, 0x2ded, 0x2dee,
    0x2def, 0x2df0, 0x2df1, 0x2df2, 0x2df3, 0x2df4, 0x2df5, 0x2df6, 0x2df7, 0x2df8, 0x2df9, 0x2dfa,
    0x2dfb, 0x2dfc, 0x2dfd, 0x2dfe, 0x2dff, 0xa66f, 0xa67c, 0xa67d, 0xa6f0, 0xa6f1, 0xa8e0, 0xa8e1,
    0xa8e2, 0xa8e3, 0xa8e4, 0xa8e5, 0xa8e6, 0xa8e7, 0xa8e8, 0xa8e9, 0xa8ea, 0xa8eb, 0xa8ec, 0xa8ed,
    0xa8ee, 0xa8ef, 0xa8f0, 0xa8f1, 0xaab0, 0xaab2, 0xaab3, 0xaab7, 0xaab8, 0xaabe, 0xaabf, 0xaac1,
    0xfe20, 0xfe21, 0xfe22, 0xfe23, 0xfe24, 0xfe25, 0xfe26, 0x10a0f, 0x10a38, 0x1d185, 0x1d186,
    0x1d187, 0x1d188, 0x1d189, 0x1d1aa, 0x1d1ab, 0x1d1ac, 0x1d1ad, 0x1d242, 0x1d243, 0x1d244,
];
