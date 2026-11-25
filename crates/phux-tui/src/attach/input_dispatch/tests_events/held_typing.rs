// ---- typing while a chosen pane opens ----------------------------------------------

use crate::attach::actions::{Adopt, PendingWindow};

/// Two panes, the first focused, and a satellite pane opening.
fn opening_env() -> Env<'static> {
    let mut env = two_pane_env(&[(tid(1), b""), (tid(2), b"")], (8, 16));
    env.fx.pending_windows.insert(
        9,
        PendingWindow {
            name: "edge/build".to_owned(),
            adopt: Some(Adopt::Existing {
                pane: ResourceId::satellite("edge", 9),
                hold: true,
            }),
        },
    );
    env
}

/// The panes one key reaches.
#[allow(clippy::future_not_send, reason = "the test env holds a !Send Terminal")]
async fn typed_into(env: &mut Env<'_>) -> Vec<ResourceId> {
    let sent = env.dispatch(vec![press(PhysicalKey::A, Some("a"))]).await;
    sent.frames
        .into_iter()
        .filter_map(|frame| match frame {
            FrameKind::InputKey { terminal_id, .. } => Some(terminal_id),
            _ => None,
        })
        .collect()
}

/// Typing meant for the pane still opening reaches no pane, whichever one
/// focus lands on without the user: the pane in view closed, another client
/// moved focus, or a floating pane is up.
#[tokio::test]
async fn typing_is_held_wherever_focus_lands_on_its_own() {
    let mut env = opening_env();
    assert_eq!(typed_into(&mut env).await, Vec::new());

    let group = env.fx.workspace.active_group_mut().expect("window");
    group.focus = Some(tid(2));
    env.focused = Some(tid(2));
    assert_eq!(typed_into(&mut env).await, Vec::new());

    env.panes.get_mut(&tid(2)).expect("slot").floating = Some("popup".to_owned());
    assert_eq!(typed_into(&mut env).await, Vec::new());

    env.fx.pending_windows.clear();
    assert_eq!(typed_into(&mut env).await, vec![tid(2)], "answered");
}

/// A pane the user clicks, the one in view included, takes typing at once.
#[tokio::test]
async fn a_click_types_there_while_another_pane_opens() {
    for (column, clicked) in [(5.0, tid(1)), (70.0, tid(2))] {
        let mut env = opening_env();
        env.dispatch(vec![
            mouse(MouseAction::Press, MouseButton::Left, column, 5.0),
            mouse(MouseAction::Release, MouseButton::Left, column, 5.0),
        ])
        .await;
        assert_eq!(typed_into(&mut env).await, vec![clicked]);
    }
}

/// So does a chord: the user is doing something else.
#[tokio::test]
async fn an_action_ends_the_hold() {
    let mut env = opening_env();
    let action = super::args::bare_action("next-window");
    super::run_action::run_action(&action, &mut env.fx.ctx(), None, &env.panes);
    assert_eq!(typed_into(&mut env).await, vec![tid(1)]);
}
