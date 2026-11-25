//! Real app regions retain the global queued-IME/Return boundary regression.
use super::ownership::{Change, replace_owner};
use super::*;

fn scene(inline: bool) -> (Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = fixture((1100.0, 720.0), "input");
    rt.poll = Some(|d, _, tx| d.return_keyboard(tx));
    rt.update(|d, tx| {
        d.phone = None;
        d.native = None;
        if inline {
            d.close_inbox();
        }
        tx.instant();
    });
    for _ in 0..120 {
        rt.update(|_, tx| tx.instant());
        rt.frame(1.0 / 60.0);
    }
    (client, rt)
}

fn target(d: &Desk, inline: bool) -> String {
    if inline {
        super::super::pane_focus(&d.connection.shown.as_ref().unwrap().resource)
    } else {
        super::super::INBOX_FOCUS.into()
    }
}

#[test]
fn global_ime_and_return_retire_to_a_nonanswering_region_until_explicit_focus() {
    for inline in [false, true] {
        for change in [Change::Activation, Change::Content, Change::Reopen] {
            let (client, mut rt) = scene(inline);
            let old = owner(&rt.state).clone();
            let old_key = old.field_key("text");
            assert!(rt.accessibility_action(&old_key, rune::accesskit::Action::Focus, None));
            settle(&mut rt);
            let _ = client.take_outbound();
            rt.ime(rune::Ime::Commit("retired batch text".into()));
            rt.key(crate::keys::plain(rune::Key::Enter));
            // Same invalidating update boundary as an app delivery/poll, not a
            // no-dirty frame that accidentally preserves the old input tree.
            rt.update(|d, tx| {
                replace_owner(d, change);
                tx.instant();
            });
            let current = owner(&rt.state).clone();
            assert_ne!(old, current);
            settle(&mut rt);
            let draft = rt.state.inbox.questions.draft(&current).unwrap();
            assert_eq!(draft.text("text"), "");
            assert!(draft.choices.is_empty());
            assert!(rt.state.inbox.questions.pending.is_empty());
            assert!(!rt.state.inbox.questions.error.contains_key(&current));
            assert!(!rt.state.inbox.questions.status.contains_key(&current));
            assert!(
                !rt.elements()
                    .iter()
                    .any(|e| e.key.as_deref() == Some(old_key.as_str()) && !e.exiting)
            );
            let rest = target(&rt.state, inline);
            rt.update(|d, tx| {
                assert_eq!(tx.focused(), Some(rest.as_str()));
                // AppSpec::on_ime (lib.rs/quick.rs) delegates here. Runtime's
                // public fixture cannot install its private IME hook; exercise
                // the actual app fallback with its real focused transaction.
                assert!(!d.runtime_ime(&rune::Ime::Commit("must not enter terminal".into()), tx));
            });
            no_input(&client);
            // Tab remains explicit navigation, not a permanent keyboard trap.
            rt.key(crate::keys::plain(rune::Key::Tab));
            settle(&mut rt);
            rt.update(|_, tx| assert_ne!(tx.focused(), Some(rest.as_str())));
            let current_key = current.field_key("text");
            assert!(rt.accessibility_action(&current_key, rune::accesskit::Action::Focus, None));
            settle(&mut rt);
            rt.ime(rune::Ime::Commit("fresh explicit input".into()));
            settle(&mut rt);
            assert_eq!(
                rt.state
                    .inbox
                    .questions
                    .draft(&current)
                    .unwrap()
                    .text("text"),
                "fresh explicit input"
            );
            assert!(rt.state.inbox.questions.pending.is_empty());
            no_input(&client);
        }
    }
}

#[test]
fn closing_the_request_also_keeps_the_retirement_target_out_of_terminal_input() {
    for (inline, clicked) in [(false, false), (true, false), (true, true)] {
        let (client, mut rt) = scene(inline);
        let old = owner(&rt.state).clone();
        let field = old.field_key("text");
        if clicked {
            let (x, y) = rt.find(&field).expect("the field");
            rt.pointer_down(x, y);
            rt.pointer_up(x, y);
        } else {
            assert!(rt.accessibility_action(&field, rune::accesskit::Action::Focus, None));
        }
        settle(&mut rt);
        let _ = client.take_outbound();
        rt.ime(rune::Ime::Commit("closed request text".into()));
        rt.key(crate::keys::plain(rune::Key::Enter));
        rt.update(|d, tx| {
            d.inbox
                .questions
                .views
                .get_mut(old.terminal())
                .unwrap()
                .1
                .questions
                .clear();
            d.inbox.questions.reconcile();
            tx.instant();
        });
        settle(&mut rt);
        let rest = target(&rt.state, inline);
        rt.update(|d, tx| {
            assert_eq!(tx.focused(), Some(rest.as_str()));
            assert!(!d.runtime_ime(&rune::Ime::Commit("not terminal input".into()), tx));
        });
        assert!(rt.state.inbox.questions.draft(&old).is_none());
        assert!(rt.state.inbox.questions.pending.is_empty());
        no_input(&client);
    }
}

