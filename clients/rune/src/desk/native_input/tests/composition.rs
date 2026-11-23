//! Native preedit retirement is lifecycle cleanup, not an enabled editor write.
use super::*;
use crate::desk::questions::{Owner, SUBMITTING};
use phux_protocol::ids::ResourceId;

#[derive(Clone, Copy, Debug)]
enum Hold {
    Submitting,
    Disconnected,
    Unsupported,
}

fn fixture() -> (
    std::sync::Arc<phux_client_runtime::Client>,
    Runtime<Desk>,
    Bridge,
    Owner,
) {
    let (client, mut rt) = crate::test_support::input_fixture();
    native(&mut rt.state);
    rt.state.connection.topology = client.with_control(|c| c.topology().cloned());
    let terminal = rt.state.connection.shown.as_ref().unwrap().resource.clone();
    let question = acp_question("acp-question:composition", "Editor", "Draft");
    rt.state.inbox.questions.views.insert(
        terminal.clone(),
        (ResourceId::local(55), acp_view("active", &question)),
    );
    rt.state.inbox.questions.reconcile();
    let owner = rt
        .state
        .inbox
        .questions
        .owner(&terminal, &question.id)
        .unwrap()
        .clone();
    rt.state
        .inbox
        .questions
        .text_mut(&owner, "text")
        .replace_text("preserved");
    rt.set_viewport((393.0, 852.0), 2.0);
    for _ in 0..120 {
        rt.update(|_, tx| tx.instant());
        rt.frame(1.0 / 60.0);
    }
    focus(&mut rt, &owner);
    let mut bridge = Bridge::default();
    preedit(&mut bridge, &mut rt);
    assert_eq!(
        rt.state.inbox.questions.draft(&owner).unwrap().text("text"),
        "preserved"
    );
    assert!(rt.state.inbox.questions.draft(&owner).unwrap().texts["text"].is_composing());
    let _ = client.take_outbound();
    (client, rt, bridge, owner)
}

fn focus(rt: &mut Runtime<Desk>, owner: &Owner) {
    assert!(rt.accessibility_action(&owner.field_key("text"), Accessibility::Focus, None));
    rt.frame(0.0);
}

fn preedit(bridge: &mut Bridge, rt: &mut Runtime<Desk>) {
    let snapshot = bridge.snapshot(rt);
    assert_eq!(snapshot.kind, "field");
    assert!(
        submit(
            bridge,
            rt,
            &snapshot,
            Action::Preedit {
                text: "日本".into(),
                anchor: 0,
                focus: 2,
            }
        )
        .unwrap()
    );
}

fn hold(d: &mut Desk, owner: &Owner, reason: Hold) {
    let questions = &mut d.inbox.questions;
    match reason {
        Hold::Submitting => {
            questions.pending.insert(77, owner.clone());
            questions.status.insert(owner.clone(), SUBMITTING.into());
        }
        Hold::Disconnected => {
            questions
                .views
                .get_mut(owner.terminal())
                .unwrap()
                .1
                .connected = false
        }
        Hold::Unsupported => {
            questions
                .views
                .get_mut(owner.terminal())
                .unwrap()
                .1
                .capabilities
                .questions = false.into()
        }
    }
}

fn restore(d: &mut Desk, owner: &Owner, reason: Hold) {
    let questions = &mut d.inbox.questions;
    match reason {
        Hold::Submitting => assert!(questions.receipt(
            77,
            phux_protocol::native_agent::NativeDisposition::Refused,
            None
        )),
        Hold::Disconnected => {
            questions
                .views
                .get_mut(owner.terminal())
                .unwrap()
                .1
                .connected = true
        }
        Hold::Unsupported => {
            questions
                .views
                .get_mut(owner.terminal())
                .unwrap()
                .1
                .capabilities
                .questions = true.into()
        }
    }
}

fn rejected(bridge: &mut Bridge, rt: &mut Runtime<Desk>, snapshot: &Snapshot) {
    assert!(
        !submit(
            bridge,
            rt,
            snapshot,
            Action::Replace {
                expected: snapshot.text.clone(),
                text: "stale".into(),
                anchor: 5,
                focus: 5,
            }
        )
        .unwrap()
    );
}

