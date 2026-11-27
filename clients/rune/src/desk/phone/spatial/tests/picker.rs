//! Real pane-picker compositions; the same exporter runs before and after.
use super::*;
use rune::Runtime;

const LONG_NAME: &str = "Review authentication and reconnect behavior on the build workstation";

fn prepare(
    size: (u32, u32),
    scale: u32,
    native: bool,
    dark: bool,
    large: bool,
) -> (std::sync::Arc<phux_client_runtime::Client>, Runtime<Desk>) {
    let (client, mut rt) = fixture(true);
    rt.set_viewport((size.0 as f32, size.1 as f32), scale as f32);
    rt.set_system_reduced_motion(true);
    rt.set_srgb_blend(true);
    rt.update(|d, _| {
        d.settings.theme = crate::theme::find(if dark { "phux" } else { "phux-light" });
        d.phone.as_mut().unwrap().keyboard = !native;
        if native {
            d.native = Some(crate::mobile::Platform::test());
        }
        if large {
            d.set_accessibility(false, "UICTContentSizeCategoryAccessibilityXXXL");
        }
        let topology = d.connection.topology.as_mut().unwrap();
        for pane in &mut topology.panes {
            pane.name = Some(LONG_NAME.to_owned());
            pane.title = Some(LONG_NAME.to_owned());
        }
    });
    settle(&mut rt);
    tap(&mut rt, "phone-pane-picker");
    settle(&mut rt);
    assert!(
        rt.elements().iter().any(|element| {
            element.key.as_deref() == Some("phone-pick-@1")
                && element
                    .label
                    .as_deref()
                    .is_some_and(|label| label.starts_with(LONG_NAME))
        }),
        "the real picker must contain the long fixture name"
    );
    (client, rt)
}

fn save(
    gpu: &rune::render::Gpu,
    renderer: &mut rune::render::Renderer,
    rt: &mut Runtime<Desk>,
    size: (u32, u32),
    name: &str,
) {
    let pixels = crate::ui::offscreen_as_app_with_renderer(gpu, renderer, rt, size);
    if let Some(directory) = std::env::var_os("RUNE_NAVIGATION_SNAP") {
        let path = std::path::PathBuf::from(directory).join(format!("{name}.png"));
        crate::test_support::visual::write(&path, rt, size, &pixels, name);
    }
}

#[test]
fn phone_pane_picker_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for (size, scale, native) in [
        ((393, 852), 1, false),
        ((320, 568), 1, false),
        ((393, 480), 2, true),
        ((320, 340), 2, true),
    ] {
        for dark in [true, false] {
            for large in [false, true] {
                let (client, mut rt) = prepare(size, scale, native, dark, large);
                let mut renderer = rune::render::Renderer::new(
                    &gpu.device,
                    rune::render::OFFSCREEN_FORMAT.remove_srgb_suffix(),
                );
                renderer.set_srgb_blend(true);
                let name = format!(
                    "pane-picker-{}-{}-{}x{}-{scale}x-native-{native}",
                    if dark { "dark" } else { "light" },
                    if large { "ax5" } else { "standard" },
                    size.0,
                    size.1
                );
                let pixels = (size.0 * scale, size.1 * scale);
                save(&gpu, &mut renderer, &mut rt, pixels, &format!("{name}-top"));
                rt.update(|_, tx| {
                    tx.focus("phone-pick-@1", true);
                    tx.scroll_into_view("phone-pick-@1");
                    tx.instant();
                });
                settle(&mut rt);
                save(
                    &gpu,
                    &mut renderer,
                    &mut rt,
                    pixels,
                    &format!("{name}-focused"),
                );
                rt.update(|_, tx| {
                    tx.scroll_to_instant("phone-pane-list", 0.0, 100_000.0);
                });
                settle(&mut rt);
                save(
                    &gpu,
                    &mut renderer,
                    &mut rt,
                    pixels,
                    &format!("{name}-bottom"),
                );
                assert_eq!(focus(&rt), id(1));
                assert!(rt.state.phone.as_ref().unwrap().picker);
                no_input(&client);
            }
        }
    }
}

