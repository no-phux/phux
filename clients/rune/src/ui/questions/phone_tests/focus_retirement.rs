//! Real app regions retain the global queued-IME/Return boundary regression.
use super::ownership::{Change, replace_owner};
use super::*;

fn scene(inline: bool) -> (Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = fixture((1100.0, 720.0), "input");
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
    for inline in [false, true] {
        let (client, mut rt) = scene(inline);
        let old = owner(&rt.state).clone();
        assert!(rt.accessibility_action(
            &old.field_key("text"),
            rune::accesskit::Action::Focus,
            None
        ));
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
