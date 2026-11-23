//! Currently supported Cursor asks, through the integrated Inbox and real input.
//! Controlled normalized publications, not a live provider or UIKit keyboard run.
use super::*;

const CALLBACK: &str = "acp-question:1";
const ITEM: &str = "item:review/9";
const OPTIONS: [(&str, &str); 2] = [
    ("choice:7", "Inspect the changed files"),
    ("choice:19", "Run the focused tests"),
];

#[derive(Clone, Copy)]
struct Case {
    name: &'static str,
    size: (u32, u32),
    scale: u32,
    phone: bool,
}

// Matched by scripts/visual_acp.py. Reduced 2× viewports are layout evidence,
// not a claim that this choice-only request opens a native keyboard.
const CASES: [Case; 6] = [
    Case {
        name: "desktop",
        size: (1100, 720),
        scale: 1,
        phone: false,
    },
    Case {
        name: "desktop-min",
        size: (640, 360),
        scale: 1,
        phone: false,
    },
    Case {
        name: "phone",
        size: (393, 852),
        scale: 1,
        phone: true,
    },
    Case {
        name: "small",
        size: (320, 568),
        scale: 1,
        phone: true,
    },
    Case {
        name: "native",
        size: (393, 480),
        scale: 2,
        phone: true,
    },
    Case {
        name: "native-small",
        size: (320, 340),
        scale: 2,
        phone: true,
    },
];

fn cursor_ask() -> NativeQuestion {
    // Exact mapping of acp/callbacks.rs::cursor_ask + prepare_question:
    // identical source IDs on different agents are not a global identity.
    NativeQuestion {
        id: CALLBACK.into(),
        kind: "ask".into(),
        title: "Choose the next review action".into(),
        message: String::new(),
        options: vec![],
        questions: vec![NativeQuestionItem {
            id: ITEM.into(),
            question: "Which check should I run next?".into(),
            header: String::new(),
            options: OPTIONS
                .iter()
                .map(|(id, label)| NativeOption {
                    id: (*id).into(),
                    label: (*label).into(),
                    description: None,
                })
                .collect(),
            multi: false,
            recommended: None,
        }],
        fields: json!({
            "toolCallId": "tool:review/17",
            "title": "Choose the next review action",
            "questions": [{
                "id": ITEM, "prompt": "Which check should I run next?",
                "options": OPTIONS.iter().map(|(id, label)| json!({"id": id, "label": label})).collect::<Vec<_>>(),
                "allowMultiple": false
            }],
            "allow_custom_input": false,
            "source_question_id": "tool:review/17"
        }),
    }
}

fn scene(case: Case, light: bool) -> (Arc<Client>, Runtime<Desk>) {
    let replica = crate::test_support::terminal::Replica::new(120, 40, 2);
    let client = replica.client.clone();
    let mut d = crate::test_support::window();
    d.connection.client = Some(client.clone());
    d.connection.topology = client.with_control(|c| c.topology().cloned());
    d.connection.layout = Some(phux_client_core::layout::LayoutNode::Leaf(
        replica.terminal.clone(),
    ));
    d.connection.shown = Some(crate::desk::Shown {
        resource: replica.terminal,
        view: Some(replica.views[0]),
    });
    d.phone = case.phone.then(crate::desk::phone::Phone::default);
    d.native = (case.phone && case.scale == 2).then(crate::mobile::Platform::test);
    d.settings.theme = crate::theme::find(if light { "phux-light" } else { "phux" });
    for (terminal, session, name) in [(1, 11, "Agent A"), (2, 21, "Agent B")] {
        let terminal = ResourceId::local(terminal);
        d.activities.apply(
            &Event::AgentMetadata {
                terminal_id: terminal.clone(),
                value: Some(
                    serde_json::to_vec(&json!({"name": name, "state": "blocked"})).unwrap(),
                ),
            },
            &[],
        );
        d.inbox.questions.views.insert(
            terminal,
            (
                ResourceId::local(session),
                NativeView {
                    activation: format!("activation-{session}"),
                    backend: "acp".into(),
                    capabilities: NativeCapabilities {
                        questions: true.into(),
                        ..Default::default()
                    },
                    connected: true,
                    turn_id: None,
                    provider_session_id: Some(format!("provider-session-{session}")),
                    questions: vec![cursor_ask()],
                },
            ),
        );
    }
    d.inbox.questions.reconcile();
    d.toggle_inbox();
    d.inbox.reply.focused = false;
    let mut rt = crate::test_support::runtime(
        d,
        crate::ui::view,
        (case.size.0 as f32, case.size.1 as f32),
        rune::Env::new(!light, case.scale as f32),
    );
    rt.set_system_reduced_motion(true);
    rt.set_srgb_blend(true);
    settle(&mut rt);
    assert_eq!(rt.state.inbox_items().len(), 2);
    no_sends(&client);
    (client, rt)
}

