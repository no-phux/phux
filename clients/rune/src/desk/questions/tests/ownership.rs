//! Real ACP callback ids start at one for every owner.
use super::*;

fn two_owners(identical: bool) -> (Questions, ResourceId, ResourceId) {
    let mut questions = Questions::default();
    let first = question("input", json!({"initial_value": "first"}));
    let second = if identical {
        first.clone()
    } else {
        question("input", json!({"initial_value": "second"}))
    };
    let a = ResourceId::local(10);
    let b = ResourceId::local(20);
    for (terminal, session, asked) in [
        (&a, ResourceId::local(11), first),
        (&b, ResourceId::local(21), second),
    ] {
        questions
            .views
            .insert(terminal.clone(), (session, view(vec![asked], true)));
    }
    questions.reconcile();
    (questions, a, b)
}

#[test]
fn different_first_callbacks_open_their_own_question_and_text() {
    let (questions, a, b) = two_owners(false);
    let id = "acp-question:1";
    let (view, draft) = questions.open(&a, id).unwrap();
    assert_eq!(draft.question, view.questions[0]);
    assert_eq!(draft.text("text"), "first");
    let (view, draft) = questions.open(&b, id).unwrap();
    assert_eq!(draft.question, view.questions[0]);
    assert_eq!(draft.text("text"), "second");
}

#[test]
fn identical_first_callbacks_do_not_share_text_or_choices() {
    let (mut questions, a, b) = two_owners(true);
    let id = "acp-question:1";
    let owner = questions.owner(&a, id).unwrap().clone();
    questions
        .text_mut(&owner, "text")
        .replace_text("edited on A");
    questions.choose(&owner, "item", "chosen-on-A", false);
    assert_eq!(
        questions.open(&a, id).unwrap().1.text("text"),
        "edited on A"
    );
    let other = questions.open(&b, id).unwrap().1;
    assert_eq!(other.text("text"), "first");
    assert!(other.choices.is_empty());
}

#[test]
fn submitting_and_late_receipts_do_not_hold_or_error_another_owner() {
    let (mut questions, a, b) = two_owners(true);
    let id = "acp-question:1";
    let owner = questions.owner(&a, id).unwrap().clone();
    let other = questions.owner(&b, id).unwrap().clone();
    questions.pending.insert(7, owner.clone());
    questions.status.insert(owner.clone(), SUBMITTING.into());
    assert!(questions.held(&owner));
    assert!(!questions.held(&other));
    assert!(questions.receipt(7, NativeDisposition::Refused, Some("old A")));
    assert!(questions.error.get(&owner).is_some());
    // Replacing A's current owner must not transfer its old receipt to B.
    questions.views.remove(&a);
    questions.reconcile();
    assert!(!questions.held(&other));
    assert!(questions.error.is_empty(), "A's error leaked to B");
}

#[test]
fn identical_question_in_a_new_activation_does_not_inherit_pending_state() {
    let (mut questions, a, _) = two_owners(true);
    let id = "acp-question:1";
    let owner = questions.owner(&a, id).unwrap().clone();
    questions
        .text_mut(&owner, "text")
        .replace_text("old activation");
    questions.pending.insert(9, owner.clone());
    questions.status.insert(owner, SUBMITTING.into());
    questions.views.get_mut(&a).unwrap().1.activation = "activation-B".into();
    questions.reconcile();
    let owner = questions.owner(&a, id).unwrap();
    let (_, draft) = questions.open(&a, id).unwrap();
    assert!(!questions.held(owner));
    assert_eq!(draft.text("text"), "first");
    assert!(questions.receipt(9, NativeDisposition::Refused, Some("late")));
    assert!(questions.error.is_empty());
}

#[test]
fn identical_cursor_asks_keep_explicit_choices_and_receipts_with_their_owner() {
    let (mut questions, a, b) = two_owners(true);
    let mut asked = question("ask", json!({"allow_custom_input": false}));
    asked.questions = vec![item("opaque:item", &["opaque:one", "opaque:two"], false)];
    for (_, published) in questions.views.values_mut() {
        published.questions = vec![asked.clone()];
    }
    questions.reconcile();
    let first = questions.owner(&a, &asked.id).unwrap().clone();
    let second = questions.owner(&b, &asked.id).unwrap().clone();
    questions.choose(&first, "opaque:item", "opaque:one", false);
    assert_eq!(
        questions.draft(&first).unwrap().choices["opaque:item"],
        ["opaque:one"]
    );
    assert!(questions.draft(&second).unwrap().choices.is_empty());
    questions.pending.insert(10, first.clone());
    questions.status.insert(first.clone(), SUBMITTING.into());
    questions.pending.insert(11, second.clone());
    questions.status.insert(second.clone(), SUBMITTING.into());
    assert!(questions.receipt(10, NativeDisposition::Refused, Some("first only")));
    assert!(!questions.held(&first));
    assert!(questions.held(&second));
    assert!(questions.error.contains_key(&first));
    assert!(!questions.error.contains_key(&second));
    assert_eq!(questions.status[&second], SUBMITTING);
}

#[test]
fn changed_and_reopened_requests_retire_callbacks_and_receipts_even_with_the_same_id() {
    let (mut questions, a, _) = two_owners(true);
    let id = "acp-question:1";
    for reopen in [false, true] {
        let old = questions.owner(&a, id).unwrap().clone();
        let original = questions.draft(&old).unwrap().question.clone();
        questions.pending.insert(12, old.clone());
        questions.status.insert(old.clone(), SUBMITTING.into());
        if reopen {
            questions.views.get_mut(&a).unwrap().1.questions.clear();
            questions.reconcile();
            questions.views.get_mut(&a).unwrap().1.questions = vec![original];
        } else {
            questions.views.get_mut(&a).unwrap().1.questions[0]
                .message
                .push_str(" changed");
            // Reject before reconciliation as well as after replacement.
            assert!(questions.field_mut(&old, "text").is_none());
        }
        questions.reconcile();
        let current = questions.owner(&a, id).unwrap().clone();
        assert_ne!(old, current);
        assert_ne!(old.field_key("text"), current.field_key("text"));
        questions.text_mut(&old, "text").replace_text("stale edit");
        questions.choose(&old, "item", "stale choice", false);
        for disposition in [
            NativeDisposition::Queued,
            NativeDisposition::Unknown,
            NativeDisposition::Refused,
        ] {
            assert!(questions.receipt(12, disposition, Some("old receipt")));
            assert!(!questions.held(&current));
            assert!(!questions.error.contains_key(&current));
            assert_eq!(questions.draft(&current).unwrap().text("text"), "first");
            assert!(questions.draft(&current).unwrap().choices.is_empty());
        }
    }
}

#[test]
fn replacing_the_entire_local_state_never_reuses_a_widget_generation() {
    let (questions, a, _) = two_owners(true);
    let old = questions.owner(&a, "acp-question:1").unwrap().clone();
    let (mut replacement, a, _) = two_owners(true);
    let current = replacement.owner(&a, "acp-question:1").unwrap().clone();
    assert_ne!(old, current);
    assert_ne!(old.field_key("text"), current.field_key("text"));
    replacement
        .text_mut(&old, "text")
        .replace_text("stale runtime");
    assert_eq!(replacement.draft(&current).unwrap().text("text"), "first");
}
