//! A hub detach acknowledges proxy removal while preserving other consumers
//! and the satellite's durable PTY work.

use super::*;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use tokio::time::timeout;

#[test]
fn event_only_subscription_before_spawn_content_attach_keeps_link_and_events_live() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot_with(Some(CommandBuilder::new("/bin/cat")), None).await;
        let mut a = fed.linked_hub().await;
        let mut observer = fed.hub().await;
        let mut legacy = fed.hub().await;
        let result = spawn_resource(
            &mut a,
            1,
            on_satellite("sat", Spawn::command(&["/bin/cat"])),
        )
        .await;
        let SpawnResult::Ok(pane) = result else {
            panic!("spawn: {result:?}");
        };
        for client in [&mut a, &mut observer] {
            let subscribe = Command::SubscribeResourceEvents {
                terminal_id: pane.clone(),
                event_types: Vec::new(),
            };
            assert_eq!(
                command_via_hub(client, 2, subscribe).await.0,
                CommandResult::Ok
            );
        }
        send_frame(
            &mut legacy,
            &FrameKind::SubscribeEvents {
                terminal: Some(pane.clone()),
                after_seq: None,
            },
        )
        .await;
        get_screen_via_hub(&mut legacy, 5, pane.clone()).await;
        attach_live(&mut a, &pane).await;
        // Fresh PTY input after the attach must reach the event observer.
        let paste = Command::RouteInput {
            terminal_id: pane.clone(),
            event: InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Trusted,
                data: b"event-after-attach\n".to_vec(),
            }),
        };
        assert_eq!(command_via_hub(&mut a, 3, paste).await.0, CommandResult::Ok);
        let event = next_event(&mut observer, |frame| {
            matches!(
                frame,
                FrameKind::Event {
                    terminal: Some(_),
                    ..
                }
            )
        })
        .await;
        assert!(matches!(event, FrameKind::Event { terminal: Some(ref id), .. } if *id == pane));
        get_screen_via_hub(&mut a, 4, pane.clone()).await;

        // The legacy subscription survives the internal content detach.
        let ask = Command::ReportAsked {
            terminal_id: pane.clone(),
            id: "post-barrier".to_owned(),
            question: "event preserved?".to_owned(),
            suggestions: Vec::new(),
            elapsed_seconds: None,
        };
        assert_eq!(command_via_hub(&mut a, 6, ask).await.0, CommandResult::Ok);
        let asked = next_event(&mut legacy, |frame| {
            matches!(frame, FrameKind::Event { event: AgentEvent::Asked { id, .. }, .. } if id == "post-barrier")
        })
        .await;
        assert!(matches!(asked, FrameKind::Event { terminal: Some(ref id), .. } if *id == pane));

        drop((a, observer, legacy));
        fed.shutdown().await;
    });
}

fn assert_no_terminal_frame(frame: &FrameKind) {
    assert!(
        !matches!(
            frame,
            FrameKind::ResourceOutput { .. }
                | FrameKind::BootstrapBegin { .. }
                | FrameKind::BootstrapChunk { .. }
                | FrameKind::BootstrapReady { .. }
                | FrameKind::BootstrapTombstone { .. }
                | FrameKind::HistoryPage { .. }
                | FrameKind::HistoryTombstone { .. }
                | FrameKind::ResourceClosed { .. }
        ),
        "terminal frame after hub detach success: {frame:?}"
    );
}

/// No terminal frame reaches `client` after its detach, though the PTYs keep writing.
async fn assert_quiet(client: &mut UnixStream) {
    let (result, frames) = get_state_via_hub_frames(client, 9000).await;
    assert!(matches!(
        result,
        CommandResult::OkWith(CommandValue::State(_))
    ));
    frames.iter().for_each(assert_no_terminal_frame);
    if let Ok((_, frame)) = timeout(Duration::from_millis(60), recv_typed(client)).await {
        assert_no_terminal_frame(&frame);
        panic!("unexpected unsolicited frame: {frame:?}");
    }
}

async fn get_state_via_hub_frames(
    client: &mut UnixStream,
    request_id: u32,
) -> (CommandResult, Vec<FrameKind>) {
    let get_state = Command::GetState {
        scope: StateScope::Server,
    };
    command_via_hub(client, request_id, get_state).await
}

