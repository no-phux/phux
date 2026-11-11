//! Real Rune layout, accessibility and GPU frames for the Settings surface.

use rune::accesskit::Action;
use rune::{Cx, El, Env, Key, KeyEvent, Mods, Runtime};

use super::*;
use crate::desk::updates::Work;

mod overflow;

fn surface(desk: &Desk, cx: &mut Cx<Desk>) -> El {
    let ink = super::super::ink(cx.system_dark(), cx.accent(), desk.settings.theme);
    view(desk, &ink)
}

fn runtime(size: (f32, f32), phone: bool) -> Runtime<Desk> {
    let mut desk = Desk::settings_test_fixture().with_phone(phone);
    desk.settings.theme = crate::theme::find("catppuccin-latte");
    let mut rt = Runtime::new(desk, surface, size, Env::new(false, 1.0));
    rt.on_key = Some(Desk::key);
    rt.frame(0.0);
    rt
}

fn rect(rt: &Runtime<Desk>, key: &str) -> [f32; 4] {
    rt.presented(key)
        .unwrap_or_else(|| panic!("missing {key}"))
        .0
}

#[test]
fn phone_categories_search_close_and_back_have_44_point_targets() {
    for size in [(393.0, 852.0), (320.0, 568.0)] {
        let mut rt = runtime(size, true);
        for key in Category::ALL
            .into_iter()
            .map(Category::key)
            .chain(["settings-close", "settings-query"])
        {
            rt.update(|_, tx| tx.scroll_into_view(key));
            rt.frame(1.0);
            let [x, y, w, h] = rect(&rt, key);
            assert!(w >= 44.0 && h >= 44.0, "{key}: {w}×{h}");
            assert!(x >= 0.0 && x + w <= size.0 + 0.5, "{key} fits horizontally");
            assert!(y >= 0.0 && y + h <= size.1 + 0.5, "{key} fits vertically");
        }
        assert!(rt.accessibility_action("settings-terminal", Action::Click, None));
        rt.frame(0.0);
        assert!(rt.find("settings-back").is_some());
        assert!(rt.find("text-size").is_some());
        for key in ["text-smaller", "text-bigger"] {
            let [x, _, w, h] = rect(&rt, key);
            assert!(w >= 44.0 && h >= 44.0);
            assert!(x + w <= size.0, "{key} stays in the card");
        }
    }
}

#[test]
fn minimum_desktop_rail_reveals_every_destination_by_tab_and_activates_it() {
    let mut rt = runtime((640.0, 360.0), false);
    assert!(rt.accessibility_action("settings-appearance", Action::Focus, None));
    for category in Category::ALL {
        rt.frame(1.0);
        rt.frame(1.0);
        let item = rt
            .elements()
            .into_iter()
            .find(|el| el.key.as_deref() == Some(category.key()))
            .unwrap();
        assert!(item.focused, "Tab reaches {}", category.name());
        assert!(
            item.visible[3] >= 47.0,
            "{} is scroll-revealed: {:?}",
            category.name(),
            item.visible
        );
        assert!(
            item.visible[1] >= 84.0 && item.visible[1] + item.visible[3] <= 328.0,
            "{} stays between panel header and bottom",
            category.name()
        );
        rt.key(KeyEvent {
            key: Key::Text(" ".into()),
            mods: Mods::default(),
        });
        rt.frame(1.0);
        assert_eq!(rt.state.settings_ui.category, Some(category));
        rt.frame(1.0);
        let selected = rt
            .elements()
            .into_iter()
            .find(|el| el.key.as_deref() == Some(category.key()))
            .unwrap();
        assert!(
            selected.visible[3] >= 47.0,
            "activated {} remains revealed: {:?}",
            category.name(),
            selected.visible
        );
        rt.key(KeyEvent {
            key: Key::Tab,
            mods: Mods::default(),
        });
    }
}

