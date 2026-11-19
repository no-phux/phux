//! Paint the actual phone navigator through Rune's GPU renderer.
use super::*;

mod header;

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
            let mut renderer = app_renderer(&gpu);
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
                d.phone.as_mut().unwrap().query.replace_text("review");
            });
            rt.update(|d, tx| d.phone_search_edited(tx));
            settle_for_snapshot(&mut rt);
            assert!(rt.find("phone-session-pane-@2").is_some());
            paint(
                &gpu,
                &mut renderer,
                &mut rt,
                size,
                &format!("matching-{appearance}"),
            );
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
            rt.update(|d, tx| {
                d.close_phone_navigation();
                d.open_phone_actions();
                tx.instant();
            });
            settle_for_snapshot(&mut rt);
            paint(
                &gpu,
                &mut renderer,
                &mut rt,
                size,
                &format!("actions-{appearance}"),
            );
            // Before the first inventory: placeholders, not an empty page.
            rt.update(|d, tx| {
                d.close_phone_navigation();
                d.connection.topology = None;
                d.open_phone_sessions();
                tx.instant();
            });
            settle_for_snapshot(&mut rt);
            assert!(rt.find("phone-session-loading").is_some());
            paint(
                &gpu,
                &mut renderer,
                &mut rt,
                size,
                &format!("loading-{appearance}"),
            );
        }
    }
}

fn settle_for_snapshot(rt: &mut Runtime<Desk>) {
    rt.set_srgb_blend(true);
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
    let pixels = as_app(gpu, renderer, rt, size);
    // Painted something: the house look is neutral grey, so the test is
    // more than one colour, not more than grey.
    let all = pixels.as_chunks::<4>().0;
    assert!(
        all.iter().any(|pixel| *pixel != all[0]),
        "{name} painted one flat colour"
    );
    let tree = rt.accessibility_json();
    touch_targets(&tree, name);
    // §1.9: the dock is the phone's one blur.
    assert!(
        crate::ui::glass_panels(rt) <= 1,
        "{name}: more than one blur on the phone"
    );
    if let Some(directory) = std::env::var_os("RUNE_NAVIGATION_SNAP") {
        let path = std::path::PathBuf::from(directory).join(format!("{name}-{}.png", size.0));
        crate::test_support::visual::write(&path, rt, size, &pixels, name);
    }
    pixels
}

/// A renderer that paints as the app does, so translucent washes land at
/// the alpha the brand composited: into a target that does not encode
/// sRGB, because it blends in sRGB itself (`App::srgb_blend`). One per
/// runtime: its glyph atlas is uploaded once.
fn app_renderer(gpu: &rune::render::Gpu) -> rune::render::Renderer {
    let mut renderer = rune::render::Renderer::new(
        &gpu.device,
        rune::render::OFFSCREEN_FORMAT.remove_srgb_suffix(),
    );
    renderer.set_srgb_blend(true);
    renderer
}

/// `rt`'s last frame, built with `set_srgb_blend(true)`, through `renderer`.
fn as_app(
    gpu: &rune::render::Gpu,
    renderer: &mut rune::render::Renderer,
    rt: &mut Runtime<Desk>,
    size: (u32, u32),
) -> Vec<u8> {
    let target = rune::render::Offscreen::with_format(
        &gpu.device,
        size.0,
        size.1,
        rune::render::OFFSCREEN_FORMAT.remove_srgb_suffix(),
    );
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

/// Every control the phone draws is a 44-point touch target in the
/// accessibility tree (§1.6, audit row 29). The drawn keyboard's letters
/// keep the system keyboard's width; they are 44 tall.
fn touch_targets(node: &serde_json::Value, name: &str) {
    let key = node["key"].as_str().unwrap_or_default();
    let clicks = node["actions"]
        .as_array()
        .is_some_and(|a| a.iter().any(|a| a == "click"));
    if key.starts_with("phone-") && clicks {
        let b = node["bounds"].as_array().expect("bounds");
        let (w, h) = (b[2].as_f64().unwrap(), b[3].as_f64().unwrap());
        assert!(h >= 44.0, "{name}: {key} is {h} tall");
        assert!(
            w >= 44.0 || key.starts_with("phone-cap-"),
            "{name}: {key} is {w} wide"
        );
    }
    for child in node["children"].as_array().into_iter().flatten() {
        touch_targets(child, name);
    }
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
    let mut renderer = app_renderer(&gpu);
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
        let mut renderer = app_renderer(&gpu);
        for (page, name) in [(Page::Sessions, "sessions"), (Page::Actions, "actions")] {
            rt.update(|d, tx| {
                d.close_phone_navigation();
                match page {
                    Page::Sessions => d.open_phone_sessions(),
                    Page::Actions => d.open_phone_actions(),
                }
                tx.instant();
            });
            rt.set_srgb_blend(true);
            rt.frame(1.0);
            rt.frame(1.0);
            paint(&gpu, &mut renderer, &mut rt, size, name);
        }
    }
}

