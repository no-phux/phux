//! The kill confirms: `kill-pane` and `kill-window` end nothing until
//! the user commits the target set the confirm named.

use super::*;

fn press_key(key: phux_protocol::input::key::PhysicalKey) -> phux_protocol::input::key::KeyEvent {
    phux_protocol::input::key::KeyEvent {
        action: phux_protocol::input::key::KeyAction::Press,
        key,
        mods: phux_protocol::input::key::ModSet::empty(),
        consumed_mods: phux_protocol::input::key::ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    }
}

/// kill-pane kills nothing on its own: it opens a confirm naming the
/// focused pane. Committing it kills that leaf, correlated and expected,
/// even after focus moved; nothing is removed until the close lands.
#[test]
fn kill_pane_confirms_then_kills_the_named_terminal() {
    let mut f = fx(three_pane_window());
    let effects = f.run(&bare_action("kill-pane"));
    assert!(effects.kill_frames.is_empty() && !effects.bell);
    assert!(f.overlays.is_active(), "the confirm is up");
    let crate::render::overlay::OverlayOutcome::RunAction(commit) = f
        .overlays
        .handle_key(&press_key(phux_protocol::input::key::PhysicalKey::Enter))
    else {
        panic!("Enter commits");
    };
    assert_eq!(commit.action, "kill-pane");
    f.workspace.groups[0].state.focus = Some(tid(2));
    let effects = f.run(&commit);
    let targets: Vec<_> = effects
        .kill_requests
        .iter()
        .map(|(_, leaf)| leaf.clone())
        .collect();
    assert_eq!(targets, vec![tid(1)]);
    assert_eq!(effects.kill_frames.len(), 1);
    assert_eq!(effects.expected_closes, vec![tid(1)]);
    assert_eq!(f.workspace.groups.len(), 1);
}

/// kill-window kills nothing on its own: it opens a confirm naming the
/// count. Committing it kills exactly the leaves it named (each correlated,
/// each an expected close); nothing is removed until the closes land.
#[test]
fn kill_window_confirms_then_kills_exactly_the_named_terminals() {
    let mut f = fx(three_pane_window());
    let effects = f.run(&bare_action("kill-window"));
    assert!(effects.kill_frames.is_empty() && !effects.bell);
    assert!(f.overlays.is_active(), "the confirm is up");
    let crate::render::overlay::OverlayOutcome::RunAction(commit) = f
        .overlays
        .handle_key(&press_key(phux_protocol::input::key::PhysicalKey::Enter))
    else {
        panic!("Enter commits");
    };
    assert!(!f.overlays.is_active());
    // A pane added after the confirm opened is not in the committed set.
    f.workspace.add_group("4".to_owned(), tid(4));
    let effects = f.run(&commit);
    let targets: Vec<_> = effects
        .kill_requests
        .iter()
        .map(|(_, leaf)| leaf.clone())
        .collect();
    assert_eq!(targets, vec![tid(1), tid(2), tid(3)]);
    assert_eq!(effects.kill_frames.len(), 3);
    assert_eq!(effects.expected_closes, vec![tid(1), tid(2), tid(3)]);
    assert_eq!(f.workspace.groups.len(), 2);

    let effects = run(&bare_action("kill-window"), &mut ArrangementDoc::default());
    assert!(effects.bell && effects.kill_frames.is_empty());
}

/// Cancelling the confirm kills nothing and leaves the window.
#[test]
fn kill_window_cancel_kills_nothing() {
    for key in [
        phux_protocol::input::key::PhysicalKey::Escape,
        phux_protocol::input::key::PhysicalKey::N,
    ] {
        let mut f = fx(three_pane_window());
        f.run(&bare_action("kill-window"));
        assert_eq!(
            f.overlays.handle_key(&press_key(key)),
            crate::render::overlay::OverlayOutcome::None
        );
        assert!(!f.overlays.is_active());
        assert_eq!(f.workspace.groups.len(), 1);
        assert!(f.expected_closes.is_empty());
    }
}

/// The confirm names a terminal also arranged in another window of this
/// session or in another session's arrangement.
#[test]
fn kill_window_confirm_names_terminals_shown_elsewhere() {
    let mut workspace = three_pane_window();
    workspace.add_group("logs".to_owned(), tid(2));
    workspace.select(0);
    let mut f = fx(workspace);
    f.sessions = vec![sinfo(1, "work"), sinfo(2, "notes")];
    f.focused_session = Some(SessionId::new(1));
    f.foreign_layouts
        .insert(SessionId::new(2), ArrangementDoc::single(tid(3)));
    let confirm =
        super::super::run_action::kill_window_confirm(&f.ctx()).expect("a window is active");
    assert_eq!(
        confirm.lines(),
        [
            "End 3 terminals in this tab.",
            "pane 2 is also shown in tab logs",
            "pane 3 is also shown in session notes",
        ]
    );
}
