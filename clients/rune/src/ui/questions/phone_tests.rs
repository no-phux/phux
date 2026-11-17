//! The delegated ACP panel uses real Inbox layout and touch dispatch.
use std::sync::Arc;

use phux_client_runtime::{Client, control::Event};
use phux_protocol::native_agent::{NativeCapabilities, NativeQuestionItem, NativeView};
use rune::{Runtime, TouchPhase};

use super::*;

const ID: &str = "acp-question:phone";
const LONG: &str =
    "Review the native input verification results before publishing this phone update";

fn option(id: &str) -> NativeOption {
    NativeOption {
        id: id.into(),
        label: LONG.into(),
        description: Some("Keep the current pairing and protocol response unchanged".into()),
    }
}

fn question(kind: &str) -> NativeQuestion {
    NativeQuestion {
        id: ID.into(),
        kind: kind.into(),
        title: "Release verification".into(),
        message: LONG.into(),
        options: vec![option("one"), option("two")],
        questions: [false, true]
            .into_iter()
            .map(|multi| NativeQuestionItem {
                id: multi.to_string(),
                question: LONG.into(),
                header: "Review".into(),
                options: vec![option("one"), option("two")],
                multi,
                recommended: Some("one".into()),
            })
            .collect(),
        fields: json!({"properties": {
            "mode": {"title": LONG, "enum": ["safe", "fast"]},
            "tags": {"type": "array", "items": {"enum": ["a", "b"]}},
            "note": {"title": LONG, "description": LONG, "type": "string"},
        }}),
    }
}

