//! The inbox drawer: its empty state, its answer chips and its rules, and
//! populated full-window cases, not an empty drawer over a shell prompt.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use phux_client_runtime::Client;
use phux_client_runtime::approvals::{Approval, Requester};
use phux_client_runtime::control::Event;
use rune::{Key, Runtime};

use crate::desk::Desk;

const QUESTION: &str = "release-review";
const WORDS: &str = "The arm64 build is ready. Should I run the native input checks before publishing the phone update?";
const ANSWERS: [&str; 3] = [
    "Run the native checks",
    "Let me review the changes before publishing",
    "Wait",
];
const APPROVAL_ID: &str = "0123456789abcdef0123456789abcdef";

#[derive(Clone, Copy)]
enum Scene {
    Question,
    Reply,
    Approval,
    Empty,
    Scrolled,
    NativeReply,
    Inline,
    InlineNoChoices,
}

impl Scene {
    fn name(self) -> &'static str {
        match self {
            Self::Question => "question",
            Self::Reply => "reply",
            Self::Approval => "approval",
            Self::Empty => "empty",
            Self::Scrolled => "scrolled",
            Self::NativeReply => "native-reply",
            Self::Inline => "inline",
            Self::InlineNoChoices => "inline-no-choices",
        }
    }

    fn populate(self, desk: &mut Desk) {
        match self {
            Self::Empty => {}
            Self::Approval | Self::Scrolled => {
                question(desk, &ANSWERS);
                desk.inbox.approvals.push(approval());
                if matches!(self, Self::Scrolled) {
                    for index in 1..6 {
                        let mut approval = approval();
                        approval.id = format!("{index:032x}");
                        desk.inbox.approvals.push(approval);
                    }
                }
            }
            Self::Question | Self::Reply | Self::NativeReply | Self::Inline => {
                question(desk, &ANSWERS)
            }
            Self::InlineNoChoices => question(desk, &[]),
        }
        if matches!(self, Self::Inline | Self::InlineNoChoices) {
            desk.phone.as_mut().unwrap().keyboard = matches!(self, Self::Inline);
            return;
        }
        if matches!(self, Self::NativeReply) {
            desk.native = Some(crate::mobile::Platform::test());
        }
        desk.toggle_inbox();
        desk.inbox.reply.focused = false;
        if let Some(phone) = desk.phone.as_mut() {
            phone.keyboard = matches!(self, Self::Reply);
        }
    }

    fn check(self, rt: &mut Runtime<Desk>) {
        match self {
            Self::Reply | Self::NativeReply => {
                assert!(rt.accessibility_action(
                    "inbox-reply",
                    rune::accesskit::Action::Focus,
                    None
                ));
                rt.key(crate::keys::plain(Key::Text(
                    "Please keep the existing pairing and run the checks.".into(),
                )));
                settle(rt);
                assert!(rt.state.inbox.reply.text.contains("existing pairing"));
            }
            Self::Scrolled => {
                let key = format!(
                    "chip-{}-Wait",
                    rt.state.connection.shown.as_ref().unwrap().resource
                );
                rt.update(|_, tx| tx.scroll_into_view(&key));
                settle(rt);
                let [_, y, _, h] = rt.presented(&key).unwrap().0;
                assert!(y >= 0.0 && y + h <= rt.viewport.1);
                assert_eq!(rt.state.inbox_items().len(), 7);
            }
            Self::Inline | Self::InlineNoChoices => {
                let id = rt.state.connection.shown.as_ref().unwrap().resource.clone();
                assert!(
                    rt.find(&format!("ask-{id}")).is_some(),
                    "actual pane owns the inline question"
                );
            }
            Self::Question => {
                assert!(
                    rt.elements()
                        .iter()
                        .any(|e| e.text.as_deref() == Some(WORDS))
                );
                assert_eq!(rt.state.inbox_items().len(), 1);
            }
            Self::Approval => {
                assert_eq!(rt.state.inbox_items().len(), 2);
                assert!(
                    rt.accessibility_json()
                        .to_string()
                        .contains("Signal terminal")
                );
            }
            Self::Empty => {
                assert!(rt.state.inbox_items().is_empty());
                assert!(
                    rt.accessibility_json()
                        .to_string()
                        .contains("Nothing needs you")
                );
            }
        }
    }
}

