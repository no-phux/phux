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
    // Feed the existing local-state API exactly as the single-owner fixtures do.
    for (terminal, session, asked) in [
        (&a, ResourceId::local(11), first),
        (&b, ResourceId::local(21), second),
    ] {
        questions
            .drafts
            .insert(asked.id.clone(), Draft::new(&asked));
        questions
            .views
            .insert(terminal.clone(), (session, view(vec![asked], true)));
    }
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
    questions.text_mut(id, "text").replace_text("edited on A");
    questions.choose(id, "item", "chosen-on-A", false);
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
    questions.pending.insert(7, id.into());
    questions.status.insert(id.into(), SUBMITTING.into());
    let (view, _) = questions.open(&a, id).unwrap();
    assert!(questions.held(view, id));
    let (view, _) = questions.open(&b, id).unwrap();
    assert!(!questions.held(view, id));
    assert!(questions.receipt(7, NativeDisposition::Refused, Some("old A")));
    assert!(questions.error.get(id).is_some());
    // Replacing A's current owner must not transfer its old receipt to B.
    questions.views.remove(&a);
    let (view, _) = questions.open(&b, id).unwrap();
    assert!(!questions.held(view, id));
    assert!(questions.error.is_empty(), "A's error leaked to B");
}

#[test]
fn identical_question_in_a_new_activation_does_not_inherit_pending_state() {
    let (mut questions, a, _) = two_owners(true);
    let id = "acp-question:1";
    questions.pending.insert(9, id.into());
    questions.status.insert(id.into(), SUBMITTING.into());
    questions
        .text_mut(id, "text")
        .replace_text("old activation");
    questions.views.get_mut(&a).unwrap().1.activation = "activation-B".into();
    let (view, draft) = questions.open(&a, id).unwrap();
    assert!(!questions.held(view, id));
    assert_eq!(draft.text("text"), "first");
    assert!(questions.receipt(9, NativeDisposition::Refused, Some("late")));
    assert!(questions.error.is_empty());
}
