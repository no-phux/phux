// ---- non-Terminal layout leaves ---------------------------------------------

/// [`two_pane_workspace`] with pane 1 a published Terminal and pane 2 a
/// document leaf, default bindings, focus on pane 1.
fn surface_leaf_env() -> Env<'static> {
    let mut fx = CtxFixture::default();
    fx.workspace = two_pane_workspace();
    let mut env = Env::new(fx)
        .with_default_bindings()
        .published(&[(&tid(1), 39, 24, b"")]);
    crate::attach::surface_leaf::declare_document(
        &mut env.fx.engine_kernel,
        &tid(2),
        phux_protocol::BootstrapStreamProfile::DocumentTypedV1,
    );
    env
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
