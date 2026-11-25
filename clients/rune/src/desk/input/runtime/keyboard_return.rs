//! The terminal has the keyboard again once the control that had it is gone
//! or a click on it is over.

use std::sync::Arc;

use phux_client_runtime::Client;
use phux_protocol::wire::frame::FrameKind;

use super::super::Desk;
use super::*;

fn window() -> (Arc<Client>, rune::Runtime<Desk>) {
    let (client, mut rt) = crate::test_support::input_fixture();
    // The first thing `Desk::poll` does. The rest of it belongs to a live
    // connection, and takes the fixture's terminal away.
    rt.poll = Some(|d, _, tx| d.return_keyboard(tx));
    settle(&mut rt);
    (client, rt)
}

fn settle(rt: &mut rune::Runtime<Desk>) {
    for _ in 0..4 {
        rt.frame(1.0);
    }
}

fn click(rt: &mut rune::Runtime<Desk>, key: &str) {
    let (x, y) = rt.find(key).unwrap_or_else(|| panic!("{key}"));
    rt.pointer_down(x, y);
    settle(rt);
    rt.pointer_up(x, y);
    settle(rt);
}

fn focus(rt: &mut rune::Runtime<Desk>, key: &str) {
    assert!(
        rt.accessibility_action(key, rune::accesskit::Action::Focus, None),
        "{key}"
    );
    settle(rt);
}

fn focused(rt: &mut rune::Runtime<Desk>) -> Option<String> {
    let mut key = None;
    rt.update(|_, tx| key = tx.focused().map(str::to_owned));
    key
}

/// A space, which would press a focused control, reaches the terminal.
fn types(client: &Client, rt: &mut rune::Runtime<Desk>, after: &str) {
    assert_eq!(focused(rt), None, "{after}");
    let _ = client.take_outbound();
    rt.key(crate::keys::plain(Key::Text(" ".into())));
    settle(rt);
    let keys = client
        .take_outbound()
        .iter()
        .filter(|bytes| {
            matches!(
                FrameKind::decode(bytes).unwrap().0,
                FrameKind::InputKey { .. }
            )
        })
        .count();
    assert_eq!(keys, 1, "{after}: the key reaches the terminal once");
}

#[test]
fn finishing_onboarding_by_pointer_or_key_hands_the_keyboard_back() {
    for by_key in [false, true] {
        let (client, mut rt) = window();
        rt.update(|d, _| d.onboarding.open = true);
        settle(&mut rt);
        assert!(rt.state.modal().is_some());
        let skip = "onboarding-skip";
        if by_key {
            focus(&mut rt, skip);
            rt.key(crate::keys::plain(Key::Text(" ".into())));
            settle(&mut rt);
        } else {
            click(&mut rt, skip);
        }
        assert!(!rt.state.onboarding.open, "by_key={by_key}");
        types(&client, &mut rt, "after onboarding");
        assert!(!rt.state.settings_open && rt.state.sidebar_shown());
    }
}

#[test]
fn a_find_bar_that_closes_hands_the_keyboard_back() {
    let (client, mut rt) = window();
    // The arrows make Rune show focus, as they do at a shell prompt.
    rt.key(crate::keys::plain(Key::Up));
    settle(&mut rt);
    rt.update(|d, _| d.open_find());
    settle(&mut rt);
    assert_eq!(focused(&mut rt).as_deref(), Some("find-query"));
    rt.key(crate::keys::plain(Key::Right));
    settle(&mut rt);
    // What `keep_find` does when the shown pane changes: no blur of its own.
    rt.update(|d, _| d.close_find());
    settle(&mut rt);
    types(&client, &mut rt, "after the find bar");
}

#[test]
fn tab_still_reaches_the_find_bars_buttons() {
    let (_client, mut rt) = window();
    rt.update(|d, _| d.open_find());
    settle(&mut rt);
    rt.set_full_keyboard_access(true);
    rt.key(crate::keys::plain(Key::Tab));
    settle(&mut rt);
    let reached = focused(&mut rt).expect("Tab moves on from the field");
    assert_ne!(reached, "find-query");
    settle(&mut rt);
    assert_eq!(focused(&mut rt), Some(reached), "and stays");
}

/// A window whose shown terminal asks, its answers on chips over it.
fn asked() -> (Arc<Client>, rune::Runtime<Desk>, String) {
    let (client, mut rt) = window();
    let terminal_id = rt.state.connection.shown.as_ref().unwrap().resource.clone();
    rt.update(|d, _| {
        d.connection.topology = client.topology();
        d.activities.apply(
            &phux_client_runtime::control::Event::AgentAsked {
                terminal_id: terminal_id.clone(),
                question_id: "q-1".into(),
                text: "Continue?".into(),
                suggestions: vec!["yes".into(), "no".into()],
                waiting_seconds: None,
            },
            &[],
        );
    });
    settle(&mut rt);
    (client, rt, format!("chip-{terminal_id}-yes"))
}

#[test]
fn pressing_an_answer_chip_hands_the_keyboard_back() {
    for by_key in [false, true] {
        let (client, mut rt, chip) = asked();
        if by_key {
            focus(&mut rt, &chip);
            assert_eq!(focused(&mut rt).as_deref(), Some(chip.as_str()));
            rt.key(crate::keys::plain(Key::Text(" ".into())));
            settle(&mut rt);
        } else {
            click(&mut rt, &chip);
        }
        assert!(!rt.state.inbox.answers.is_empty(), "the chip answered");
        let after = format!("after a chip, by_key={by_key}");
        // Its card stays while the answer sends; the terminal draws on.
        rt.update(|d, _| d.generation += 1);
        settle(&mut rt);
        types(&client, &mut rt, &after);
        // The answer landed: the card goes.
        rt.update(|d, _| d.activities = crate::activity::Activities::default());
        settle(&mut rt);
        types(&client, &mut rt, &after);
    }
}

#[test]
fn a_control_removed_under_the_keyboard_hands_it_back() {
    let (client, mut rt) = window();
    rt.update(|d, _| d.say(crate::desk::NoticeKind::Error, "Gone soon"));
    settle(&mut rt);
    let close = rt
        .elements()
        .iter()
        .filter(|e| e.focusable)
        .filter_map(|e| e.key.clone())
        .find(|key| key.contains("toast"));
    let close = close.expect("the toast has a control");
    focus(&mut rt, &close);
    assert_eq!(focused(&mut rt).as_deref(), Some(close.as_str()));
    rt.update(|d, _| {
        d.dismiss_notices(|_| true);
    });
    settle(&mut rt);
    types(&client, &mut rt, "after the toast");
}

#[test]
fn a_sheet_opened_from_a_focused_button_returns_to_it() {
    let (_client, mut rt) = window();
    focus(&mut rt, "settings-button");
    rt.key(crate::keys::plain(Key::Text(" ".into())));
    settle(&mut rt);
    assert!(rt.state.settings_open, "Space presses the focused control");
    rt.key(crate::keys::plain(Key::Escape));
    settle(&mut rt);
    assert!(!rt.state.settings_open);
    assert_eq!(focused(&mut rt).as_deref(), Some("settings-button"));
    rt.set_full_keyboard_access(true);
    rt.key(crate::keys::plain(Key::Tab));
    settle(&mut rt);
    let next = focused(&mut rt).expect("Tab goes on through the chrome");
    assert_ne!(next, "settings-button");
}