#[test]
fn retina_navigation_renders_in_native_safe_area_and_desktop_phone_viewports() {
    let gpu = rune::render::Gpu::headless();
    for (size, scale, native) in [
        ((320, 568), 2, false),
        ((393, 852), 3, false),
        // UIKit has already removed the status/home safe areas and keyboard.
        ((320, 340), 2, true),
        ((393, 480), 3, true),
    ] {
        let (_, mut rt) = fixture();
        detailed_inventory(&mut rt);
        rt.set_viewport((size.0 as f32, size.1 as f32), scale as f32);
        rt.set_system_reduced_motion(true);
        rt.update(|d, _| {
            if native {
                d.native = Some(crate::mobile::Platform::test());
            }
            d.open_phone_sessions();
        });
        settle_for_snapshot(&mut rt);
        tap(&mut rt, "phone-session-query", 0.1);
        rt.update(|d, tx| {
            d.phone.as_mut().unwrap().query.replace_text("review");
            d.phone_search_edited(tx);
        });
        settle_for_snapshot(&mut rt);
        rt.update(|_, tx| tx.scroll_to_instant("phone-session-list", 0.0, 100_000.0));
        settle_for_snapshot(&mut rt);
        let pane = rt.presented("phone-session-pane-@2").unwrap().0;
        let dock = rt.presented("phone-session-new").unwrap().0;
        let list = rt.presented("phone-session-list").unwrap().0;
        assert!(pane[1] >= list[1] && pane[1] + pane[3] <= dock[1]);
        assert!(dock[1] + dock[3] <= size.1 as f32);
        assert_eq!(rt.find("phone-keyboard").is_none(), native);
        let mut renderer = app_renderer(&gpu);
        let pixels = paint(
            &gpu,
            &mut renderer,
            &mut rt,
            (size.0 * scale, size.1 * scale),
            &format!("retina-{scale}x-native-{native}"),
        );
        assert_eq!(pixels.len(), (size.0 * size.1 * scale * scale * 4) as usize);
    }
}

/// The design fixture on the phone (§5.2): the house look and catalog
/// themes, each standard, at increased contrast, with Reduce Transparency
/// and at the largest text size, over the terminal and the session page.
/// The dock is the phone's one glass, and only where glass may draw.
#[test]
fn the_phone_paints_the_design_fixture() {
    let gpu = rune::render::Gpu::headless();
    let themes = [
        ("dark", None),
        ("light", crate::theme::find("phux-light")),
        ("solarized-light", crate::theme::find("solarized-light")),
        ("windows-98", crate::theme::find("windows-98")),
    ];
    for (tag, theme) in themes {
        assert!(tag == "dark" || theme.is_some(), "{tag} is in the catalog");
        for mode in [
            "standard",
            "contrast-more",
            "reduce-transparency",
            "largest-text",
        ] {
            let (_, mut rt) = fixture();
            detailed_inventory(&mut rt);
            let mut renderer = app_renderer(&gpu);
            rt.set_viewport((393.0, 852.0), 1.0);
            rt.set_system_increased_contrast(mode == "contrast-more");
            rt.update(|d, tx| {
                let size = if mode == "largest-text" {
                    "UICTContentSizeCategoryAccessibilityXXXL"
                } else {
                    "UICTContentSizeCategoryL"
                };
                d.set_accessibility(mode == "reduce-transparency", size);
                d.settings.theme = theme;
                tx.instant();
            });
            settle_for_snapshot(&mut rt);
            let name = format!("fixture-{tag}-{mode}");
            paint(
                &gpu,
                &mut renderer,
                &mut rt,
                (393, 852),
                &format!("{name}-terminal"),
            );
            rt.update(|d, tx| {
                d.open_phone_sessions();
                tx.instant();
            });
            settle_for_snapshot(&mut rt);
            let blurs = rt
                .instances
                .iter()
                .filter(|i| i.params[1] == rune::render::KIND_BACKDROP_BLUR)
                .count();
            let glass = matches!(mode, "standard" | "largest-text");
            assert_eq!(blurs, usize::from(glass), "{name}: the dock's glass");
            paint(
                &gpu,
                &mut renderer,
                &mut rt,
                (393, 852),
                &format!("{name}-sessions"),
            );
        }
    }
}
