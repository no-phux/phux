use super::*;
use crate::desk::native_input::{Action, Bridge, Operation};
use crate::ui::element;
use phux_client_runtime::control::DeliveryOutcome;
use phux_protocol::input::{InputEvent, paste::PasteTrust};
use phux_protocol::wire::frame::Command;

fn phone(size: (f32, f32), light: bool) -> (std::sync::Arc<Client>, Runtime<Desk>) {
    let (client, mut rt) = crate::test_support::input_fixture_showing(
        "\x1b[36m~\r\n❯ \x1b[0mgit status --short\r\n M clients/rune/src/ui/phone.rs\r\n\r\n\x1b[36m❯ \x1b[0m",
    );
    rt.set_viewport(size, 2.0);
    rt.set_system_reduced_motion(true);
    rt.update(|d, tx| {
        d.phone = Some(Phone::default());
        d.native = Some(crate::mobile::Platform::test());
        if light {
            d.settings.theme = crate::theme::find("phux-light");
        }
        d.set_accessibility(true, "UICTContentSizeCategoryL");
        tx.instant();
    });
    settle(&mut rt);
    (client, rt)
}

fn write(bridge: &mut Bridge, rt: &mut Runtime<Desk>, text: &str) {
    let before = bridge.snapshot(rt);
    assert_eq!(before.kind, "field");
    assert!(
        bridge
            .apply(
                rt,
                Operation {
                    token: before.token,
                    revision: before.revision,
                    action: Action::Replace {
                        expected: before.text,
                        text: text.to_owned(),
                        anchor: text.encode_utf16().count(),
                        focus: text.encode_utf16().count(),
                    },
                }
            )
            .unwrap()
    );
    settle(rt);
}

fn press_host_button(rt: &mut Runtime<Desk>, key: &str) {
    let controls = crate::mobile::controls(rt);
    assert!(
        controls
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == key && c["enabled"] == true)
    );
    assert!(crate::mobile::press(rt, key));
    settle(rt);
}

#[test]
fn composer_toggle_reclaims_terminal_space_and_restores_the_hidden_keyboard() {
    let (_client, mut rt) = phone((320.0, 568.0), false);
    let mut bridge = Bridge::default();
    rt.update(|d, _| d.phone.as_mut().unwrap().keyboard = false);
    settle(&mut rt);
    let stage = element(&rt, "phone-stage").rect;
    assert!(rt.find("phone-composer").is_none());
    assert!(!bridge.snapshot(&rt).wanted);

    press_host_button(&mut rt, "phone-compose");
    write(&mut bridge, &mut rt, "Keep this draft");
    assert!(element(&rt, "phone-stage").rect[3] < stage[3]);
    press_host_button(&mut rt, "phone-compose");
    assert!(rt.find("phone-composer").is_none());
    assert_eq!(element(&rt, "phone-stage").rect, stage);
    let raw = bridge.snapshot(&rt);
    assert_eq!(raw.kind, "terminal");
    assert!(!raw.wanted);

    press_host_button(&mut rt, "phone-compose");
    assert_eq!(bridge.snapshot(&rt).text, "Keep this draft");
}

#[test]
fn staged_input_only_reaches_its_pane_on_send_and_survives_refusal() {
    let (client, mut rt) = phone((393.0, 480.0), false);
    let mut bridge = Bridge::default();
    let raw = bridge.snapshot(&rt);
    assert_eq!(raw.kind, "terminal");
    press_host_button(&mut rt, "phone-compose");
    write(&mut bridge, &mut rt, "printf 'hello 🦊'");
    assert_eq!(bridge.snapshot(&rt).text, "printf 'hello 🦊'");
    no_terminal_input(&client);
    press_host_button(&mut rt, "phone-compose-send");
    let owner = rt.state.composer_owner().unwrap();
    let id = rt
        .state
        .composer_draft(&owner)
        .unwrap()
        .sending
        .as_ref()
        .unwrap()
        .0;
    let batches: Vec<_> = client
        .take_outbound()
        .into_iter()
        .filter_map(|bytes| match FrameKind::decode(&bytes).unwrap().0 {
            FrameKind::Command {
                command:
                    Command::ApplyInput {
                        terminal_id,
                        events,
                        ..
                    },
                ..
            } => Some((terminal_id, events)),
            FrameKind::InputKey { .. } | FrameKind::InputPaste { .. } => panic!("raw input leaked"),
            _ => None,
        })
        .collect();
    let [(terminal, events)] = batches.as_slice() else {
        panic!("expected one atomic submission")
    };
    assert_eq!(
        terminal,
        &rt.state.connection.shown.as_ref().unwrap().resource
    );
    let [InputEvent::Paste(paste), InputEvent::Key(enter)] = events.as_slice() else {
        panic!("expected one paste and one key")
    };
    assert_eq!(paste.trust, PasteTrust::Untrusted);
    assert_eq!(paste.data, "printf 'hello 🦊'".as_bytes());
    assert_eq!(enter.key, phux_protocol::input::key::PhysicalKey::Enter);
    rt.update(|d, _| assert!(d.composer_delivered(id, DeliveryOutcome::Refused, "test refusal")));
    settle(&mut rt);
    assert_eq!(bridge.snapshot(&rt).text, "printf 'hello 🦊'");
    tap(&mut rt, "phone-native-done", 0.5);
    assert_eq!(bridge.snapshot(&rt).kind, "terminal");
    tap(&mut rt, "phone-compose", 0.7);
    assert_eq!(bridge.snapshot(&rt).text, "printf 'hello 🦊'");
}

