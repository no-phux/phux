//! Delayed receipts through the real acknowledged-input wire path.
use phux_client_runtime::Client;
use phux_protocol::wire::frame::{AgentEvent, Command, CommandResult, ErrorCode, FrameKind};
use rune::{Key, Runtime};

use super::{Desk, QUESTION, Scene, WORDS, settle};

pub(super) const DRAFT: &str = "Keep the pairing and run the checks.";
pub(super) const NEXT: &str = "Review the next build first.";

fn ask(rt: &mut Runtime<Desk>, id: &str) {
    let client = rt.state.connection.client.as_ref().unwrap().clone();
    let terminal = rt.state.connection.shown.as_ref().unwrap().resource.clone();
    client
        .feed(FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::Asked {
                id: id.into(),
                question: WORDS.into(),
                suggestions: vec![],
                elapsed_seconds: None,
            },
            stamp: None,
        })
        .unwrap();
    poll(rt);
}

fn poll(rt: &mut Runtime<Desk>) {
    rt.update(|d, tx| {
        d.poll(0.0, tx);
    });
    settle(rt);
}

fn type_reply(rt: &mut Runtime<Desk>, words: &str) {
    assert!(rt.accessibility_action("inbox-reply", rune::accesskit::Action::Focus, None));
    rt.key(crate::keys::plain(Key::Text(words.into())));
    settle(rt);
}

fn send_touch(
    rt: &mut Runtime<Desk>,
    bridge: &mut crate::desk::native_input::Bridge,
    finish: rune::TouchPhase,
) {
    let before = bridge.snapshot(rt);
    let (x, y) = rt.find("inbox-send").unwrap();
    rt.touch(1, rune::TouchPhase::Started, x, y, 0.1);
    rt.frame(1.0 / 60.0);
    rt.frame(1.0 / 60.0);
    bridge.reconcile(rt);
    let held = bridge.snapshot(rt);
    assert_eq!(
        held.kind, "field",
        "touch-down must not resign the native editor"
    );
    assert!(held.wanted);
    assert_eq!(held.token, before.token);
    assert!(crate::ui::element(rt, "inbox-reply").focused);
    rt.touch(1, finish, x, y, 0.15);
    settle(rt);
    bridge.reconcile(rt);
    let released = bridge.snapshot(rt);
    assert_eq!(released.kind, "field", "release/cancel keeps the keyboard");
    assert!(released.wanted);
    assert_eq!(released.token, before.token);
}

fn send_accessibility(rt: &Runtime<Desk>) -> serde_json::Value {
    fn find(value: &serde_json::Value) -> Option<&serde_json::Value> {
        match value {
            serde_json::Value::Object(fields)
                if fields.get("key").is_some_and(|key| key == "inbox-send") =>
            {
                Some(value)
            }
            serde_json::Value::Object(fields) => fields.values().find_map(find),
            serde_json::Value::Array(values) => values.iter().find_map(find),
            _ => None,
        }
    }
    find(&rt.accessibility_json()).unwrap().clone()
}

fn assert_send_disabled(rt: &mut Runtime<Desk>, client: &Client) {
    let node = send_accessibility(rt);
    assert!(
        node["states"]
            .as_array()
            .unwrap()
            .iter()
            .any(|state| state == "disabled")
    );
    assert!(node["actions"].is_null());
    let before = (
        rt.state.inbox.answers.len(),
        rt.state.inbox.reply.clone(),
        rt.state.inbox.open,
    );
    // Rune may queue a forced request even though the AX node offers no Click.
    // Its eventual application effect, not queue admission, must be inert.
    let _ = rt.accessibility_action("inbox-send", rune::accesskit::Action::Click, None);
    settle(rt);
    assert_eq!(
        (
            rt.state.inbox.answers.len(),
            rt.state.inbox.reply.clone(),
            rt.state.inbox.open
        ),
        before
    );
    super::no_actions(client);
}

