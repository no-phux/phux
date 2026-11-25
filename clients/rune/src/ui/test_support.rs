//! Shared element lookup and offscreen rendering for view regression tests.

use crate::desk::Desk;

/// The element keyed `key` in `rt`'s last frame.
pub fn element(rt: &rune::Runtime<Desk>, key: &str) -> rune::ElementInfo {
    rt.elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key))
        .unwrap_or_else(|| panic!("no {key}"))
}

/// The glass panels `rt`'s last frame draws (`Tokens::glass_panel`),
/// asserting none sits inside a one-layer ancestor: a faded group or a
/// card still scaling in, which would flatten its backdrop.
pub fn glass_panels(rt: &rune::Runtime<Desk>) -> usize {
    let all = rt.elements();
    let glass: Vec<usize> = (all.iter().enumerate())
        .filter(|(_, e)| e.trace_name == Some(crate::ui::tokens::glass::GLASS_TRACE) && !e.exiting)
        .map(|(i, _)| i)
        .collect();
    for &i in &glass {
        let mut up = Some(i);
        while let Some(j) = up {
            let e = &all[j];
            assert!(
                e.opacity >= 0.999 && (e.scale - 1.0).abs() < 1e-3,
                "glass {:?} under a one-layer ancestor {:?} (opacity {}, scale {})",
                all[i].key,
                e.key,
                e.opacity,
                e.scale
            );
            up = e.parent;
        }
    }
    glass.len()
}

/// `rt`'s last frame painted `size` pixels big through Rune's real
/// renderer, as RGBA.
pub fn offscreen<S>(
    gpu: &rune::render::Gpu,
    renderer: &mut rune::render::Renderer,
    rt: &mut rune::Runtime<S>,
    size: (u32, u32),
) -> Vec<u8> {
    let target = rune::render::Offscreen::new(&gpu.device, size.0, size.1);
    renderer.upload_images(&gpu.device, &gpu.queue, &mut rt.images);
    renderer.prepare(
        &gpu.device,
        &gpu.queue,
        &mut rt.text.atlas,
        &rt.instances,
        size,
    );
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    renderer.draw(&mut encoder, &target.view, [0.0; 4]);
    target.copy(&mut encoder);
    renderer.submit(&gpu.queue, encoder);
    target.read(&gpu.device)
}

/// `rt`'s last frame as the app paints it: blended in sRGB (`App::srgb_blend`,
/// on by default) into a non-sRGB target, so translucent washes land as
/// the brand composited them. The runtime must have built that frame with
/// `Runtime::set_srgb_blend(true)`.
pub fn offscreen_as_app(
    gpu: &rune::render::Gpu,
    rt: &mut rune::Runtime<Desk>,
    size: (u32, u32),
) -> Vec<u8> {
    let format = rune::render::OFFSCREEN_FORMAT.remove_srgb_suffix();
    let mut renderer = rune::render::Renderer::new(&gpu.device, format);
    renderer.set_srgb_blend(true);
    offscreen_as_app_with_renderer(gpu, &mut renderer, rt, size)
}

/// Paint the current frame without advancing time. Retain this app-format
/// renderer across staged frames: its uploaded glyph atlas belongs to `rt`.
pub fn offscreen_as_app_with_renderer(
    gpu: &rune::render::Gpu,
    renderer: &mut rune::render::Renderer,
    rt: &mut rune::Runtime<Desk>,
    size: (u32, u32),
) -> Vec<u8> {
    let format = rune::render::OFFSCREEN_FORMAT.remove_srgb_suffix();
    let target = rune::render::Offscreen::with_format(&gpu.device, size.0, size.1, format);
    renderer.upload_images(&gpu.device, &gpu.queue, &mut rt.images);
    renderer.prepare(
        &gpu.device,
        &gpu.queue,
        &mut rt.text.atlas,
        &rt.instances,
        size,
    );
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    renderer.draw(&mut encoder, &target.view, rt.clear_color());
    target.copy(&mut encoder);
    renderer.submit(&gpu.queue, encoder);
    target.read(&gpu.device)
}
