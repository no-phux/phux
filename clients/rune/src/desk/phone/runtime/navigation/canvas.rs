//! The boxless inventory still has hierarchy, selection and keyboard affordances.
use super::*;
use crate::ui::element;

#[test]
fn session_headers_sit_on_ground_over_grouped_cards_of_their_panes() {
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
            let search = element(&rt, "phone-session-search").rect;
            let footer = element(&rt, "phone-session-new").rect;
            let list = element(&rt, "phone-session-list").rect;
            let heading = element(&rt, "phone-navigation-heading").rect;
            assert!((heading[0] - search[0]).abs() < 0.5);
            for key in [
                "phone-session-connections",
                "phone-session-section-1",
                "phone-needs-you",
            ] {
                let rect = element(&rt, key).rect;
                assert!(
                    (rect[0] - search[0]).abs() < 0.5 && (rect[2] - search[2]).abs() < 0.5,
                    "{key}: {rect:?} vs {search:?}"
                );
            }
            assert!((footer[0] - search[0]).abs() < 0.5 && (footer[2] - search[2]).abs() < 0.5);
            assert!(list[1] + list[3] <= footer[1], "list and footer overlap");
            assert!(
                pane.rect[0] > title.rect[0] + phux_design::tokens::SPACE.s4,
                "pane names must read as children: {title:?}, {pane:?}"
            );
            for key in [
                "phone-session-section-1",
                "phone-session-section-2",
                "phone-quiet",
                "phone-session-1",
                "phone-session-pane-@1",
                "phone-session-pane-@2",
                "phone-session-connections",
            ] {
                // Headers and rows rest on the page or their card; only the card
                // (the grouped surface) is filled.
                assert!(element(&rt, key).fill.a < 0.001, "resting fill: {key}");
            }
            // The current pane is marked as a system list marks a choice:
            // one check at the trailing edge, on the title's line.
            let current = element(&rt, "phone-session-current-@1");
            let row = element(&rt, "phone-session-pane-@1");
            assert!(current.inert && row.item_selected);
            assert!(current.rect[0] > pane.rect[0] + pane.rect[2]);
            let centre = |r: [f32; 4]| r[1] + r[3] / 2.0;
            assert!((centre(current.rect) - centre(pane.rect)).abs() < 0.5);
            assert!(rt.find("phone-session-current-@2").is_none());
            let last = element(&rt, "phone-session-pane-@3");
            let next = element(&rt, "phone-session-2");
            assert!(
                next.rect[1] - (last.rect[1] + last.rect[3]) >= phux_design::tokens::SPACE.s5 - 0.5
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
        // Quiet shown, so its empty session's header is reached too.
        rt.update(|d, _| d.phone.as_mut().unwrap().quiet_shown = true);
        rt.update(|d, _| d.open_phone_sessions());
        settle(&mut rt);
        for key in [
            "phone-session-1",
            "phone-session-pane-@2",
            "phone-needs-you-pane-@4",
            "phone-quiet",
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
        let width = destination.rect[2] - 2.0 * phux_design::tokens::SPACE.s4;
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

/// One grid, measured: every leading mark on one centre line, every title
/// on one text column, section names on the content inset, the check on
/// the trailing column, the search text on its field's centre line, and
/// the page title on Done's, at both phone widths, the default text size
/// and XXL, desktop-hosted and native.
#[test]
fn the_sessions_page_keeps_one_grid_at_every_width_and_text_size() {
    for width in [320.0, 393.0] {
        for category in ["UICTContentSizeCategoryL", "UICTContentSizeCategoryXXL"] {
            for native in [false, true] {
                let (_, mut rt) = fixture();
                detailed_inventory(&mut rt);
                rt.set_viewport((width, 852.0), 2.0);
                rt.update(|d, _| {
                    if native {
                        d.native = Some(crate::mobile::Platform::test());
                    }
                    d.set_accessibility(false, category);
                    d.open_phone_sessions();
                });
                settle(&mut rt);
                assert_one_grid(&rt);
            }
        }
    }
}

fn near(a: f32, b: f32, what: &str) {
    assert!((a - b).abs() <= 0.5, "{what}: {a} vs {b}");
}

fn mid_x(r: [f32; 4]) -> f32 {
    r[0] + r[2] / 2.0
}

fn mid_y(r: [f32; 4]) -> f32 {
    r[1] + r[3] / 2.0
}

fn assert_one_grid(rt: &Runtime<Desk>) {
    let inset = phux_design::tokens::SPACE.s4;
    let at = |key: &str| element(rt, key).rect;
    let card = at("phone-session-connections");
    let margin = crate::ui::tokens::phone::page_margin();
    for key in [
        "phone-session-connections",
        "phone-session-search",
        "phone-navigation-heading",
    ] {
        near(at(key)[0], margin, key);
    }
    let marks = [
        "phone-session-search-mark",
        "phone-connections-mark",
        "phone-session-pane-mark-@1",
        "phone-session-pane-mark-@2",
        "phone-needs-you-pane-mark-@4",
    ];
    for key in marks {
        near(mid_x(at(key)), mid_x(at(marks[0])), key);
        near(at(key)[0], card[0] + inset, key);
    }
    near(
        at("phone-session-title-1")[0],
        card[0] + inset,
        "section name",
    );
    let text = at("phone-session-query")[0];
    for key in [
        "phone-connections-title",
        "phone-session-pane-title-@1",
        "phone-needs-you-pane-title-@4",
    ] {
        near(at(key)[0], text, key);
    }
    // The check ends within the trailing column, an inset from the edge.
    let check = at("phone-session-current-@1");
    let trail = card[0] + card[2] - inset;
    assert!(check[0] + check[2] <= trail + 0.5, "{check:?} vs {card:?}");
    assert!(check[0] >= trail - 2.0 * inset, "{check:?}");
    // The field's line, magnifier and box share a centre line.
    let field = at("phone-session-search");
    near(mid_y(at("phone-session-query")), mid_y(field), "query line");
    near(mid_y(at(marks[0])), mid_y(field), "magnifier");
    // A pane's mark and check sit on its title's line.
    let title = mid_y(at("phone-session-pane-title-@1"));
    near(
        mid_y(at("phone-session-pane-mark-@1")),
        title,
        "mark on title",
    );
    near(mid_y(check), title, "check on title");
    near(
        mid_y(at("phone-navigation-heading")),
        mid_y(at("phone-navigation-close")),
        "title and Done",
    );
}
