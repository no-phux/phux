use super::*;
use phux_protocol::{
    ResourceId,
    ids::{SessionId, WindowId},
    wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo},
};

fn topology() -> Topology {
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(7))
        .with_sessions(vec![SessionInfo::new(SessionId::new(1), "project")])
        .with_windows(vec![WindowInfo::new(
            WindowId::new(1),
            SessionId::new(1),
            "shell",
        )])
        .with_resources(vec![ResourceInfo::new(
            ResourceId::local(7),
            WindowId::new(1),
            80,
            24,
        )]);
    let mut topology = Topology::from_snapshot(&snapshot);
    topology.panes[0].cwd = Some("/repo/worktree".into());
    topology
}

fn pending_capture(follow: bool) -> State {
    let connection =
        crate::connection::Connection::connect("/capture-test.sock".into(), "test".into()).unwrap();
    State {
        draft: "original thought".into(),
        error: None,
        editor: None,
        pending: Some(Pending {
            id: 42,
            follow,
            text: "original thought".into(),
            server_id: "original-server".into(),
            terminal: None,
            client: connection.client,
        }),
    }
}

fn spawned(error: Option<&str>) -> Event {
    Event::TerminalSpawned {
        request_id: 42,
        terminal_id: error.is_none().then(|| ResourceId::local(8)),
        error: error.map(str::to_owned),
    }
}

