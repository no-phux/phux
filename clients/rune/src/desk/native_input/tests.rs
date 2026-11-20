use super::*;
use phux_protocol::native_agent::{NativeCapabilities, NativeQuestion, NativeView};
use rune::accesskit::Action as Accessibility;

mod retirement;

fn native(d: &mut Desk) {
    d.phone = Some(super::super::phone::Phone::default());
    d.native = Some(crate::mobile::Platform::test());
}

fn tap(rt: &mut Runtime<Desk>, key: &str, at: f64) {
    let (x, y) = rt.find(key).unwrap_or_else(|| {
        panic!(
            "no {key}: {:?}",
            rt.elements()
                .iter()
                .filter(|e| e.key.as_deref().is_some_and(|key| key.starts_with("phone")))
                .map(|e| (&e.key, e.rect, e.inert))
                .collect::<Vec<_>>()
        )
    });
    assert!(
        (0.0..393.0).contains(&x) && (0.0..852.0).contains(&y),
        "off-screen accessory {key}: {x},{y}"
    );
    rt.touch(12, rune::TouchPhase::Started, x, y, at);
    rt.frame(0.0);
    rt.touch(12, rune::TouchPhase::Ended, x, y, at + 0.05);
    rt.frame(0.0);
}

#[test]
fn system_keyboard_consumes_the_drawn_accessory_latches_once() {
    use phux_protocol::input::key::{ModSet, PhysicalKey};
    use phux_protocol::wire::frame::FrameKind;
    let (client, mut rt) = crate::test_support::input_fixture();
    native(&mut rt.state);
    rt.set_viewport((393.0, 852.0), 2.0);
    rt.frame(0.0);
    let mut bridge = Bridge::default();
    let _ = bridge.snapshot(&rt);
    let _ = client.take_outbound();
    for (index, (latch, button, action, key, mods)) in [
        (
            super::super::phone::Latch::Ctrl,
            "phone-latch-Control",
            Action::Text { text: "c".into() },
            PhysicalKey::C,
            ModSet::CTRL,
        ),
        (
            super::super::phone::Latch::Alt,
            "phone-latch-Option",
            Action::Text { text: "b".into() },
            PhysicalKey::B,
            ModSet::ALT,
        ),
        (
            super::super::phone::Latch::Shift,
            "phone-latch-Shift",
            Action::Key {
                name: "Tab".into(),
                ctrl: false,
                alt: false,
                shift: false,
                command: false,
            },
            PhysicalKey::Tab,
            ModSet::SHIFT,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let (x, y) = rt.find(button).unwrap();
        assert!((0.0..393.0).contains(&x) && (0.0..852.0).contains(&y));
        let at = 1.0 + index as f64;
        for (phase, seconds) in [
            (rune::TouchPhase::Started, at),
            (rune::TouchPhase::Ended, at + 0.05),
        ] {
            rt.touch(7, phase, x, y, seconds);
            rt.frame(0.0);
            bridge.reconcile(&mut rt);
            bridge.snapshot(&rt);
        }
        assert!(rt.state.phone.as_ref().unwrap().latched(latch));
        let _ = client.take_outbound();
        let snapshot = bridge.snapshot(&rt);
        assert!(submit(&mut bridge, &mut rt, &snapshot, action).unwrap());
        assert!(!rt.state.phone.as_ref().unwrap().latched(latch));
        let sent: Vec<_> = client
            .take_outbound()
            .iter()
            .filter_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
                FrameKind::InputKey { event, .. } => Some(event),
                _ => None,
            })
            .collect();
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0].key, sent[0].mods), (key, mods));
    }
}

