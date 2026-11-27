//! Whole targets and field-owned touch, not merely 44-point visible fragments.
use super::*;
use crate::ui::element;

fn accessory_scene(
    size: (f32, f32),
    category: &str,
    light: bool,
    field: &str,
) -> (std::sync::Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport(size, 2.0);
    rt.set_system_reduced_motion(true);
    rt.update(|d, tx| {
        d.native = Some(crate::mobile::Platform::test());
        d.settings.theme = if light {
            crate::theme::find("phux-light")
        } else {
            None
        };
        d.set_accessibility(false, category);
        if field == "settings-query" {
            d.settings_open = true;
        } else if !field.is_empty() {
            d.open_phone_sessions();
        }
        tx.instant();
    });
    settle_for_snapshot(&mut rt);
    if !field.is_empty() {
        reveal_accessory(&mut rt, field);
        tap(&mut rt, field, 0.1);
    }
    settle_for_snapshot(&mut rt);
    (client, rt)
}

fn inside(inner: [f32; 4], outer: [f32; 4]) {
    for axis in [0, 1] {
        assert!(
            inner[axis] >= outer[axis] - 0.5,
            "{inner:?} outside {outer:?}"
        );
        assert!(
            inner[axis] + inner[axis + 2] <= outer[axis] + outer[axis + 2] + 0.5,
            "{inner:?} outside {outer:?}"
        );
    }
}

fn whole_accessory(rt: &Runtime<Desk>, key: &str) {
    let control = element(rt, key);
    assert!(!control.inert && !control.exiting, "{control:?}");
    assert!(
        control.rect[2] >= 44.0 && control.rect[3] >= 44.0,
        "{control:?}"
    );
    inside(control.rect, [0.0, 0.0, rt.viewport.0, rt.viewport.1]);
    inside(control.rect, control.visible);
    let children: Vec<_> = rt
        .elements()
        .into_iter()
        .filter(|e| !e.exiting && e.path.starts_with(&format!("{}/", control.path)))
        .collect();
    assert!(
        !children.is_empty(),
        "{key} must retain its visible text/icon"
    );
    for child in children {
        inside(child.rect, control.rect);
        inside(child.rect, child.visible);
    }
}

fn reveal_accessory(rt: &mut Runtime<Desk>, key: &str) {
    rt.update(|_, tx| tx.scroll_into_view(key));
    settle_for_snapshot(rt);
}

#[test]
fn session_and_action_lists_keep_their_key_bar_free_composition() {
    for size in [(320.0, 340.0), (393.0, 480.0)] {
        for keyboard in [false, true] {
            let (client, mut rt) = accessory_scene(
                size,
                "UICTContentSizeCategoryAccessibilityXXXL",
                false,
                "phone-session-query",
            );
            rt.update(|d, _| d.phone.as_mut().unwrap().keyboard = keyboard);
            rt.key(crate::keys::plain(Key::Text("review".into())));
            settle_for_snapshot(&mut rt);
            assert!(rt.find("phone-accessory").is_none());
            assert!(rt.state.phone.as_ref().unwrap().query.focused);
            rt.key(crate::keys::plain(Key::Enter));
            settle_for_snapshot(&mut rt);
            let phone = rt.state.phone.as_ref().unwrap();
            assert_eq!(phone.page, Some(Page::Sessions));
            assert!(!phone.query.focused);
            assert_eq!(phone.query.text, "review");
            assert_eq!(phone.keyboard, keyboard);
            assert!(rt.find("phone-accessory").is_none());
            rt.update(|d, _| {
                d.close_phone_navigation();
                d.open_phone_actions();
            });
            settle_for_snapshot(&mut rt);
            assert_eq!(rt.state.phone_page(), Some(Page::Actions));
            assert!(rt.find("phone-accessory").is_none());
            no_terminal_input(&client);
        }
    }
}

