use super::*;

#[test]
fn phone_colour_choices_have_complete_reachable_touch_targets() {
    for size in [(320, 568), (393, 852)] {
        for keyboard in [false, true] {
            let mut rt = fixture(size, true, true, 2.0);
            rt.update(|d, _| {
                d.phone.as_mut().unwrap().keyboard = keyboard;
                d.set_accessibility(false, "UICTContentSizeCategoryAccessibilityXXXL");
            });
            settle(&mut rt);
            let keys = phux_config_core::host_color::PALETTE
                .iter()
                .map(|(name, _)| format!("machine-swatch-{LOCAL}-{name}"))
                .chain(std::iter::once(format!("machine-swatch-{LOCAL}-auto")));
            for key in keys {
                keyboard_focus(&mut rt, &key);
                let e = crate::ui::element(&rt, &key);
                let minimum = phux_design::tokens::SPACE.touch_min;
                assert!(
                    e.visible[2] >= minimum && e.visible[3] >= minimum,
                    "{key}: visible {:?}",
                    e.visible
                );
                assert!(
                    e.target[2] >= minimum && e.target[3] >= minimum,
                    "{key}: target {:?}",
                    e.target
                );
                assert_eq!(e.visible, e.rect, "whole {key} is visible");
                assert!(!e.inert, "{key} is actionable");
                let elements = rt.elements();
                let parent = elements
                    .iter()
                    .position(|e| e.key.as_deref() == Some(key.as_str()))
                    .unwrap();
                for child in elements.iter().filter(|e| e.parent == Some(parent)) {
                    contained(child.rect, e.rect);
                }
            }
        }
    }
}

fn contained(inner: [f32; 4], outer: [f32; 4]) {
    assert!(inner[0] >= outer[0] && inner[1] >= outer[1]);
    assert!(inner[0] + inner[2] <= outer[0] + outer[2]);
    assert!(inner[1] + inner[3] <= outer[1] + outer[3]);
}

#[test]
fn keyboard_reveal_keeps_auto_clear_of_the_sheet_fade() {
    for size in [(320, 568), (393, 852)] {
        let mut rt = fixture(size, true, true, 2.0);
        rt.update(|d, _| {
            d.phone.as_mut().unwrap().keyboard = true;
            d.set_accessibility(false, "UICTContentSizeCategoryAccessibilityXXXL");
        });
        settle(&mut rt);
        keyboard_focus(&mut rt, &format!("machine-swatch-{LOCAL}-auto"));
        let auto = crate::ui::element(&rt, &format!("machine-swatch-{LOCAL}-auto"));
        let panel = crate::ui::element(&rt, "machines");
        // Sheet::build's third child is its scrolling body, after header/rule.
        let path = format!("{}/#2", panel.path);
        let viewport = rt.elements().into_iter().find(|e| e.path == path).unwrap();
        let bottom = viewport.rect[1] + viewport.rect[3];
        let gap = bottom - auto.rect[1] - auto.rect[3];
        assert!(gap >= phux_design::tokens::SPACE.s6, "fade gap {gap}");
        assert_eq!(auto.rect, auto.visible);
        assert!(auto.focused && auto.focus_visible);
    }
}

fn input_fixture() -> (std::sync::Arc<phux_client_runtime::Client>, Runtime<Desk>) {
    let (client, mut rt) = crate::test_support::input_fixture();
    rt.set_viewport((320.0, 568.0), 2.0);
    rt.update(|d, _| {
        d.phone = Some(crate::desk::phone::Phone::default());
        d.phone.as_mut().unwrap().keyboard = false;
        d.machines.open = true;
        d.machines.picking = Some(LOCAL.to_owned());
        d.machines.field.replace_text("draft@computer");
    });
    settle(&mut rt);
    (client, rt)
}

fn no_terminal_input(client: &phux_client_runtime::Client) {
    use phux_protocol::wire::frame::FrameKind;
    for bytes in client.take_outbound() {
        assert!(
            !matches!(
                FrameKind::decode(&bytes).unwrap().0,
                FrameKind::InputKey { .. } | FrameKind::InputPaste { .. }
            ),
            "a colour choice must not type into the terminal"
        );
    }
}

#[test]
fn extended_colour_targets_support_touch_keyboard_and_accessibility() {
    let (client, mut rt) = input_fixture();
    let layout = rt.state.connection.layout.clone();
    let shown = rt.state.connection.shown.as_ref().unwrap().resource.clone();
    let key = format!("machine-swatch-{LOCAL}-blue");
    let [x, y, w, h] = crate::ui::element(&rt, &key).rect;
    // The enlarged edge, not just the original 24-point target's centre.
    rt.touch(7, rune::TouchPhase::Started, x + w - 2.0, y + h / 2.0, 2.0);
    rt.frame(0.01);
    rt.touch(7, rune::TouchPhase::Ended, x + w - 2.0, y + h / 2.0, 2.05);
    settle(&mut rt);
    assert!(rt.state.machines.picking.is_none());

    rt.update(|d, tx| {
        d.machines.picking = Some(LOCAL.to_owned());
        tx.focus(format!("machine-swatch-{LOCAL}-auto"), true);
    });
    settle(&mut rt);
    rt.key(crate::keys::plain(rune::Key::Text(" ".into())));
    settle(&mut rt);
    assert!(rt.state.machines.picking.is_none());

    rt.update(|d, _| d.machines.picking = Some(LOCAL.to_owned()));
    settle(&mut rt);
    assert!(rt.accessibility_action(&key, rune::accesskit::Action::Click, None));
    settle(&mut rt);
    assert!(rt.state.machines.picking.is_none());
    assert_eq!(rt.state.machines.field.text, "draft@computer");
    assert!(!rt.state.phone.as_ref().unwrap().keyboard);
    assert_eq!(rt.state.connection.layout, layout);
    assert_eq!(rt.state.connection.shown.as_ref().unwrap().resource, shown);
    no_terminal_input(&client);
}

#[test]
fn cancelled_colour_press_does_not_choose_or_type() {
    let (client, mut rt) = input_fixture();
    let key = format!("machine-swatch-{LOCAL}-blue");
    let [x, y, w, h] = crate::ui::element(&rt, &key).rect;
    let (x, y) = (x + w / 2.0, y + h / 2.0);
    rt.touch(7, rune::TouchPhase::Started, x, y, 2.0);
    rt.frame(0.01);
    rt.touch(7, rune::TouchPhase::Cancelled, x, y, 2.05);
    settle(&mut rt);
    assert_eq!(rt.state.machines.picking.as_deref(), Some(LOCAL));
    assert_eq!(rt.state.machines.field.text, "draft@computer");
    no_terminal_input(&client);
}
