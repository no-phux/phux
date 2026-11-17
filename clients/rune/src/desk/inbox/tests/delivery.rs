use super::*;
use rune::Runtime;

const DRAFT: &str = "Run the checks";

fn fixture() -> (std::sync::Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = crate::test_support::input_fixture();
    rt.state.connection.topology = client.topology();
    let terminal = rt.state.connection.shown.as_ref().unwrap().resource.clone();
    rt.update(|d, _| {
        d.activities.apply(
            &Event::AgentAsked {
                terminal_id: terminal,
                question_id: "first".into(),
                text: "Run checks?".into(),
                suggestions: vec![],
                waiting_seconds: None,
            },
            &[],
        );
        d.toggle_inbox();
        d.inbox.reply.insert(DRAFT);
    });
    rt.frame(0.0);
    (client, rt)
}

fn pending(rt: &mut Runtime<Desk>, answer: &str) -> u64 {
    let ItemKey::Terminal(terminal, Some(question)) = rt.state.inbox.picked.clone().unwrap() else {
        panic!("question")
    };
    rt.update(|d, _| d.answer(&terminal, &question, answer));
    assert_eq!(rt.state.inbox.answers.len(), 1);
    *rt.state.inbox.answers.keys().next().unwrap()
}

fn delivered(client: &Client, rt: &mut Runtime<Desk>, delivery_id: u64) {
    rt.update(|d, _| {
        d.inbox_event(
            client,
            &Event::InputDelivery {
                delivery_id,
                outcome: DeliveryOutcome::Delivered,
                code: None,
                message: String::new(),
            },
        );
    });
}

#[test]
fn a_suggestion_does_not_own_a_different_typed_draft() {
    let (client, mut rt) = fixture();
    let id = pending(&mut rt, "Wait");
    delivered(&client, &mut rt, id);
    assert_eq!(rt.state.inbox.reply.text, DRAFT);
}

#[test]
fn an_old_receipt_cannot_clear_identical_text_in_a_reopened_editor() {
    let (client, mut rt) = fixture();
    let id = pending(&mut rt, DRAFT);
    rt.update(|d, _| {
        d.close_inbox();
        d.toggle_inbox();
        d.inbox.reply.insert(DRAFT);
    });
    delivered(&client, &mut rt, id);
    assert_eq!(rt.state.inbox.reply.text, DRAFT);
}

#[test]
fn revising_a_question_under_the_same_id_preserves_the_current_draft() {
    let (client, mut rt) = fixture();
    let id = pending(&mut rt, DRAFT);
    let terminal = rt.state.connection.shown.as_ref().unwrap().resource.clone();
    rt.update(|d, _| {
        d.activities.apply(
            &Event::AgentAsked {
                terminal_id: terminal,
                question_id: "first".into(),
                text: "Run the newly added destructive checks?".into(),
                suggestions: vec![],
                waiting_seconds: None,
            },
            &[],
        );
    });
    delivered(&client, &mut rt, id);
    assert_eq!(rt.state.inbox.reply.text, DRAFT);
}

#[test]
fn even_identical_text_after_an_edit_is_not_the_submitted_model() {
    let (client, mut rt) = fixture();
    let id = pending(&mut rt, DRAFT);
    rt.update(|d, _| {
        d.inbox.reply.insert("!");
        d.inbox.reply.key(&crate::keys::plain(Key::Backspace), None);
    });
    assert_eq!(rt.state.inbox.reply.text, DRAFT);
    delivered(&client, &mut rt, id);
    assert_eq!(rt.state.inbox.reply.text, DRAFT);
}

#[test]
fn a_machine_switch_retires_old_answer_receipts() {
    let (client, mut rt) = fixture();
    let id = pending(&mut rt, DRAFT);
    rt.update(|d, _| d.replace_connection(crate::desk::link::Replacement::Machine));
    delivered(&client, &mut rt, id);
    assert!(rt.state.inbox.reply.text.is_empty());
    assert!(rt.state.inbox.answers.is_empty());
    assert!(rt.state.notice.is_none());
}

#[test]
fn inline_answers_for_another_terminal_do_not_own_the_selected_draft() {
    let (client, mut rt) = fixture();
    let terminal = ResourceId::local(99);
    rt.update(|d, _| d.answer(&terminal, "other", "yes"));
    let id = *rt.state.inbox.answers.keys().next().unwrap();
    delivered(&client, &mut rt, id);
    assert_eq!(rt.state.inbox.reply.text, DRAFT);
}

#[test]
fn returning_to_a_row_with_identical_text_starts_a_new_draft() {
    let (client, mut rt) = fixture();
    let id = pending(&mut rt, DRAFT);
    rt.update(|d, tx| {
        d.inbox.approvals.push(Approval {
            id: "0123456789abcdef0123456789abcdef".into(),
            requester: phux_client_runtime::approvals::Requester {
                client: 7,
                credential_id: None,
                client_name: None,
            },
            method: "SIGNAL_TERMINAL".into(),
            signal: Some("TERM".into()),
            subjects: vec!["terminal:worker".into()],
            credential: None,
            requested_at_ms: 0,
            expires_at_ms: u64::MAX,
        });
        assert!(matches!(d.inbox_items()[0], Item::Approval(_)));
        d.pick_item(0, tx);
        d.pick_item(1, tx);
        d.inbox.reply.insert(DRAFT);
    });
    delivered(&client, &mut rt, id);
    assert_eq!(rt.state.inbox.reply.text, DRAFT);
}

#[test]
fn a_successful_manual_retry_replaces_the_previous_failure_feedback() {
    let (client, mut rt) = fixture();
    let id = pending(&mut rt, DRAFT);
    rt.update(|d, _| {
        d.inbox_event(
            &client,
            &Event::InputDelivery {
                delivery_id: id,
                outcome: DeliveryOutcome::Refused,
                code: None,
                message: "Try again after reading the pane.".into(),
            },
        );
    });
    assert!(crate::test_support::notice(&rt.state).starts_with("Answer not sent"));
    assert_eq!(rt.state.inbox.reply.text, DRAFT);
    let retry = pending(&mut rt, DRAFT);
    delivered(&client, &mut rt, retry);
    assert_eq!(crate::test_support::notice(&rt.state), "Answer sent.");
    assert!(rt.state.inbox.reply.text.is_empty());
}