#[test]
fn composer_fits_with_keyboard_and_does_not_follow_a_reused_handle() {
    for (width, height, light) in [(320.0, 340.0, false), (393.0, 480.0, true)] {
        let (_client, mut rt) = phone((width, height), light);
        let mut bridge = Bridge::default();
        tap(&mut rt, "phone-compose", 0.1);
        write(
            &mut bridge,
            &mut rt,
            "Check the failing test, then explain what changed.\n🦊 Second line",
        );
        let card = element(&rt, "phone-composer").rect;
        let stage = element(&rt, "phone-stage").rect;
        let bar = element(&rt, "phone-accessory").rect;
        let send = element(&rt, "phone-compose-send").rect;
        assert!(stage[3] > 0.0 && stage[1] + stage[3] <= card[1] + 0.5);
        assert!(card[1] + card[3] <= bar[1] + 0.5);
        assert!(send[2] >= 44.0 && send[3] >= 44.0 && send[1] + send[3] <= height);
        if let Some(dir) = std::env::var_os("RUNE_COMPOSER_SNAP") {
            for _ in 0..90 {
                rt.frame(1.0 / 60.0);
            }
            let dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            let gpu = rune::render::Gpu::headless();
            let size = (width as u32 * 2, height as u32 * 2);
            let pixels = crate::ui::offscreen_as_app(&gpu, &mut rt, size);
            crate::test_support::visual::write(
                &dir.join(format!("composer-{width}.png")),
                &rt,
                size,
                &pixels,
                "native-sized, keyboard-reduced viewport",
            );
        }
        let old = bridge.snapshot(&rt);
        rt.update(|d, _| d.connection.view_server = Some(vec![2; 16]));
        settle(&mut rt);
        assert!(
            !bridge
                .apply(
                    &mut rt,
                    Operation {
                        token: old.token,
                        revision: old.revision,
                        action: Action::Text {
                            text: "wrong pane".into()
                        },
                    }
                )
                .unwrap()
        );
        tap(&mut rt, "phone-compose", 0.3);
        assert!(bridge.snapshot(&rt).text.is_empty());
    }
}

#[test]
fn multiline_draft_requires_a_separate_confirmation_and_cancel_keeps_it() {
    let (client, mut rt) = phone((320.0, 340.0), false);
    let mut bridge = Bridge::default();
    tap(&mut rt, "phone-compose", 0.1);
    write(&mut bridge, &mut rt, "printf first\nprintf second");
    tap(&mut rt, "phone-compose-send", 0.3);
    assert!(matches!(
        rt.state.pending,
        Some(crate::desk::Pending::SendDraft(_))
    ));
    assert!(!client.take_outbound().iter().any(|frame| matches!(
        FrameKind::decode(frame).unwrap().0,
        FrameKind::Command {
            command: Command::ApplyInput { .. },
            ..
        } | FrameKind::InputKey { .. }
            | FrameKind::InputPaste { .. }
    )));
    bridge.reconcile(&mut rt);
    rt.update(|d, _| d.cancel_pending());
    settle(&mut rt);
    tap(&mut rt, "phone-compose", 0.5);
    assert_eq!(bridge.snapshot(&rt).text, "printf first\nprintf second");
}

/// An empty draft is one row, as Messages draws it: the field beside its
/// send button, no taller than a touch target and its inset.
#[test]
fn an_empty_composer_is_one_row() {
    let (_client, mut rt) = phone((393.0, 480.0), false);
    tap(&mut rt, "phone-compose", 0.1);
    let card = element(&rt, "phone-composer").rect;
    let send = element(&rt, "phone-compose-send").rect;
    let inset = phux_design::tokens::SPACE.s0_5;
    assert!(card[3] <= 44.0 + 2.0 * inset + 0.5, "{card:?}");
    assert!((send[1] + send[3] - (card[1] + card[3] - inset)).abs() < 0.5);
}