#[test]
fn native_accessory_targets_and_content_fit_at_all_supported_text_sizes() {
    for size in [(320.0, 340.0), (393.0, 480.0)] {
        for category in [
            "UICTContentSizeCategoryL",
            "UICTContentSizeCategoryAccessibilityL",
            "UICTContentSizeCategoryAccessibilityXL",
            "UICTContentSizeCategoryAccessibilityXXXL",
        ] {
            for light in [false, true] {
                // Sessions/Actions deliberately have no terminal key bar.
                // Settings search and the terminal still own this accessory.
                for field in ["", "settings-query"] {
                    let (client, mut rt) = accessory_scene(size, category, light, field);
                    let end = if field.is_empty() {
                        "phone-keyboard-toggle"
                    } else {
                        "phone-native-done"
                    };
                    whole_accessory(&rt, "phone-command");
                    whole_accessory(&rt, end);
                    let pinned = [element(&rt, "phone-command").rect, element(&rt, end).rect];
                    let strip = element(&rt, "phone-strip");
                    assert!(
                        !element(&rt, end)
                            .path
                            .starts_with(&format!("{}/", strip.path))
                    );
                    let mut keys = vec!["phone-key-Escape", "phone-key-Tab", "phone-pad"];
                    if field.is_empty() {
                        keys.extend([
                            "phone-latch-Control",
                            "phone-latch-Option",
                            "phone-key-Interrupt",
                            "phone-key-Left",
                            "phone-key-Down",
                            "phone-key-Up",
                            "phone-key-Right",
                            "phone-key-Pipe",
                            "phone-key-Tilde",
                            "phone-key-Slash",
                            "phone-key-Hyphen",
                            "phone-key-Paste",
                            "phone-attach",
                        ]);
                    } else {
                        assert!(rt.find("phone-attach").is_none());
                        assert!(rt.find("phone-latch-Control").is_none());
                        assert!(
                            element(&rt, "phone-pad")
                                .path
                                .starts_with(&format!("{}/", strip.path))
                        );
                    }
                    for key in keys {
                        reveal_accessory(&mut rt, key);
                        whole_accessory(&rt, key);
                        let control = element(&rt, key);
                        if control.path.starts_with(&format!("{}/", strip.path)) {
                            inside(control.rect, element(&rt, "phone-strip").rect);
                        }
                        whole_accessory(&rt, "phone-command");
                        whole_accessory(&rt, end);
                        assert_eq!(
                            [element(&rt, "phone-command").rect, element(&rt, end).rect],
                            pinned
                        );
                    }
                    no_terminal_input(&client);
                }
            }
        }
    }
}

#[test]
fn revealed_native_tab_done_and_escape_keep_the_settings_search_owner() {
    for keyboard in [false, true] {
        let (client, mut rt) = accessory_scene(
            (320.0, 340.0),
            "UICTContentSizeCategoryAccessibilityXXXL",
            false,
            "settings-query",
        );
        rt.update(|d, _| d.phone.as_mut().unwrap().keyboard = keyboard);
        rt.key(crate::keys::plain(Key::Text("review".into())));
        reveal_accessory(&mut rt, "phone-key-Tab");
        whole_accessory(&rt, "phone-key-Tab");
        tap(&mut rt, "phone-key-Tab", 1.0);
        assert!(
            !rt.state.settings_search_focused(),
            "Tab finishes search editing"
        );
        assert_eq!(rt.state.settings_ui.query.text, "review");
        assert!(rt.state.settings_open);
        assert!(rt.find("phone-accessory").is_none());
        tap(&mut rt, "settings-query", 1.5);
        whole_accessory(&rt, "phone-native-done");
        tap(&mut rt, "phone-native-done", 2.0);
        assert!(rt.state.settings_open);
        assert_eq!(rt.state.settings_ui.query.text, "review");
        assert!(!rt.state.settings_search_focused());
        assert_eq!(rt.state.phone.as_ref().unwrap().keyboard, keyboard);
        tap(&mut rt, "settings-query", 2.5);
        reveal_accessory(&mut rt, "phone-key-Escape");
        whole_accessory(&rt, "phone-key-Escape");
        tap(&mut rt, "phone-key-Escape", 3.0);
        assert!(rt.state.settings_open && rt.state.settings_search_focused());
        assert!(rt.state.settings_ui.query.text.is_empty());
        assert_eq!(rt.state.phone.as_ref().unwrap().keyboard, keyboard);
        no_terminal_input(&client);
    }
}

#[test]
fn native_done_blurs_but_does_not_submit_or_discard_the_rename_draft() {
    for keyboard in [false, true] {
        let (client, mut rt) = accessory_scene(
            (320.0, 340.0),
            "UICTContentSizeCategoryAccessibilityXXXL",
            true,
            "",
        );
        rt.update(|d, tx| {
            d.phone.as_mut().unwrap().keyboard = keyboard;
            d.connection.tabs.renaming = Some(crate::desk::Renaming::Tab([1; 16]));
            d.connection.tabs.rename_field.replace_text("unsaved draft");
            d.connection.tabs.rename_field.focused = true;
            tx.instant();
        });
        settle_for_snapshot(&mut rt);
        whole_accessory(&rt, "phone-native-done");
        tap(&mut rt, "phone-native-done", 1.0);
        assert!(rt.state.connection.tabs.renaming.is_some());
        assert_eq!(rt.state.connection.tabs.rename_field.text, "unsaved draft");
        assert!(!rt.state.connection.tabs.rename_field.focused);
        assert_eq!(rt.state.phone.as_ref().unwrap().keyboard, keyboard);
        no_terminal_input(&client);
    }
}

