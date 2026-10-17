//! Full native chrome pixels, using the same Metal renderer as the terminal fixtures.

use gpui::Keystroke;
use phux_desktop::app::{AppOptions, Desk, Modal};
use std::sync::Arc;

pub(super) fn navigation_and_settings_render() {
    for light in [false, true] {
        render_appearance(light);
    }
}

fn render_appearance(light: bool) {
    let platform = gpui_platform::current_platform(true);
    let mut cx = gpui::HeadlessAppContext::with_platform(
        platform.text_system(),
        Arc::new(phux_desktop::assets::Assets),
        gpui_platform::current_headless_renderer,
    );
    let root = std::env::temp_dir().join(format!("phux-hub-pixels-{}-{light}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut prefs = phux_desktop::settings::default_display();
    prefs.ghostty_keys = false;
    if let Some(theme) = phux_desktop::settings::theme::themes()
        .into_iter()
        .find(|theme| (theme.appearance == "light") == light)
    {
        prefs.theme_id = theme.id;
    }
    let layout = phux_desktop::workspace::persist::save_layout("", &[], &prefs, None);
    std::fs::write(
        root.join("layout.json"),
        serde_json::to_vec(&phux_desktop::workspace::persist::layout_value(&layout)).unwrap(),
    )
    .unwrap();
    let window = cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        phux_desktop::app::open(
            AppOptions {
                socket: root.join("no-server.sock"),
                session: String::new(),
                phux: root.join("not-installed"),
                layout_path: root.join("layout.json"),
                fresh: true,
                initial_terminal: None,
                quick: false,
                dev_telemetry: false,
                startup_error: None,
            },
            cx,
        )
        .unwrap()
    });
    let desk = window
        .update(&mut cx, |root, _, _| {
            root.view().clone().downcast::<Desk>().unwrap()
        })
        .unwrap();
    let appearance = if light { "light" } else { "dark" };
    for (command, name) in [
        ("goto", "work"),
        ("palette", "actions"),
        ("settings", "settings"),
    ] {
        cx.update_window(window.into(), |_, window, cx| {
            desk.update(cx, |desk, cx| desk.execute(command, window, cx))
        })
        .unwrap();
        draw(&mut cx, window.into());
        save(&mut cx, window.into(), &format!("{appearance}-{name}"));
    }
    for character in "layout".chars() {
        cx.update_window(window.into(), |_, window, cx| {
            let key = character.to_string();
            window.dispatch_keystroke(
                Keystroke {
                    key: key.clone(),
                    key_char: Some(key),
                    ..Default::default()
                },
                cx,
            );
        })
        .unwrap();
        draw(&mut cx, window.into());
    }
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_keystroke(Keystroke::parse("enter").unwrap(), cx);
    })
    .unwrap();
    draw(&mut cx, window.into());
    assert_eq!(
        desk.read_with(&cx, |desk, _| desk.modal.clone()),
        Modal::Settings
    );
    save(&mut cx, window.into(), &format!("{appearance}-layout"));
    ghostty_settings_render(&mut cx, window.into(), &desk, appearance);
    cx.update(|cx| desk.update(cx, |desk, cx| desk.prepare_close(cx)));
    drop(cx);
    std::fs::remove_dir_all(root).unwrap();
}

/// A reloaded Ghostty config with a duplicated chord, a malformed line and a
/// valid line this app cannot use: the reload rebinds, repaints the Ghostty
/// theme, posts one notice for the malformed line only, and Settings renders
/// the conflict and both line groups.
fn ghostty_settings_render(
    cx: &mut gpui::HeadlessAppContext,
    window: gpui::AnyWindowHandle,
    desk: &gpui::Entity<Desk>,
    appearance: &str,
) {
    use phux_desktop::settings::GhosttySource;
    let sources = [GhosttySource {
        path: Some("/fixture/ghostty/config".into()),
        text: "background = #123456\nforeground = #fedcba\n\
               keybind = super+d=new_split:right\nkeybind = super+d=new_split:down\n\
               not a setting\npalette = 200=#ffffff\n"
            .to_owned(),
    }];
    cx.update(|cx| {
        desk.update(cx, |desk, cx| {
            desk.prefs.ghostty_keys = true;
            desk.prefs.theme_id = "ghostty".to_owned();
            desk.apply_ghostty_sources(Some(&sources), cx);
        });
    });
    draw(cx, window);
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.palette().theme.background, "#123456");
        assert_eq!(
            desk.shown_chord("split-down"),
            Some(phux_desktop::settings::display_chord("cmd+d"))
        );
        // Refusals show as the peeked notice, not in the feed list.
        let notice = desk.feed.peek().expect("reload notice");
        assert_eq!(notice.title, "Skipped 1 Ghostty config line");
        let body = notice.body.as_deref().unwrap_or_default();
        assert!(body.contains("config:5: not a setting"), "{body}");
        assert_eq!(desk.ghostty().unsupported.len(), 1);
    });
    for query in ["conflicts", "skipped"] {
        // `settings` toggles, so an open panel closes first and reopens
        // with an empty search.
        for _ in 0..2 {
            cx.update_window(window, |_, window, cx| {
                desk.update(cx, |desk, cx| desk.execute("settings", window, cx));
            })
            .unwrap();
            draw(cx, window);
            if desk.read_with(cx, |desk, _| desk.modal.clone()) == Modal::Settings {
                break;
            }
        }
        type_text(cx, window, query);
        cx.update_window(window, |_, window, cx| {
            window.dispatch_keystroke(Keystroke::parse("enter").unwrap(), cx);
        })
        .unwrap();
        draw(cx, window);
        assert_eq!(
            desk.read_with(cx, |desk, _| desk.modal.clone()),
            Modal::Settings
        );
        save(cx, window, &format!("{appearance}-ghostty-{query}"));
    }
}

fn type_text(cx: &mut gpui::HeadlessAppContext, window: gpui::AnyWindowHandle, text: &str) {
    for character in text.chars() {
        cx.update_window(window, |_, window, cx| {
            let key = character.to_string();
            window.dispatch_keystroke(
                Keystroke {
                    key: key.clone(),
                    key_char: Some(key),
                    ..Default::default()
                },
                cx,
            );
        })
        .unwrap();
        draw(cx, window);
    }
}

fn draw(cx: &mut gpui::HeadlessAppContext, window: gpui::AnyWindowHandle) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.draw(cx).clear(cx);
        })
        .unwrap();
    }
}

fn save(cx: &mut gpui::HeadlessAppContext, window: gpui::AnyWindowHandle, name: &str) {
    let image = cx.capture_screenshot(window).expect("native hub pixels");
    assert!(image.width() >= 720 && image.height() >= 480);
    if let Some(dir) = std::env::var_os("PHUX_DESKTOP_UI_EVIDENCE") {
        let dir = std::path::PathBuf::from(dir);
        assert!(
            dir.starts_with(std::env::temp_dir()) || dir.starts_with("/tmp"),
            "visual evidence must stay disposable"
        );
        std::fs::create_dir_all(&dir).unwrap();
        image.save(dir.join(format!("{name}.png"))).unwrap();
    }
}
