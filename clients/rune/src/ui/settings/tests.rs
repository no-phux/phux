//! Real Rune layout, accessibility and GPU frames for the Settings surface.

use rune::accesskit::Action;
use rune::{Cx, El, Env, Key, KeyEvent, Mods, Runtime};

use super::*;
use crate::desk::updates::Work;
use crate::launch::Resolved;

fn surface(desk: &Desk, cx: &mut Cx<Desk>) -> El {
    let ink = super::super::ink(cx.system_dark(), cx.accent(), desk.settings.theme);
    view(desk, &ink)
}

fn runtime(size: (f32, f32), phone: bool) -> Runtime<Desk> {
    let mut desk = Desk::new(&Resolved::offline("settings layout")).with_phone(phone);
    desk.settings_open = true;
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
    }
}
