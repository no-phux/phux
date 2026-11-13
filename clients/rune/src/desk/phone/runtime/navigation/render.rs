//! Paint the actual phone navigator on the native renderer at both sizes.
use super::*;

#[test]
fn phone_navigation_native_render_frames() {
    let gpu = rune::render::Gpu::headless();
    for size in [(393, 852), (320, 568)] {
        let (_, mut rt) = fixture();
        inventory(&mut rt);
        rt.set_viewport((size.0 as f32, size.1 as f32), 1.0);
        let mut renderer = rune::render::Renderer::new(&gpu.device, rune::render::OFFSCREEN_FORMAT);
        for (page, name) in [(Page::Sessions, "sessions"), (Page::Actions, "actions")] {
            rt.update(|d, tx| {
                d.close_phone_navigation();
                match page {
                    Page::Sessions => d.open_phone_sessions(),
                    Page::Actions => d.open_phone_actions(),
                }
                tx.instant();
            });
            rt.frame(1.0);
            rt.frame(1.0);
            let target = rune::render::Offscreen::new(&gpu.device, size.0, size.1);
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
            let pixels = target.read(&gpu.device);
            assert!(
                pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[0] != pixel[1])
            );
            if let Some(directory) = std::env::var_os("RUNE_NAVIGATION_SNAP") {
                let path =
                    std::path::PathBuf::from(directory).join(format!("{name}-{}.png", size.0));
                rune::render::write_png(&path, size.0, size.1, &pixels).unwrap();
            }
        }
    }
}