fn fixture(size: (f32, f32), kind: &str) -> (Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = crate::test_support::input_fixture();
    rt.text = rune::text::TextSystem::new(crate::grid::FONTS, crate::grid::FAMILIES);
    rt.set_viewport(size, 1.0);
    rt.set_system_reduced_motion(true);
    rt.update(|d, tx| {
        tx.instant();
        d.phone = Some(crate::desk::phone::Phone::default());
        d.native = Some(crate::mobile::Platform::test());
        d.connection.topology = client.with_control(|c| c.topology().cloned());
        let terminal = d.connection.shown.as_ref().unwrap().resource.clone();
        d.activities.apply(
            &Event::AgentMetadata {
                terminal_id: terminal.clone(),
                value: Some(br#"{"name":"ACP reviewer","state":"blocked"}"#.to_vec()),
            },
            &[],
        );
        let question = question(kind);
        let questions = &mut d.inbox.questions;
        questions.drafts.insert(ID.into(), Draft::new(&question));
        questions.views.insert(
            terminal,
            (
                ResourceId::local(7),
                NativeView {
                    activation: "phone".into(),
                    backend: "acp".into(),
                    capabilities: NativeCapabilities {
                        questions: true.into(),
                        ..NativeCapabilities::default()
                    },
                    connected: true,
                    turn_id: None,
                    provider_session_id: None,
                    questions: vec![question],
                },
            ),
        );
        d.toggle_inbox();
        d.inbox.reply.focused = false;
    });
    settle(&mut rt);
    let _ = client.take_outbound();
    (client, rt)
}

fn settle(rt: &mut Runtime<Desk>) {
    rt.settle_all();
    rt.frame(1.0);
    rt.frame(1.0);
}

fn reveal(rt: &mut Runtime<Desk>, key: &str) {
    rt.update(|_, tx| tx.scroll_into_view(key));
    settle(rt);
    let element = crate::ui::element(rt, key);
    let [x, y, w, h] = element.visible;
    assert!(w >= 44.0 && h >= 44.0, "{key}: {:?}", element.visible);
    assert!(x >= 0.0 && x + w <= rt.viewport.0 + 0.5, "{key} width");
    assert!(y >= 0.0 && y + h <= rt.viewport.1 + 0.5, "{key} height");
}

fn touch(rt: &mut Runtime<Desk>, key: &str, finish: TouchPhase) {
    reveal(rt, key);
    let (x, y) = rt.find(key).unwrap();
    rt.touch(1, TouchPhase::Started, x, y, 0.1);
    rt.frame(1.0 / 60.0);
    rt.frame(1.0 / 60.0);
    rt.touch(1, finish, x, y, 0.15);
    settle(rt);
}

fn no_input(client: &Client) {
    use phux_protocol::wire::frame::{Command, FrameKind};
    for bytes in client.take_outbound() {
        let frame = FrameKind::decode(&bytes).unwrap().0;
        match frame {
            FrameKind::InputKey { .. }
            | FrameKind::InputPaste { .. }
            | FrameKind::SetMetadata { .. }
            | FrameKind::Command {
                command: Command::ApplyInput { .. } | Command::NativeAgent { .. },
                ..
            } => panic!("unintended input: {frame:?}"),
            _ => {}
        }
    }
}

#[test]
fn phone_acp_controls_wrap_and_scroll_to_full_touch_targets() {
    for size in [(320.0, 568.0), (393.0, 852.0), (320.0, 280.0)] {
        for kind in ["select", "ask", "form", "input"] {
            let (client, mut rt) = fixture(size, kind);
            assert!(rt.state.inbox.questions.drafts[ID].choices.is_empty());
            let keys: Vec<_> = rt
                .elements()
                .into_iter()
                .filter_map(|e| e.key)
                .filter(|key| key.starts_with(&format!("acp-{ID}-")))
                .collect();
            assert!(keys.len() >= 2, "{kind} exposes its controls");
            for key in keys {
                touch(&mut rt, &key, TouchPhase::Cancelled);
            }
            // Terminal draw-text remains in the tree behind the modal. Its
            // unwrapped grid rows aren't ACP controls or phone sheet content.
            for e in rt
                .elements()
                .into_iter()
                .filter(|e| e.text.is_some() && e.path.contains("/inbox/"))
            {
                assert!(
                    e.rect[0] >= -0.5 && e.rect[0] + e.rect[2] <= size.0 + 0.5,
                    "{kind} text overflow: {:?} {:?}",
                    e.text,
                    e.rect
                );
            }
            assert!(rt.state.inbox.questions.drafts[ID].choices.is_empty());
            assert!(rt.state.inbox.questions.error.is_empty());
            assert!(rt.state.inbox.questions.pending.is_empty());
            rt.ime(rune::Ime::Commit("never type behind the modal".into()));
            settle(&mut rt);
            no_input(&client);
        }
    }
}

#[test]
fn phone_acp_finished_touch_selects_explicitly_and_centers_the_input_caret() {
    let (client, mut rt) = fixture((320.0, 568.0), "ask");
    touch(&mut rt, &format!("acp-{ID}-false-two"), TouchPhase::Ended);
    touch(&mut rt, &format!("acp-{ID}-true-one"), TouchPhase::Ended);
    touch(&mut rt, &format!("acp-{ID}-true-two"), TouchPhase::Ended);
    let draft = &rt.state.inbox.questions.drafts[ID];
    assert_eq!(draft.choices["false"], ["two"]);
    assert_eq!(draft.choices["true"], ["one", "two"]);
    no_input(&client);
    let (client, mut rt) = fixture((320.0, 280.0), "input");
    let key = format!("acp-{ID}-field-text");
    touch(&mut rt, &key, TouchPhase::Ended);
    rt.ime(rune::Ime::Commit("reviewed".into()));
    settle(&mut rt);
    let field = crate::ui::element(&rt, &key);
    let [_, y, _, h] = field.rect;
    let [_, cy, _, ch] = field.caret.expect("touch focuses the native ACP field");
    assert!((cy + ch / 2.0 - y - h / 2.0).abs() <= 1.0);
    assert_eq!(rt.state.inbox.questions.drafts[ID].text("text"), "reviewed");
    no_input(&client);
}

#[test]
fn phone_acp_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for kind in ["select", "ask", "form", "input"] {
        for size in [(393, 852), (320, 568), (320, 280)] {
            for light in [false, true] {
                let (_, mut rt) = fixture((size.0 as f32, size.1 as f32), kind);
                rt.update(|d, tx| {
                    tx.instant();
                    d.settings.theme = crate::theme::find(if light {
                        "catppuccin-latte"
                    } else {
                        "catppuccin-mocha"
                    });
                });
                settle(&mut rt);
                assert_eq!(rt.state.inbox_items().len(), 1);
                assert_eq!(rt.state.inbox.questions.drafts[ID].question.kind, kind);
                assert!(
                    rt.elements()
                        .iter()
                        .any(|e| e.text.as_deref() == Some(LONG))
                );
                let cancel = format!("acp-{ID}-cancel");
                assert!(
                    rt.elements()
                        .iter()
                        .any(|e| { e.key.as_deref() == Some(cancel.as_str()) })
                );
                if size.1 == 280 {
                    let control = match kind {
                        "select" => "one",
                        "ask" => "false-one",
                        "form" => "field-note",
                        _ => "field-text",
                    };
                    rt.update(|_, tx| tx.scroll_into_view(format!("acp-{ID}-{control}")));
                    settle(&mut rt);
                }
                // A renderer owns its runtime's glyph atlas; never reuse it for
                // a fresh TextSystem, including the baseline's smaller fonts.
                let mut renderer =
                    rune::render::Renderer::new(&gpu.device, rune::render::OFFSCREEN_FORMAT);
                let rgba = crate::ui::offscreen(&gpu, &mut renderer, &mut rt, size);
                assert_eq!(rgba.len(), (size.0 * size.1 * 4) as usize);
                if let Some(directory) = std::env::var_os("RUNE_INBOX_SNAP") {
                    let theme = if light { "light" } else { "dark" };
                    let name = format!("phone-acp-{kind}-{theme}-{}x{}-1x.png", size.0, size.1);
                    crate::test_support::visual::write(
                        &std::path::PathBuf::from(directory).join(name),
                        &rt,
                        size,
                        &rgba,
                        &format!("acp-{kind}"),
                    );
                }
            }
        }
    }
}
