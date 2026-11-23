//! Opaque item IDs must not become Rune path syntax or a reserved role prefix.
use super::*;

fn desktop(d: &Desk, _: &mut rune::Cx<Desk>) -> El {
    let t = Tokens::resolve(&crate::ui::tokens::Inputs::system(true));
    let terminal = ResourceId::local(10);
    panel(d, &terminal, &d.inbox.questions.ask(&terminal).unwrap(), &t)
}

fn fixture(second: &str, custom: bool) -> (Runtime<Desk>, Owner) {
    let mut d = crate::test_support::window();
    let mut asked = question("ask");
    asked.title = "Opaque item identities".into();
    asked.message.clear();
    asked.fields = json!({"allow_custom_input": custom});
    asked.questions = ["x", second]
        .into_iter()
        .map(|id| NativeQuestionItem {
            id: id.into(),
            question: id.into(),
            header: String::new(),
            options: ["one", "two"]
                .into_iter()
                .map(|id| NativeOption {
                    id: id.into(),
                    label: id.into(),
                    description: None,
                })
                .collect(),
            multi: false,
            recommended: None,
        })
        .collect();
    let terminal = ResourceId::local(10);
    d.inbox.questions.views.insert(
        terminal.clone(),
        (
            ResourceId::local(11),
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
                questions: vec![asked],
            },
        ),
    );
    d.inbox.questions.reconcile();
    let owner = d.inbox.questions.owner(&terminal, ID).unwrap().clone();
    let mut rt =
        crate::test_support::runtime(d, desktop, (1100.0, 1200.0), rune::Env::new(true, 1.0));
    settle(&mut rt);
    (rt, owner)
}

fn at_path(rt: &Runtime<Desk>, path: &str) -> rune::ElementInfo {
    rt.elements()
        .into_iter()
        .find(|e| e.path == path)
        .expect("actual mounted full path")
}

fn click(rt: &mut Runtime<Desk>, path: &str) {
    let [x, y, w, h] = at_path(rt, path).rect;
    rt.pointer_down(x + w / 2.0, y + h / 2.0);
    rt.pointer_up(x + w / 2.0, y + h / 2.0);
    settle(rt);
}

#[test]
fn desktop_opaque_item_paths_keep_radios_focus_and_text_independent() {
    for (second, custom) in [
        ("x/items/item0", false),
        ("x/items/item0", true),
        ("field-x", true),
    ] {
        let (mut rt, owner) = fixture(second, custom);
        let first_group = crate::ui::element(&rt, &owner.widget(Widget::Choices("x")));
        let second_group = crate::ui::element(&rt, &owner.widget(Widget::Choices(second)));
        let first_option = format!("{}/items/item0", first_group.path);
        let second_option = format!("{}/items/item1", second_group.path);
        assert_ne!(
            first_option, second_group.path,
            "a radio must not alias another group"
        );
        let paths: Vec<_> = rt.elements().into_iter().map(|e| e.path).collect();
        assert_eq!(
            paths.len(),
            paths.iter().collect::<std::collections::HashSet<_>>().len()
        );
        for item in ["x", second] {
            assert!(!owner.widget(Widget::Choices(item)).contains('/'));
            assert!(!owner.field_key(item).contains('/'));
        }
        click(&mut rt, &first_option);
        let draft = rt.state.inbox.questions.draft(&owner).unwrap();
        assert_eq!(draft.choices["x"], ["one"]);
        assert!(!draft.choices.contains_key(second));
        assert!(at_path(&rt, &first_option).focused);
        click(&mut rt, &second_option);
        let draft = rt.state.inbox.questions.draft(&owner).unwrap();
        assert_eq!(draft.choices["x"], ["one"]);
        assert_eq!(draft.choices[second], ["two"]);
        assert!(!at_path(&rt, &first_option).focused);
        assert!(at_path(&rt, &second_option).focused);
        if custom {
            for (item, text) in [("x", "first text"), (second, "second text")] {
                assert!(rt.accessibility_action(
                    &owner.field_key(item),
                    rune::accesskit::Action::Focus,
                    None
                ));
                settle(&mut rt);
                rt.ime(rune::Ime::Commit(text.into()));
                settle(&mut rt);
            }
            let draft = rt.state.inbox.questions.draft(&owner).unwrap();
            assert_eq!(draft.text("x"), "first text");
            assert_eq!(draft.text(second), "second text");
            assert_eq!(draft.choices["x"], ["one"]);
            assert_eq!(draft.choices[second], ["two"]);
        }
    }
}

#[test]
fn provider_option_named_cancel_does_not_alias_the_internal_cancel_action() {
    let (mut rt, old) = fixture("x/items/item0", false);
    let asked = &mut rt
        .state
        .inbox
        .questions
        .views
        .get_mut(old.terminal())
        .unwrap()
        .1
        .questions[0];
    asked.kind = "permission".into();
    asked.questions.clear();
    asked.options = vec![NativeOption {
        id: "cancel".into(),
        label: "Provider option named cancel".into(),
        description: None,
    }];
    rt.state.inbox.questions.reconcile();
    let owner = rt
        .state
        .inbox
        .questions
        .owner(old.terminal(), ID)
        .unwrap()
        .clone();
    rt.update(|_, tx| tx.instant());
    settle(&mut rt);
    let provider = crate::ui::element(&rt, &owner.widget(Widget::Option("cancel")));
    let internal = crate::ui::element(&rt, &owner.widget(Widget::Action("cancel")));
    assert_ne!(provider.path, internal.path);
    let elements = rt.elements();
    for (button, label) in [
        (provider, "Provider option named cancel"),
        (internal, "Cancel request"),
    ] {
        assert!(
            elements
                .iter()
                .any(|e| e.path.starts_with(&button.path) && e.text.as_deref() == Some(label))
        );
    }
}
