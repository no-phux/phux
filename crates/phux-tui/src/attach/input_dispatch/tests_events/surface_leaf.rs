// ---- non-Terminal layout leaves ---------------------------------------------

/// [`two_pane_workspace`] with pane 1 a published Terminal and pane 2 a
/// document leaf, default bindings, focus on pane 1.
fn surface_leaf_env() -> Env<'static> {
    let mut fx = CtxFixture::default();
    fx.workspace = two_pane_workspace();
    let mut env = Env::new(fx)
        .with_default_bindings()
        .published(&[(&tid(1), 39, 24, b"")]);
    learn_kind(&mut env.fx, tid(2), phux_protocol::ResourceKind::Document);
    env
}

/// The server's graph names `id` a `kind` member of session 1; no
/// bootstrap of it reaches this client.
fn learn_kind(fx: &mut CtxFixture, id: ResourceId, kind: phux_protocol::ResourceKind) {
    fx.leaves.learn(&[
        phux_protocol::wire::info::ResourceInfo::resource(id, kind)
            .with_session(Some(phux_protocol::ids::SessionId::new(1))),
    ]);
}

/// The keys, paste, and focus report a user types at a focused pane.
fn typing() -> Vec<InputEvent> {
    use phux_protocol::input::paste::{PasteEvent, PasteTrust};
    vec![
        press(PhysicalKey::A, Some("a")),
        press(PhysicalKey::Enter, None),
        InputEvent::Paste(PasteEvent {
            trust: PasteTrust::Untrusted,
            data: b"line one\nline two\n".to_vec(),
        }),
        InputEvent::Focus(FocusEvent::Lost),
    ]
}

fn sends_input(frames: &[FrameKind]) -> bool {
    frames.iter().any(|frame| {
        matches!(
            frame,
            FrameKind::InputKey { .. }
                | FrameKind::InputMouse { .. }
                | FrameKind::InputPaste { .. }
                | FrameKind::InputFocus { .. }
        )
    })
}

/// Focus can land on a document leaf by keyboard and leave it again; typing
/// there sends nothing, and the Terminal still takes input afterwards.
#[tokio::test]
async fn keyboard_focus_crosses_a_document_leaf_and_typing_there_is_a_no_op() {
    let mut env = surface_leaf_env();
    env.dispatch(vec![ctrl_a(), press(PhysicalKey::L, Some("l"))])
        .await;
    assert_eq!(env.focused, Some(tid(2)));
    let sent = env.dispatch(typing()).await;
    assert!(!sends_input(&sent.frames), "{:?}", sent.frames);
    assert!(!env.fx.overlays.is_active(), "no paste confirmation for it");

    // Zoom and cycling over the placeholder neither panic nor send input.
    let sent = env
        .dispatch(vec![
            ctrl_a(),
            press(PhysicalKey::Z, Some("z")),
            ctrl_a(),
            press(PhysicalKey::Z, Some("z")),
            ctrl_a(),
            press(PhysicalKey::O, Some("o")),
        ])
        .await;
    assert!(!sends_input(&sent.frames), "{:?}", sent.frames);
    assert_eq!(env.focused, Some(tid(1)));
    let sent = env.dispatch(vec![press(PhysicalKey::A, Some("a"))]).await;
    assert!(
        matches!(sent.frames.as_slice(), [FrameKind::InputKey { terminal_id, .. }] if *terminal_id == tid(1)),
        "{:?}",
        sent.frames
    );
}

/// A click on the placeholder focuses it and forwards no mouse report.
#[tokio::test]
async fn a_click_on_a_document_leaf_focuses_it_without_a_mouse_report() {
    let mut env = surface_leaf_env();
    let sent = env
        .dispatch(vec![
            mouse(MouseAction::Press, MouseButton::Left, 70.0, 5.0),
            mouse(MouseAction::Release, MouseButton::Left, 70.0, 5.0),
            mouse(MouseAction::Press, MouseButton::Four, 70.0, 5.0),
        ])
        .await;
    assert_eq!(env.focused, Some(tid(2)));
    assert!(!sends_input(&sent.frames), "{:?}", sent.frames);
}

/// A window of terminal 1 beside document 3, focused on `focus`.
fn terminal_beside_document(focus: u32) -> CtxFixture {
    let mut workspace = crate::layout::ArrangementDoc::single(tid(1));
    let tree = crate::layout::split_at(
        &crate::layout::LayoutNode::Leaf(tid(1)),
        &tid(1),
        &tid(3),
        crate::layout::SplitDir::Horizontal,
        0.5,
    )
    .expect("split");
    let window = workspace.active_group_mut().expect("window");
    window.tree = Some(tree);
    window.focus = Some(tid(focus));
    let mut f = fx(workspace);
    learn_kind(&mut f, tid(3), phux_protocol::ResourceKind::Document);
    f
}