#[test]
fn native_accessory_escape_cancels_rename_without_typing_behind_it() {
    let (client, mut rt, _) = field("rename-field");
    native(&mut rt.state);
    rt.update(|_, tx| tx.instant());
    rt.set_viewport((393.0, 852.0), 2.0);
    rt.frame(0.0);
    tap(&mut rt, "phone-key-Escape", 1.0);
    assert!(rt.state.connection.tabs.renaming.is_none());
    no_terminal_input(&client);
    rt.update(|d, tx| {
        d.connection.tabs.renaming = Some(super::super::Renaming::Tab([1; 16]));
        d.connection.tabs.rename_field.focused = true;
        tx.instant();
    });
    rt.frame(0.0);
    assert!(rt.find("phone-latch-Control").is_none());
    assert!(rt.find("phone-latch-Shift").is_none());
    assert!(rt.find("phone-native-done").is_some());
    tap(&mut rt, "phone-native-done", 2.0);
    assert!(
        rt.state.connection.tabs.renaming.is_some(),
        "Done must edit focus, not hit the rename scrim"
    );
    assert!(!rt.state.connection.tabs.rename_field.focused);
    assert!(
        rt.state.phone.as_ref().unwrap().keyboard,
        "a sheet's Done leaves the terminal's keyboard choice alone"
    );
    no_terminal_input(&client);
}

fn no_terminal_input(client: &phux_client_runtime::Client) {
    use phux_protocol::wire::frame::FrameKind;
    for bytes in client.take_outbound() {
        assert!(
            !matches!(
                FrameKind::decode(&bytes).unwrap().0,
                FrameKind::InputKey { .. } | FrameKind::InputPaste { .. }
            ),
            "accessory typed behind a sheet"
        );
    }
}

#[test]
fn native_terminal_candidate_geometry_follows_the_real_grid_cursor() {
    let (_, mut rt) = crate::test_support::input_fixture();
    let shown = rt.state.connection.shown.as_ref().unwrap();
    let terminal = shown.resource.clone();
    let frame = rt
        .state
        .connection
        .client
        .as_ref()
        .unwrap()
        .acquire_view(shown.view.unwrap())
        .unwrap();
    rt.state.connection.origins.insert(terminal, [12.0, 34.0]);
    let metrics = rt.state.metrics();
    let expected = [
        12.0 + f32::from(frame.cursor.col) * metrics.cell_w,
        34.0 + f32::from(frame.cursor.row) * metrics.cell_h,
        metrics.cell_w,
        metrics.cell_h,
    ];
    assert_eq!(Bridge::default().snapshot(&rt).caret, Some(expected));
}

#[test]
fn sanitized_paste_preserves_selection_and_undo_boundaries() {
    let (_, rt, _) = field("settings-query");
    let mut edit = rt.state.settings_ui.query.clone();
    edit.replace_text("old");
    edit.set_selection(2, 1);
    unicode::edit(
        &mut edit,
        Action::Replace {
            expected: "old".into(),
            text: "a\r\nb🦀".into(),
            anchor: 6,
            focus: 0,
        },
    )
    .unwrap();
    assert_eq!(edit.text, "ab🦀");
    assert_eq!((edit.anchor, edit.focus), (6, 0));
    edit.undo();
    assert_eq!((&*edit.text, edit.anchor, edit.focus), ("old", 2, 1));
    edit.redo();
    assert_eq!((&*edit.text, edit.anchor, edit.focus), ("ab🦀", 6, 0));
}

#[test]
fn a_native_connection_replacement_keeps_its_host_and_trust() {
    crate::test_support::isolated_preferences(
        "desk::native_input::tests::a_native_connection_replacement_keeps_its_host_and_trust",
        || {
            let mut state = crate::mobile::state(crate::mobile::Configuration {
                endpoint: "quic://localhost:8788".into(),
                name: None,
                tls_server_name: None,
                certificate_pin: Some("11".repeat(32)),
                token: Some("22".repeat(32)),
                session: Some("work".into()),
                authority_ca: None,
                chain_pem: None,
                key_id: None,
            })
            .unwrap();
            state.native.as_mut().unwrap().active = false;
            state.replace_connection(super::super::link::Replacement::Retry);
            let native = state.native.as_ref().unwrap();
            assert!(!native.active);
            assert_eq!(native.target.cert_fingerprint, Some("11".repeat(32)));
            assert_eq!(native.target.token, Some("22".repeat(32)));
            assert_eq!(
                native.target.transport,
                phux_client_runtime::Transport::Quic("localhost:8788".into())
            );
            assert!(!state.launch.can_ensure());
        },
    );
}