fn fixture(size: (u32, u32), phone: bool, scale: f32) -> (Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = crate::test_support::input_fixture_showing(
        "\x1b[2J\x1b[HRelease verification\r\n  tests: passed\r\n  native input: waiting for review\r\n",
    );
    rt.text = rune::text::TextSystem::new(crate::grid::FONTS, crate::grid::FAMILIES);
    rt.state.phone = phone.then(crate::desk::phone::Phone::default);
    rt.state.connection.topology = client.with_control(|c| c.topology().cloned());
    rt.set_viewport((size.0 as f32, size.1 as f32), scale);
    rt.set_system_reduced_motion(true);
    (client, rt)
}

fn question(desk: &mut Desk, suggestions: &[&str]) {
    let terminal_id = desk.connection.shown.as_ref().unwrap().resource.clone();
    desk.activities.apply(
        &Event::AgentMetadata {
            terminal_id: terminal_id.clone(),
            value: Some(
                r#"{"name":"Claude — native release verification","state":"blocked"}"#
                    .as_bytes()
                    .to_vec(),
            ),
        },
        &[],
    );
    desk.activities.apply(
        &Event::AgentAsked {
            terminal_id,
            question_id: QUESTION.into(),
            text: WORDS.into(),
            suggestions: suggestions.iter().map(|word| (*word).to_owned()).collect(),
            waiting_seconds: None,
        },
        &[],
    );
}

fn approval() -> Approval {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    Approval {
        id: APPROVAL_ID.into(),
        requester: Requester {
            client: 7,
            credential_id: None,
            client_name: Some("Release verification worker".into()),
        },
        method: "SIGNAL_TERMINAL".into(),
        signal: Some("TERM".into()),
        subjects: vec!["terminal:release-verification-arm64-worker".into()],
        credential: None,
        requested_at_ms: now,
        // Created anew for every frame case; remains "5m" during its export.
        expires_at_ms: now + 330_000,
    }
}

fn settle(rt: &mut Runtime<Desk>) {
    rt.settle_all();
    rt.frame(1.0);
    // The isolated fixture has no app frame hook. Rebuild after the real
    // pane/stage layout callbacks, as the application's next tick does.
    rt.update(|_, _| {});
    rt.frame(1.0);
    // Stage geometry mounts the pane; its own layout then sizes the card.
    rt.update(|_, _| {});
    rt.frame(1.0);
}

fn prepare(rt: &mut Runtime<Desk>, scene: Scene, light: bool) {
    rt.update(|d, tx| {
        d.settings.theme = crate::theme::find(if light {
            "catppuccin-latte"
        } else {
            "catppuccin-mocha"
        });
        scene.populate(d);
        tx.instant();
    });
    settle(rt);
    scene.check(rt);
}

fn render_case(
    gpu: &rune::render::Gpu,
    phone: bool,
    size: (u32, u32),
    scale: u32,
    light: bool,
    scene: Scene,
) {
    let (_, mut rt) = fixture(size, phone, scale as f32);
    prepare(&mut rt, scene, light);
    let mut renderer = rune::render::Renderer::new(&gpu.device, rune::render::OFFSCREEN_FORMAT);
    let pixels = (size.0 * scale, size.1 * scale);
    let rgba = crate::ui::offscreen(gpu, &mut renderer, &mut rt, pixels);
    assert_eq!(rgba.len(), (pixels.0 * pixels.1 * 4) as usize);
    if let Some(directory) = std::env::var_os("RUNE_INBOX_SNAP") {
        let theme = if light { "light" } else { "dark" };
        let form = if phone { "phone" } else { "desktop" };
        let name = format!(
            "{form}-{}-{theme}-{}x{}-{scale}x.png",
            scene.name(),
            size.0,
            size.1
        );
        crate::test_support::visual::write(
            &std::path::PathBuf::from(directory).join(name),
            &rt,
            pixels,
            &rgba,
            scene.name(),
        );
    }
}

