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
    let mut rt = crate::test_support::runtime(
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

#[test]
fn large_phone_theme_names_wrap_below_the_title_without_a_narrow_value_column() {
    for size in [(393, 852), (320, 568), (393, 480), (320, 340)] {
        for theme in [
            None,
            crate::theme::find("phux-light"),
            crate::theme::find("catppuccin-macchiato"),
        ] {
            let mut rt = scene(true, size, 2, true, false);
            rt.update(|d, tx| {
                d.settings.theme = theme;
                tx.instant();
                tx.scroll_key_to("settings-theme", 24.0);
            });
            rt.frame(1.0);
            rt.frame(1.0);
            let elements = rt.elements();
            let button = crate::ui::element(&rt, "settings-theme");
            let copy = |text: &str| {
                elements
                    .iter()
                    .find(|e| {
                        e.path.starts_with(&format!("{}/", button.path))
                            && e.text.as_deref() == Some(text)
                    })
                    .unwrap()
                    .rect
            };
            let title = copy("Theme");
            let value = copy(theme.map_or("System", |t| t.name));
            let detail = copy(
                "System, built-in and local themes. Preview live, Return to keep, Escape to restore.",
            );
            assert!(
                title[1] + title[3] <= value[1],
                "title precedes value: {title:?} {value:?}"
            );
            assert!(
                value[1] + value[3] < detail[1],
                "help starts below the name"
            );
            let tokens = crate::ui::Tokens::for_phone(
                &rt.state,
                false,
                theme,
                crate::ui::tokens::phone::Scale::Free,
            );
            let line = tokens.ty.body.line_px();
            assert!(value[3] >= line - 0.5, "the requested type is not shrunk");
            if theme == crate::theme::find("phux-light") {
                assert!(
                    (value[3] - line).abs() < 0.5,
                    "phux Light uses one natural line, not the old narrow column: \
                     viewport={size:?}, value={value:?}, title={title:?}, \
                     button={:?}, expected_line={line}, font_families={:?}",
                    button.rect,
                    rt.text.families()
                );
            }
            assert!(
                detail[2] >= button.rect[2] - 32.5,
                "help uses the full content gutter"
            );
            for rect in [title, value, detail] {
                assert!(rect[0] >= 32.0 && rect[0] + rect[2] <= size.0 as f32 - 31.5);
            }
            assert!(button.visible[2] >= 44.0 && button.visible[3] >= 44.0);
        }
    }
}