#[gpui::test]
fn enter_keeps_the_capture_keyboard_until_a_later_bootstrap_places_the_agent(
    cx: &mut gpui::TestAppContext,
) {
    use gpui::Focusable as _;
    use phux_protocol::{
        caps::BootstrapStreamProfile,
        ids::{BootstrapId, StreamId},
        wire::frame::{FrameKind, SpawnResult},
    };
    let _dial = crate::connection::DeterministicDial::arm();
    let (handle, desk) = crate::app::overlays::palette_tests::fixture(cx);
    let mut connection = crate::workspace::view_tests::attached();
    let workspace = crate::workspace::view_tests::opened(&mut connection);
    let cx = &mut gpui::VisualTestContext::from_window(handle, cx);
    cx.update(|window, _| window.activate_window());
    desk.update(cx, |desk, _| {
        desk.link = Some(crate::app::desk::Link {
            connection,
            workspace,
        });
        desk.capture.draft = "first prompt".into();
    });
    cx.update(|window, cx| desk.update(cx, |desk, cx| desk.execute("capture", window, cx)));
    let terminal = ResourceId::local(9);
    desk.update(cx, |desk, cx| {
        let link = desk.link.as_mut().unwrap();
        let id = link
            .workspace
            .launch_capture(&link.connection, SpawnRequest::default(), true)
            .unwrap();
        desk.capture.pending = Some(Pending {
            id,
            follow: true,
            text: desk.capture.draft.clone(),
            server_id: link.connection.server_id().unwrap(),
            terminal: None,
            client: std::sync::Arc::clone(&link.connection.client),
        });
        link.connection
            .client
            .feed(FrameKind::ResourceSpawned {
                request_id: id,
                result: SpawnResult::Ok(terminal.clone()),
            })
            .unwrap();
        let events = link.connection.drain();
        link.workspace.handle(&events, &link.connection);
        link.workspace.settle(&link.connection);
        desk.capture_receipts(&events, cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        desk.read_with(cx, |desk, cx| {
            assert_eq!(desk.modal, Modal::Capture);
            assert_eq!(desk.capture.draft, "first prompt");
            let (input, _) = desk.capture.editor.as_ref().unwrap();
            assert!(
                input.read(cx).focus_handle(cx).is_focused(window),
                "a spawn receipt must not return keys to the original shell"
            );
        });
    });
    desk.update(cx, |desk, cx| {
        let link = desk.link.as_mut().unwrap();
        let stream_id = StreamId::new(2).unwrap();
        let bootstrap_id = BootstrapId::new(2).unwrap();
        for frame in [
            FrameKind::BootstrapBegin {
                terminal_id: terminal.clone(),
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 80,
                rows: 24,
                base_seq: 0,
            },
            FrameKind::BootstrapChunk {
                terminal_id: terminal.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: b"agent ready".to_vec().into(),
            },
            FrameKind::BootstrapReady {
                terminal_id: terminal.clone(),
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
        ] {
            link.connection.client.feed(frame).unwrap();
        }
        let events = link.connection.drain();
        link.workspace.handle(&events, &link.connection);
        link.workspace.settle(&link.connection);
        desk.capture_receipts(&events, cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    desk.read_with(cx, |desk, _| {
        assert_eq!(desk.modal, Modal::None);
        assert!(desk.capture.draft.is_empty());
        assert_eq!(desk.focused_terminal(), Some(terminal));
    });
    // Verify the actual terminal focus handle, not just workspace selection.
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
    }
    cx.update(|window, cx| {
        desk.update(cx, |desk, cx| {
            let placement = desk.focused_placement().unwrap().id.clone();
            let terminal = desk
                .panes_mut()
                .get(&placement)
                .expect("rendered agent terminal");
            assert!(
                terminal.read(cx).focus_handle().is_focused(window),
                "the ready agent must own the GPUI keyboard"
            );
        });
    });
    cx.update(|window, cx| desk.update(cx, |desk, cx| desk.execute("capture", window, cx)));
    cx.run_until_parked();
    desk.read_with(cx, |desk, cx| {
        assert!(
            desk.capture.draft.is_empty(),
            "blur must not resurrect confirmed text"
        );
        let (input, _) = desk.capture.editor.as_ref().unwrap();
        assert!(input.read(cx).value().is_empty());
    });
}

#[test]
fn receipt_keeps_follow_pending_until_placement_and_never_erases_in_flight_edits() {
    let _dial = crate::connection::DeterministicDial::arm();
    for follow in [false, true] {
        let mut state = pending_capture(follow);
        state.draft = "edited while awaiting receipt".into();
        state.record_receipt(&[spawned(None)]);
        assert_eq!(
            state.pending.as_ref().unwrap().terminal,
            Some(ResourceId::local(8))
        );
        assert_eq!(state.draft, "edited while awaiting receipt");
        assert!(
            state.is_pending(),
            "a spawn receipt alone must not hand keyboard focus back"
        );
    }
}

#[test]
fn asynchronous_refusal_and_connection_replacement_retain_the_draft() {
    let _dial = crate::connection::DeterministicDial::arm();
    let mut state = pending_capture(true);
    state.record_receipt(&[spawned(Some("spawn refused"))]);
    assert!(!state.is_pending());
    assert_eq!(state.error.as_deref(), Some("spawn refused"));
    assert_eq!(state.draft, "original thought");

    let mut state = pending_capture(false);
    state.cancel_pending();
    state.record_receipt(&[spawned(None)]); // reused id from a replacement client
    assert!(!state.is_pending());
    assert_eq!(state.draft, "original thought");
    assert!(state.error.unwrap().contains("Delivery is unknown"));
}

#[test]
fn focused_route_preserves_unicode_and_shell_characters_as_one_argument() {
    let topology = topology();
    let text = "fix 東京; $(touch /bad) 'quote'\nsecond line";
    let request = resolve(text, &topology, topology.panes.first()).unwrap();
    assert_eq!(request.cwd.as_deref(), Some("/repo/worktree"));
    assert_eq!(request.session_id, Some(1));
    assert_eq!(request.command.unwrap(), vec!["opencode", "--prompt", text]);
}

#[test]
fn explicit_route_does_not_need_focus_but_refuses_ambiguous_worktrees() {
    let mut topology = topology();
    assert_eq!(
        resolve("@project fix it", &topology, None)
            .unwrap()
            .command
            .unwrap()[2],
        "fix it"
    );
    let mut second = topology.panes[0].clone();
    second.terminal_id = ResourceId::local(8);
    topology.panes.push(second);
    assert!(
        resolve("@project fix it", &topology, None).is_ok(),
        "same cwd is unambiguous"
    );
    topology.panes[1].cwd = Some("/repo/other-worktree".into());
    assert!(
        resolve("@project fix it", &topology, None)
            .unwrap_err()
            .contains("multiple directories")
    );
}

#[test]
fn missing_destinations_or_empty_prompts_never_fall_back_to_another_directory() {
    let mut topology = topology();
    for text in ["", "  ", "@missing fix it", "@project", "@project   "] {
        assert!(resolve(text, &topology, None).is_err(), "{text:?}");
    }
    assert!(resolve("fix it", &topology, None).is_err());
    topology.panes[0].cwd = None;
    assert!(resolve("fix it", &topology, topology.panes.first()).is_err());
    assert!(resolve("@project fix it", &topology, None).is_err());
}

#[test]
fn bounds_and_remote_paths_fail_closed() {
    let mut topology = topology();
    for text in ["x\0y".to_owned(), "é".repeat(8193)] {
        assert!(resolve(&text, &topology, topology.panes.first()).is_err());
    }
    assert!(resolve(&"é".repeat(8192), &topology, topology.panes.first()).is_ok());
    topology.panes[0].terminal_id = ResourceId::satellite("remote", 7);
    assert!(resolve("fix it", &topology, topology.panes.first()).is_err());
    assert!(resolve("@project fix it", &topology, None).is_err());
}