#[test]
fn phone_inbox_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for (phone, size, scale) in [
        (false, (1100, 720), 1),
        (false, (640, 360), 1),
        (true, (393, 852), 1),
        (true, (320, 568), 1),
        (true, (393, 852), 2),
        (true, (320, 568), 2),
    ] {
        for light in [false, true] {
            for scene in [
                Scene::Question,
                Scene::Reply,
                Scene::Approval,
                Scene::Empty,
                Scene::Scrolled,
            ] {
                render_case(&gpu, phone, size, scale, light, scene);
            }
        }
    }
    for size in [(393, 420), (320, 280)] {
        for light in [false, true] {
            render_case(&gpu, true, size, 1, light, Scene::NativeReply);
        }
    }
    for size in [(393, 852), (320, 568)] {
        for light in [false, true] {
            for scene in [Scene::Inline, Scene::InlineNoChoices] {
                render_case(&gpu, true, size, 1, light, scene);
            }
        }
    }
}

fn reveal(rt: &mut Runtime<Desk>, key: &str) -> [f32; 4] {
    rt.update(|_, tx| tx.scroll_into_view(key));
    settle(rt);
    let element = rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key))
        .unwrap();
    let [x, y, w, h] = element.visible;
    assert!(w >= 44.0 && h >= 44.0, "{key}: visible {w}×{h}");
    assert!(x >= 0.0 && x + w <= rt.viewport.0 + 0.5, "{key} fits width");
    assert!(
        y >= 0.0 && y + h <= rt.viewport.1 + 0.5,
        "{key} fits height"
    );
    element.rect
}

fn no_actions(client: &Client) {
    use phux_protocol::wire::frame::{Command, FrameKind};
    for bytes in client.take_outbound() {
        let frame = FrameKind::decode(&bytes).unwrap().0;
        assert!(
            !matches!(
                frame,
                FrameKind::InputKey { .. }
                    | FrameKind::InputPaste { .. }
                    | FrameKind::SetMetadata { .. }
                    | FrameKind::Command {
                        command: Command::ApplyInput { .. },
                        ..
                    }
            ),
            "an Inbox gesture sent an unintended action: {frame:?}"
        );
    }
}

fn touch(rt: &mut Runtime<Desk>, key: &str, finish: rune::TouchPhase) {
    let (x, y) = rt.find(key).unwrap();
    rt.touch(1, rune::TouchPhase::Started, x, y, 0.1);
    rt.frame(1.0 / 60.0);
    rt.frame(1.0 / 60.0);
    rt.touch(1, finish, x, y, 0.15);
    settle(rt);
}

#[test]
fn phone_inbox_controls_are_reachable_touch_targets_and_keep_safe_dismissal() {
    for size in [(393, 852), (320, 568)] {
        let (client, mut rt) = fixture(size, true, 1.0);
        prepare(&mut rt, Scene::Reply, false);
        let editor = rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some("inbox-reply"))
            .unwrap();
        assert!(
            editor.visible[3] >= 44.0,
            "editing reveals the whole phone input: {editor:?}"
        );
        assert!(
            editor.rect[0] + editor.rect[2] <= size.0 as f32,
            "long drafts fit horizontally"
        );
        let terminal = rt.state.connection.shown.as_ref().unwrap().resource.clone();
        for answer in ANSWERS {
            let key = format!("chip-{terminal}-{answer}");
            reveal(&mut rt, &key);
            touch(&mut rt, &key, rune::TouchPhase::Cancelled);
            assert!(rt.state.inbox.answers.is_empty(), "cancel cannot answer");
        }
        reveal(&mut rt, "inbox-reply");
        reveal(&mut rt, "inbox-close");
        no_actions(&client);
        touch(&mut rt, "inbox-close", rune::TouchPhase::Cancelled);
        assert!(rt.state.inbox.open);
        assert!(!rt.state.inbox.reply.text.is_empty());
        touch(&mut rt, "inbox-close", rune::TouchPhase::Ended);
        assert!(!rt.state.inbox.open);
        assert!(
            rt.state.inbox.reply.text.is_empty(),
            "same draft semantics as Escape"
        );
        assert!(!rt.state.inbox.reply.focused);
        no_actions(&client);
    }
}