pub(super) fn prepare(rt: &mut Runtime<Desk>, scene: Scene) {
    ask(rt, QUESTION);
    type_reply(rt, DRAFT);
    let client = rt.state.connection.client.as_ref().unwrap().clone();
    let _ = client.take_outbound();
    rt.key(crate::keys::plain(Key::Enter));
    settle(rt);
    let requests: Vec<_> = client
        .take_outbound()
        .iter()
        .filter_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
            FrameKind::Command {
                request_id,
                command: Command::ApplyInput { .. },
            } => Some(request_id),
            FrameKind::ResizeTerminal { .. } => None,
            other => panic!("unexpected answer traffic: {other:?}"),
        })
        .collect();
    let [request] = requests.as_slice() else {
        panic!("one acknowledged answer: {requests:?}")
    };
    assert_eq!(rt.state.inbox.answers.len(), 1);
    if matches!(scene, Scene::Sending) {
        return;
    }
    if matches!(scene, Scene::OtherQuestion) {
        ask(rt, "next-review");
    }
    if matches!(scene, Scene::OtherQuestion | Scene::NewDraft) {
        rt.key(crate::keys::plain(Key::Command('a')));
        type_reply(rt, NEXT);
        assert_eq!(rt.state.inbox.reply.text, NEXT);
    }
    let result = match scene {
        Scene::Refused => CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message: "The question has changed.".into(),
        },
        Scene::Unknown => CommandResult::Error {
            code: ErrorCode::InputDeliveryUnknown,
            message: "Delivery could not be confirmed.".into(),
        },
        _ => CommandResult::Ok,
    };
    client
        .feed(FrameKind::CommandResult {
            request_id: *request,
            result,
        })
        .unwrap();
    poll(rt);
    assert!(
        rt.state.inbox.answers.is_empty(),
        "receipt ends the pending state"
    );
}

#[test]
fn answer_receipts_retire_only_the_submitted_untouched_draft() {
    for phone in [false, true] {
        for scene in [
            Scene::Delivered,
            Scene::Refused,
            Scene::Unknown,
            Scene::NewDraft,
            Scene::OtherQuestion,
        ] {
            let (client, mut rt) = super::fixture((393, 852), phone, 1.0);
            super::prepare(&mut rt, scene, false);
            let expected = match scene {
                Scene::Delivered => "",
                Scene::NewDraft | Scene::OtherQuestion => NEXT,
                _ => DRAFT,
            };
            assert_eq!(rt.state.inbox.reply.text, expected, "{}", scene.name());
            let notice = crate::test_support::notice(&rt.state);
            match scene {
                Scene::Refused => assert!(notice.starts_with("Answer not sent")),
                Scene::Unknown => assert!(notice.contains("read the pane first")),
                _ => assert!(notice.starts_with("Answer sent")),
            }
            super::no_actions(&client);
        }
    }
}