#[test]
fn a_confirm_bar_has_priority_over_a_still_focused_find_field() {
    let (_, mut rt, mut bridge) = field("find-query");
    rt.state.pending = Some(super::super::Pending::Paste(
        rt.state.connection.shown.as_ref().unwrap().resource.clone(),
        "two\nlines".into(),
    ));
    assert_eq!(bridge.snapshot(&rt).kind, "control");
}

#[test]
fn identical_acp_questions_in_new_activations_retire_the_owner() {
    use phux_protocol::ids::ResourceId;
    let (client, mut rt) = crate::test_support::input_fixture();
    native(&mut rt.state);
    let question = acp_question("q", "Title", "Message");
    let terminal = rt.state.connection.shown.as_ref().unwrap().resource.clone();
    let view = acp_view("first", &question);
    rt.state
        .inbox
        .questions
        .views
        .insert(terminal.clone(), (ResourceId::local(2), view));
    rt.state
        .inbox
        .questions
        .drafts
        .insert("q".into(), super::super::questions::Draft::new(&question));
    let (_, before) = target::Field::question(&rt.state, "acp-q-field-text").unwrap();
    rt.state
        .inbox
        .questions
        .views
        .get_mut(&terminal)
        .unwrap()
        .1
        .activation = "second".into();
    let (field, after) = target::Field::question(&rt.state, "acp-q-field-text").unwrap();
    assert_ne!(before, after);
    rt.update(|d, tx| field.key(d, &event("Enter", false, false, false, false).unwrap(), tx));
    assert_eq!(field.edit(&rt.state).unwrap().text, "\n");
    field.edit_mut(&mut rt.state).unwrap().focused = true;
    let _ = client.take_outbound();
    rt.update(|d, tx| d.soft(super::super::phone::Soft::Key(Key::Enter), tx));
    assert_eq!(field.edit(&rt.state).unwrap().text, "\n\n");
    let duplicate = rt
        .state
        .inbox
        .questions
        .views
        .get(&terminal)
        .unwrap()
        .clone();
    rt.state
        .inbox
        .questions
        .views
        .insert(ResourceId::local(77), duplicate);
    assert!(target::Field::question(&rt.state, "acp-q-field-text").is_none());
    rt.update(|d, tx| d.soft(super::super::phone::Soft::Key(Key::Backspace), tx));
    assert_eq!(field.edit(&rt.state).unwrap().text, "\n\n");
    assert!(client.take_outbound().is_empty());
}

fn acp_question(id: &str, title: &str, message: &str) -> NativeQuestion {
    NativeQuestion {
        id: id.into(),
        kind: "editor".into(),
        title: title.into(),
        message: message.into(),
        options: vec![],
        questions: vec![],
        fields: serde_json::Value::Null,
    }
}

/// A connected ACP view at `activation` asking `question`.
fn acp_view(activation: &str, question: &NativeQuestion) -> NativeView {
    NativeView {
        activation: activation.into(),
        backend: "acp".into(),
        connected: true,
        capabilities: NativeCapabilities {
            questions: true.into(),
            ..Default::default()
        },
        turn_id: None,
        provider_session_id: None,
        questions: vec![question.clone()],
    }
}

