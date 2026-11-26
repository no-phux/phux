//! The geometry and semantics behind the visual hierarchy, not pixel goldens.
use super::*;

use crate::ui::element;

#[test]
fn long_names_and_attention_fit_without_displacing_the_pane_column() {
    for width in [320.0, 393.0, 768.0] {
        let (_, mut rt) = fixture();
        detailed_inventory(&mut rt);
        rt.set_viewport((width, 852.0), 2.0);
        rt.update(|d, _| d.open_phone_sessions());
        settle(&mut rt);
        for key in [
            "phone-session-1",
            "phone-session-2",
            "phone-session-pane-@1",
            "phone-session-pane-@2",
            "phone-session-pane-@3",
            "phone-session-pane-@4",
            "phone-session-new",
        ] {
            let e = element(&rt, key);
            assert!(e.rect[2] > 0.0 && e.rect[3] >= 44.0, "{key}: {e:?}");
            assert!(
                e.rect[0] >= 0.0 && e.rect[0] + e.rect[2] <= width + 0.5,
                "{key}: {e:?}"
            );
        }
        let first = element(&rt, "phone-session-pane-@1");
        let next = element(&rt, "phone-session-pane-@2");
        assert_eq!(first.rect[0], next.rect[0]);
        assert_eq!(first.rect[2], next.rect[2]);
        assert!(first.item_selected);
        assert!(!next.item_selected);
        assert!(first.label.unwrap().contains("current pane"));
        let header = element(&rt, "phone-session-1").label.unwrap();
        assert!(
            header.contains("3 panes") && header.contains("1 needs you"),
            "{header}"
        );
        let texts: Vec<_> = rt.elements().into_iter().filter_map(|e| e.text).collect();
        assert!(texts.iter().any(|s| s == "Claude · needs you"));
        assert!(texts.iter().any(|s| s == "Codex · working"));
        assert!(!texts.iter().any(|s| s.contains("phux · current")));
    }
}

#[test]
fn search_chrome_stays_fixed_and_the_last_row_clears_the_glass_dock() {
    let (_, mut rt) = fixture();
    let session = SessionId::new(1);
    set_inventory(
        &mut rt,
        SessionSnapshot::new(session, WindowId::new(1), ResourceId::local(1)).with_sessions(
            (1..=20)
                .map(|n| SessionInfo::new(SessionId::new(n), format!("session {n}")))
                .collect(),
        ),
    );
    rt.set_viewport((320.0, 568.0), 2.0);
    // Nineteen empty sessions are quiet; shown, they make the long list.
    rt.update(|d, _| d.phone.as_mut().unwrap().quiet_shown = true);
    rt.update(|d, _| d.open_phone_sessions());
    settle(&mut rt);
    let query = element(&rt, "phone-session-query").rect;
    let connections = element(&rt, "phone-session-connections").rect;
    let dock = element(&rt, "phone-session-new").rect;
    rt.update(|_, tx| {
        tx.scroll_to("phone-session-list", 0.0, 100_000.0);
        tx.instant();
    });
    settle(&mut rt);
    assert_eq!(element(&rt, "phone-session-query").rect, query);
    assert_eq!(element(&rt, "phone-session-connections").rect, connections);
    assert_eq!(element(&rt, "phone-session-new").rect, dock);
    let last = element(&rt, "phone-session-20");
    assert!(last.visible[3] >= 44.0, "{last:?}");
    assert!(
        last.rect[1] + last.rect[3] <= dock[1],
        "{last:?} vs {dock:?}"
    );
    tap(&mut rt, "phone-session-query", 0.1);
    rt.update(|d, _| {
        d.phone
            .as_mut()
            .unwrap()
            .query
            .replace_text("long name ".repeat(20))
    });
    settle(&mut rt);
    let create = element(&rt, "phone-session-new");
    assert!(create.visible[3] >= 44.0);
    assert!(create.rect[0] + create.rect[2] <= 320.5);
    let label = element(&rt, "phone-session-new-label").rect;
    assert!(label[0] >= create.rect[0] && label[0] + label[2] <= create.rect[0] + create.rect[2]);
    assert!(create.label.unwrap().contains(&"long name ".repeat(19)));
}

