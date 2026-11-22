//! Matched integrated editing states, including the strip's hidden controls.
use super::*;

mod behavior;

#[test]
fn phone_accessory_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for (size, native, scale) in [
        ((393, 852), false, 1),
        ((320, 568), false, 1),
        ((393, 480), true, 2),
        ((320, 340), true, 2),
    ] {
        for (appearance, theme) in [("dark", None), ("light", crate::theme::find("phux-light"))] {
            for (text_size, category) in [
                ("standard", "UICTContentSizeCategoryL"),
                ("ax2", "UICTContentSizeCategoryAccessibilityL"),
                ("ax3", "UICTContentSizeCategoryAccessibilityXL"),
                ("ax5", "UICTContentSizeCategoryAccessibilityXXXL"),
            ] {
                for (flow, field, query) in [
                    ("sessions", "phone-session-query", "review"),
                    ("settings", "settings-query", "theme"),
                ] {
                    let (client, mut rt) =
                        editing_fixture(size, native, scale, theme, category, field);
                    rt.key(crate::keys::plain(Key::Text(query.into())));
                    settle_for_snapshot(&mut rt);
                    let mut renderer = app_renderer(&gpu);
                    let name = format!(
                        "accessory-{flow}-{text_size}-{appearance}-native-{native}-{}x{}-{scale}x",
                        size.0, size.1,
                    );
                    for (edge, offset) in [("leading", 0.0), ("trailing", 100_000.0)] {
                        rt.update(|_, tx| tx.scroll_to_instant("phone-strip", offset, 0.0));
                        settle_for_snapshot(&mut rt);
                        paint(
                            &gpu,
                            &mut renderer,
                            &mut rt,
                            (size.0 * scale, size.1 * scale),
                            &format!("{name}-{edge}"),
                        );
                    }
                    rt.update(|_, tx| tx.scroll_into_view("phone-key-Tab"));
                    settle_for_snapshot(&mut rt);
                    paint(
                        &gpu,
                        &mut renderer,
                        &mut rt,
                        (size.0 * scale, size.1 * scale),
                        &format!("{name}-tab-revealed"),
                    );
                    no_terminal_input(&client);
                }
            }
        }
    }
}

fn editing_fixture(
    size: (u32, u32),
    native: bool,
    scale: u32,
    theme: Option<&'static crate::theme::Theme>,
    category: &str,
    field: &str,
) -> (std::sync::Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((size.0 as f32, size.1 as f32), scale as f32);
    rt.set_system_reduced_motion(true);
    rt.update(|d, _| {
        if native {
            d.native = Some(crate::mobile::Platform::test());
        }
        d.settings.theme = theme;
        d.set_accessibility(false, category);
        if field == "settings-query" {
            d.settings_open = true;
        } else {
            d.open_phone_sessions();
        }
    });
    settle_for_snapshot(&mut rt);
    tap(&mut rt, field, 0.1);
    (client, rt)
}

#[test]
fn phone_accessory_resize_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for size in [(320, 340), (393, 480)] {
        for (appearance, theme) in [("dark", None), ("light", crate::theme::find("phux-light"))] {
            for settings in [false, true] {
                resize_frames(&gpu, size, appearance, theme, settings);
            }
        }
    }
}

fn resize_frames(
    gpu: &rune::render::Gpu,
    size: (u32, u32),
    appearance: &str,
    theme: Option<&'static crate::theme::Theme>,
    settings: bool,
) {
    let field = if settings {
        "settings-query"
    } else {
        "phone-session-query"
    };
    let (client, mut rt) = editing_fixture(
        size,
        true,
        2,
        theme,
        "UICTContentSizeCategoryAccessibilityXXXL",
        field,
    );
    rt.set_system_reduced_motion(false);
    rt.frame(0.0);
    let mut renderer = app_renderer(gpu);
    for (direction, open) in [("grow", false), ("shrink", true)] {
        rt.update(|d, tx| {
            if settings {
                d.settings_open = open;
                if open {
                    d.focus_settings_search();
                } else {
                    d.blur_settings_search(tx);
                }
            } else if open {
                d.open_phone_sessions();
            } else {
                d.close_phone_navigation();
            }
        });
        let flow = if settings { "settings-" } else { "" };
        let name = format!(
            "accessory-resize-{flow}{direction}-{appearance}-{}x{}-2x",
            size.0, size.1
        );
        let mut previous = 0;
        for (stage, frame) in [("first", 1), ("middle", 6), ("settled", 60)] {
            for _ in previous..frame {
                rt.frame(1.0 / 60.0);
            }
            save_resize_frame(gpu, &mut renderer, &mut rt, size, &name, stage, frame);
            previous = frame;
        }
    }
    no_terminal_input(&client);
}

fn save_resize_frame(
    gpu: &rune::render::Gpu,
    renderer: &mut rune::render::Renderer,
    rt: &mut Runtime<Desk>,
    size: (u32, u32),
    name: &str,
    stage: &str,
    frame: u32,
) {
    let size = (size.0 * 2, size.1 * 2);
    let pixels = as_app(gpu, renderer, rt, size);
    touch_targets(&rt.accessibility_json(), name);
    if let Some(directory) = std::env::var_os("RUNE_NAVIGATION_SNAP") {
        let path = std::path::PathBuf::from(directory).join(format!("{name}-{stage}.png"));
        crate::test_support::visual::write(&path, rt, size, &pixels, name);
        std::fs::write(
            path.with_extension("motion.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema": 1, "scenario": name, "stage": stage,
                "frame": frame, "elapsed_ms": f64::from(frame) * 1000.0 / 60.0,
            }))
            .unwrap(),
        )
        .unwrap();
    }
}
