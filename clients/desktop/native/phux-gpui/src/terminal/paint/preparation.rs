//! Prepare a cached or incrementally rebuilt scene for one draw.

use super::*;

/// `fraction` is the part of a row above the viewport that a smooth scroll
/// has moved past, in `[0, 1)`. The scene is kept at whole-row positions,
/// so changing only the fraction reuses it and shifts it at paint time; the
/// frame's fringe row fills the space the shift opens at the bottom.
pub(in crate::terminal) fn prepare(
    frame: Result<Arc<GridFrame>, String>,
    settings: Settings,
    fraction: f32,
    bounds: Bounds<Pixels>,
    cache: &SceneCache,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) -> Prepared {
    let started = Instant::now();
    let mut prepared = Prepared {
        report: Observation {
            view_id: None,
            ..Default::default()
        },
        fallback: (!settings.host_fills_background)
            .then(|| settings.background.unwrap_or_else(gpui::black)),
        scene: None,
        layers: Vec::new(),
        shift: px(0.),
    };
    match frame {
        Ok(frame) => {
            let scene = scene(frame, settings, bounds, cache, window, cx);
            if has_fringe(&scene.frame) {
                prepared.shift = row_shift(scene.geometry, fraction);
            }
            prepared.layers = opacity_layers(&scene, prepared.shift, window, cx);
            prepared.scene = Some(scene);
        }
        Err(error) => {
            // A rejected reacquire is not a blank. The last scene stays while
            // its bounds still match, so a reconnect does not flash the pane.
            prepared.report.error = Some(error);
            if let Some(scene) = scene_at(cache, bounds) {
                prepared.layers = opacity_layers(&scene, prepared.shift, window, cx);
                prepared.scene = Some(scene);
            }
        }
    }
    prepared.report.prepare_micros = started.elapsed().as_micros();
    crate::perf::PREPARE.record_elapsed(started);
    prepared
}

/// The cached scene when a reacquire failed but the pane has not moved.
fn scene_at(cache: &SceneCache, bounds: Bounds<Pixels>) -> Option<Rc<Scene>> {
    let scene = Rc::clone(cache.borrow().as_ref()?);
    (scene.bounds == bounds).then_some(scene)
}

/// The cached scene when it still paints these inputs, else a new one.
fn scene(
    frame: Arc<GridFrame>,
    settings: Settings,
    bounds: Bounds<Pixels>,
    cache: &SceneCache,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) -> Rc<Scene> {
    let scale = window.scale_factor();
    let raster = crate::platform::glyph_raster(window);
    if let Some(scene) = cache.borrow().as_ref()
        && scene.reusable(&frame, bounds, (scale, raster), &settings)
    {
        crate::perf::PREPARE_REUSED.incr();
        return Rc::clone(scene);
    }
    let textures = cache
        .borrow()
        .as_ref()
        .and_then(|scene| scene.images.clone());
    if let Some(previous) = take_scene(cache)
        && let Some(mut patched) = patch(
            previous,
            frame.clone(),
            settings.clone(),
            bounds,
            (scale, raster),
            window,
        )
    {
        crate::perf::PREPARE_PATCHED.incr();
        images::prepare(&mut patched, window, cx);
        let patched = Rc::new(patched);
        *cache.borrow_mut() = Some(Rc::clone(&patched));
        return patched;
    }
    let mut scene = Builder::new(frame, settings, bounds, window).build(window);
    scene.images = textures;
    images::prepare(&mut scene, window, cx);
    let scene = Rc::new(scene);
    *cache.borrow_mut() = Some(Rc::clone(&scene));
    scene
}

fn take_scene(cache: &SceneCache) -> Option<Scene> {
    let existing = cache.borrow_mut().take()?;
    match Rc::try_unwrap(existing) {
        Ok(scene) => Some(scene),
        Err(shared) => {
            *cache.borrow_mut() = Some(shared);
            None
        }
    }
}

/// Minus `fraction` of a row, rounded to device pixels so glyph masks and
/// cell edges stay on the pixel grid while the grid moves.
fn row_shift(geometry: Geometry, fraction: f32) -> Pixels {
    let scale = geometry.scale.max(1.);
    -px((f32::from(geometry.cell_height) * fraction.clamp(0., 1.) * scale).round() / scale)
}

pub(super) fn opacity_layers(
    scene: &Scene,
    shift: Pixels,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) -> Vec<OpacityLayer> {
    scene
        .inks
        .iter()
        .filter_map(|ink| match &ink.ink {
            Ink::Glyph(glyph) if glyph.opacity < 1. => {
                Some(glyph.opacity_layer(scene.geometry, shift, window, cx))
            }
            _ => None,
        })
        .collect()
}