#[test]
fn phone_typography_and_wrapping_fit_long_labels_and_values_without_shrinking() {
    let mut rt = runtime((320.0, 568.0), true);
    rt.update(|d, tx| {
        tx.instant();
        d.settings.theme = crate::theme::find("catppuccin-macchiato");
        d.machines.current = Some("a-very-long-remote-machine-name.example".into());
    });
    rt.frame(1.0);
    assert_eq!(super::primary(&rt.state, COPY).size, 17.0);
    assert_eq!(super::primary(&rt.state, COPY).line_px(), 22.0);
    assert_eq!(super::secondary(&rt.state, CAPTION).size, 13.0);
    assert_eq!(super::secondary(&rt.state, CAPTION).line_px(), 18.0);
    for category in Category::ALL {
        let [x, _, width, height] = rect(&rt, category.key());
        assert!(height >= 56.0, "{}", category.name());
        assert!(x >= 0.0 && x + width <= 320.0);
    }
    for item in rt.elements().into_iter().filter(|el| el.text.is_some()) {
        assert!(
            item.rect[0] >= 0.0 && item.rect[0] + item.rect[2] <= 320.5,
            "text must wrap inside phone: {:?} {:?}",
            item.text,
            item.rect
        );
    }
}

#[test]
fn native_search_editing_and_tab_do_not_change_preferences() {
    let mut rt = runtime((1100.0, 720.0), false);
    let before = rt.state.settings;
    assert!(rt.accessibility_action("settings-query", Action::Focus, None));
    rt.key(KeyEvent {
        key: Key::Text("clipboard".into()),
        mods: Mods::default(),
    });
    rt.frame(0.0);
    assert_eq!(rt.state.settings_ui.query.text, "clipboard");
    assert!(rt.find("set-copy-select").is_some());
    rt.key(KeyEvent {
        key: Key::Tab,
        mods: Mods::default(),
    });
    rt.frame(0.0);
    assert_eq!(rt.state.settings, before);
    assert_eq!(rt.state.settings_ui.query.text, "clipboard");
}

fn assert_search_caret_centered(rt: &Runtime<Desk>) {
    let query = rt
        .elements()
        .into_iter()
        .find(|el| el.key.as_deref() == Some("settings-query"))
        .unwrap();
    let [_, y, _, h] = query.rect;
    let [_, caret_y, _, caret_h] = query.caret.expect("focused search has a native caret");
    assert!(
        h >= 44.0,
        "the complete native field remains a 44-point target"
    );
    assert!(
        (caret_y + caret_h / 2.0 - y - h / 2.0).abs() <= 1.0,
        "caret {caret_y} + {caret_h}/2 must align with field {y} + {h}/2"
    );
}

#[test]
fn search_placeholder_and_typed_caret_are_centered_in_the_full_touch_field() {
    for (phone, size) in [
        (false, (1100.0, 720.0)),
        (true, (393.0, 852.0)),
        (true, (320.0, 568.0)),
    ] {
        let mut rt = runtime(size, phone);
        assert!(rt.accessibility_action("settings-query", Action::Focus, None));
        rt.frame(0.0);
        assert_search_caret_centered(&rt);
        rt.key(KeyEvent {
            key: Key::Text("clipboard".into()),
            mods: Mods::default(),
        });
        rt.frame(0.0);
        assert_eq!(rt.state.settings_ui.query.text, "clipboard");
        assert_search_caret_centered(&rt);
    }
}

#[test]
fn touching_clear_keeps_the_search_keyboard_and_draft_blur_preserves_text() {
    let mut rt = runtime((393.0, 852.0), true);
    assert!(rt.accessibility_action("settings-query", Action::Focus, None));
    rt.key(KeyEvent {
        key: Key::Text("clipboard".into()),
        mods: Mods::default(),
    });
    rt.frame(0.0);
    let (x, y) = rt.find("settings-clear").unwrap();
    rt.touch(1, rune::TouchPhase::Started, x, y, 0.0);
    rt.touch(1, rune::TouchPhase::Ended, x, y, 0.05);
    rt.frame(0.0);
    assert!(rt.state.settings_ui.query.text.is_empty());
    assert!(rt.state.settings_search_focused());
    rt.update(|d, tx| {
        d.settings_soft(
            &KeyEvent {
                key: Key::Text("copy".into()),
                mods: Mods::default(),
            },
            tx,
        );
        d.blur_settings_search(tx);
    });
    rt.frame(0.0);
    assert_eq!(rt.state.settings_ui.query.text, "copy");
    assert!(!rt.state.settings_search_focused());
}

fn downloading(rt: &mut Runtime<Desk>) {
    rt.update(|d, tx| {
        tx.instant();
        d.settings_category(Category::About);
        d.updater.install = Ok("/not-an-installed-app".into());
        d.updater.work = Work::Downloading(42);
    });
    rt.frame(0.0);
}