#[test]
fn revealed_native_pad_owns_the_drag_without_scrolling_and_releases_on_cancel() {
    let (client, mut rt) = accessory_scene(
        (320.0, 340.0),
        "UICTContentSizeCategoryAccessibilityXXXL",
        false,
        "settings-query",
    );
    rt.key(crate::keys::plain(Key::Text("review".into())));
    reveal_accessory(&mut rt, "phone-pad");
    whole_accessory(&rt, "phone-pad");
    let markers = [
        "phone-pad",
        "phone-key-Escape",
        "settings-query",
        "settings-content",
    ];
    let positions = |rt: &Runtime<Desk>| {
        markers.map(|key| {
            let e = element(rt, key);
            (e.rect, e.scrolled)
        })
    };
    let before = positions(&rt);
    let (x, y) = rt.find("phone-pad").unwrap();
    rt.touch(71, TouchPhase::Started, x, y, 10.0);
    rt.frame(0.65);
    for (phase, at) in [
        (TouchPhase::Moved, 10.7),
        (TouchPhase::Moved, 10.8),
        (TouchPhase::Cancelled, 10.9),
    ] {
        rt.touch(71, phase, x - 32.0, y, at);
        settle_for_snapshot(&mut rt);
        assert_eq!(
            positions(&rt),
            before,
            "pad must not scroll either ancestor viewport"
        );
        let edit = &rt.state.settings_ui.query;
        assert!(edit.focused);
        assert_eq!((&*edit.text, edit.anchor, edit.focus), ("review", 4, 4));
    }
    assert!(!rt.touching());
    rt.touch(71, TouchPhase::Moved, x - 64.0, y, 11.0);
    rt.touch(71, TouchPhase::Ended, x - 64.0, y, 11.1);
    settle_for_snapshot(&mut rt);
    let edit = &rt.state.settings_ui.query;
    assert_eq!((&*edit.text, edit.anchor, edit.focus), ("review", 4, 4));
    assert_eq!(positions(&rt), before);
    assert!(rt.state.pointer.menu.is_none());
    no_terminal_input(&client);
}

#[test]
fn native_accessory_overflow_cue_tracks_scroll_without_covering_keys() {
    for size in [(320.0, 340.0), (393.0, 480.0)] {
        let (client, mut rt) =
            accessory_scene(size, "UICTContentSizeCategoryL", false, "settings-query");
        let cue = element(&rt, "phone-strip-cue");
        assert!(cue.inert);
        assert_eq!(cue.opacity, 0.0, "no cue when every field key fits");
        rt.update(|d, _| {
            d.set_accessibility(false, "UICTContentSizeCategoryAccessibilityXXXL");
        });
        settle_for_snapshot(&mut rt);
        let cue = element(&rt, "phone-strip-cue");
        assert_eq!(cue.opacity, 1.0);
        let strip = element(&rt, "phone-strip");
        let maximum = element(&rt, "phone-command").rect[3]
            .max(strip.rect[3])
            .max(element(&rt, "phone-native-done").rect[3]);
        assert!(
            (element(&rt, "phone-accessory").rect[3] - maximum - phux_design::tokens::SPACE.s1)
                .abs()
                < 0.5,
            "the cue sits inside the strip, adding no height to the bar"
        );
        assert!(cue.rect[1] >= strip.rect[1] - 0.5);
        assert!((cue.rect[1] + cue.rect[3] - strip.rect[1] - strip.rect[3]).abs() < 0.5);
        let before = element(&rt, "phone-strip-thumb").rect;
        inside(before, cue.rect);
        assert!(
            before[3] > element(&rt, "phone-strip-track").rect[3],
            "position must remain distinct even when forced colours make both marks the same ink"
        );
        let pinned = [
            element(&rt, "phone-command").rect,
            element(&rt, "phone-native-done").rect,
        ];
        rt.update(|_, tx| tx.scroll_to_instant("phone-strip", 100_000.0, 0.0));
        settle_for_snapshot(&mut rt);
        let after = element(&rt, "phone-strip-thumb").rect;
        inside(after, cue.rect);
        assert!(after[0] > before[0]);
        assert_eq!(after[2], before[2]);
        rt.update(|d, _| d.settings_ui.query.replace_text("unrelated rebuild"));
        settle_for_snapshot(&mut rt);
        assert_eq!(element(&rt, "phone-strip-thumb").rect, after);
        assert_eq!(element(&rt, "phone-strip-cue").opacity, 1.0);
        assert_eq!(
            [
                element(&rt, "phone-command").rect,
                element(&rt, "phone-native-done").rect
            ],
            pinned
        );
        reveal_accessory(&mut rt, "phone-key-Tab");
        whole_accessory(&rt, "phone-key-Tab");
        whole_accessory(&rt, "phone-native-done");
        no_terminal_input(&client);
    }
}

