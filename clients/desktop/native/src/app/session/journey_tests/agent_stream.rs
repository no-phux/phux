//! Live native agent conversation, draft, and input journeys.

use super::*;

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI and disposable XDG_STATE_HOME"]
fn agent_stream_enables_native_face_without_restart_and_keeps_drafts(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    let handle = server.launch(&desktop, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&desk, cx)
            && desk.read_with(cx, |desk, _| {
                desk.focused_terminal()
                    .is_some_and(|id| desk.link.as_ref().unwrap().connection.ready(&id))
            })
    });
    let (placement, terminal) = desk.read_with(cx, |desk, _| {
        let placement = desk.focused_placement().unwrap();
        (placement.id.clone(), placement.terminal_id.clone().unwrap())
    });
    let selector = match &terminal {
        phux_protocol::ResourceId::Local { id } => format!("@{id}"),
        phux_protocol::ResourceId::Satellite { .. } => panic!("private fixture must be local"),
    };
    server.run(&[
        "agent",
        "session",
        "open",
        &selector,
        "--provider",
        "omp",
        "--native-id",
        "native-journey",
    ]);
    server.run(&[
        "agent",
        "emit",
        &selector,
        "--type",
        "session_start",
        "--data",
        r#"{"provider":"omp","native_id":"native-journey"}"#,
    ]);
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| desk.conversation_eligible(&placement))
    });
    desk.read_with(cx, |desk, _| {
        assert!(
            !desk.conversation_visible(&placement),
            "unsolicited agent start does not switch faces"
        );
        assert_eq!(desk.focused_terminal(), Some(terminal.clone()));
    });
    server.run(&["agent", "emit", &selector, "--type", "provider_raw", "--data",
        r#"{"provider":"omp","schema":"phux.transcript/v1","entry":{"id":"reply","role":"assistant","text":"Streaming","truncated":false,"final":false}}"#]);
    server.run(&["agent", "emit", &selector, "--type", "provider_raw", "--data",
        r#"{"provider":"omp","schema":"phux.transcript/v1","entry":{"id":"reply","role":"assistant","text":"Streaming complete","truncated":false,"final":true}}"#]);
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            desk.link
                .as_ref()
                .unwrap()
                .connection
                .conversation(&terminal)
                .unwrap()
                .entries
                .iter()
                .any(|entry| entry.id == "reply" && entry.is_final)
        })
    });
    desk.read_with(cx, |desk, _| {
        let conversation = desk
            .link
            .as_ref()
            .unwrap()
            .connection
            .conversation(&terminal)
            .unwrap();
        assert_eq!(
            conversation
                .entries
                .iter()
                .filter(|entry| entry.id == "reply")
                .count(),
            1
        );
    });
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| {
            desk.toggle_conversation(&placement, window, cx);
            desk.conversation_faces[&placement]
                .input
                .clone()
                .update(cx, |input, cx| {
                    input.set_value("two\nlines", window, cx);
                });
            desk.toggle_conversation(&placement, window, cx);
            assert!(!desk.conversation_visible(&placement));
            desk.toggle_conversation(&placement, window, cx);
            assert_eq!(
                desk.conversation_faces[&placement].input.read(cx).value(),
                "two\nlines"
            );
        });
    })
    .unwrap();
    cx.run_until_parked();
    // Exercise the real Kit textarea, not a separate text model.
    press(cx, *handle, "shift-enter");
    desk.read_with(cx, |desk, cx| {
        let draft = desk.conversation_faces[&placement].input.read(cx).value();
        assert_eq!(
            draft.matches('\n').count(),
            2,
            "Shift-Enter inserts a line and does not submit: {draft:?}"
        );
    });
    // Enter uses the production acknowledged paste+Return lane to the same PTY.
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| {
            desk.conversation_faces[&placement]
                .input
                .clone()
                .update(cx, |input, cx| {
                    input.set_value("printf 'OMP_NATIVE_INPUT_JOURNEY\\n'", window, cx);
                });
        });
    })
    .unwrap();
    press(cx, *handle, "enter");
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, cx| {
            desk.conversation_faces[&placement]
                .input
                .read(cx)
                .value()
                .is_empty()
        })
    });
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            desk.client()
                .and_then(|client| client.acquire_view(desk.focused_view()?))
                .is_some_and(|frame| {
                    frame
                        .text()
                        .lines()
                        .any(|line| line.trim() == "OMP_NATIVE_INPUT_JOURNEY")
                })
        })
    });
    server.run(&[
        "agent",
        "emit",
        &selector,
        "--type",
        "prompt",
        "--data",
        r#"{"length":1}"#,
    ]);
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| desk.conversation_writable(&placement, true))
    });
    desk.update(cx, |desk, cx| {
        desk.stop_conversation(&placement, cx);
        assert!(
            desk.link
                .as_ref()
                .unwrap()
                .connection
                .conversation(&terminal)
                .unwrap()
                .stop_requested
        );
        assert!(
            !desk.conversation_writable(&placement, true),
            "a second Escape cannot open rewind"
        );
        desk.stop_conversation(&placement, cx);
    });
    server.run(&[
        "agent",
        "emit",
        &selector,
        "--type",
        "notification",
        "--data",
        r#"{"kind":"permission"}"#,
    ]);
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            desk.link
                .as_ref()
                .unwrap()
                .connection
                .conversation(&terminal)
                .unwrap()
                .state
                .status
                == phux_client_core::session::agent_stream::AgentSessionStatus::Blocked
        })
    });
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| {
            desk.conversation_faces[&placement]
                .input
                .clone()
                .update(cx, |input, cx| {
                    input.set_value("preserve approval draft", window, cx);
                });
            assert!(!desk.conversation_writable(&placement, false));
            desk.send_conversation(&placement, cx);
            assert_eq!(
                desk.conversation_faces[&placement].input.read(cx).value(),
                "preserve approval draft"
            );
        });
    })
    .unwrap();
    // Closing only the stream keeps readable rows, but refuses shell writes and Stop.
    server.run(&["agent", "session", "close", &selector]);
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| {
            desk.link
                .as_ref()
                .unwrap()
                .connection
                .conversation(&terminal)
                .is_some_and(|conversation| conversation.closed)
        })
    });
    desk.update(cx, |desk, cx| {
        assert!(desk.conversation_visible(&placement));
        assert!(!desk.conversation_writable(&placement, false));
        assert!(!desk.conversation_writable(&placement, true));
        let draft = desk.conversation_faces[&placement].input.read(cx).value();
        desk.send_conversation(&placement, cx);
        assert_eq!(
            desk.conversation_faces[&placement].input.read(cx).value(),
            draft
        );
        assert!(
            desk.link.as_ref().unwrap().connection.ready(&terminal),
            "closing agent leaves terminal alive"
        );
    });
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| {
            desk.toggle_conversation(&placement, window, cx)
        });
    })
    .unwrap();
    assert!(!desk.read_with(cx, |desk, _| desk.conversation_visible(&placement)));
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
}