fn unchanged(rt: &Runtime<Desk>, owner: &Owner) {
    let draft = rt.state.inbox.questions.draft(owner).unwrap();
    assert_eq!(draft.text("text"), "preserved");
    assert!(!draft.texts["text"].is_composing());
}

fn no_traffic(client: &phux_client_runtime::Client) {
    use phux_protocol::wire::frame::{Command, FrameKind};
    for bytes in client.take_outbound() {
        assert!(
            !matches!(
                FrameKind::decode(&bytes).unwrap().0,
                FrameKind::InputKey { .. }
                    | FrameKind::InputPaste { .. }
                    | FrameKind::Command {
                        command: Command::ApplyInput { .. } | Command::NativeAgent { .. },
                        ..
                    }
            ),
            "native retirement must not submit or type into the terminal"
        );
    }
}

#[test]
fn held_native_questions_retire_preedit_without_editing_or_replaying_it() {
    for reason in [Hold::Submitting, Hold::Disconnected, Hold::Unsupported] {
        let (client, mut rt, mut bridge, owner) = fixture();
        let old = bridge.snapshot(&rt);
        hold(&mut rt.state, &owner, reason);
        assert!(rt.state.inbox.questions.field_mut(&owner, "text").is_none());
        assert!(bridge.reconcile(&mut rt));
        unchanged(&rt, &owner);
        rejected(&mut bridge, &mut rt, &old);
        unchanged(&rt, &owner);
        restore(&mut rt.state, &owner, reason);
        rt.update(|_, tx| tx.instant());
        rt.frame(0.0);
        focus(&mut rt, &owner);
        unchanged(&rt, &owner);
        rejected(&mut bridge, &mut rt, &old);
        unchanged(&rt, &owner);
        no_traffic(&client);
    }
}

#[test]
fn dismissing_a_held_captured_field_can_cancel_preedit_but_not_edit_it() {
    let (client, mut rt, bridge, owner) = fixture();
    let target = bridge.target.clone().unwrap();
    hold(&mut rt.state, &owner, Hold::Disconnected);
    dismiss(&mut rt, &target);
    unchanged(&rt, &owner);
    assert!(rt.state.inbox.questions.field_mut(&owner, "text").is_none());
    no_traffic(&client);
}

#[test]
fn old_native_cleanup_never_cancels_a_changed_or_reopened_owners_preedit() {
    for reopen in [false, true] {
        let (client, mut rt, mut old_bridge, old) = fixture();
        let snapshot = old_bridge.snapshot(&rt);
        let original = rt
            .state
            .inbox
            .questions
            .draft(&old)
            .unwrap()
            .question
            .clone();
        let view = &mut rt
            .state
            .inbox
            .questions
            .views
            .get_mut(old.terminal())
            .unwrap()
            .1;
        if reopen {
            view.questions.clear();
            rt.state.inbox.questions.reconcile();
            rt.state
                .inbox
                .questions
                .views
                .get_mut(old.terminal())
                .unwrap()
                .1
                .questions = vec![original.clone()];
        } else {
            view.questions[0].message.push_str(" revised");
        }
        rt.state.inbox.questions.reconcile();
        let current = rt
            .state
            .inbox
            .questions
            .owner(old.terminal(), &original.id)
            .unwrap()
            .clone();
        assert_ne!(old, current);
        rt.update(|_, tx| tx.instant());
        rt.frame(0.0);
        focus(&mut rt, &current);
        let mut new_bridge = Bridge::default();
        preedit(&mut new_bridge, &mut rt);
        let before = rt
            .state
            .inbox
            .questions
            .draft(&current)
            .unwrap()
            .text("text");
        assert!(old_bridge.reconcile(&mut rt));
        old_bridge.cancel_preedit(&mut rt);
        let draft = rt.state.inbox.questions.draft(&current).unwrap();
        assert!(
            draft.texts["text"].is_composing(),
            "old lifecycle cleanup reached the replacement"
        );
        assert_eq!(draft.text("text"), before);
        assert!(!submit(&mut old_bridge, &mut rt, &snapshot, Action::Dismiss).unwrap());
        rejected(&mut old_bridge, &mut rt, &snapshot);
        assert!(rt.state.inbox.questions.draft(&current).unwrap().texts["text"].is_composing());
        no_traffic(&client);
    }
}
