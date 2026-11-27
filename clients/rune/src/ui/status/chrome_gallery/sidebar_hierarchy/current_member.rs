//! A split's current resource stays identifiable without colour or another Tab stop.
use super::*;

fn assert_current(rt: &Runtime<Desk>, current: Option<u32>) {
    let indicators: Vec<_> = rt
        .elements()
        .into_iter()
        .filter(|e| e.key.as_deref() == Some("sidebar-current-member"))
        .collect();
    let Some(id) = current else {
        assert!(indicators.is_empty(), "no member is currently shown");
        return;
    };
    assert_eq!(indicators.len(), 1, "one actual current member");
    let member = crate::ui::element(rt, &format!("sidebar-member-@{id}"));
    let indicator = &indicators[0];
    assert!(indicator.path.starts_with(&format!("{}/", member.path)));
    assert!(!member.focusable, "the containing row owns keyboard focus");
    assert!(indicator.inert && !indicator.focusable);
    assert!(indicator.rect[2] > 0.0 && indicator.rect[3] > 0.0);
    assert!(indicator.rect[0] >= member.rect[0]);
    assert!(indicator.rect[1] >= member.rect[1]);
    assert!(indicator.rect[0] + indicator.rect[2] <= member.rect[0] + member.rect[2] + 0.01);
    assert!(indicator.rect[1] + indicator.rect[3] <= member.rect[1] + member.rect[3] + 0.01);
    let tree = rt.accessibility_tree();
    let (_, node) = tree
        .nodes
        .iter()
        .find(|(_, node)| node.author_id() == Some(format!("sidebar-member-@{id}").as_str()))
        .expect("current member remains accessible");
    assert_eq!(node.description(), Some("Current pane"));
    assert!(
        tree.nodes
            .iter()
            .all(|(_, node)| node.author_id() != Some("sidebar-current-member")),
        "the underline is decoration, not another accessible control"
    );
}

#[test]
fn a_split_current_member_is_identifiable_without_colour() {
    for size in [(1100, 720), (640, 360)] {
        for dark in [true, false] {
            for mode in ["normal", "large", "forced"] {
                let mut rt = window(size, dark, mode);
                state(&mut rt, "split");
                assert_current(&rt, Some(1));
                // An acknowledged selection changes only which member is current.
                rt.update(|d, _| {
                    d.connection.shown = Some(crate::desk::Shown {
                        resource: ResourceId::local(2),
                        view: None,
                    });
                });
                server::settle(&mut rt);
                assert_current(&rt, Some(2));
                rt.update(|d, _| d.connection.shown = None);
                server::settle(&mut rt);
                assert_current(&rt, None);
            }
        }
    }
}

#[test]
fn pending_or_cancelled_member_presses_do_not_move_the_current_cue() {
    // The last resource is listed but has not streamed its first source.
    // An already-streamed target can legitimately open and clear `wanted`.
    let mut rt = window_with_replica(
        (1100, 720),
        true,
        "normal",
        crate::test_support::terminal::Replica::pending(120, 40, 3),
    );
    state(&mut rt, "split");
    let layout = rt.state.connection.layout.clone();
    let client = rt.state.connection.client.as_ref().unwrap().clone();
    let _ = client.take_outbound();
    let (x, y) = rt.find("sidebar-member-@3").unwrap();
    rt.pointer_down(x, y);
    rt.frame(3.0);
    assert_current(&rt, Some(1));
    rt.pointer_move(x + 150.0, y + 80.0);
    rt.pointer_up(x + 150.0, y + 80.0);
    server::settle(&mut rt);
    assert_current(&rt, Some(1));
    press(&mut rt, "sidebar-member-@3");
    assert_eq!(
        rt.state.connection.wanted.as_ref().map(|(id, _)| id),
        Some(&ResourceId::local(3))
    );
    assert_current(&rt, Some(1));
    assert_eq!(rt.state.connection.layout, layout);
    super::tests::no_terminal_input(&rt);
}