#[gpui::test]
#[ignore = "requires PHUX_DESKTOP_JOURNEY_TEST_BIN pointing at a checkout-built CLI and disposable XDG_STATE_HOME"]
fn native_agents_snaps_every_agent_pane_and_later_agents_follow(cx: &mut TestAppContext) {
    let server = Server::new();
    let desktop = desktop(cx);
    let handle = server.launch(&desktop, cx);
    let desk = desktop.read_with(cx, |desktop, _| desktop.windows[0].desk.clone());
    wait_for(cx, |cx| {
        attached(&desk, cx)
            && desk.read_with(cx, |desk, _| {
                desk.focused_terminal()
                    .is_some_and(|id| desk.link.as_ref().unwrap().connection.ready(&id))
            })
    });
    let (placement, terminal) = desk.read_with(cx, |desk, _| {
        let placement = desk.focused_placement().unwrap();
        (placement.id.clone(), placement.terminal_id.clone().unwrap())
    });
    let selector = match &terminal {
        phux_protocol::ResourceId::Local { id } => format!("@{id}"),
        phux_protocol::ResourceId::Satellite { .. } => panic!("private fixture must be local"),
    };
    // On before any agent exists: nothing to show, so the terminal stays.
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| desk.execute("native-agents", window, cx));
    })
    .unwrap();
    desk.read_with(cx, |desk, _| {
        assert!(desk.prefs.native_agents);
        assert!(!desk.conversation_visible(&placement));
    });
    server.run(&[
        "agent",
        "session",
        "open",
        &selector,
        "--provider",
        "omp",
        "--native-id",
        "snap",
    ]);
    server.run(&[
        "agent",
        "emit",
        &selector,
        "--type",
        "session_start",
        "--data",
        r#"{"provider":"omp","native_id":"snap"}"#,
    ]);
    wait_for(cx, |cx| {
        desk.read_with(cx, |desk, _| desk.conversation_eligible(&placement))
    });
    assert!(
        desk.read_with(cx, |desk, _| desk.conversation_visible(&placement)),
        "an agent that starts while Native Agents is on opens on its conversation"
    );
    cx.run_until_parked();
    cx.update_window(*handle, |_, window, cx| {
        desk.update(cx, |desk, cx| {
            // The pane's own switch still wins until the preference moves.
            desk.toggle_conversation(&placement, window, cx);
            assert!(!desk.conversation_visible(&placement));
            desk.execute("native-agents", window, cx);
            assert!(!desk.prefs.native_agents);
            assert!(!desk.conversation_visible(&placement));
            desk.execute("native-agents", window, cx);
            assert!(desk.prefs.native_agents);
            assert!(
                desk.conversation_visible(&placement),
                "turning it on snaps a pane that was switched back"
            );
            desk.execute("native-agents", window, cx);
            assert!(!desk.conversation_visible(&placement));
        });
    })
    .unwrap();
    desk.update(cx, |desk, cx| desk.prepare_close(cx));
}