#[test]
fn native_accessory_cue_stays_inside_presented_width_during_layout_springs() {
    let (client, mut rt) = accessory_scene(
        (320.0, 340.0),
        "UICTContentSizeCategoryAccessibilityXXXL",
        false,
        "settings-query",
    );
    rt.set_system_reduced_motion(false);
    rt.frame(0.0);
    for open in [false, true] {
        rt.update(|d, tx| {
            d.settings_open = open;
            if open {
                d.focus_settings_search();
            } else {
                d.blur_settings_search(tx);
            }
        });
        let mut moving = false;
        for frame in 1..=60 {
            rt.frame(1.0 / 60.0);
            let strip = element(&rt, "phone-strip");
            let cue = element(&rt, "phone-strip-cue");
            let track = element(&rt, "phone-strip-track");
            let thumb = element(&rt, "phone-strip-thumb");
            moving |= (strip.rect[2] - strip.target[2]).abs() > 0.5;
            println!(
                "{}",
                serde_json::json!({
                    "open": open, "frame": frame, "elapsed_ms": f64::from(frame) * 1000.0 / 60.0,
                    "strip": strip.rect, "target": strip.target, "cue": cue.rect,
                    "track": track.rect, "thumb": thumb.rect,
                })
            );
            inside(track.rect, cue.rect);
            inside(thumb.rect, cue.rect);
        }
        assert!(moving, "exercise an actual non-instant width transition");
    }
    no_terminal_input(&client);
}

#[test]
fn swiping_native_keys_reveals_more_without_activating_escape_or_typing() {
    let (client, mut rt) = accessory_scene(
        (320.0, 340.0),
        "UICTContentSizeCategoryAccessibilityXXXL",
        false,
        "settings-query",
    );
    rt.key(crate::keys::plain(Key::Text("review".into())));
    settle_for_snapshot(&mut rt);
    let before = element(&rt, "phone-key-Escape").scrolled[0];
    let (x, y) = rt.find("phone-key-Escape").unwrap();
    for (phase, dx, at) in [
        (TouchPhase::Started, 0.0, 10.0),
        (TouchPhase::Moved, -40.0, 10.05),
        (TouchPhase::Moved, -80.0, 10.1),
        (TouchPhase::Ended, -80.0, 10.15),
    ] {
        rt.touch(103, phase, x + dx, y, at);
        rt.frame(1.0 / 60.0);
    }
    settle(&mut rt);
    assert!(element(&rt, "phone-key-Escape").scrolled[0] > before + 8.0);
    assert!(rt.state.settings_open, "a swipe must not dismiss Settings");
    assert!(rt.state.settings_search_focused());
    assert_eq!(
        rt.state.settings_ui.query.text, "review",
        "a swipe must not press Escape"
    );
    assert!(
        !rt.state.palette_open,
        "the swipe cannot press Commands either"
    );
    no_terminal_input(&client);
}

#[test]
fn large_drawn_settings_reveal_every_relocated_accessory_key() {
    for size in [(320.0, 568.0), (393.0, 852.0)] {
        let (client, mut rt) = accessory_scene(
            size,
            "UICTContentSizeCategoryAccessibilityXXXL",
            false,
            "settings-query",
        );
        rt.update(|d, _| d.native = None);
        rt.set_viewport(size, 1.0);
        settle_for_snapshot(&mut rt);
        let strip = element(&rt, "phone-strip");
        assert!(
            element(&rt, "phone-pad")
                .path
                .starts_with(&format!("{}/", strip.path))
        );
        for key in [
            "phone-key-Escape",
            "phone-key-Tab",
            "phone-latch-Control",
            "phone-latch-Option",
            "phone-key-Interrupt",
            "phone-key-Left",
            "phone-key-Down",
            "phone-key-Up",
            "phone-key-Right",
            "phone-key-Pipe",
            "phone-key-Tilde",
            "phone-key-Slash",
            "phone-key-Hyphen",
            "phone-key-Paste",
            "phone-pad",
        ] {
            reveal_accessory(&mut rt, key);
            whole_accessory(&rt, key);
            whole_accessory(&rt, "phone-command");
            whole_accessory(&rt, "phone-keyboard-toggle");
        }
        no_terminal_input(&client);
    }
}
