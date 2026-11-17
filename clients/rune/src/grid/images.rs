//! A pane's Kitty images, from the placements the runtime resolved
//! (positions, crops, z order, Unicode placeholders, deletion).
//!
//! Each terminal image keeps one texture, refreshed only when its content
//! changes, so a still image uploads once and an animation's frames reuse
//! their atlas slot instead of allocating one per frame.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Weak};

use phux_client_runtime::publication::{GridFrame, ImageData, ImagePlacement};
use phux_protocol::ResourceId;
use rune::{El, Pixels, el};

use super::Metrics;

/// The Unicode placeholder (U+10EEEE) a virtual placement's cells print.
pub(super) const PLACEHOLDER: char = '\u{10EEEE}';

/// A texture and the content it was last given.
struct Texture {
    content: Weak<ImageData>,
    pixels: Pixels,
}

thread_local! {
    static TEXTURES: RefCell<HashMap<(ResourceId, u32), Texture>> = RefCell::default();
}

/// The images under the text (`z < 0`) and over it, each a layer clipped to
/// the grid; `None` for a layer with nothing in it.
///
/// ponytail: Kitty puts `-2^30 <= z < 0` over non-default cell backgrounds;
/// here every negative z sits under them, which matters only where a program
/// paints both. Draw that band between the row backgrounds and text if a
/// program depends on it.
pub(super) fn layers(frame: &GridFrame, m: &Metrics) -> (Option<El>, Option<El>) {
    let size = (
        f32::from(frame.cols) * m.cell_w,
        f32::from(frame.rows) * m.cell_h,
    );
    TEXTURES.with_borrow_mut(|textures| {
        textures.retain(|(terminal, id), _| {
            *terminal != frame.terminal_id || frame.images.iter().any(|p| p.image_id == *id)
        });
        let mut under = Vec::new();
        let mut over = Vec::new();
        for placement in &frame.images {
            let pixels = texture(textures, &frame.terminal_id, placement);
            if let Some(image) = image_el(placement, &pixels, m) {
                if placement.z < 0 {
                    &mut under
                } else {
                    &mut over
                }
                .push(image);
            }
        }
        let layer = |images: Vec<El>| {
            (!images.is_empty()).then(|| {
                el().absolute(0.0, 0.0)
                    .size(size.0, size.1)
                    .clip_rounded(0.0)
                    .children(images)
            })
        };
        (layer(under), layer(over))
    })
}

/// `placement`'s texture, given its content when that changed.
fn texture(
    textures: &mut HashMap<(ResourceId, u32), Texture>,
    terminal: &ResourceId,
    placement: &ImagePlacement,
) -> Pixels {
    let texture = textures
        .entry((terminal.clone(), placement.image_id))
        .or_insert_with(|| Texture {
            content: Weak::new(),
            pixels: Pixels::new(),
        });
    if !Weak::ptr_eq(&texture.content, &Arc::downgrade(&placement.image)) {
        let image = &placement.image;
        crate::perf::IMAGE_UPLOADS.incr();
        texture
            .pixels
            .set(image.width, image.height, image.rgba.to_vec());
        texture.content = Arc::downgrade(image);
    }
    texture.pixels.clone()
}

/// One placement: its source rectangle scaled onto its cells. A crop (or a
/// placeholder cell's slice) shows through a clip the size of the
/// destination.
fn image_el(placement: &ImagePlacement, pixels: &Pixels, m: &Metrics) -> Option<El> {
    let [sx, sy, sw, sh] = placement.source.map(|v| v as f32);
    let (width, height) = (placement.image.width as f32, placement.image.height as f32);
    if sw <= 0.0 || sh <= 0.0 || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let (dw, dh) = (
        placement.cols as f32 * m.cell_w,
        placement.rows as f32 * m.cell_h,
    );
    let at = el()
        .absolute(
            placement.col as f32 * m.cell_w,
            placement.row as f32 * m.cell_h,
        )
        .size(dw, dh);
    if sx == 0.0 && sy == 0.0 && sw == width && sh == height {
        return Some(at.pixels(pixels));
    }
    let (kx, ky) = (dw / sw, dh / sh);
    let whole = el()
        .absolute(-sx * kx, -sy * ky)
        .size(width * kx, height * ky)
        .pixels(pixels);
    Some(at.clip_rounded(0.0).child(whole))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod tests {
    use crate::test_support::{SCALE, SIZE, input_fixture_showing};
    use phux_protocol::ResourceId;

    /// A one-pixel red image placed over 40x20 cells.
    const DIRECT: &str = "\x1b[1;1H\x1b_Ga=T,f=32,s=1,v=1,i=1,c=40,r=20,C=1,q=2;/wAA/w==\x1b\\";

    /// The pixel at `cell` of the pane, in device pixels, as RGBA.
    fn pixel_at(direct: &str, cell: (f32, f32)) -> [u8; 4] {
        let (_, mut rt) = input_fixture_showing(direct);
        rt.frame(0.0);
        rt.frame(1.0 / 60.0);
        let gpu = rune::render::Gpu::headless();
        let mut renderer = rune::render::Renderer::new(&gpu.device, rune::render::OFFSCREEN_FORMAT);
        let size = ((SIZE.0 * SCALE) as u32, (SIZE.1 * SCALE) as u32);
        let rgba = crate::ui::offscreen(&gpu, &mut renderer, &mut rt, size);
        let [x, y] = rt.state.connection.origins[&ResourceId::local(1)];
        let m = rt.state.metrics();
        let at = (
            ((x + cell.0 * m.cell_w) * SCALE) as usize,
            ((y + cell.1 * m.cell_h) * SCALE) as usize,
        );
        let i = (at.1 * size.0 as usize + at.0) * 4;
        rgba[i..i + 4].try_into().unwrap()
    }

    fn red(pixel: [u8; 4]) -> bool {
        pixel[0] > 200 && pixel[1] < 60 && pixel[2] < 60
    }

    #[test]
    fn a_kitty_image_is_drawn_over_its_cells_and_nowhere_else() {
        assert!(red(pixel_at(DIRECT, (20.0, 10.0))), "inside the image");
        assert!(!red(pixel_at(DIRECT, (60.0, 30.0))), "outside it");
    }

    /// A virtual placement shows where a program prints its placeholder
    /// cells (the id in the foreground colour), as tmux-safe tools do.
    #[test]
    fn unicode_placeholder_cells_show_their_image() {
        let virt = concat!(
            "\x1b_Ga=T,U=1,f=32,s=1,v=1,i=1,c=4,r=2,q=2;/wAA/w==\x1b\\",
            "\x1b[3;1H\x1b[38;5;1m",
            "\u{10EEEE}\u{305}\u{305}\u{10EEEE}\u{305}\u{30D}",
            "\u{10EEEE}\u{305}\u{30E}\u{10EEEE}\u{305}\u{310}",
            "\x1b[m",
        );
        assert!(red(pixel_at(virt, (1.5, 2.5))), "the placeholder cells");
        assert!(!red(pixel_at(virt, (10.0, 2.5))), "past the run");
    }
}