fn field(
    key: &str,
) -> (
    std::sync::Arc<phux_client_runtime::Client>,
    Runtime<Desk>,
    Bridge,
) {
    let (client, mut rt) = crate::test_support::input_fixture();
    rt.update(|d, tx| {
        tx.instant();
        match key {
            "palette-query" => d.toggle_palette(),
            "phone-session-query" => {
                d.phone = Some(super::super::phone::Phone::default());
                d.open_phone_sessions();
            }
            "find-query" => d.open_find(),
            "settings-query" => {
                d.toggle_settings();
                d.focus_settings_search();
            }
            "rename-field" => {
                d.connection.tabs.renaming = Some(super::super::Renaming::Tab([1; 16]));
                d.connection.tabs.rename_field.focused = true;
            }
            "machine-field" => {
                d.machines.open = true;
                d.machines.field.focused = true;
            }
            _ => panic!("unknown fixture"),
        }
    });
    rt.frame(0.0);
    assert!(rt.accessibility_action(key, Accessibility::Focus, None));
    rt.frame(0.0);
    let _ = client.take_outbound();
    (client, rt, Bridge::default())
}

fn replace(
    bridge: &mut Bridge,
    rt: &mut Runtime<Desk>,
    text: &str,
    anchor: usize,
    focus: usize,
) -> Result<bool, String> {
    let snap = bridge.snapshot(rt);
    let action = Action::Replace {
        expected: snap.text.clone(),
        text: text.into(),
        anchor,
        focus,
    };
    submit(bridge, rt, &snap, action)
}

/// Apply `action` against `snap`'s token and revision.
fn submit(
    bridge: &mut Bridge,
    rt: &mut Runtime<Desk>,
    snap: &Snapshot,
    action: Action,
) -> Result<bool, String> {
    let (token, revision) = (snap.token, snap.revision);
    bridge.apply(
        rt,
        Operation {
            token,
            revision,
            action,
        },
    )
}

#[test]
fn native_fields_edit_synchronously_and_never_reach_the_terminal() {
    for key in [
        "palette-query",
        "phone-session-query",
        "find-query",
        "settings-query",
        "rename-field",
        "machine-field",
    ] {
        let (client, mut rt, mut bridge) = field(key);
        let editor = crate::ui::element(&rt, key);
        assert!(
            editor.label.is_some_and(|label| !label.is_empty()),
            "{key} needs a name, not just a placeholder"
        );
        assert!(replace(&mut bridge, &mut rt, "a🦀c", 3, 1).unwrap());
        let snap = bridge.snapshot(&rt);
        assert_eq!(snap.text, "a🦀c", "{key}");
        assert_eq!((snap.anchor, snap.focus), (3, 1));
        assert!(client.take_outbound().is_empty(), "{key}");
        assert!(replace(&mut bridge, &mut rt, "a🦀c", 2, 2).is_err());
        assert_eq!(bridge.snapshot(&rt).text, "a🦀c");
    }
}

#[test]
fn native_session_filter_returns_to_the_first_result_after_scrolling() {
    use phux_client_runtime::control::Topology;
    use phux_protocol::ids::{ResourceId, SessionId, WindowId};
    use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};

    let (client, mut rt, mut bridge) = field("phone-session-query");
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
        .with_sessions(
            (1..=20)
                .map(|n| SessionInfo::new(SessionId::new(n), format!("session {n}")))
                .collect(),
        );
    rt.update(|d, _| d.connection.topology = Some(Topology::from_snapshot(&snapshot)));
    rt.frame(0.0);
    rt.update(|_, tx| tx.scroll_to_instant("phone-session-list", 0.0, 500.0));
    rt.frame(0.0);
    assert!(replace(&mut bridge, &mut rt, "session", 7, 7).unwrap());
    rt.settle_all();
    rt.frame(1.0);
    let list = rt.presented("phone-session-list").unwrap().0;
    let first = rt.presented("phone-session-1").unwrap().0;
    assert!(
        first[1] >= list[1] && first[1] - list[1] < 8.0,
        "{first:?} vs {list:?}"
    );
    assert_eq!(bridge.snapshot(&rt).text, "session");
    no_terminal_input(&client);
}

