//! Fit immutable publications without acquiring terminal presentation ownership.

use super::*;

/// Fit an immutable publication through the same native cell/glyph painter,
/// without any input, presentation acknowledgement or runtime size ownership.
pub(in crate::terminal) fn prepare_preview(
    frame: Arc<GridFrame>,
    mut settings: Settings,
    bounds: Bounds<Pixels>,
    cache: &SceneCache,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) -> Prepared {
    let mut geometry = Geometry::measure(&settings, bounds, frame.cols, frame.rows, window);
    let factor = preview_scale(
        bounds.size,
        geometry.cell_width * f32::from(frame.cols),
        geometry.cell_height * f32::from(frame.rows),
    );
    settings.font_size *= factor;
    geometry.cell_width *= factor;
    geometry.cell_height *= factor;
    geometry.baseline *= factor;
    let scale = window.scale_factor();
    // Small cells may be subpixel-sized. Do not remeasure them through the
    // full-size painter's one-point minimum or the grid would be cropped.
    let scene = if let Some(scene) = cache.borrow().as_ref()
        && scene.reusable(&frame, bounds, (scale, None), &settings)
    {
        Rc::clone(scene)
    } else {
        Rc::new(
            Builder::from_kept(frame, settings, geometry, (bounds, scale, None), None)
                .build(window),
        )
    };
    *cache.borrow_mut() = Some(Rc::clone(&scene));
    Prepared {
        layers: opacity_layers(&scene, px(0.), window, cx),
        scene: Some(scene),
        fallback: None,
        report: Observation::default(),
        shift: px(0.),
    }
}

fn preview_scale(available: gpui::Size<Pixels>, width: Pixels, height: Pixels) -> f32 {
    (available.width / width.max(px(1.)))
        .min(available.height / height.max(px(1.)))
        .clamp(f32::MIN_POSITIVE, 1.)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_fits_both_axes_even_when_cells_are_smaller_than_one_point() {
        assert_eq!(
            preview_scale(size(px(250.), px(150.)), px(1000.), px(500.)),
            0.25
        );
        assert_eq!(
            preview_scale(size(px(250.), px(50.)), px(1000.), px(500.)),
            0.1
        );
        assert_eq!(
            preview_scale(size(px(250.), px(150.)), px(20.), px(10.)),
            1.
        );
        assert!(preview_scale(size(px(1.), px(1.)), px(1000.), px(500.)).is_finite());
    }
}