fn reveal(rt: &mut Runtime<Desk>, key: &str) {
    rt.update(|_, tx| {
        tx.focus(key, true);
        tx.scroll_into_view(key);
        tx.instant();
    });
    settle(rt);
}

fn contained(inner: [f32; 4], outer: [f32; 4]) -> bool {
    inner[0] >= outer[0] - 0.5
        && inner[1] >= outer[1] - 0.5
        && inner[0] + inner[2] <= outer[0] + outer[2] + 0.5
        && inner[1] + inner[3] <= outer[1] + outer[3] + 0.5
}

fn whole_control(rt: &Runtime<Desk>, key: &str) {
    let button = geometry(rt, key);
    let text = geometry(rt, &format!("{key}-text"));
    assert!(
        button.rect[2] >= 44.0 && button.rect[3] >= 44.0,
        "{key}: {button:?}"
    );
    assert!(
        contained(text.rect, button.rect),
        "{key}: text {text:?} outside {button:?}"
    );
    assert!(
        contained(button.rect, button.visible),
        "{key}: incomplete target {button:?}"
    );
    assert!(
        contained(text.rect, text.visible),
        "{key}: incomplete text {text:?}"
    );
    // A pinned Done sits under the list, in the panel; the rows scroll in it.
    let pinned = !button.path.contains("phone-pane-list");
    let viewport = geometry(
        rt,
        if pinned {
            "phone-pane-panel"
        } else {
            "phone-pane-list"
        },
    )
    .visible;
    let margin = phux_design::tokens::SPACE.s2;
    assert!(
        button.rect[1] >= viewport[1] + margin - 0.5
            && button.rect[1] + button.rect[3] <= viewport[1] + viewport[3] - margin + 0.5,
        "{key}: focus-ring clearance lost: {button:?} in {viewport:?}"
    );
}

#[test]
fn picker_rows_fit_and_reveal_whole_targets_without_shrinking_requested_text() {
    for (size, scale, native) in [
        ((393, 852), 1, false),
        ((320, 568), 1, false),
        ((393, 480), 2, true),
        ((320, 340), 2, true),
    ] {
        for dark in [false, true] {
            for large in [false, true] {
                let (client, mut rt) = prepare(size, scale, native, dark, large);
                let before = stored(&client);
                if !large {
                    // Done and the arrows pinned under the list still leave
                    // a whole row to pick as it opens, keyboard or not.
                    let first = geometry(&rt, "phone-pick-@1");
                    assert!(
                        contained(first.rect, first.visible),
                        "{size:?}: no whole row above the pinned controls: {first:?}"
                    );
                }
                let label = geometry(&rt, "phone-pick-@1").label.unwrap();
                assert_eq!(label, format!("{LONG_NAME} · @1 · current"));
                assert_eq!(
                    geometry(&rt, "phone-pick-@1-text").text.unwrap(),
                    format!("@1 · {LONG_NAME} · current")
                );
                let requested =
                    crate::ui::tokens::phone::ty(rt.state.content_size().factor()).label;
                let line = requested.line_px();
                assert_eq!(
                    large,
                    line > 44.0,
                    "the actual requested text size is preserved"
                );
                assert!(geometry(&rt, "phone-pick-@1-text").rect[3] >= line - 0.5);
                for key in [
                    "phone-direction-Left",
                    "phone-direction-Up",
                    "phone-direction-Down",
                    "phone-direction-Right",
                    "phone-pick-@1",
                    "phone-pick-@2",
                    "phone-pick-@3",
                    "phone-pick-@4",
                    "phone-picker-actions",
                    "phone-picker-close",
                ] {
                    reveal(&mut rt, key);
                    whole_control(&rt, key);
                }
                assert_eq!(geometry(&rt, "phone-navigation-toolbar").rect[3], 44.0);
                assert_eq!(geometry(&rt, "phone-navigation").rect[3], 44.0);
                assert_eq!(
                    focus(&rt),
                    id(1),
                    "focus reveal must not select another pane"
                );
                assert_eq!(stored(&client), before);
                no_input(&client);
            }
        }
    }
}