#[test]
fn phone_send_is_named_reachable_and_sends_once_without_return() {
    for (size, native) in [
        ((393, 852), false),
        ((320, 568), false),
        ((393, 420), true),
        ((320, 280), true),
    ] {
        let (client, mut rt) = super::fixture(size, true, 1.0);
        rt.state.native = native.then(crate::mobile::Platform::test);
        super::prepare(&mut rt, Scene::Question, false);
        ask(&mut rt, QUESTION);
        let button = crate::ui::element(&rt, "inbox-send");
        assert_eq!(button.role, "button");
        assert_send_disabled(&mut rt, &client);
        super::touch(&mut rt, "inbox-send", rune::TouchPhase::Ended);
        assert!(
            rt.state.inbox.open,
            "an empty Send cannot open the terminal"
        );
        assert!(rt.state.inbox.answers.is_empty());
        super::no_actions(&client);
        type_reply(&mut rt, DRAFT);
        super::reveal(&mut rt, "inbox-send");
        assert!(rt.accessibility_json().to_string().contains("Send reply"));
        let mut bridge = crate::desk::native_input::Bridge::default();
        let snapshot = bridge.snapshot(&rt);
        let _ = client.take_outbound();
        send_touch(&mut rt, &mut bridge, rune::TouchPhase::Cancelled);
        super::no_actions(&client);
        send_touch(&mut rt, &mut bridge, rune::TouchPhase::Ended);
        assert_eq!(rt.state.inbox.answers.len(), 1);
        assert!(
            crate::ui::element(&rt, "inbox-reply").focused,
            "Send keeps editing focus instead of focusing Close"
        );
        if native {
            bridge.reconcile(&mut rt);
            let pending = bridge.snapshot(&rt);
            assert_eq!(pending.kind, "field");
            assert!(pending.wanted);
            assert_eq!(pending.token, snapshot.token);
        }
        let requests: Vec<_> = client
            .take_outbound()
            .iter()
            .filter_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
                FrameKind::Command {
                    request_id,
                    command: Command::ApplyInput { .. },
                } => Some(request_id),
                FrameKind::ResizeTerminal { .. } => None,
                other => panic!("unexpected send traffic: {other:?}"),
            })
            .collect();
        let [request] = requests.as_slice() else {
            panic!("one send: {requests:?}")
        };
        assert_send_disabled(&mut rt, &client);
        rt.key(crate::keys::plain(Key::Enter));
        // Rune runs field callbacks on its frame. Deliver this pending key
        // before feeding the later receipt, not after the receipt cleared it.
        settle(&mut rt);
        assert!(
            rt.state.inbox.open,
            "pending Return must not dismiss: {:?}, draft={:?}",
            rt.state.inbox_items(),
            rt.state.inbox.reply.text
        );
        super::no_actions(&client);
        client
            .feed(FrameKind::CommandResult {
                request_id: *request,
                result: CommandResult::Ok,
            })
            .unwrap();
        poll(&mut rt);
        assert!(
            rt.state.inbox.reply.text.is_empty(),
            "touch submission clears its own draft"
        );
        assert!(rt.state.inbox.open);
        assert!(crate::test_support::notice(&rt.state).contains("Answer sent"));
        if native {
            bridge.reconcile(&mut rt);
            let after = bridge.snapshot(&rt);
            assert_eq!(after.kind, "field");
            assert!(after.revision > snapshot.revision);
            assert!(
                !bridge
                    .apply(
                        &mut rt,
                        crate::desk::native_input::Operation {
                            token: snapshot.token,
                            revision: snapshot.revision,
                            action: crate::desk::native_input::Action::Paste {
                                text: "stale".into()
                            },
                        }
                    )
                    .unwrap(),
                "a pre-receipt edit snapshot cannot type after delivery"
            );
            assert!(rt.state.inbox.reply.text.is_empty());
            super::no_actions(&client);
        }
    }
}

#[test]
fn accessible_send_works_without_taking_the_native_editor() {
    for native in [false, true] {
        let (client, mut rt) = super::fixture((320, 568), true, 1.0);
        rt.state.native = native.then(crate::mobile::Platform::test);
        super::prepare(&mut rt, Scene::Question, false);
        ask(&mut rt, QUESTION);
        assert_send_disabled(&mut rt, &client);
        type_reply(&mut rt, DRAFT);
        let mut bridge = crate::desk::native_input::Bridge::default();
        let before = bridge.snapshot(&rt);
        let node = send_accessibility(&rt);
        assert_eq!(node["name"], "Send reply");
        assert_eq!(node["role"], "button");
        assert!(node["states"].is_null());
        assert!(
            node["actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|action| action == "click")
        );
        let _ = client.take_outbound();
        assert!(rt.accessibility_action("inbox-send", rune::accesskit::Action::Click, None));
        settle(&mut rt);
        bridge.reconcile(&mut rt);
        let after = bridge.snapshot(&rt);
        assert_eq!(after.kind, "field");
        assert_eq!(after.token, before.token);
        assert!(after.wanted);
        assert_eq!(rt.state.inbox.answers.len(), 1);
        let attempts: Vec<_> = client
            .take_outbound()
            .iter()
            .filter_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
                FrameKind::Command {
                    command:
                        Command::ApplyInput {
                            terminal_id,
                            events,
                            ..
                        },
                    ..
                } => Some((terminal_id, events)),
                FrameKind::ResizeTerminal { .. } => None,
                other => panic!("unexpected accessible Send traffic: {other:?}"),
            })
            .collect();
        let [(terminal, events)] = attempts.as_slice() else {
            panic!("one accessible Send")
        };
        assert_eq!(
            terminal,
            &rt.state.connection.shown.as_ref().unwrap().resource
        );
        assert!(events.iter().any(|event| matches!(event, phux_protocol::input::InputEvent::Paste(paste) if paste.data == DRAFT.as_bytes())));
        assert_send_disabled(&mut rt, &client);
    }
}

