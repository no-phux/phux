//! Paint the actual phone navigator through Rune's GPU renderer.
use super::*;

#[test]
fn session_browser_renders_realistic_dark_light_and_search_states() {
    let gpu = rune::render::Gpu::headless();
    let light = crate::theme::all().find(|theme| !theme.dark).unwrap();
    for size in [(393, 852), (320, 568), (768, 1024)] {
        for (appearance, theme) in [("dark", None), ("light", Some(light))] {
            let (_, mut rt) = fixture();
            detailed_inventory(&mut rt);
            rt.set_viewport((size.0 as f32, size.1 as f32), 1.0);
            rt.update(|d, tx| {
                d.settings.theme = theme;
                d.open_phone_sessions();
                tx.instant();
            });
            settle_for_snapshot(&mut rt);
            let mut renderer =
                rune::render::Renderer::new(&gpu.device, rune::render::OFFSCREEN_FORMAT);
            paint(
                &gpu,
                &mut renderer,
                &mut rt,
                size,
                &format!("inventory-{appearance}"),
            );
            rt.update(|_, tx| {
                tx.scroll_to("phone-session-list", 0.0, 130.0);
                tx.instant();
            });
            settle_for_snapshot(&mut rt);
            paint(
                &gpu,
                &mut renderer,
                &mut rt,
                size,
                &format!("scrolled-{appearance}"),
            );
            tap(&mut rt, "phone-session-query", 0.1);
            rt.update(|d, _| {
                d.phone
                    .as_mut()
                    .unwrap()
                    .query
                    .replace_text("nothing matches")
            });
            rt.update(|d, tx| d.phone_search_edited(tx));
            settle_for_snapshot(&mut rt);
            paint(
                &gpu,
                &mut renderer,
                &mut rt,
                size,
                &format!("search-{appearance}"),
            );
        }
    }
}

fn settle_for_snapshot(rt: &mut Runtime<Desk>) {
    rt.settle_all();
    rt.frame(1.0);
    rt.frame(1.0);
}

fn paint(
    gpu: &rune::render::Gpu,
    renderer: &mut rune::render::Renderer,
    rt: &mut Runtime<Desk>,
    size: (u32, u32),
    name: &str,
) -> Vec<u8> {
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
        let path = std::path::PathBuf::from(directory).join(format!("{name}-{}.png", size.0));
        rune::render::write_png(&path, size.0, size.1, &pixels).unwrap();
    }
    pixels
}

#[test]
fn the_floating_session_dock_blurs_actual_list_pixels_not_just_a_tint() {
    let gpu = rune::render::Gpu::headless();
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((320.0, 568.0), 1.0);
    rt.update(|d, tx| {
        d.open_phone_sessions();
        tx.instant();
    });
    settle_for_snapshot(&mut rt);
    rt.update(|_, tx| {
        tx.scroll_to("phone-session-list", 0.0, 130.0);
        tx.instant();
    });
    settle_for_snapshot(&mut rt);
    let blurs = rt
        .instances
        .iter()
        .filter(|i| i.params[1] == rune::render::KIND_BACKDROP_BLUR)
        .count();
    assert_eq!(blurs, 1, "only the floating dock needs a blur pass");
    let mut renderer = rune::render::Renderer::new(&gpu.device, rune::render::OFFSCREEN_FORMAT);
    let frosted = paint(&gpu, &mut renderer, &mut rt, (320, 568), "dock-frosted");
    rt.instances
        .retain(|i| i.params[1] != rune::render::KIND_BACKDROP_BLUR);
    let tinted = paint(&gpu, &mut renderer, &mut rt, (320, 568), "dock-tint-only");
    let [x, y, w, h] = rt.presented("phone-session-new").unwrap().0;
    let changed = (y.ceil() as usize..(y + h).floor() as usize).any(|row| {
        (x.ceil() as usize..(x + w).floor() as usize).any(|column| {
            let at = (row * 320 + column) * 4;
            frosted[at..at + 3] != tinted[at..at + 3]
        })
    });
    assert!(
        changed,
        "GPU blur must change the inventory pixels beneath the dock"
    );
}

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
            paint(&gpu, &mut renderer, &mut rt, size, name);
        }
    }
}