#[test]
fn a_matching_pane_fits_whole_above_the_dock_with_the_search_keyboard_open() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((320.0, 568.0), 2.0);
    rt.update(|d, _| d.open_phone_sessions());
    settle(&mut rt);
    tap(&mut rt, "phone-session-query", 0.1);
    for c in "review".chars() {
        rt.update(|d, tx| d.soft(Soft::Char(c), tx));
        settle(&mut rt);
    }
    rt.update(|_, tx| tx.scroll_to_instant("phone-session-list", 0.0, 100_000.0));
    rt.settle_all();
    settle(&mut rt);
    assert!(rt.state.phone_keyboard_visible());
    assert!(rt.find("phone-session-connections").is_none());
    let list = element(&rt, "phone-session-list").rect;
    let dock = element(&rt, "phone-session-new").rect;
    let pane = element(&rt, "phone-session-pane-@2");
    assert!(pane.visible[3] >= 60.0, "{pane:?}");
    assert!(
        pane.rect[1] >= list[1] && pane.rect[1] + pane.rect[3] <= dock[1],
        "{pane:?} vs {dock:?}"
    );
}

#[test]
fn editing_a_filter_resets_the_old_scroll_offset_to_the_first_result() {
    let (_, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_viewport((320.0, 568.0), 2.0);
    rt.update(|d, _| d.open_phone_sessions());
    settle(&mut rt);
    tap(&mut rt, "phone-session-query", 0.1);
    rt.update(|_, tx| tx.scroll_to_instant("phone-session-list", 0.0, 150.0));
    settle(&mut rt);
    rt.update(|d, tx| d.soft(Soft::Char('p'), tx));
    rt.settle_all();
    settle(&mut rt);
    let list = element(&rt, "phone-session-list").rect;
    let first = element(&rt, "phone-session-1").rect;
    // The list's top inset (clear of the edge fade), and nothing of the
    // old offset.
    let space = phux_design::tokens::SPACE;
    let inset = space.s2 + space.s1;
    assert!(
        first[1] >= list[1] && first[1] - list[1] < inset + 1.0,
        "{first:?} vs {list:?}"
    );
}

#[test]
fn ungrouped_satellite_panes_keep_their_owning_host_visible() {
    let (_, mut rt) = fixture();
    set_inventory(
        &mut rt,
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
            .with_resources(vec![
                ResourceInfo::new(
                    ResourceId::satellite("build-box", 7),
                    WindowId::new(0),
                    80,
                    24,
                )
                .with_title(Some("editor".into())),
            ]),
    );
    rt.update(|d, _| d.open_phone_sessions());
    settle(&mut rt);
    assert!(rt.find("phone-session-pane-build-box/@7").is_some());
    assert!(
        rt.elements()
            .into_iter()
            .any(|e| e.text.as_deref() == Some("build-box"))
    );
}

#[test]
fn connections_names_the_destination_and_keeps_long_context_inside_its_button() {
    for width in [320.0, 393.0] {
        for machine in [
            "Build studio",
            "build-studio-integration-runner",
            "開発コンピュータ",
        ] {
            let (_, mut rt) = fixture();
            rt.set_viewport((width, 568.0), 2.0);
            rt.update(|d, _| {
                d.machines.current = Some(machine.into());
                d.connection.status = phux_client_runtime::control::Status::Closed;
                d.open_phone_sessions();
            });
            settle(&mut rt);
            let button = element(&rt, "phone-session-connections");
            assert_eq!(
                button.label.as_deref(),
                Some(format!("Connections · {machine} · not connected").as_str())
            );
            assert!(button.rect[2] >= 44.0 && button.visible[3] >= 44.0);
            for key in ["phone-connections-title", "phone-connections-detail"] {
                let child = element(&rt, key);
                assert!(child.rect[0] >= button.rect[0], "{key}: {child:?}");
                assert!(child.rect[0] + child.rect[2] <= button.rect[0] + button.rect[2]);
                assert!(child.rect[1] >= button.rect[1]);
                assert!(child.rect[1] + child.rect[3] <= button.rect[1] + button.rect[3]);
            }
            assert_eq!(
                element(&rt, "phone-connections-title").text.as_deref(),
                Some("Connections")
            );
            assert_eq!(
                element(&rt, "phone-connections-detail").text.as_deref(),
                Some(format!("{machine} · not connected").as_str())
            );
        }
    }
}

#[test]
fn large_connections_and_search_scroll_without_covering_the_last_action() {
    for (width, height) in [(320.0, 478.0), (393.0, 759.0)] {
        let (_, mut rt) = fixture();
        detailed_inventory(&mut rt);
        rt.set_viewport((width, height), 2.0);
        rt.update(|d, _| {
            d.native = Some(crate::mobile::Platform::test());
            d.machines.current = Some("Build studio".into());
            d.set_accessibility(false, "UICTContentSizeCategoryAccessibilityXXXL");
            d.open_phone_sessions();
        });
        settle(&mut rt);
        let connections = element(&rt, "phone-session-connections").rect;
        let list = element(&rt, "phone-session-list").rect;
        let search = element(&rt, "phone-session-query").rect;
        assert!(connections[1] >= list[1]);
        assert!(list[3] >= connections[3], "{list:?} vs {connections:?}");
        rt.update(|_, tx| tx.scroll_to_instant("phone-session-list", 0.0, 100_000.0));
        rt.settle_all();
        settle(&mut rt);
        assert!(element(&rt, "phone-session-connections").rect[1] < connections[1] - 44.0);
        assert!(element(&rt, "phone-session-query").rect[1] < search[1] - 44.0);
        let last = element(&rt, "phone-session-new");
        assert!(last.radius <= phux_design::tokens::RADIUS.lg + 0.5);
        assert!(last.rect[1] >= list[1], "{last:?} vs {list:?}");
        assert!(last.rect[1] + last.rect[3] <= list[1] + list[3]);
        assert!(last.visible[3] >= last.rect[3] - 0.5, "{last:?}");
        let label = element(&rt, "phone-session-new-label");
        assert!(label.rect[0] >= last.rect[0]);
        assert!(label.rect[0] + label.rect[2] <= last.rect[0] + last.rect[2]);
        assert!(label.rect[1] >= last.rect[1]);
        assert!(label.rect[1] + label.rect[3] <= last.rect[1] + last.rect[3]);
    }
}

#[test]
fn keyboard_reduced_large_search_keeps_the_whole_create_action_reachable() {
    for size in [(320.0, 340.0), (393.0, 480.0)] {
        let (client, mut rt) = fixture();
        detailed_inventory(&mut rt);
        rt.set_viewport(size, 2.0);
        rt.update(|d, _| {
            d.native = Some(crate::mobile::Platform::test());
            d.set_accessibility(false, "UICTContentSizeCategoryAccessibilityXXXL");
            d.open_phone_sessions();
        });
        settle(&mut rt);
        tap(&mut rt, "phone-session-query", 0.1);
        rt.key(crate::keys::plain(rune::Key::Text("review".into())));
        settle(&mut rt);
        let editor = element(&rt, "phone-session-query");
        assert!(editor.visible[3] >= editor.rect[3] - 0.5, "{editor:?}");
        rt.update(|_, tx| tx.scroll_to_instant("phone-session-list", 0.0, 100_000.0));
        rt.settle_all();
        settle(&mut rt);
        let list = element(&rt, "phone-session-list");
        let action = element(&rt, "phone-session-new");
        assert!(
            action.visible[3] >= action.rect[3] - 0.5,
            "complete create action must fit above the keyboard: {action:?} vs {list:?}"
        );
        let label = element(&rt, "phone-session-new-label");
        assert!(label.visible[3] >= label.rect[3] - 0.5, "{label:?}");
        assert!(label.rect[0] >= action.rect[0]);
        assert!(label.rect[0] + label.rect[2] <= action.rect[0] + action.rect[2]);
        assert!(label.rect[1] >= action.rect[1]);
        assert!(label.rect[1] + label.rect[3] <= action.rect[1] + action.rect[3]);
        no_terminal_input(&client);
    }
}