#[test]
fn phone_keyboard_tab_keeps_question_navigation_and_return_answers_that_question() {
    use phux_protocol::ids::ResourceId;
    let replica = crate::test_support::terminal::Replica::new(120, 40, 2);
    let client = replica.client;
    let mut desk = crate::test_support::window();
    desk.phone = Some(crate::desk::phone::Phone::default());
    desk.connection.client = Some(client.clone());
    desk.connection.topology = client.topology();
    desk.connection.shown = Some(crate::desk::Shown {
        resource: replica.terminal.clone(),
        view: Some(replica.views[0]),
    });
    desk.connection.layout = Some(phux_client_core::layout::LayoutNode::Leaf(replica.terminal));
    let mut rt = Runtime::new(
        desk,
        crate::ui::view,
        (393.0, 852.0),
        rune::Env::new(true, 1.0),
    );
    rt.on_key = Some(Desk::key);
    for index in 1..=2 {
        client
            .feed(FrameKind::Event {
                terminal: Some(ResourceId::local(index)),
                event: AgentEvent::Asked {
                    id: format!("question-{index}"),
                    question: format!("Review build {index}?"),
                    suggestions: vec![],
                    elapsed_seconds: None,
                },
                stamp: None,
            })
            .unwrap();
    }
    poll(&mut rt);
    rt.update(|d, _| d.toggle_inbox());
    settle(&mut rt);
    let first = rt.state.inbox.picked.clone();
    assert!(rt.accessibility_action("inbox-reply", rune::accesskit::Action::Focus, None));
    rt.key(crate::keys::plain(Key::Tab));
    settle(&mut rt);
    assert_ne!(rt.state.inbox.picked, first);
    let Some(crate::inbox::Key::Terminal(target, Some(_))) = rt.state.inbox.picked.clone() else {
        panic!("picked question")
    };
    type_reply(&mut rt, DRAFT);
    let _ = client.take_outbound();
    rt.key(crate::keys::plain(Key::Enter));
    settle(&mut rt);
    let destinations: Vec<_> = client
        .take_outbound()
        .iter()
        .filter_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
            FrameKind::Command {
                command: Command::ApplyInput { terminal_id, .. },
                ..
            } => Some(terminal_id),
            FrameKind::ResizeTerminal { .. } => None,
            other => panic!("unexpected keyboard traffic: {other:?}"),
        })
        .collect();
    assert_eq!(destinations, [target]);
    assert_eq!(rt.state.inbox.answers.len(), 1);
}

#[test]
fn phone_receipt_confirmation_keeps_the_question_and_editor_in_place() {
    for (size, native) in [((320, 568), false), ((393, 420), true), ((320, 280), true)] {
        let (_, mut rt) = super::fixture(size, true, 1.0);
        rt.state.native = native.then(crate::mobile::Platform::test);
        super::prepare(&mut rt, Scene::Reply, false);
        let before = crate::ui::element(&rt, "inbox-scroll").rect;
        let editor = crate::ui::element(&rt, "inbox-reply").rect;
        rt.update(|d, _| {
            d.say(
                crate::desk::NoticeKind::Info,
                "Answer sent; current draft kept.",
            )
        });
        settle(&mut rt);
        assert_eq!(
            crate::ui::element(&rt, "inbox-scroll").rect,
            before,
            "confirmation does not hide the question"
        );
        assert_eq!(crate::ui::element(&rt, "inbox-reply").rect, editor);
        let status = crate::ui::element(&rt, "inbox-receipt-status");
        assert_eq!(status.role, "status");
        assert_eq!(
            rt.accessibility_json()
                .to_string()
                .matches("Answer sent; current draft kept.")
                .count(),
            1,
            "one polite confirmation, not a duplicate footer"
        );
        assert_eq!(
            status.visible, status.rect,
            "the whole confirmation fits the header"
        );
        let [_, y, _, h] = crate::ui::element(&rt, "inbox-header-rule").rect;
        assert!(status.rect[1] + status.rect[3] <= y + h);
        rt.update(|d, _| {
            d.say(
                crate::desk::NoticeKind::Error,
                "The answer may not have landed; read the pane first.",
            )
        });
        settle(&mut rt);
        assert!(
            crate::ui::element(&rt, "inbox-scroll").rect[3] < before[3],
            "errors retain their full wrapping footer"
        );
    }
}
