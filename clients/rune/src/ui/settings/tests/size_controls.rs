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
                    rt.update(|_, tx| {
                        tx.scroll_into_view(control);
                        tx.instant();
                    });
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

#[test]
fn phone_size_controls_keep_their_full_targets_inside_the_card_at_ax5() {
    for size in [(393, 852), (320, 568), (393, 480), (320, 340)] {
        for dark in [true, false] {
            let mut rt = scene(true, size, 2, true, dark);
            for (font, interface) in [(8.0, 80), (32.0, 200)] {
                rt.update(|d, _| {
                    d.font_size = font;
                    d.interface_size = interface;
                });
                for (category, stem, title, value) in [
                    (
                        Category::Terminal,
                        "text",
                        "Text size",
                        format!("{font:.0} pt"),
                    ),
                    (
                        Category::Appearance,
                        "interface",
                        "Interface size",
                        format!("{interface}%"),
                    ),
                ] {
                    rt.update(|d, tx| {
                        d.settings_category(category);
                        tx.instant();
                    });
                    rt.frame(1.0);
                    for suffix in ["smaller", "bigger"] {
                        let key = format!("{stem}-{suffix}");
                        reveal(&mut rt, &key);
                        let control = crate::ui::element(&rt, &key);
                        assert_inside_card(control.rect, size.0, &key);
                        assert!(control.rect[2] >= 44.0 && control.rect[3] >= 44.0);
                        for axis in [2, 3] {
                            assert!(
                                (control.visible[axis] - control.rect[axis]).abs() <= 0.5,
                                "{size:?} {key} must be entirely visible: {:?} vs {:?}",
                                control.visible,
                                control.rect,
                            );
                        }
                        let elements = rt.elements();
                        let label = elements
                            .iter()
                            .find(|el| el.text.as_deref() == Some(title))
                            .unwrap();
                        let readout = elements
                            .iter()
                            .find(|el| el.text.as_deref() == Some(value.as_str()))
                            .unwrap();
                        assert_inside_card(label.rect, size.0, title);
                        assert_inside_card(readout.rect, size.0, &value);
                        assert!(
                            label.rect[1] + label.rect[3] <= control.rect[1] + 0.5,
                            "{title} must be above, never overlap, the enlarged controls",
                        );
                    }
                }
            }
        }
    }
}

fn reveal(rt: &mut Runtime<Desk>, key: &str) {
    assert!(rt.accessibility_action(key, Action::Focus, None));
    rt.update(|_, tx| {
        tx.scroll_into_view(key);
        tx.instant();
    });
    rt.frame(1.0);
    rt.frame(1.0);
}

fn assert_inside_card(rect: [f32; 4], width: u32, name: &str) {
    assert!(
        rect[0] >= 16.0 && rect[0] + rect[2] <= width as f32 - 16.0 + 0.5,
        "{name} must fit the card: {rect:?}",
    );
}

#[test]
fn phone_size_controls_still_step_both_values_and_keep_adjustable_semantics() {
    for size in [(393, 480), (320, 340)] {
        let mut rt = scene(true, size, 2, true, true);
        let original = rt.state.font_size;
        reveal(&mut rt, "text-bigger");
        let [x, y, w, h] = rect(&rt, "text-bigger");
        rt.touch(1, rune::TouchPhase::Started, x + w / 2.0, y + h / 2.0, 0.0);
        rt.touch(1, rune::TouchPhase::Ended, x + w / 2.0, y + h / 2.0, 0.05);
        rt.frame(1.0);
        assert_eq!(rt.state.font_size, original + 1.0);
        assert!(rt.accessibility_action("text-size", Action::Decrement, None));
        rt.frame(1.0);
        assert_eq!(rt.state.font_size, original);
        rt.update(|d, tx| {
            d.settings_category(Category::Appearance);
            tx.instant();
        });
        rt.frame(1.0);
        reveal(&mut rt, "interface-bigger");
        assert!(rt.accessibility_action("interface-bigger", Action::Click, None));
        rt.frame(1.0);
        assert_eq!(rt.state.interface_size, 110);
        assert!(rt.accessibility_action("interface-size", Action::Decrement, None));
        rt.frame(1.0);
        assert_eq!(rt.state.interface_size, 100);
        assert!(!rt.state.settings_search_focused());
        for (value, key) in [(80, "interface-smaller"), (200, "interface-bigger")] {
            rt.update(|d, _| d.set_interface_size(Some(value)));
            rt.frame(1.0);
            reveal(&mut rt, key);
            assert!(rt.accessibility_action(key, Action::Click, None));
            rt.frame(1.0);
            assert_eq!(rt.state.interface_size, value, "the limit remains clamped");
        }
        rt.update(|d, tx| {
            d.settings_category(Category::Terminal);
            tx.instant();
        });
        rt.frame(1.0);
        for (value, key) in [(8.0, "text-smaller"), (32.0, "text-bigger")] {
            rt.update(|d, _| d.zoom(Some(value)));
            rt.frame(1.0);
            reveal(&mut rt, key);
            assert!(rt.accessibility_action(key, Action::Click, None));
            rt.frame(1.0);
            assert_eq!(rt.state.font_size, value, "the limit remains clamped");
        }
    }
}

#[test]
fn tab_reveals_phone_size_buttons_without_a_manual_scroll_request() {
    for size in [(393, 480), (320, 340)] {
        let mut rt = scene(true, size, 2, true, true);
        rt.update(|d, tx| {
            d.settings_category(Category::Appearance);
            tx.instant();
        });
        rt.frame(1.0);
        assert!(rt.accessibility_action("settings-query", Action::Focus, None));
        rt.frame(1.0);
        for key in ["interface-bigger", "interface-smaller"] {
            rt.key(KeyEvent {
                key: Key::Tab,
                mods: Mods {
                    shift: true,
                    ..Mods::default()
                },
            });
            rt.frame(1.0);
            rt.frame(1.0);
            let button = crate::ui::element(&rt, key);
            assert!(button.focused, "{key} is reached by Shift-Tab");
            for axis in [2, 3] {
                assert!(
                    (button.visible[axis] - button.rect[axis]).abs() <= 0.5,
                    "{size:?} {key} is fully revealed by keyboard navigation",
                );
            }
        }
    }
}
