//! Immutable Kitty pixels become one GPUI texture per content identity.

use super::*;
use phux_client_runtime::publication::ImageData;

struct Texture {
    source: Arc<ImageData>,
    rendered: Arc<gpui::RenderImage>,
}

#[derive(Default)]
pub(super) struct Images {
    textures: Vec<Texture>,
}

#[derive(Clone, Copy)]
pub(super) enum Layer {
    BelowBackground,
    BelowText,
    AboveText,
}

impl Layer {
    fn contains(self, z: i32) -> bool {
        match self {
            Self::BelowBackground => z < -1_073_741_824,
            Self::BelowText => (-1_073_741_824..0).contains(&z),
            Self::AboveText => z >= 0,
        }
    }
}

/// Keep textures across row patches, font changes, and viewport resizing. The
/// entity's release hook evicts GPU atlas entries when the terminal disappears.
pub(super) fn prepare(scene: &mut Scene, window: &mut gpui::Window, cx: &mut gpui::App) {
    if scene.images.is_none() && scene.frame.images.is_empty() {
        return;
    }
    let images = scene.images.get_or_insert_with(|| {
        let images = cx.new(|_| Images::default());
        cx.observe_release(&images, |images, cx| {
            for texture in images.textures.drain(..) {
                cx.drop_image(texture.rendered, None);
            }
        })
        .detach();
        images
    });
    images.update(cx, |images, cx| {
        images.textures.retain(|texture| {
            if scene
                .frame
                .images
                .iter()
                .any(|placement| Arc::ptr_eq(&placement.image, &texture.source))
            {
                true
            } else {
                cx.drop_image(Arc::clone(&texture.rendered), Some(window));
                false
            }
        });
        for placement in &scene.frame.images {
            if images
                .textures
                .iter()
                .any(|texture| Arc::ptr_eq(&texture.source, &placement.image))
            {
                continue;
            }
            // GPUI's atlas takes straight-alpha BGRA. Swizzle exactly once;
            // neither cached redraws nor new placements copy image payloads.
            let mut bgra = placement.image.rgba.to_vec();
            for pixel in bgra.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
            let Some(buffer) =
                image::RgbaImage::from_raw(placement.image.width, placement.image.height, bgra)
            else {
                log::error!("invalid published Kitty pixel dimensions");
                continue;
            };
            images.textures.push(Texture {
                source: Arc::clone(&placement.image),
                rendered: Arc::new(gpui::RenderImage::new([image::Frame::new(buffer)])),
            });
        }
    });
}

pub(super) fn paint(
    scene: &Scene,
    layer: Layer,
    shift: Pixels,
    clip: Bounds<Pixels>,
    window: &mut gpui::Window,
    cx: &gpui::App,
) -> Result<(), String> {
    let Some(images) = &scene.images else {
        return Ok(());
    };
    let images = images.read(cx);
    let geometry = scene.geometry;
    for placement in &scene.frame.images {
        if !layer.contains(placement.z) {
            continue;
        }
        let Some(texture) = images
            .textures
            .iter()
            .find(|texture| Arc::ptr_eq(&texture.source, &placement.image))
        else {
            return Err("Kitty image texture unavailable".into());
        };
        // GPUI/Metal positions and UVs are f32; the engine retains exact f64 geometry.
        let [x, y, width, height] = placement.source.map(|value| value as f32);
        if width <= 0. || height <= 0. || placement.cols <= 0. || placement.rows <= 0. {
            continue;
        }
        let target = Bounds::new(
            point(
                geometry.bounds.left() + geometry.cell_width * placement.col as f32,
                geometry.bounds.top() + geometry.cell_height * placement.row as f32 + shift,
            ),
            size(
                geometry.cell_width * placement.cols as f32,
                geometry.cell_height * placement.rows as f32,
            ),
        );
        let edge = |value: Pixels| px((f32::from(value) * geometry.scale).round() / geometry.scale);
        let visible = target.intersect(&clip);
        let visible = Bounds::from_corners(
            point(edge(visible.left()), edge(visible.top())),
            point(edge(visible.right()), edge(visible.bottom())),
        );
        if visible.size.width <= px(0.) || visible.size.height <= px(0.) {
            continue;
        }
        let scale_x = target.size.width / width;
        let scale_y = target.size.height / height;
        let full = Bounds::new(
            point(target.left() - scale_x * x, target.top() - scale_y * y),
            size(
                scale_x * placement.image.width as f32,
                scale_y * placement.image.height as f32,
            ),
        );
        // Separate orders also preserve blending when overlapping images share
        // a z layer. Runtime placements are sorted by (z, image id).
        window
            .paint_layer(visible, |window| {
                // Clip the full image instead of quantizing a source sub-tile
                // to integer atlas pixels (placeholder slices can be <1 pixel).
                window.with_content_mask(Some(ContentMask { bounds: visible }), |window| {
                    crate::platform::paint_image(
                        window,
                        full,
                        placement.sample_source.map(|value| value as f32),
                        Arc::clone(&texture.rendered),
                    )
                })
            })
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// An explicitly coloured background still masks the lowest image plane when
/// its RGB equals the default, so the ordinary background coalescer omitted it.
pub(super) fn mask_explicit_backgrounds(scene: &Scene, shift: Pixels, window: &mut gpui::Window) {
    if !scene
        .frame
        .images
        .iter()
        .any(|image| image.z < -1_073_741_824)
    {
        return;
    }
    let theme = scene.settings.theme();
    let defaults = draw::default_colors(&scene.frame, &theme);
    let rows = scene.frame.rows + u16::from(has_fringe(&scene.frame));
    for row in 0..rows {
        for col in 0..scene.frame.cols {
            let Some((cell, metadata)) = painted_cell(&scene.frame, row, col) else {
                continue;
            };
            if metadata.background_color_is_default
                && cell.flags
                    & (phux_client_core::grid::CELL_INVERSE | phux_client_core::grid::CELL_SELECTED)
                    == 0
            {
                continue;
            }
            let color = cell_colors(cell, metadata, &scene.frame, &theme, defaults).background;
            if color == scene.background {
                window.paint_quad(fill(
                    shifted(scene.geometry.cell_bounds(row, col, 1), shift),
                    color,
                ));
            }
        }
    }
}
