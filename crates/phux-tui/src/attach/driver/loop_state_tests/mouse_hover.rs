#[tokio::test(flavor = "current_thread")]
async fn application_hover_follows_visible_modes_not_only_focus() {
    let mut rig = LoopRig::plain();
    let a = ResourceId::local(1);
    let b = ResourceId::local(2);
    rig.state.viewport_dims = (80, 24);
    publish_panes(
        &mut rig.state,
        &[(&a, 40, 24, b"\x1b[?1003h"), (&b, 40, 24, b"\x1b[?1002h")],
    );
    rig.state.mirror.workspace = ArrangementDoc::single(a.clone());
    rig.state.mirror.workspace.active_group_mut().unwrap().tree = Some(
        crate::layout::split_at(
            &crate::layout::LayoutNode::Leaf(a.clone()),
            &a,
            &b,
            crate::layout::SplitDir::Horizontal,
            0.5,
        )
        .unwrap(),
    );
    rig.state.mirror.workspace.active_group_mut().unwrap().focus = Some(b.clone());
    rig.state.mirror.focused_resource = Some(b.clone());
    assert!(
        rig.state.application_wants_hover(None),
        "a visible unfocused app needs hover"
    );
    rig.state.mirror.zoomed = Some(b.clone());
    assert!(
        !rig.state.application_wants_hover(None),
        "zoomed-away app cannot capture"
    );
    rig.state.mirror.zoomed = None;
    rig.state.mouse_optout.insert(a.clone());
    assert!(
        !rig.state.application_wants_hover(None),
        "pane opt-out wins"
    );
    rig.state.mouse_optout.clear();
    rig.state.mirror.workspace = ArrangementDoc::single(b.clone());
    assert!(
        !rig.state.application_wants_hover(None),
        "hidden any-motion app cannot capture"
    );
    show_alone(
        &mut rig.state,
        &b,
        (80, 24),
        b"\x1b[?1003h\x1b[?1003l\x1b[?1002h",
    );
    assert!(
        !rig.state.application_wants_hover(None),
        "resetting any-motion releases capture"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn application_hover_includes_floating_panes_but_not_exited_ones() {
    let mut rig = LoopRig::plain();
    let a = ResourceId::local(1);
    let floating = ResourceId::local(2);
    rig.state.viewport_dims = (80, 24);
    publish_panes(
        &mut rig.state,
        &[(&a, 80, 24, b""), (&floating, 60, 18, b"\x1b[?1003h")],
    );
    rig.state.mirror.workspace = ArrangementDoc::single(a);
    rig.state.mirror.panes.get_mut(&floating).unwrap().floating = Some("browser".into());
    assert!(rig.state.application_wants_hover(None));
    rig.state.mirror.panes.get_mut(&floating).unwrap().exited =
        Some(crate::attach::pane_state::ExitMark {
            status: Some(0),
            signal: None,
        });
    assert!(!rig.state.application_wants_hover(None));
}