#[test]
fn updater_progress_and_actions_fit_narrow_settings() {
    let mut rt = runtime((320.0, 568.0), true);
    downloading(&mut rt);
    let [x, _, w, _] = rect(&rt, "update-progress");
    assert!(
        w > 0.0 && x + w <= 320.0,
        "progress fills its card, not a fixed 524 points"
    );
    rt.update(|d, _| {
        d.updater.work = Work::Idle;
        d.updater.staged = Some("0.2.0".into());
    });
    rt.frame(0.0);
    for key in ["update-act-settings", "update-check"] {
        let [x, _, w, h] = rect(&rt, key);
        assert!(
            x >= 0.0 && x + w <= 320.0 && h >= 44.0,
            "{key}: {x} {w} {h}"
        );
    }
}

#[test]
fn phone_update_preference_text_shares_the_card_content_gutter() {
    let mut rt = runtime((320.0, 568.0), true);
    downloading(&mut rt);
    let elements = rt.elements();
    let x = |text: &str| {
        elements
            .iter()
            .find(|el| el.text.as_deref() == Some(text))
            .unwrap()
            .rect[0]
    };
    assert_eq!(x("Check automatically"), x("Downloading… 42%"));
    assert_eq!(
        x("At launch and every six hours. Nothing installs without a click."),
        x("Downloading… 42%")
    );
}

#[test]
fn settings_ink_keeps_text_and_control_contrast_on_its_surfaces() {
    use crate::theme::{DIM_BAR, TEXT_BAR, contrast};
    for theme in std::iter::once(None).chain(crate::theme::all().map(Some)) {
        for dark in [true, false] {
            let ink = crate::ui::ink(dark, rune::oklch(0.6, 0.15, 250.0), theme);
            for ground in [ink.sidebar, super::surface(&ink)] {
                assert!(contrast(ink.text, ground) >= TEXT_BAR);
                assert!(contrast(ink.dim, ground) >= DIM_BAR);
                assert!(contrast(ink.accent, ground) >= DIM_BAR);
            }
        }
    }
}

fn render(
    gpu: &rune::render::Gpu,
    renderer: &mut rune::render::Renderer,
    rt: &mut Runtime<Desk>,
    size: (u32, u32),
    name: &str,
) {
    // A focus reveal commits its scroll after layout. Paint the following
    // frame too, rather than exporting the previous frame's draw instances.
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
    if let Some(directory) = std::env::var_os("RUNE_SETTINGS_SNAP") {
        let path = std::path::PathBuf::from(directory).join(format!("{name}.png"));
        rune::render::write_png(&path, size.0, size.1, &pixels).unwrap();
        std::fs::write(
            path.with_extension("json"),
            rt.accessibility_json().to_string(),
        )
        .unwrap();
    }
}

#[test]
fn settings_native_render_frames() {
    let gpu = rune::render::Gpu::headless();
    for (phone, size, name) in [
        (false, (1100, 720), "desktop"),
        (false, (640, 360), "desktop-min"),
        (true, (393, 852), "phone"),
        (true, (320, 568), "small"),
    ] {
        let mut rt = runtime((size.0 as f32, size.1 as f32), phone);
        let mut renderer = rune::render::Renderer::new(&gpu.device, rune::render::OFFSCREEN_FORMAT);
        render(&gpu, &mut renderer, &mut rt, size, name);
        rt.update(|d, tx| {
            tx.instant();
            d.settings_category(Category::Terminal);
        });
        rt.frame(0.0);
        render(
            &gpu,
            &mut renderer,
            &mut rt,
            size,
            &format!("{name}-terminal"),
        );
        downloading(&mut rt);
        render(
            &gpu,
            &mut renderer,
            &mut rt,
            size,
            &format!("{name}-updates"),
        );
        rt.update(|d, tx| {
            tx.instant();
            d.settings_ui.category = None;
            d.settings.theme = crate::theme::find("catppuccin-mocha");
        });
        rt.frame(0.0);
        render(&gpu, &mut renderer, &mut rt, size, &format!("{name}-dark"));
        assert!(rt.accessibility_action("settings-query", Action::Focus, None));
        rt.key(KeyEvent {
            key: Key::Text("clipboard".into()),
            mods: Mods::default(),
        });
        rt.frame(0.0);
        render(
            &gpu,
            &mut renderer,
            &mut rt,
            size,
            &format!("{name}-typed-search"),
        );
        if !phone {
            rt.update(|d, tx| {
                tx.instant();
                d.clear_settings_search();
                d.blur_settings_search(tx);
            });
            rt.frame(0.0);
            assert!(rt.accessibility_action("settings-appearance", Action::Focus, None));
            rt.frame(1.0);
            for _ in 1..Category::ALL.len() {
                rt.key(KeyEvent {
                    key: Key::Tab,
                    mods: Mods::default(),
                });
                rt.frame(1.0);
                rt.frame(1.0);
            }
            rt.key(KeyEvent {
                key: Key::Text(" ".into()),
                mods: Mods::default(),
            });
            rt.frame(1.0);
            assert_eq!(rt.state.settings_ui.category, Some(Category::About));
            render(
                &gpu,
                &mut renderer,
                &mut rt,
                size,
                &format!("{name}-about-revealed"),
            );
        }
    }
}