async fn live_sequence(client: &mut UnixStream, pane: &ResourceId) -> u64 {
    let deadline = tokio::time::Instant::now() + STEP_DEADLINE;
    recv_until_deadline(client, deadline, |_, frame| {
        let FrameKind::ResourceOutput {
            terminal_id,
            seq,
            bytes,
            ..
        } = frame
        else {
            return None;
        };
        assert_eq!(&terminal_id, pane, "exact satellite output routing");
        assert!(!bytes.is_empty());
        Some(seq)
    })
    .await
    .expect("live satellite output")
}

async fn detach(client: &mut UnixStream, pane: &ResourceId) {
    let detach = Command::DetachResource {
        terminal_id: pane.clone(),
    };
    assert_eq!(
        command_via_hub(client, 8000, detach).await.0,
        CommandResult::Ok
    );
}

async fn attach_live(client: &mut UnixStream, pane: &ResourceId) {
    let attach = Command::AttachResource {
        terminal_id: pane.clone(),
        role_policy: None,
    };
    let (result, frames) = command_via_hub(client, 7000, attach).await;
    assert_eq!(
        result,
        CommandResult::Ok,
        "link stays connected through churn"
    );
    assert!(frames.iter().any(
        |frame| matches!(frame, FrameKind::BootstrapReady { terminal_id, .. } if terminal_id == pane)
    ));
}

async fn cycle(a: &mut UnixStream, b: &mut UnixStream, round: u32) -> ResourceId {
    let writer = Spawn::command(&[
        "/bin/sh",
        "-c",
        "i=0; while :; do i=$((i+1)); printf 'HUB-LIVE-%s\\r\\n' \"$i\"; sleep 0.01; done",
    ]);
    let result = spawn_resource(a, 100 + round, on_satellite("sat", writer)).await;
    let SpawnResult::Ok(pane) = result else {
        panic!("remote spawn failed: {result:?}");
    };
    if round == 0 {
        // SPAWN publishes upstream even before a proxy is attached.
        detach(a, &pane).await;
        assert_quiet(a).await;
    }
    attach_live(a, &pane).await;
    attach_live(b, &pane).await;
    let before = live_sequence(b, &pane).await;
    live_sequence(a, &pane).await;
    detach(a, &pane).await;
    assert_quiet(a).await;
    // A satellite round trip drains B's backlog; its next output is live.
    get_screen_via_hub(b, 9002, pane.clone()).await;
    assert!(
        live_sequence(b, &pane).await > before,
        "other subscriber keeps output"
    );
    // Last-proxy detach then immediate reattach: DETACH lands before ATTACH.
    detach(b, &pane).await;
    attach_live(b, &pane).await;
    live_sequence(b, &pane).await;
    detach(b, &pane).await;
    assert_quiet(b).await;
    pane
}

#[test]
fn satellite_detach_fences_twenty_live_spawns_and_preserves_shared_consumers() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot_with(Some(CommandBuilder::new("/bin/cat")), None).await;
        let mut a = fed.linked_hub().await;
        let mut b = fed.hub().await;
        let mut spawned = Vec::new();
        for round in 0..20 {
            spawned.push(cycle(&mut a, &mut b, round).await);
        }
        let (result, frames) = get_state_via_hub_frames(&mut a, 9001).await;
        frames.iter().for_each(assert_no_terminal_frame);
        let CommandResult::OkWith(CommandValue::State(state)) = result else {
            panic!("state unavailable");
        };
        let panes: Vec<_> = state.resources.iter().map(|p| &p.id).collect();
        assert!(
            panes
                .iter()
                .all(|id| matches!(id, ResourceId::Satellite { .. })),
            "no local PTY fallback"
        );
        for pane in &spawned {
            assert!(panes.contains(&pane), "detach must preserve durable work");
        }
        attach_live(&mut a, &spawned[0]).await;
        let first = live_sequence(&mut a, &spawned[0]).await;
        assert!(live_sequence(&mut a, &spawned[0]).await > first);

        drop((a, b));
        fed.shutdown().await;
    });
}