#[test]
fn phone_approval_cancel_and_plain_return_never_decide() {
    for size in [(393, 852), (320, 568)] {
        let (client, mut rt) = fixture(size, true, 1.0);
        prepare(&mut rt, Scene::Approval, true);
        for label in ["Deny", "Approve"] {
            let key = format!("decide-{label}-{APPROVAL_ID}");
            reveal(&mut rt, &key);
            touch(&mut rt, &key, rune::TouchPhase::Cancelled);
        }
        rt.key(crate::keys::plain(Key::Enter));
        settle(&mut rt);
        assert!(
            rt.state.notice.is_none(),
            "no unsupported decision was attempted"
        );
        no_actions(&client);
    }
}

#[test]
fn completed_phone_touches_use_the_existing_answer_and_decision_protocols() {
    use phux_protocol::ids::ApprovalId;
    use phux_protocol::wire::frame::{AgentEvent, Command, FrameKind};

    let (client, mut rt) = fixture((320, 568), true, 1.0);
    prepare(&mut rt, Scene::Question, false);
    let terminal = rt.state.connection.shown.as_ref().unwrap().resource.clone();
    client
        .feed(FrameKind::Event {
            terminal: Some(terminal.clone()),
            event: AgentEvent::Asked {
                id: QUESTION.into(),
                question: WORDS.into(),
                suggestions: ANSWERS.iter().map(|word| (*word).to_owned()).collect(),
                elapsed_seconds: None,
            },
            stamp: None,
        })
        .unwrap();
    no_actions(&client);
    let key = format!("chip-{terminal}-{}", ANSWERS[0]);
    reveal(&mut rt, &key);
    touch(&mut rt, &key, rune::TouchPhase::Ended);
    let frames: Vec<_> = client
        .take_outbound()
        .iter()
        .map(|bytes| FrameKind::decode(bytes).unwrap().0)
        .collect();
    assert_eq!(
        frames
            .iter()
            .filter(|frame| matches!(
                frame,
                FrameKind::Command {
                    command: Command::ApplyInput { .. },
                    ..
                }
            ))
            .count(),
        1,
        "a completed answer uses acknowledged input exactly once"
    );

    let (client, mut rt) = fixture((320, 568), true, 1.0);
    prepare(&mut rt, Scene::Approval, false);
    let key = format!("decide-Approve-{APPROVAL_ID}");
    reveal(&mut rt, &key);
    no_actions(&client);
    touch(&mut rt, &key, rune::TouchPhase::Ended);
    let decide_key = ApprovalId::parse(APPROVAL_ID).unwrap().decide_key();
    let frames: Vec<_> = client
        .take_outbound()
        .iter()
        .map(|bytes| FrameKind::decode(bytes).unwrap().0)
        .collect();
    assert_eq!(
        frames
            .iter()
            .filter(|frame| matches!(frame,
                FrameKind::SetMetadata { key, .. } if key == &decide_key
            ))
            .count(),
        1,
        "a completed approval uses the held-action decision gate"
    );
}