#[test]
fn secondary_mouse_reporting_releases_retired_focus_and_restores_terminal_typing() {
    use phux_protocol::input::key::{KeyAction, PhysicalKey};
    use phux_protocol::input::mouse::{MouseAction, MouseButton};
    use phux_protocol::wire::frame::{Command, FrameKind};

    let (client, mut rt) = scene(true);
    let old = owner(&rt.state).clone();
    let terminal = old.terminal().clone();
    client
        .feed(phux_wire_fixtures::output(
            &terminal,
            1,
            b"\x1b[?1000h\x1b[?1006h",
        ))
        .expect("enable real terminal mouse reporting");
    assert!(rt.accessibility_action(&old.field_key("text"), rune::accesskit::Action::Focus, None));
    settle(&mut rt);
    let _ = client.take_outbound();
    rt.ime(rune::Ime::Commit("retired batch text".into()));
    rt.key(crate::keys::plain(rune::Key::Enter));
    rt.update(|d, tx| {
        replace_owner(d, Change::Activation);
        tx.instant();
    });
    let current = owner(&rt.state).clone();
    settle(&mut rt);
    let rest = target(&rt.state, true);
    rt.update(|d, tx| {
        assert_eq!(tx.focused(), Some(rest.as_str()));
        assert!(!d.runtime_ime(&rune::Ime::Commit("still retired".into()), tx));
    });
    assert_eq!(
        rt.state
            .inbox
            .questions
            .draft(&current)
            .unwrap()
            .text("text"),
        ""
    );
    assert!(rt.state.inbox.questions.pending.is_empty());
    assert!(!rt.state.inbox.questions.error.contains_key(&current));
    no_input(&client);

    // The grid's left edge is outside the inline card at its right corner.
    let [left, top] = rt.state.connection.origins[&terminal];
    let metrics = rt.state.metrics();
    let (x, y) = (left + metrics.cell_w * 1.5, top + metrics.cell_h * 1.5);
    rt.secondary_down(x, y);
    rt.secondary_up(x, y);
    settle(&mut rt);
    assert!(
        rt.state.pointer.menu.is_none(),
        "the tracking program owns this secondary click"
    );
    assert!(rt.popup_menu().is_none(), "no context-menu substitute");
    rt.update(|_, tx| assert_eq!(tx.focused(), None));
    let mouse: Vec<_> = client
        .take_outbound()
        .iter()
        .filter_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
            FrameKind::InputMouse { terminal_id, event } => {
                Some((terminal_id, event.action, event.button))
            }
            FrameKind::InputKey { .. }
            | FrameKind::InputPaste { .. }
            | FrameKind::Command {
                command: Command::ApplyInput { .. } | Command::NativeAgent { .. },
                ..
            } => panic!("secondary click must report only its mouse sequence"),
            _ => None,
        })
        .collect();
    assert_eq!(
        mouse,
        [
            (terminal.clone(), MouseAction::Press, MouseButton::Right),
            (terminal.clone(), MouseAction::Release, MouseButton::Right),
        ]
    );

    rt.key(crate::keys::plain(rune::Key::Down));
    settle(&mut rt);
    // Same actual AppSpec IME fallback checked above, now with restored
    // terminal ownership. The public test Runtime has no app-IME hook setter.
    rt.update(|d, tx| {
        assert_eq!(tx.focused(), None);
        assert!(d.runtime_ime(&rune::Ime::Commit("z".into()), tx));
    });
    let keys: Vec<_> = client
        .take_outbound()
        .iter()
        .filter_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
            FrameKind::InputKey { terminal_id, event } => Some((terminal_id, event)),
            FrameKind::InputPaste { .. }
            | FrameKind::Command {
                command: Command::ApplyInput { .. } | Command::NativeAgent { .. },
                ..
            } => panic!("typing after secondary click must not answer the question"),
            _ => None,
        })
        .collect();
    assert_eq!(keys.len(), 2);
    assert_eq!(
        (&keys[0].0, keys[0].1.action, keys[0].1.key),
        (&terminal, KeyAction::Press, PhysicalKey::ArrowDown)
    );
    assert_eq!(
        (
            &keys[1].0,
            keys[1].1.action,
            keys[1].1.key,
            keys[1].1.text.as_deref()
        ),
        (&terminal, KeyAction::Press, PhysicalKey::Z, Some("z"))
    );
    assert_eq!(
        rt.state
            .inbox
            .questions
            .draft(&current)
            .unwrap()
            .text("text"),
        ""
    );
    assert!(rt.state.inbox.questions.pending.is_empty());
    assert!(!rt.state.inbox.questions.error.contains_key(&current));
}
