//! A hub detach acknowledges proxy removal while preserving other consumers
//! and the satellite's durable PTY work.

use super::*;
use phux_protocol::wire::frame::StateScope;
use tokio::time::timeout;

#[test]
fn event_only_subscription_before_spawn_content_attach_keeps_link_and_events_live() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let ws_port = free_port();
        let (sat_shutdown, sat_task) =
            spawn_satellite_with_cat(tmp.path().join("sat.sock"), ws_port);
        let (hub_shutdown, hub_task) = spawn_hub(
            tmp.path().join("hub.sock"),
            vec![satellite_entry("sat", ws_port)],
        );
        let seed = discover_satellite_pane(ws_port).await;
        let mut a = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;
        let mut observer = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;
        let mut legacy = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;
        get_screen_until_ok(&mut a, seed).await;
        let result =
            spawn_via_stream(&mut a, 1, Some("sat"), Some(vec!["/bin/cat".to_owned()])).await;
        let SpawnResult::Ok(pane) = result else {
            panic!("spawn: {result:?}");
        };
        for client in [&mut a, &mut observer] {
            let (result, _) = command_via_hub(
                client,
                2,
                Command::SubscribeTerminalEvents {
                    terminal_id: pane.clone(),
                    event_types: Vec::new(),
                },
            )
            .await;
            assert_eq!(result, CommandResult::Ok);
        }
        send_frame(
            &mut legacy,
            &FrameKind::SubscribeEvents {
                terminal: Some(pane.clone()),
            },
        )
        .await;
        get_screen_via_hub(&mut legacy, 5, pane.clone()).await;
        attach_live(&mut a, &pane).await;
        // A satellite round trip proves this is the same live link, then fresh
        // PTY input must reach both the content client and the event observer.
        let (result, _) = command_via_hub(
            &mut a,
            3,
            Command::RouteInput {
                terminal_id: pane.clone(),
                event: phux_protocol::input::InputEvent::Paste(
                    phux_protocol::input::paste::PasteEvent {
                        trust: phux_protocol::input::paste::PasteTrust::Trusted,
                        data: b"event-after-attach\n".to_vec(),
                    },
                ),
            },
        )
        .await;
        assert_eq!(result, CommandResult::Ok);
        timeout(STEP_DEADLINE, async {
            loop {
                if let FrameKind::Event {
                    terminal: Some(id), ..
                } = recv_typed(&mut observer).await.1
                {
                    assert_eq!(id, pane);
                    break;
                }
            }
        })
        .await
        .expect("event subscription survives content-generation barrier");
        get_screen_via_hub(&mut a, 4, pane.clone()).await;
        assert_legacy_event_after_cut(&mut a, &mut legacy, &pane).await;
        drop(a);
        drop(observer);
        drop(legacy);
        drop(hub_shutdown);
        timeout(STEP_DEADLINE, hub_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(sat_shutdown);
        timeout(STEP_DEADLINE, sat_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    });
}

async fn assert_legacy_event_after_cut(
    client: &mut UnixStream,
    observer: &mut UnixStream,
    pane: &TerminalId,
) {
    let (result, _) = command_via_hub(
        client,
        6,
        Command::ReportAsked {
            terminal_id: pane.clone(),
            id: "post-barrier".to_owned(),
            question: "event preserved?".to_owned(),
            suggestions: Vec::new(),
            elapsed_seconds: None,
        },
    )
    .await;
    assert_eq!(result, CommandResult::Ok);
    timeout(STEP_DEADLINE, async {
        loop {
            if let FrameKind::Event {
                terminal,
                event: AgentEvent::Asked { id, .. },
            } = recv_typed(observer).await.1
            {
                assert_eq!(terminal.as_ref(), Some(pane));
                assert_eq!(id, "post-barrier");
                break;
            }
        }
    })
    .await
    .expect("legacy event subscription survives the internal content detach");
}

fn assert_no_terminal_frame(frame: &FrameKind) {
    assert!(
        !matches!(
            frame,
            FrameKind::TerminalOutput { .. }
                | FrameKind::BootstrapBegin { .. }
                | FrameKind::BootstrapChunk { .. }
                | FrameKind::BootstrapReady { .. }
                | FrameKind::BootstrapTombstone { .. }
                | FrameKind::HistoryPage { .. }
                | FrameKind::HistoryTombstone { .. }
                | FrameKind::TerminalClosed { .. }
        ),
        "terminal frame after hub detach success: {frame:?}"
    );
}

async fn assert_quiet(client: &mut UnixStream) {
    let (result, frames) = command_via_hub(
        client,
        9000,
        Command::GetState {
            scope: StateScope::Server,
        },
    )
    .await;
    assert!(matches!(
        result,
        CommandResult::OkWith(CommandValue::State(_))
    ));
    for frame in frames {
        assert_no_terminal_frame(&frame);
    }
    // The continuously writing PTYs remain alive; silence cannot be obtained
    // by killing the terminal or by sending no output after detach.
    if let Ok((_, frame)) = timeout(Duration::from_millis(60), recv_typed(client)).await {
        assert_no_terminal_frame(&frame);
        panic!("unexpected unsolicited frame: {frame:?}");
    }
}

