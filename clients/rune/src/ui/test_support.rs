//! Shared element lookup and offscreen rendering for view regression tests.

use crate::desk::Desk;

/// The element keyed `key` in `rt`'s last frame.
pub fn element(rt: &rune::Runtime<Desk>, key: &str) -> rune::ElementInfo {
    rt.elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key))
        .unwrap_or_else(|| panic!("no {key}"))
}

/// `rt`'s last frame painted `size` pixels big through Rune's real
/// renderer, as RGBA.
pub fn offscreen(
    gpu: &rune::render::Gpu,
    renderer: &mut rune::render::Renderer,
    rt: &mut rune::Runtime<Desk>,
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