fn card_key(rt: &Runtime<Desk>, terminal: u32) -> String {
    let index = rt.state.inbox_items().iter().position(|item| {
        matches!(item, crate::inbox::Item::Terminal(w) if w.terminal == ResourceId::local(terminal))
    }).unwrap();
    format!("inbox-item-{index}")
}

/// Address a rendered radio by card path and visible label, not generated owner
/// keys. This also preserves the baseline's duplicate-key behavior for comparison.
fn choice_path(rt: &Runtime<Desk>, terminal: u32, option: usize) -> String {
    let card = crate::ui::element(rt, &card_key(rt, terminal));
    let prefix = format!("{}/", card.path);
    let found: Vec<_> = rt
        .elements()
        .into_iter()
        .filter(|e| {
            e.path.starts_with(&prefix)
                && e.role == "radio"
                && e.label.as_deref() == Some(OPTIONS[option].1)
                && !e.exiting
                && !e.inert
        })
        .collect();
    assert_eq!(
        found.len(),
        1,
        "one live radio in the intended agent's card"
    );
    found[0].path.clone()
}

fn at_path(rt: &Runtime<Desk>, path: &str) -> rune::ElementInfo {
    rt.elements()
        .into_iter()
        .find(|e| e.path == path && !e.exiting)
        .unwrap_or_else(|| panic!("no live element at {path}"))
}

fn show(rt: &mut Runtime<Desk>, terminal: u32, option: usize) -> String {
    let card = card_key(rt, terminal);
    // Invoke the existing row-selection handler through its mounted callback.
    assert!(rt.accessibility_action(&card, rune::accesskit::Action::Click, None));
    settle(rt);
    assert_eq!(
        rt.state.inbox.picked,
        Some(crate::inbox::Key::Terminal(
            ResourceId::local(terminal),
            Some(CALLBACK.into()),
        ))
    );
    let path = choice_path(rt, terminal, option);
    rt.update(|_, tx| tx.scroll_into_view(path.clone()));
    settle(rt);
    let e = at_path(rt, &path);
    for (full, visible) in e.rect.into_iter().zip(e.visible) {
        assert!(
            (full - visible).abs() < 0.5,
            "{path}: complete target visible"
        );
    }
    path
}

fn select(rt: &mut Runtime<Desk>, case: Case, terminal: u32, option: usize) {
    let path = show(rt, terminal, option);
    let e = at_path(rt, &path);
    let [x, y, w, h] = e.rect;
    let (x, y) = (x + w / 2.0, y + h / 2.0);
    if case.phone {
        assert!(w >= 44.0 && h >= 44.0, "whole phone touch target");
        rt.touch(1, TouchPhase::Started, x, y, 0.1);
        rt.frame(1.0 / 60.0);
        rt.touch(1, TouchPhase::Ended, x, y, 0.15);
    } else {
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.frame(1.0 / 60.0);
        rt.pointer_up(x, y);
    }
    settle(rt);
}

fn selected(rt: &Runtime<Desk>, terminal: u32) -> Vec<String> {
    let draft = rt
        .state
        .inbox
        .questions
        .open(&ResourceId::local(terminal), CALLBACK)
        .unwrap()
        .1;
    assert!(
        draft.texts.is_empty(),
        "real Cursor asks do not offer custom input"
    );
    draft.choices.get(ITEM).cloned().unwrap_or_default()
}

fn no_sends(client: &Client) {
    use phux_protocol::wire::frame::{Command, FrameKind};
    for bytes in client.take_outbound() {
        let frame = FrameKind::decode(&bytes).unwrap().0;
        assert!(
            !matches!(
                frame,
                FrameKind::InputKey { .. }
                    | FrameKind::InputPaste { .. }
                    | FrameKind::InputMouse { .. }
                    | FrameKind::InputFocus { .. }
                    | FrameKind::SetMetadata { .. }
                    | FrameKind::Command {
                        command: Command::ApplyInput { .. } | Command::NativeAgent { .. },
                        ..
                    }
            ),
            "choice-only review sent unintended input: {frame:?}"
        );
    }
}

