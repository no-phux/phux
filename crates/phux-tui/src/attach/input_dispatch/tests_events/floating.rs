// ---- ADR-0147: the floating plugin overlay -----------------------------------

/// An env on pane 1 with a floating overlay `@9` open over it.
fn env_with_floating() -> Env<'static> {
    let mut env = Env::new(CtxFixture::default())
        .with_default_bindings()
        .published(&[(&tid(1), 80, 24, b""), (&tid(9), 60, 18, b"")]);
    env.panes.get_mut(&tid(9)).expect("overlay slot").floating = Some("Board".to_owned());
    env
}

fn ctrl_a() -> InputEvent {
    InputEvent::Key(KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::A,
        mods: ModSet::CTRL,
        consumed_mods: ModSet::CTRL,
        composing: false,
        text: None,
        unshifted_codepoint: Some(u32::from(b'a')),
    })
}

fn kills(frames: &[FrameKind]) -> Vec<ResourceId> {
    frames
        .iter()
        .filter_map(|frame| match frame {
            FrameKind::Command {
                command: Command::KillResource { terminal_id, .. },
                ..
            } => Some(terminal_id.clone()),
            _ => None,
        })
        .collect()
}

/// Keys and pastes reach the overlay, never the layout pane beneath it.
#[tokio::test]
async fn the_floating_overlay_takes_keyboard_input() {
    let mut env = env_with_floating();
    let sent = env.dispatch(vec![press(PhysicalKey::Q, Some("q"))]).await;
    assert!(
        matches!(sent.frames.as_slice(), [FrameKind::InputKey { terminal_id, .. }] if *terminal_id == tid(9)),
        "{:?}",
        sent.frames
    );
}

/// `kill-pane` dismisses the overlay and nothing else: its Terminal is
/// killed (silently), its slot goes at once, and the layout pane survives.
#[tokio::test]
async fn kill_pane_dismisses_only_the_floating_overlay() {
    let mut env = env_with_floating();
    let sent = env
        .dispatch(vec![ctrl_a(), press(PhysicalKey::X, Some("x"))])
        .await;
    assert_eq!(kills(&sent.frames), vec![tid(9)]);
    assert!(sent.repainted);
    assert!(!env.panes.contains_key(&tid(9)));
    assert!(env.fx.expected_closes.contains(&tid(9)));
    assert_eq!(
        crate::attach::arrangement_shape(&env.fx.workspace),
        crate::attach::arrangement_shape(&crate::layout::ArrangementDoc::single(tid(1)))
    );
    // Input is back on the layout pane.
    let sent = env.dispatch(vec![press(PhysicalKey::Q, Some("q"))]).await;
    assert!(
        matches!(sent.frames.as_slice(), [FrameKind::InputKey { terminal_id, .. }] if *terminal_id == tid(1)),
        "{:?}",
        sent.frames
    );
}

/// Any other action dismisses the overlay first, then runs.
#[tokio::test]
async fn another_action_dismisses_the_overlay_then_runs() {
    let mut env = env_with_floating();
    let sent = env
        .dispatch(vec![ctrl_a(), press(PhysicalKey::C, Some("c"))])
        .await;
    assert_eq!(kills(&sent.frames), vec![tid(9)]);
    assert!(
        sent.frames
            .iter()
            .any(|frame| matches!(frame, FrameKind::SpawnResource { .. })),
        "new-window still spawns: {:?}",
        sent.frames
    );
}

/// Copy mode does not mutate the layout, but dismissing the floating box
/// must still ask the driver for a full repaint.
#[tokio::test]
async fn a_non_layout_action_repaints_when_it_dismisses_the_floating_overlay() {
    let mut env = env_with_floating();
    let sent = env
        .dispatch(vec![ctrl_a(), press(PhysicalKey::BracketLeft, Some("["))])
        .await;
    assert_eq!(kills(&sent.frames), vec![tid(9)]);
    assert!(!env.panes.contains_key(&tid(9)));
    assert!(env.fx.expected_closes.contains(&tid(9)));
    assert_eq!(
        crate::attach::arrangement_shape(&env.fx.workspace),
        crate::attach::arrangement_shape(&crate::layout::ArrangementDoc::single(tid(1)))
    );
    assert!(
        env.fx.overlays.copy_selection().is_some(),
        "copy mode still opens"
    );
    assert!(
        sent.repainted,
        "dismissing the floating box needs a full repaint"
    );
}

/// Waiting for the initial layout read blocks the action, not the repaint
/// needed by the floating overlay it already dismissed.
#[tokio::test]
async fn a_blocked_layout_action_repaints_when_it_dismisses_the_floating_overlay() {
    let mut env = env_with_floating();
    env.fx.layout_read_complete = false;
    let sent = env
        .dispatch(vec![ctrl_a(), press(PhysicalKey::C, Some("c"))])
        .await;
    assert_eq!(kills(&sent.frames), vec![tid(9)]);
    assert_eq!(sent.frames.len(), 1, "new-window must stay blocked");
    assert!(!env.panes.contains_key(&tid(9)));
    assert_eq!(
        crate::attach::arrangement_shape(&env.fx.workspace),
        crate::attach::arrangement_shape(&crate::layout::ArrangementDoc::single(tid(1)))
    );
    assert!(
        sent.repainted,
        "even a blocked action dismissed the floating box"
    );
}

/// The pointer inside the box reaches the overlay pane-local; a press
/// outside the box dismisses it.
#[tokio::test]
async fn the_pointer_inside_reaches_the_overlay_and_a_press_outside_dismisses() {
    let inner = crate::attach::floating::floating_box(content_rect((80, 24), None, None)).inner;
    let mut env = env_with_floating();
    let inside = InputEvent::Mouse(mev(
        MouseAction::Press,
        MouseButton::Left,
        f64::from(inner.x + 2),
        f64::from(inner.y + 1),
    ));
    let sent = env.dispatch(vec![inside]).await;
    match sent.frames.as_slice() {
        [FrameKind::InputMouse { terminal_id, event }] => {
            assert_eq!(*terminal_id, tid(9));
            assert!((event.x - 2.0).abs() < f64::EPSILON && (event.y - 1.0).abs() < f64::EPSILON);
        }
        other => panic!("expected one overlay mouse frame, got {other:?}"),
    }
    let outside = InputEvent::Mouse(mev(MouseAction::Press, MouseButton::Left, 0.0, 23.0));
    let sent = env.dispatch(vec![outside]).await;
    assert_eq!(kills(&sent.frames), vec![tid(9)]);
    assert!(sent.repainted && !env.panes.contains_key(&tid(9)));
}
