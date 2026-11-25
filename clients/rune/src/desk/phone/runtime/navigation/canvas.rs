//! The boxless inventory still has hierarchy, selection and keyboard affordances.
use super::*;
use crate::ui::element;

#[test]
fn session_anchors_and_nested_panes_share_ground_without_resting_slabs() {
    for width in [320.0, 393.0] {
        for contrast in [false, true] {
            let (_, mut rt) = fixture();
            detailed_inventory(&mut rt);
            rt.set_viewport((width, 852.0), 2.0);
            rt.set_system_increased_contrast(contrast);
            rt.update(|d, _| d.open_phone_sessions());
            settle(&mut rt);
            let title = element(&rt, "phone-session-title-1");
            let pane = element(&rt, "phone-session-pane-title-@1");
            assert!(
                pane.rect[0] > title.rect[0] + phux_design::tokens::SPACE.s4,
                "pane names must read as children: {title:?}, {pane:?}"
            );
            for key in [
                "phone-session-section-1",
                "phone-session-section-2",
                "phone-session-section-3",
                "phone-session-1",
                "phone-session-pane-@1",
                "phone-session-pane-@2",
                "phone-session-connections",
            ] {
                assert!(element(&rt, key).fill.a < 0.001, "resting fill: {key}");
            }
            let current = element(&rt, "phone-session-current-@1");
            let row = element(&rt, "phone-session-pane-@1");
            assert!(current.inert && row.item_selected);
            assert_eq!(current.rect[2], phux_design::tokens::SPACE.s0_5);
            assert!(current.rect[0] + current.rect[2] < pane.rect[0]);
            assert!(current.rect[1] >= row.rect[1]);
            assert!(current.rect[1] + current.rect[3] <= row.rect[1] + row.rect[3]);
            assert!(rt.find("phone-session-current-@2").is_none());
            let last = element(&rt, "phone-session-pane-@3");
            let next = element(&rt, "phone-session-2");
            assert!(
                next.rect[1] - (last.rect[1] + last.rect[3]) >= phux_design::tokens::SPACE.s6 - 0.5
            );
        }
    }
}

#[test]
fn boxless_rows_reveal_complete_keyboard_rings_and_keep_pane_selection() {
    for width in [320.0, 393.0] {
        let (client, mut rt) = fixture();
        detailed_inventory(&mut rt);
        rt.set_viewport((width, 568.0), 2.0);
        rt.update(|d, _| d.open_phone_sessions());
        settle(&mut rt);
        for key in [
            "phone-session-1",
            "phone-session-pane-@2",
            "phone-session-3",
        ] {
            focus_by_tab(&mut rt, key);
            assert_ring_inside(&rt, key);
        }
        assert!(element(&rt, "phone-session-pane-@1").item_selected);
        no_terminal_input(&client);
        focus_by_tab(&mut rt, "phone-session-pane-@2");
        rt.key(crate::keys::plain(Key::Enter));
        settle(&mut rt);
        assert!(rt.state.phone_page().is_none());
        // Inventory advertises @2 but has not attached its terminal replica.
        // The same request as a touch must wait for that real attachment.
        assert_eq!(
            rt.state.connection.wanted.as_ref().unwrap().0,
            ResourceId::local(2)
        );
        no_terminal_input(&client);
    }
}

fn focus_by_tab(rt: &mut Runtime<Desk>, key: &str) {
    // Direct focus does not request scroll reveal; real keyboard traversal does.
    rt.update(|_, tx| tx.focus(key, false));
    settle(rt);
    rt.key(rune::KeyEvent {
        key: Key::Tab,
        mods: rune::Mods {
            shift: true,
            ..rune::Mods::default()
        },
    });
    settle(rt);
    rt.key(crate::keys::plain(Key::Tab));
    rt.settle_all();
    settle(rt);
}

#[test]
fn large_connections_give_text_the_full_measure_and_remain_one_destination() {
    for size in [(320.0, 478.0), (393.0, 759.0)] {
        let (client, mut rt) = fixture();
        detailed_inventory(&mut rt);
        rt.set_viewport(size, 2.0);
        rt.update(|d, tx| {
            d.native = Some(crate::mobile::Platform::test());
            d.machines.current = Some("開発コンピュータ".into());
            d.set_accessibility(false, "UICTContentSizeCategoryAccessibilityXXXL");
            d.open_phone_sessions();
            tx.focus("phone-session-connections", true);
            tx.scroll_into_view("phone-session-connections");
            tx.instant();
        });
        rt.settle_all();
        settle(&mut rt);
        let destination = element(&rt, "phone-session-connections");
        let title = element(&rt, "phone-connections-title");
        let detail = element(&rt, "phone-connections-detail");
        let width = destination.rect[2] - 2.0 * phux_design::tokens::SPACE.s3;
        assert!((title.rect[2] - width).abs() < 0.5, "{title:?}");
        assert!((detail.rect[2] - width).abs() < 0.5, "{detail:?}");
        assert!(destination.visible[3] >= destination.rect[3] - 0.5);
        assert!(destination.focused && destination.focus_visible);
        for key in ["phone-session-new", "phone-session-connections"] {
            rt.update(|_, tx| {
                tx.focus(key, true);
                tx.scroll_into_view(key);
                tx.instant();
            });
            rt.settle_all();
            settle(&mut rt);
            assert_ring_inside(&rt, key);
        }
        rt.key(crate::keys::plain(Key::Enter));
        settle(&mut rt);
        assert!(rt.state.native.as_ref().unwrap().connection_requested);
        assert!(rt.state.phone_page().is_none());
        no_terminal_input(&client);
    }
}

fn assert_ring_inside(rt: &Runtime<Desk>, key: &str) {
    let action = element(rt, key);
    let list = element(rt, "phone-session-list");
    let ring = phux_design::tokens::SPACE.focus_ring;
    let clearance = ring.width + ring.offset;
    assert!(action.focused && action.focus_visible, "{action:?}");
    for axis in [0, 1] {
        assert!(
            action.rect[axis] - clearance >= list.rect[axis] - 0.5
                && action.rect[axis] + action.rect[axis + 2] + clearance
                    <= list.rect[axis] + list.rect[axis + 2] + 0.5,
            "focus ring clipped: {action:?} vs {list:?}"
        );
    }
}
