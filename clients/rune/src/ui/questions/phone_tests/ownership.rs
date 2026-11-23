//! Mounted callbacks retain their owner, not a terminal/id to resolve later.
use super::*;

#[derive(Clone, Copy, Debug)]
enum Change {
    Activation,
    Content,
    Reopen,
}

fn replace_owner(d: &mut Desk, change: Change) {
    let terminal = d.connection.shown.as_ref().unwrap().resource.clone();
    let original = d.inbox.questions.views[&terminal].1.questions[0].clone();
    match change {
        Change::Activation => d
            .inbox
            .questions
            .views
            .get_mut(&terminal)
            .unwrap()
            .1
            .activation
            .push_str("-new"),
        Change::Content => d
            .inbox
            .questions
            .views
            .get_mut(&terminal)
            .unwrap()
            .1
            .questions[0]
            .message
            .push_str(" revised"),
        Change::Reopen => {
            d.inbox
                .questions
                .views
                .get_mut(&terminal)
                .unwrap()
                .1
                .questions
                .clear();
            d.inbox.questions.reconcile();
            d.inbox
                .questions
                .views
                .get_mut(&terminal)
                .unwrap()
                .1
                .questions = vec![original];
        }
    }
    d.inbox.questions.reconcile();
}

#[test]
fn queued_real_widget_edits_choices_and_answers_cannot_retarget_a_replacement() {
    for change in [Change::Activation, Change::Content, Change::Reopen] {
        for (kind, control) in [
            ("input", "field-text"),
            ("ask", "false-one"),
            ("confirm", "yes"),
        ] {
            let (client, mut rt) = fixture((393.0, 852.0), kind);
            let old = owner(&rt.state).clone();
            let widget = old.widget(control);
            reveal(&mut rt, &widget);
            if kind == "input" {
                assert!(rt.accessibility_action(&widget, rune::accesskit::Action::Focus, None));
                settle(&mut rt);
                rt.ime(rune::Ime::Commit("stale widget edit".into()));
            } else {
                assert!(rt.accessibility_action(&widget, rune::accesskit::Action::Click, None));
            }
            // No frame/update: the queued event still holds the mounted old
            // handler when the authoritative local question is replaced.
            replace_owner(&mut rt.state, change);
            let current = owner(&rt.state).clone();
            assert_ne!(old, current, "{kind} {change:?}");
            settle(&mut rt);
            let current_draft = rt.state.inbox.questions.draft(&current).unwrap();
            assert!(current_draft.choices.is_empty(), "{kind} {change:?}");
            assert_eq!(current_draft.text("text"), "", "{kind} {change:?}");
            assert!(!rt.state.inbox.questions.status.contains_key(&current));
            assert!(!rt.state.inbox.questions.error.contains_key(&current));
            assert!(rt.state.inbox.questions.pending.is_empty());
            no_input(&client);
        }
    }
}

fn paired(d: &Desk, _: &mut rune::Cx<Desk>) -> El {
    let t = Tokens::resolve(&crate::ui::tokens::Inputs::system(true));
    let mut panels = col().w_full();
    for terminal in [ResourceId::local(10), ResourceId::local(20)] {
        panels = panels.child(panel(
            d,
            &terminal,
            &d.inbox.questions.ask(&terminal).unwrap(),
            &t,
        ));
    }
    panels
}

#[test]
fn coexisting_same_id_widgets_have_distinct_keys_and_choose_only_their_owner() {
    let mut d = crate::test_support::window();
    d.phone = Some(crate::desk::phone::Phone::default());
    for (terminal, session) in [(10, 11), (20, 21)] {
        d.inbox.questions.views.insert(
            ResourceId::local(terminal),
            (
                ResourceId::local(session),
                NativeView {
                    activation: "first".into(),
                    backend: "acp".into(),
                    capabilities: NativeCapabilities {
                        questions: true.into(),
                        ..Default::default()
                    },
                    connected: true,
                    turn_id: None,
                    provider_session_id: None,
                    questions: vec![question("ask")],
                },
            ),
        );
    }
    d.inbox.questions.reconcile();
    let first = d
        .inbox
        .questions
        .owner(&ResourceId::local(10), ID)
        .unwrap()
        .clone();
    let second = d
        .inbox
        .questions
        .owner(&ResourceId::local(20), ID)
        .unwrap()
        .clone();
    let mut rt =
        crate::test_support::runtime(d, paired, (800.0, 4000.0), rune::Env::new(true, 1.0));
    settle(&mut rt);
    let keys: Vec<_> = rt
        .elements()
        .into_iter()
        .filter_map(|e| e.key)
        .filter(|key| key.starts_with("acp-"))
        .collect();
    let unique: std::collections::HashSet<_> = keys.iter().collect();
    assert_eq!(
        keys.len(),
        unique.len(),
        "coexisting controls may not share mount identity"
    );
    for owner in [&first, &second] {
        assert!(rt.find(&owner.widget("false-one")).is_some());
        assert!(rt.find(&owner.field_key("false")).is_some());
        assert!(rt.find(&owner.widget("submit")).is_some());
    }
    assert!(rt.accessibility_action(
        &first.widget("false-one"),
        rune::accesskit::Action::Click,
        None
    ));
    settle(&mut rt);
    assert_eq!(
        rt.state.inbox.questions.draft(&first).unwrap().choices["false"],
        ["one"]
    );
    assert!(
        rt.state
            .inbox
            .questions
            .draft(&second)
            .unwrap()
            .choices
            .is_empty()
    );
}
