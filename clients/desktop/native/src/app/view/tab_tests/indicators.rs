//! Identity and layout stay legible without selecting the work they describe.

use super::*;
use crate::connection::{AgentAttention, AgentInfo, AgentState};
use phux_protocol::ResourceId;

#[gpui::test]
fn urgent_attention_stays_visible_on_selected_side_tabs_and_terminal_rows(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, selectable_work);
    for position in ["tabs-side", "tabs-top", "tabs-bottom"] {
        run(&desk, position, cx);
        for state in [AgentState::Unknown, AgentState::Working, AgentState::Idle] {
            desk.update(cx, |desk, cx| {
                let link = desk.link.as_mut().unwrap();
                link.workspace.active_id = "fixture-a".into();
                link.connection.agents.insert(
                    ResourceId::local(1),
                    AgentInfo {
                        name: "reviewer".into(),
                        kind: Some("opencode".into()),
                        state,
                        attention: AgentAttention::High,
                        changed_at_ms: 0,
                    },
                );
                cx.notify();
            });
            draw(cx);
            let tab = cx.debug_bounds("tab-fixture-a").unwrap();
            let alert = cx
                .debug_bounds("harness-attention")
                .expect("urgent pip independent of lifecycle");
            assert!(tab.contains(&alert.center()), "{position}, {state:?}");
        }
    }
    // With the session expanded, its terminal row keeps the same alert even
    // after its tab is no longer the selected one.
    run(&desk, "tabs-side", cx);
    desk.update(cx, |desk, cx| {
        let link = desk.link.as_mut().unwrap();
        link.connection.agents.remove(&ResourceId::local(1));
        link.connection.agents.insert(
            ResourceId::local(4),
            AgentInfo {
                name: "background".into(),
                kind: Some("pi".into()),
                state: AgentState::Unknown,
                attention: AgentAttention::High,
                changed_at_ms: 0,
            },
        );
        cx.notify();
    });
    run(&desk, "expand-sessions", cx);
    let pane = cx.debug_bounds("pane-@4").unwrap();
    let alert = cx.debug_bounds("harness-attention").unwrap();
    assert!(pane.contains(&alert.center()));
    assert!(cx.debug_bounds("harness-mark-Pi").is_some());
}

#[gpui::test]
fn harness_marks_survive_attention_and_inactive_tabs_in_every_placement(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, selectable_work);
    for position in ["tabs-side", "tabs-top", "tabs-bottom"] {
        run(&desk, position, cx);
        for state in [
            AgentState::Working,
            AgentState::Blocked,
            AgentState::Done,
            AgentState::Idle,
        ] {
            desk.update(cx, |desk, cx| {
                let link = desk.link.as_mut().unwrap();
                link.workspace.active_id = "fixture-c".into();
                link.connection.agents.insert(
                    ResourceId::local(1),
                    AgentInfo {
                        name: "reviewer".into(),
                        kind: Some("opencode".into()),
                        state,
                        attention: AgentAttention::None,
                        changed_at_ms: 0,
                    },
                );
                cx.notify();
            });
            draw(cx);
            let tab = cx.debug_bounds("tab-fixture-a").unwrap();
            let mark = cx
                .debug_bounds("harness-mark-OpenCode")
                .expect("identity, not just a state dot");
            assert!(tab.contains(&mark.center()), "{position}, {state:?}");
        }
    }
}

#[gpui::test]
fn routine_states_have_no_pip_and_attention_never_resizes_tabs(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, selectable_work);
    for position in ["tabs-top", "tabs-bottom", "tabs-side"] {
        run(&desk, position, cx);
        let mut bounds = None;
        for state in [
            AgentState::Working,
            AgentState::Idle,
            AgentState::Blocked,
            AgentState::Done,
        ] {
            desk.update(cx, |desk, cx| {
                let link = desk.link.as_mut().unwrap();
                link.workspace.active_id = "fixture-c".into();
                link.connection.agents.insert(
                    ResourceId::local(1),
                    AgentInfo {
                        name: "reviewer".into(),
                        kind: Some("opencode".into()),
                        state,
                        attention: AgentAttention::None,
                        changed_at_ms: 0,
                    },
                );
                cx.notify();
            });
            draw(cx);
            let tab = cx.debug_bounds("tab-fixture-a").unwrap();
            assert_eq!(*bounds.get_or_insert(tab), tab, "{position}, {state:?}");
            assert_eq!(
                cx.debug_bounds("harness-attention").is_some(),
                matches!(state, AgentState::Blocked | AgentState::Done),
                "{position}, {state:?}"
            );
            assert!(cx.debug_bounds("harness-mark-OpenCode").is_some());
        }
    }
}

#[gpui::test]
fn layered_mosaics_preserve_split_ratios_and_survive_selection_and_zoom(cx: &mut TestAppContext) {
    let _dial = DeterministicDial::arm();
    let (handle, desk, _) = fixture(cx);
    let cx = &mut VisualTestContext::from_window(handle, cx);
    desk.update(cx, selectable_work);
    for axis in [Axis::Row, Axis::Column] {
        desk.update(cx, |desk, cx| {
            let link = desk.link.as_mut().unwrap();
            link.workspace.tabs = tabs(3);
            let tab = &mut link.workspace.tabs[0];
            layout::split_at(
                &mut tab.root,
                "fixture-a",
                Placement {
                    id: "map-second".into(),
                    terminal_id: None,
                    terminal_label: String::new(),
                    view_id: None,
                },
                axis,
                false,
            );
            let LayoutNode::Split { ratio, .. } = &mut tab.root else {
                unreachable!()
            };
            *ratio = 0.3;
            cx.notify();
        });
        for position in ["tabs-side", "tabs-top", "tabs-bottom"] {
            run(&desk, position, cx);
            for (active, zoomed) in [
                ("fixture-a", false),
                ("fixture-c", false),
                ("fixture-c", true),
            ] {
                desk.update(cx, |desk, cx| {
                    let workspace = &mut desk.link.as_mut().unwrap().workspace;
                    workspace.active_id = active.into();
                    workspace.tabs[0].zoomed_id = zoomed.then(|| "fixture-a".into());
                    cx.notify();
                });
                draw(cx);
                let map = cx.debug_bounds("tab-map-fixture-a").unwrap();
                let first = cx.debug_bounds("layout-cell-fixture-a").unwrap();
                let second = cx.debug_bounds("layout-cell-map-second").unwrap();
                assert!(map.contains(&first.center()) && map.contains(&second.center()));
                assert!(first.size.width > px(0.) && first.size.height > px(0.));
                let (small, large) = match axis {
                    Axis::Row => {
                        assert!(first.right() < second.left());
                        (first.size.width, second.size.width)
                    }
                    Axis::Column => {
                        assert!(first.bottom() < second.top());
                        (first.size.height, second.size.height)
                    }
                };
                assert!(
                    (small / (small + large) - 0.3).abs() < 0.1,
                    "split ratio survives gaps and pixel snapping"
                );
                let label = cx.debug_bounds("tab-label-fixture-a").unwrap();
                assert!(label.right() <= map.left());
            }
        }
    }
}
