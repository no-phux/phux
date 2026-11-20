//! Theme rows at the actual integrated phone and desktop text sizes.
use super::*;

fn scene(phone: bool, size: (u32, u32), scale: u32, large: bool, dark: bool) -> Runtime<Desk> {
    let mut desk = crate::test_support::settings_window().with_phone(phone);
    desk.settings.theme = crate::theme::find(if dark { "phux-dark" } else { "phux-light" });
    desk.settings_category(Category::Appearance);
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

fn reveal(rt: &mut Runtime<Desk>) {
    assert!(rt.accessibility_action("settings-theme", Action::Focus, None));
    rt.update(|_, tx| {
        tx.scroll_into_view("settings-theme");
        tx.instant();
    });
    rt.frame(1.0);
    rt.frame(1.0);
}

fn shoot(
    rt: &mut Runtime<Desk>,
    gpu: &rune::render::Gpu,
    renderer: &mut rune::render::Renderer,
    pixels: (u32, u32),
    name: &str,
) {
    rt.frame(1.0);
    rt.frame(1.0);
    let rgba = crate::ui::offscreen(gpu, renderer, rt, pixels);
    if let Some(directory) = std::env::var_os("RUNE_SETTINGS_SNAP") {
        let path = std::path::PathBuf::from(directory).join(format!("{name}.png"));
        crate::test_support::visual::write(&path, rt, pixels, &rgba, name);
    }
}

#[test]
fn settings_theme_visual_review_frames() {
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
                let name = format!("theme-{tag}-{mode}-{type_size}");
                let pixels = (size.0 * scale, size.1 * scale);
                let mut renderer =
                    rune::render::Renderer::new(&gpu.device, rune::render::OFFSCREEN_FORMAT);
                reveal(&mut rt);
                shoot(
                    &mut rt,
                    &gpu,
                    &mut renderer,
                    pixels,
                    &format!("{name}-revealed-{scale}x"),
                );
                // A row taller than the viewport reveals its bottom on focus.
                // Also retain its heading where a user can scroll to read it.
                rt.update(|_, tx| {
                    tx.scroll_key_to("settings-theme", 24.0);
                    tx.instant();
                });
                shoot(
                    &mut rt,
                    &gpu,
                    &mut renderer,
                    pixels,
                    &format!("{name}-heading-{scale}x"),
                );
                assert!(rt.accessibility_action("settings-query", Action::Focus, None));
                rt.key(crate::keys::plain(Key::Text("theme".into())));
                rt.frame(1.0);
                shoot(
                    &mut rt,
                    &gpu,
                    &mut renderer,
                    pixels,
                    &format!("{name}-search-{scale}x"),
                );
                let original = rt.state.settings.theme;
                theme_query(&mut rt);
                shoot(
                    &mut rt,
                    &gpu,
                    &mut renderer,
                    pixels,
                    &format!("{name}-chooser-{scale}x"),
                );
                rt.key(crate::keys::plain(Key::Escape));
                rt.frame(1.0);
                assert!(rt.state.settings_open && !rt.state.palette_open);
                assert_eq!(rt.state.settings.theme, original);
                reveal(&mut rt);
                shoot(
                    &mut rt,
                    &gpu,
                    &mut renderer,
                    pixels,
                    &format!("{name}-cancelled-{scale}x"),
                );
                theme_query(&mut rt);
                rt.key(crate::keys::plain(Key::Enter));
                rt.frame(1.0);
                assert!(rt.state.settings_open && !rt.state.palette_open);
                assert_eq!(rt.state.settings.theme, crate::theme::find("nord"));
                reveal(&mut rt);
                shoot(
                    &mut rt,
                    &gpu,
                    &mut renderer,
                    pixels,
                    &format!("{name}-accepted-{scale}x"),
                );
            }
        }
    }
}