fn theme_scene(desk: &Desk, cx: &mut Cx<Desk>) -> El {
    if desk.palette_open {
        // Render the existing palette through its actual phone composition.
        crate::ui::view(desk, cx)
    } else {
        // The parent supplies this stage in the integrated phone shell.
        surface(desk, cx)
    }
}

fn theme_query(rt: &mut Runtime<Desk>) {
    assert!(rt.accessibility_action("settings-theme", Action::Click, None));
    rt.frame(1.0);
    assert!(rt.state.palette_open);
    rt.key(KeyEvent {
        key: Key::Text("nord".into()),
        mods: Mods::default(),
    });
    rt.frame(1.0);
    assert_eq!(rt.state.settings.theme, crate::theme::find("nord"));
}

#[test]
fn mobile_appearance_and_real_theme_palette_render_accept_and_cancel() {
    let gpu = rune::render::Gpu::headless();
    for (size, name) in [((393, 852), "phone"), ((320, 568), "small")] {
        let mut desk = Desk::settings_test_fixture().with_phone(true);
        desk.settings_category(Category::Appearance);
        desk.settings.theme = crate::theme::find("catppuccin-latte");
        let before = desk.settings.theme;
        let mut rt = Runtime::new(
            desk,
            theme_scene,
            (size.0 as f32, size.1 as f32),
            Env::new(false, 1.0),
        );
        rt.on_key = Some(Desk::key);
        rt.frame(1.0);
        let mut renderer = rune::render::Renderer::new(&gpu.device, rune::render::OFFSCREEN_FORMAT);
        render(
            &gpu,
            &mut renderer,
            &mut rt,
            size,
            &format!("{name}-appearance-before"),
        );
        theme_query(&mut rt);
        render(
            &gpu,
            &mut renderer,
            &mut rt,
            size,
            &format!("{name}-appearance-chooser"),
        );
        rt.key(KeyEvent {
            key: Key::Escape,
            mods: Mods::default(),
        });
        rt.frame(1.0);
        assert!(rt.state.settings_open);
        assert!(!rt.state.palette_open);
        assert_eq!(rt.state.settings.theme, before);
        render(
            &gpu,
            &mut renderer,
            &mut rt,
            size,
            &format!("{name}-appearance-cancel"),
        );
        theme_query(&mut rt);
        rt.key(KeyEvent {
            key: Key::Enter,
            mods: Mods::default(),
        });
        rt.frame(1.0);
        assert!(rt.state.settings_open);
        assert!(!rt.state.palette_open);
        assert_eq!(rt.state.settings.theme, crate::theme::find("nord"));
        render(
            &gpu,
            &mut renderer,
            &mut rt,
            size,
            &format!("{name}-appearance-accepted"),
        );
    }
}

#[test]
fn typing_with_no_field_focused_starts_a_search() {
    let mut rt = runtime((1100.0, 720.0), false);
    rt.frame(0.0);
    assert!(!rt.state.settings_ui.query.focused);
    for c in ["c", "l", "i", "p"] {
        rt.key(KeyEvent {
            key: Key::Text(c.into()),
            mods: Mods::default(),
        });
        rt.frame(0.0);
    }
    assert_eq!(rt.state.settings_ui.query.text, "clip");
    assert!(rt.find("set-copy-select").is_some());
    // A chord is a command, never the start of a search.
    rt.state.settings_ui.query.clear();
    rt.state.settings_ui.query.focused = false;
    rt.key(KeyEvent {
        key: Key::Text("q".into()),
        mods: Mods {
            ctrl: true,
            ..Mods::default()
        },
    });
    rt.frame(0.0);
    assert!(rt.state.settings_ui.query.text.is_empty());
}