#[test]
fn picker_final_touch_actions_preserve_keyboard_layout_and_terminal_input() {
    for action in ["phone-picker-close", "phone-picker-actions"] {
        let (client, mut rt) = prepare((320, 340), 2, true, true, true);
        let before = stored(&client);
        let keyboard = rt.state.phone.as_ref().unwrap().keyboard;
        reveal(&mut rt, action);
        whole_control(&rt, action);
        tap(&mut rt, action);
        settle(&mut rt);
        assert!(!rt.state.phone.as_ref().unwrap().picker);
        assert_eq!(
            rt.state.phone_page(),
            (action == "phone-picker-actions")
                .then_some(crate::desk::phone::navigation::Page::Actions)
        );
        assert_eq!(rt.state.phone.as_ref().unwrap().keyboard, keyboard);
        assert_eq!(focus(&rt), id(1));
        assert_eq!(stored(&client), before);
        no_input(&client);
    }
}

#[test]
fn picker_bounds_unbroken_multiline_and_grapheme_names_and_keeps_full_identity() {
    let (client, mut rt) = prepare((320, 340), 2, true, false, true);
    let names = [
        "authentication_reconnect_workstation_validation_without_word_breaks",
        "Authentication review\nReconnect diagnostics\nWorkstation",
        "👩🏽‍💻 Review e\u{301}criture and 🔐 authentication",
    ];
    rt.update(|d, _| {
        for (pane, name) in d
            .connection
            .topology
            .as_mut()
            .unwrap()
            .panes
            .iter_mut()
            .zip(names)
        {
            pane.name = Some(name.to_owned());
        }
    });
    settle(&mut rt);
    for (number, name) in (1..).zip(names) {
        let key = format!("phone-pick-@{number}");
        reveal(&mut rt, &key);
        whole_control(&rt, &key);
        let suffix = if number == 1 { " · current" } else { "" };
        assert_eq!(
            geometry(&rt, &key).label.unwrap(),
            format!("{name} · @{number}{suffix}")
        );
    }
    no_input(&client);
}

#[test]
fn picker_keeps_two_lines_through_regular_xxxl_before_accessibility_sizes() {
    let (client, mut rt) = prepare((320, 340), 2, true, true, false);
    for (category, two_lines) in [
        ("UICTContentSizeCategoryXXXL", true),
        ("UICTContentSizeCategoryAccessibilityM", false),
    ] {
        rt.update(|d, _| {
            d.set_accessibility(false, category);
        });
        reveal(&mut rt, "phone-pick-@1");
        whole_control(&rt, "phone-pick-@1");
        let line = crate::ui::tokens::phone::ty(rt.state.content_size().factor())
            .label
            .line_px();
        let height = geometry(&rt, "phone-pick-@1-text").rect[3];
        assert_eq!(
            height > line * 1.5,
            two_lines,
            "{category}: {height} vs {line}"
        );
        assert!(height >= line - 0.5, "never shrink the requested type");
    }
    no_input(&client);
}

#[test]
fn picker_keyboard_traversal_reveals_every_whole_row_and_activates_done() {
    let (client, mut rt) = prepare((320, 340), 2, true, true, true);
    let before = stored(&client);
    // Start at a named control rather than assuming modal-open focus is empty.
    reveal(&mut rt, "phone-direction-Left");
    whole_control(&rt, "phone-direction-Left");
    for key in [
        "phone-direction-Up",
        "phone-direction-Down",
        "phone-direction-Right",
        "phone-pick-@1",
        "phone-pick-@2",
        "phone-pick-@3",
        "phone-pick-@4",
        "phone-picker-actions",
        "phone-picker-close",
    ] {
        rt.key(crate::keys::plain(rune::Key::Tab));
        settle(&mut rt);
        assert!(geometry(&rt, key).focused, "Tab should focus {key}");
        whole_control(&rt, key);
    }
    rt.key(crate::keys::plain(rune::Key::Enter));
    settle(&mut rt);
    assert!(!rt.state.phone.as_ref().unwrap().picker);
    assert_eq!(stored(&client), before);
    assert_eq!(focus(&rt), id(1));
    no_input(&client);
}