struct Capture<'a> {
    gpu: &'a rune::render::Gpu,
    renderer: rune::render::Renderer,
    case: Case,
    prefix: String,
}

impl<'a> Capture<'a> {
    fn new(gpu: &'a rune::render::Gpu, case: Case, light: bool) -> Self {
        let format = rune::render::OFFSCREEN_FORMAT.remove_srgb_suffix();
        let mut renderer = rune::render::Renderer::new(&gpu.device, format);
        renderer.set_srgb_blend(true);
        Self {
            gpu,
            renderer,
            case,
            prefix: format!(
                "acp-ask-owner-{}-{}",
                case.name,
                if light { "light" } else { "dark" }
            ),
        }
    }

    fn frame(
        &mut self,
        rt: &mut Runtime<Desk>,
        client: &Client,
        stage: &str,
        expected: [Vec<String>; 2],
    ) -> Option<String> {
        no_sends(client);
        assert!(rt.state.inbox.questions.pending.is_empty());
        assert!(rt.state.inbox.questions.status.is_empty());
        assert!(rt.state.inbox.questions.error.is_empty());
        let actual = [selected(rt, 1), selected(rt, 2)];
        let pixels = (
            self.case.size.0 * self.case.scale,
            self.case.size.1 * self.case.scale,
        );
        let rgba =
            crate::ui::offscreen_as_app_with_renderer(self.gpu, &mut self.renderer, rt, pixels);
        assert_eq!(rt.env.scale, self.case.scale as f32);
        assert_eq!(rgba.len(), (pixels.0 * pixels.1 * 4) as usize);
        let name = format!(
            "{}-{stage}-{}x{}-{}x",
            self.prefix, self.case.size.0, self.case.size.1, self.case.scale
        );
        if let Some(directory) = std::env::var_os("RUNE_INBOX_SNAP") {
            let path = std::path::PathBuf::from(directory).join(format!("{name}.png"));
            crate::test_support::visual::write(&path, rt, pixels, &rgba, &name);
            std::fs::write(path.with_extension("state.json"), serde_json::to_vec_pretty(&json!({
                "schema": 1, "scenario": name, "callback": CALLBACK,
                "owners": [{"terminal": 1, "session": 11}, {"terminal": 2, "session": 21}],
                "actual_choices": actual, "expected_choices": expected,
                "no_native_or_terminal_input": true,
                "limits": "Controlled supported ask contract; no live provider, submission, or native keyboard qualification."
            })).unwrap()).unwrap();
        }
        no_sends(client);
        (actual != expected).then(|| format!("{name}: expected {expected:?}; observed {actual:?}"))
    }
}

fn replay(gpu: &rune::render::Gpu, case: Case, light: bool) -> Vec<String> {
    let (client, mut rt) = scene(case, light);
    let mut capture = Capture::new(gpu, case, light);
    let mut errors = Vec::new();
    show(&mut rt, 1, 0);
    errors.extend(capture.frame(&mut rt, &client, "a-open", [vec![], vec![]]));
    select(&mut rt, case, 1, 0);
    errors.extend(capture.frame(
        &mut rt,
        &client,
        "a-selected",
        [vec![OPTIONS[0].0.into()], vec![]],
    ));
    show(&mut rt, 2, 0);
    errors.extend(capture.frame(
        &mut rt,
        &client,
        "b-open",
        [vec![OPTIONS[0].0.into()], vec![]],
    ));
    select(&mut rt, case, 2, 1);
    errors.extend(capture.frame(
        &mut rt,
        &client,
        "b-selected",
        [vec![OPTIONS[0].0.into()], vec![OPTIONS[1].0.into()]],
    ));
    show(&mut rt, 1, 0);
    errors.extend(capture.frame(
        &mut rt,
        &client,
        "a-return",
        [vec![OPTIONS[0].0.into()], vec![OPTIONS[1].0.into()]],
    ));
    errors
}

#[test]
fn acp_ask_owner_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    let mut errors = Vec::new();
    for light in [false, true] {
        for case in CASES {
            errors.extend(replay(&gpu, case, light));
        }
    }
    // Retain every observed frame before failing the unchanged isolation
    // expectation; a mechanically adapted old-API baseline must remain RED.
    assert!(
        errors.is_empty(),
        "ask ownership diverged:\n{}",
        errors.join("\n")
    );
}