fn active_leaves(f: &CtxFixture) -> Vec<ResourceId> {
    f.workspace
        .active_group()
        .and_then(|w| w.tree.as_ref())
        .map(crate::layout::leaves)
        .unwrap_or_default()
}

/// kill-pane on a placeholder closes its view: the document and its
/// producer belong to another client, so nothing is killed.
#[test]
fn kill_pane_on_a_placeholder_closes_its_view_only() {
    let mut f = terminal_beside_document(3);
    let effects = f.run(&bare_action("kill-pane"));
    assert!(effects.kill_frames.is_empty() && effects.kill_requests.is_empty());
    assert!(effects.expected_closes.is_empty() && !effects.bell);
    assert!(effects.layout_mutated && effects.set_metadata);
    assert_eq!(active_leaves(&f), vec![tid(1)]);
}

/// kill-window counts and kills only the tab's terminals; committing it
/// closes the placeholders' views instead of ending them.
#[test]
fn kill_window_kills_terminals_and_only_closes_placeholder_views() {
    let mut f = terminal_beside_document(1);
    f.run(&bare_action("kill-window"));
    let confirm = crate::attach::input_dispatch::run_action::kill_window_confirm(&f.ctx())
        .expect("a terminal to end");
    assert_eq!(confirm.lines(), ["End 1 terminal in this tab."]);
    let InputEvent::Key(enter) = press(PhysicalKey::Enter, None) else {
        unreachable!("press builds a key");
    };
    let crate::render::overlay::OverlayOutcome::RunAction(commit) = f.overlays.handle_key(&enter)
    else {
        panic!("Enter commits");
    };
    let effects = f.run(&commit);
    let killed: Vec<_> = effects.kill_requests.iter().map(|(_, id)| id.clone()).collect();
    assert_eq!(killed, vec![tid(1)]);
    assert_eq!(effects.kill_frames.len(), 1);
    assert_eq!(effects.expected_closes, vec![tid(1)]);
    assert_eq!(active_leaves(&f), vec![tid(1)], "the document's view closed");

    // A tab holding only placeholders ends nothing: no confirm, views close.
    let mut f = fx(crate::layout::ArrangementDoc::single(tid(3)));
    learn_kind(&mut f, tid(3), phux_protocol::ResourceKind::PixelSurface);
    let effects = f.run(&bare_action("kill-window"));
    assert!(!f.overlays.is_active() && effects.kill_frames.is_empty());
    assert!(effects.set_metadata && !effects.bell);
    assert!(f.workspace.groups.is_empty());
}

/// Copy mode, the input lease, and signals need a terminal: on a
/// placeholder they bell and send nothing.
#[test]
fn terminal_only_actions_on_a_placeholder_bell_locally() {
    for action in [
        bare_action("copy-mode"),
        bare_action("take-input"),
        bare_action("give-input"),
        act("signal-terminal", &[("signal", toml::Value::String("interrupt".to_owned()))]),
    ] {
        let mut f = terminal_beside_document(3);
        let effects = f.run(&action);
        assert!(effects.bell, "{} bells", action.action);
        assert!(effects.command_frames.is_empty(), "{}", action.action);
        assert!(!f.overlays.is_active(), "{}", action.action);
    }
}

/// reopen-views shows the session's hidden documents too, never its agent
/// sessions (they belong to their pane).
#[test]
fn reopen_views_restores_hidden_documents() {
    let mut f = fx(crate::layout::ArrangementDoc::single(tid(1)));
    f.session_members = HashSet::from([tid(1)]);
    f.focused_session = Some(phux_protocol::ids::SessionId::new(1));
    learn_kind(&mut f, tid(5), phux_protocol::ResourceKind::Document);
    learn_kind(&mut f, tid(6), phux_protocol::ResourceKind::AgentSession);
    let effects = f.run(&bare_action("reopen-views"));
    assert!(effects.set_metadata && !effects.bell);
    let shown: Vec<_> = f
        .workspace
        .groups
        .iter()
        .filter_map(|group| group.state.tree.as_ref())
        .flat_map(crate::layout::leaves)
        .collect();
    assert_eq!(shown, vec![tid(1), tid(5)]);
}
