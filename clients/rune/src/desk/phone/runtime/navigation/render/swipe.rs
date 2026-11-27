//! The sideways thumb, painted: a page mid-swipe naming where it goes, the
//! drawer in the finger's hand, and the drawer open over the blur.
use super::*;
use rune::TouchPhase::{Cancelled, Moved, Started};

/// A release batched with several moves: the drawer must follow the final
/// position, not the first move timed during that frame. This exporter also
/// runs against the unfixed app; behavior assertions live beside the handlers.
#[test]
fn batched_swipe_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for size in [(393, 852), (320, 568), (393, 480), (320, 340)] {
        for (tag, theme) in [("dark", None), ("light", crate::theme::find("phux-light"))] {
            for reduced in [false, true] {
                batched_frames(&gpu, size, tag, theme, reduced);
            }
        }
    }
}

fn batched_frames(
    gpu: &rune::render::Gpu,
    size: (u32, u32),
    theme_name: &str,
    theme: Option<&'static crate::theme::Theme>,
    reduced: bool,
) {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((size.0 as f32, size.1 as f32), 2.0);
    rt.set_system_reduced_motion(reduced);
    rt.set_srgb_blend(true);
    rt.update(|d, tx| {
        d.native = Some(crate::mobile::Platform::test());
        d.settings.theme = theme;
        d.phone.as_mut().unwrap().keyboard = size.1 < 500;
        tx.instant();
    });
    for _ in 0..90 {
        rt.frame(1.0 / 60.0);
    }
    let _ = client.take_outbound();
    let mut renderer = app_renderer(gpu);
    let pixels = (size.0 * 2, size.1 * 2);
    let stage = crate::ui::element(&rt, "phone-spatial-stage").rect;
    let y = stage[1] + stage[3] * 0.5;
    let name = format!(
        "batched-swipe-{theme_name}-{}x{}-2x-reduced-{reduced}",
        size.0, size.1
    );
    let capture =
        |rt: &mut Runtime<Desk>, renderer: &mut rune::render::Renderer, state, frame: u32| {
            let image = as_app(gpu, renderer, rt, pixels);
            touch_targets(&rt.accessibility_json(), &name);
            assert!(crate::ui::glass_panels(rt) <= 1);
            if let Some(directory) = std::env::var_os("RUNE_NAVIGATION_SNAP") {
                let path = std::path::PathBuf::from(directory).join(format!("{name}-{state}.png"));
                crate::test_support::visual::write(&path, rt, pixels, &image, &name);
                std::fs::write(
                    path.with_extension("motion.json"),
                    serde_json::to_vec_pretty(&serde_json::json!({
                        "schema": 1, "scenario": name, "stage": state, "frame": frame,
                        "elapsed_ms": f64::from(frame) * 1000.0 / 60.0,
                        "runtime_clock": rt.clock,
                        "reduced_motion": reduced, "native_sized_keyboard_viewport": size.1 < 500,
                        "batch_travel_points": [20.0, 240.0],
                        "sessions_open": rt.state.phone_page() == Some(Page::Sessions),
                        "pull": rt.state.phone.as_ref().unwrap().swipe.pull,
                    }))
                    .unwrap(),
                )
                .unwrap();
            }
        };
    capture(&mut rt, &mut renderer, "idle", 0);
    rt.touch(9, Started, 4.0, y, 1.0);
    rt.update(|d, tx| {
        d.poll(1.0, tx);
    });
    rt.frame(1.0 / 60.0);
    capture(&mut rt, &mut renderer, "start", 1);
    rt.touch(9, Moved, 24.0, y, 1.016);
    rt.touch(9, Moved, 244.0, y, 1.020);
    rt.touch(9, rune::TouchPhase::Ended, 244.0, y, 1.024);
    rt.update(|d, tx| {
        d.poll(1.032, tx);
    });
    rt.frame(1.0 / 60.0);
    capture(&mut rt, &mut renderer, "release", 2);
    for _ in 0..6 {
        rt.frame(1.0 / 60.0);
    }
    capture(&mut rt, &mut renderer, "release-plus-100ms", 8);
    for _ in 0..120 {
        rt.frame(1.0 / 60.0);
    }
    capture(&mut rt, &mut renderer, "settled", 128);
    no_terminal_input(&client);
}

fn frames(gpu: &rune::render::Gpu, theme: Option<&'static crate::theme::Theme>, tag: &str) {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((393.0, 852.0), 2.0);
    rt.update(|d, tx| {
        d.native = Some(crate::mobile::Platform::test());
        d.settings.theme = theme;
        d.phone.as_mut().unwrap().keyboard = false;
        tx.instant();
    });
    for _ in 0..90 {
        rt.frame(1.0 / 60.0);
    }
    let mut renderer = app_renderer(gpu);
    let stage = crate::ui::element(&rt, "phone-spatial-stage").rect;
    let y = stage[1] + stage[3] * 0.5;
    let held = |rt: &mut Runtime<Desk>, from: f32, to: f32, at: f64| {
        rt.touch(9, Started, from, y, at);
        rt.update(|d, tx| {
            d.poll(at, tx);
        });
        rt.frame(1.0 / 60.0);
        rt.touch(9, Moved, to, y, at + 0.016);
        rt.update(|d, tx| {
            d.poll(at + 0.016, tx);
        });
        settle_for_snapshot(rt);
    };
    held(&mut rt, 300.0, 120.0, 1.0);
    paint(
        gpu,
        &mut renderer,
        &mut rt,
        (786, 1704),
        &format!("swipe-{tag}-paging"),
    );
    rt.touch(9, Cancelled, 120.0, y, 2.0);
    for _ in 0..90 {
        rt.frame(1.0 / 60.0);
    }
    held(&mut rt, 4.0, 230.0, 3.0);
    paint(
        gpu,
        &mut renderer,
        &mut rt,
        (786, 1704),
        &format!("swipe-{tag}-pulling"),
    );
    rt.touch(9, Cancelled, 230.0, y, 4.0);
    rt.update(|d, _| d.open_phone_sessions());
    for _ in 0..90 {
        rt.frame(1.0 / 60.0);
    }
    settle_for_snapshot(&mut rt);
    paint(
        gpu,
        &mut renderer,
        &mut rt,
        (786, 1704),
        &format!("swipe-{tag}-drawer"),
    );
}

#[test]
fn swipe_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    frames(&gpu, None, "dark");
    frames(&gpu, crate::theme::find("phux-light"), "light");
}