async fn live_sequence(client: &mut UnixStream, pane: &TerminalId) -> u64 {
    timeout(STEP_DEADLINE, async {
        loop {
            if let FrameKind::TerminalOutput {
                terminal_id,
                seq,
                bytes,
                ..
            } = recv_typed(client).await.1
            {
                assert_eq!(&terminal_id, pane, "exact satellite output routing");
                assert!(!bytes.is_empty());
                return seq;
            }
        }
    })
    .await
    .expect("live satellite output")
}

async fn detach(client: &mut UnixStream, pane: &TerminalId) {
    let (result, _) = command_via_hub(
        client,
        8000,
        Command::DetachTerminal {
            terminal_id: pane.clone(),
        },
    )
    .await;
    assert_eq!(result, CommandResult::Ok);
}

async fn attach_live(client: &mut UnixStream, pane: &TerminalId) -> Vec<FrameKind> {
    let (result, frames) = command_via_hub(
        client,
        7000,
        Command::AttachTerminal {
            terminal_id: pane.clone(),
        },
    )
    .await;
    assert_eq!(
        result,
        CommandResult::Ok,
        "live link must remain connected throughout churn"
    );
    assert!(frames.iter().any(|frame| matches!(frame,
        FrameKind::BootstrapReady { terminal_id, .. } if terminal_id == pane)));
    frames
}

async fn cycle(a: &mut UnixStream, b: &mut UnixStream, round: u32) -> TerminalId {
    let result = spawn_via_stream(
        a,
        100 + round,
        Some("sat"),
        Some(vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "i=0; while :; do i=$((i+1)); printf 'HUB-LIVE-%s\\r\\n' \"$i\"; sleep 0.01; done"
                .to_owned(),
        ]),
    )
    .await;
    let SpawnResult::Ok(pane) = result else {
        panic!("remote spawn failed: {result:?}");
    };
    assert!(matches!(&pane, TerminalId::Satellite { host, .. } if host.as_str() == "sat"));
    if round == 0 {
        // SPAWN itself publishes upstream even before a proxy is attached.
        detach(a, &pane).await;
        assert_quiet(a).await;
    }
    attach_live(a, &pane).await;
    attach_live(b, &pane).await;
    let before = live_sequence(b, &pane).await;
    live_sequence(a, &pane).await;
    detach(a, &pane).await;
    assert_quiet(a).await;
    // A correlated satellite round trip drains B's pre-detach backlog. Its
    // next output must come from the still-live shared subscription.
    get_screen_via_hub(b, 9002, pane.clone()).await;
    assert!(
        live_sequence(b, &pane).await > before,
        "other subscriber must keep receiving output"
    );
    // Last-proxy teardown is followed immediately by a reattach on the same
    // client. The satellite sees DETACH before ATTACH despite separate queues.
    detach(b, &pane).await;
    let frames = attach_live(b, &pane).await;
    assert!(frames.iter().any(
        |f| matches!(f, FrameKind::BootstrapReady { terminal_id, .. } if terminal_id == &pane)
    ));
    live_sequence(b, &pane).await;
    detach(b, &pane).await;
    assert_quiet(b).await;
    pane
}

#[test]
fn satellite_detach_fences_twenty_live_spawns_and_preserves_shared_consumers() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let ws_port = free_port();
        let (sat_shutdown, sat_task) =
            spawn_satellite_with_cat(tmp.path().join("sat.sock"), ws_port);
        let (hub_shutdown, hub_task) = spawn_hub(
            tmp.path().join("hub.sock"),
            vec![satellite_entry("sat", ws_port)],
        );
        let seed = discover_satellite_pane(ws_port).await;
        let mut a = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;
        let mut b = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;
        get_screen_until_ok(&mut a, seed).await;
        let mut spawned = Vec::new();
        for round in 0..20 {
            spawned.push(cycle(&mut a, &mut b, round).await);
        }
        let (result, frames) = command_via_hub(
            &mut a,
            9001,
            Command::GetState {
                scope: StateScope::Server,
            },
        )
        .await;
        for frame in frames {
            assert_no_terminal_frame(&frame);
        }
        let CommandResult::OkWith(CommandValue::State(state)) = result else {
            panic!("state unavailable");
        };
        let panes: Vec<_> = state.panes.iter().map(|p| &p.id).collect();
        assert!(
            panes
                .iter()
                .all(|id| matches!(id, TerminalId::Satellite { .. })),
            "no local PTY fallback"
        );
        for pane in &spawned {
            assert!(panes.contains(&pane), "detach must preserve durable work");
        }
        attach_live(&mut a, &spawned[0]).await;
        let first = live_sequence(&mut a, &spawned[0]).await;
        assert!(live_sequence(&mut a, &spawned[0]).await > first);
        drop(a);
        drop(b);
        drop(hub_shutdown);
        timeout(STEP_DEADLINE, hub_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(sat_shutdown);
        timeout(STEP_DEADLINE, sat_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    });
}