#[test]
fn composition_is_staged_and_committed_once_with_native_selection() {
    let (_, mut rt, mut bridge) = field("settings-query");
    let snap = bridge.snapshot(&rt);
    let preedit = Action::Preedit {
        text: "日本🦀".into(),
        anchor: 2,
        focus: 4,
    };
    assert!(submit(&mut bridge, &mut rt, &snap, preedit).unwrap());
    assert!(rt.state.settings_ui.query.text.is_empty());
    assert_eq!(
        rt.state.settings_ui.query.preedit().unwrap().1,
        Some((6, 10))
    );
    assert!(replace(&mut bridge, &mut rt, "日本🦀", 4, 4).unwrap());
    assert!(!rt.state.settings_ui.query.is_composing());
    assert_eq!(rt.state.settings_ui.query.text, "日本🦀");
    let snap = bridge.snapshot(&rt);
    let stale = Operation {
        token: snap.token,
        revision: snap.revision.saturating_sub(1),
        action: Action::Text {
            text: "again".into(),
        },
    };
    assert!(!bridge.apply(&mut rt, stale).unwrap());
}

#[test]
fn a_rename_retarget_and_closed_field_reject_held_callbacks() {
    let (client, mut rt, mut bridge) = field("rename-field");
    let snap = bridge.snapshot(&rt);
    rt.state
        .connection
        .tabs
        .rename_field
        .ime(&rune::Ime::Preedit("held".into(), None, Vec::new()));
    rt.state.connection.tabs.renaming = Some(super::super::Renaming::Tab([2; 16]));
    let wrong_tab = Action::Replace {
        expected: snap.text.clone(),
        text: "wrong tab".into(),
        anchor: 9,
        focus: 9,
    };
    assert!(!submit(&mut bridge, &mut rt, &snap, wrong_tab).unwrap());
    assert!(rt.state.connection.tabs.rename_field.text.is_empty());
    assert!(!rt.state.connection.tabs.rename_field.is_composing());
    let snap = bridge.snapshot(&rt);
    rt.state.cancel_rename();
    let text = Action::Text {
        text: "must not enter terminal".into(),
    };
    assert!(!submit(&mut bridge, &mut rt, &snap, text).unwrap());
    assert!(client.take_outbound().is_empty());
}

#[test]
fn changed_text_or_revision_and_invalid_selection_are_atomic_rejections() {
    let (_, mut rt, mut bridge) = field("settings-query");
    assert!(replace(&mut bridge, &mut rt, "initial", 7, 7).unwrap());
    let snap = bridge.snapshot(&rt);
    let bad = Action::Replace {
        expected: "stale".into(),
        text: "bad".into(),
        anchor: 3,
        focus: 3,
    };
    assert!(submit(&mut bridge, &mut rt, &snap, bad).is_err());
    assert!(replace(&mut bridge, &mut rt, "bad", usize::MAX, 0).is_err());
    assert_eq!(rt.state.settings_ui.query.text, "initial");
    bridge.retire();
    assert!(!submit(&mut bridge, &mut rt, &snap, Action::Cancel).unwrap());
}

#[test]
fn a_native_shell_uses_the_full_available_stage_without_drawn_keys() {
    let (client, mut rt) = crate::test_support::input_fixture();
    rt.state.phone = Some(super::super::phone::Phone::default());
    rt.state.native = Some(crate::mobile::Platform::test());
    rt.set_viewport((393.0, 600.0), 2.0);
    for _ in 0..120 {
        rt.frame(1.0 / 60.0);
    }
    assert!(rt.find("phone-keyboard").is_none());
    assert!(rt.find("phone-home").is_none());
    let phone = rt.presented("phone").unwrap().0;
    assert_eq!(phone[1], 0.0);
    assert!(phone[3] >= 599.0);
    let _ = client.take_outbound();
    rt.state.toggle_machines();
    assert!(rt.state.native.as_ref().unwrap().connection_requested);
    assert!(!rt.state.machines.open);
    assert!(client.take_outbound().is_empty());
}