#[test]
fn inline_phone_answers_scroll_within_the_pane_and_no_choices_has_an_honest_hint() {
    let size = (320, 568);
    let (client, mut rt) = fixture(size, true, 1.0);
    prepare(&mut rt, Scene::Inline, false);
    let terminal = rt.state.connection.shown.as_ref().unwrap().resource.clone();
    let key = format!("chip-{terminal}-Wait");
    let card = crate::ui::element(&rt, &format!("ask-{terminal}"));
    assert_eq!(card.role, "group");
    assert!(
        !card.focusable,
        "the scroll boundary is not a phantom button"
    );
    let before = rt.presented(&key).unwrap().0[1];
    let question = rt
        .elements()
        .into_iter()
        .find(|e| e.text.as_deref() == Some(WORDS))
        .unwrap();
    let [x, y, w, h] = question.visible;
    assert!(h > 0.0, "the swipe starts on actual question text");
    let start = (x + w / 2.0, y + h / 2.0);
    rt.touch(8, rune::TouchPhase::Started, start.0, start.1, 0.1);
    rt.frame(1.0 / 60.0);
    rt.touch(8, rune::TouchPhase::Moved, start.0, start.1 - 100.0, 0.2);
    rt.frame(1.0 / 60.0);
    rt.touch(8, rune::TouchPhase::Ended, start.0, start.1 - 100.0, 0.25);
    settle(&mut rt);
    let after = rt.presented(&key).unwrap().0[1];
    assert!(
        after < before - 20.0,
        "text swipe scrolls the card, not the terminal: {before} → {after}"
    );
    reveal(&mut rt, &key);
    touch(&mut rt, &key, rune::TouchPhase::Cancelled);
    no_actions(&client);
    assert!(!rt.state.inbox.open);

    let (_, mut rt) = fixture(size, true, 1.0);
    prepare(&mut rt, Scene::InlineNoChoices, false);
    let tree = rt.accessibility_json().to_string();
    assert!(tree.contains("Reply in Inbox"));
    assert!(!tree.contains("Tap an answer"));
}

fn asked(rt: &mut Runtime<Desk>, suggestions: &[&str]) {
    let terminal_id = rt.state.connection.shown.as_ref().unwrap().resource.clone();
    rt.state.connection.topology = rt
        .state
        .connection
        .client
        .as_ref()
        .and_then(|c| c.topology());
    rt.state.activities.apply(
        &Event::AgentAsked {
            terminal_id,
            question_id: "q-1".into(),
            text: "Continue?".into(),
            suggestions: suggestions.iter().map(|s| (*s).to_owned()).collect(),
            waiting_seconds: None,
        },
        &[],
    );
}

#[test]
fn an_empty_inbox_says_nothing_needs_you_under_the_done_mark() {
    let (_, mut rt) = crate::test_support::input_fixture();
    rt.update(|d, _| d.toggle_inbox());
    rt.frame(0.0);
    let text = rt.accessibility_json().to_string();
    assert!(text.contains("Nothing needs you"));
    for key in [
        "inbox-header-rule",
        "inbox-footer-rule",
        "inbox",
        "inbox-scrim",
    ] {
        assert!(rt.find(key).is_some(), "{key}");
    }
}

#[test]
fn suggested_answers_are_named_buttons() {
    let (_, mut rt) = crate::test_support::input_fixture();
    rt.frame(0.0);
    asked(&mut rt, &["yes", "no"]);
    rt.update(|d, _| d.toggle_inbox());
    rt.frame(0.0);
    let chips: Vec<_> = (rt.elements().into_iter())
        .filter(|e| e.key.as_deref().is_some_and(|k| k.starts_with("chip-")))
        .collect();
    assert_eq!(chips.len(), 2, "one chip per answer");
    for chip in &chips {
        assert_eq!(chip.role, "button");
        assert!(chip.focusable, "a chip is a Tab stop");
        assert!(matches!(chip.label.as_deref(), Some("yes" | "no")));
    }
}
