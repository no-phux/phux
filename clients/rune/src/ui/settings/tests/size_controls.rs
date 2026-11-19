//! Settings' actual integrated type scaling, not the unscaled isolated sheet.
use super::*;

fn scene(phone: bool, size: (u32, u32), scale: u32, large: bool, dark: bool) -> Runtime<Desk> {
    let mut desk = crate::test_support::settings_window().with_phone(phone);
    desk.settings.theme = crate::theme::find(if dark { "phux-dark" } else { "phux-light" });
    desk.settings_category(Category::Terminal);
    if phone && scale == 2 {
        desk.native = Some(crate::mobile::Platform::test());
    }
    if large {
        if phone {
            desk.set_accessibility(false, "UICTContentSizeCategoryAccessibilityXXXL");
        } else {
            desk.set_text_scale(1.4);
        }
    }
    let mut rt = Runtime::new(
        desk,
        crate::ui::view,
        (size.0 as f32, size.1 as f32),
        Env::new(dark, scale as f32),
    );
    rt.on_key = Some(Desk::key);
    for _ in 0..3 {
        rt.frame(1.0);
    }
    rt
}

#[test]
fn settings_size_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for (phone, size, scale, tag) in [
        (false, (1100, 720), 1, "desktop"),
        (false, (640, 360), 1, "desktop-min"),
        (true, (393, 852), 1, "phone"),
        (true, (320, 568), 1, "small"),
        (true, (393, 480), 2, "native"),
        (true, (320, 340), 2, "native-small"),
    ] {
        for dark in [true, false] {
            for large in [false, true] {
                let mut rt = scene(phone, size, scale, large, dark);
                let mode = if dark { "dark" } else { "light" };
                let type_size = if large { "ax5" } else { "standard" };
                let mut renderer =
                    rune::render::Renderer::new(&gpu.device, rune::render::OFFSCREEN_FORMAT);
                for (category, control) in [
                    (Category::Terminal, "text-bigger"),
                    (Category::Appearance, "interface-bigger"),
                ] {
                    rt.update(|d, tx| {
                        d.settings_category(category);
                        tx.instant();
                    });
                    rt.frame(1.0);
                    assert!(rt.accessibility_action(control, Action::Focus, None));
                    rt.frame(1.0);
                    rt.frame(1.0);
                    let pixels_size = (size.0 * scale, size.1 * scale);
                    let pixels = crate::ui::offscreen(&gpu, &mut renderer, &mut rt, pixels_size);
                    if let Some(directory) = std::env::var_os("RUNE_SETTINGS_SNAP") {
                        let name = format!("size-{tag}-{mode}-{type_size}-{control}-{scale}x");
                        let path = std::path::PathBuf::from(directory).join(format!("{name}.png"));
                        crate::test_support::visual::write(&path, &rt, pixels_size, &pixels, &name);
                    }
                }
            }
        }
    }
}
