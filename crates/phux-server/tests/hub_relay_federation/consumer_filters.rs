//! Hub consumers of one satellite Terminal keep their own event filters,
//! and lose their scope, with a `journal_gap`, when the link goes (ADR-0123).
//!
//! The link is a single client on the satellite, where the latest
//! `SUBSCRIBE_RESOURCE_EVENTS` filter on a scope wins. Forwarded as-is, one
//! consumer's filter would narrow every other consumer's stream; the hub
//! therefore subscribes the link unfiltered and filters per consumer.

use super::*;

/// Read until the reply to `request_id`, returning it and every `EVENT`
/// that arrived first.
async fn reply(hub: &mut UnixStream, request_id: u32) -> (CommandResult, Vec<AgentEvent>) {
    let deadline = Instant::now() + STEP_DEADLINE;
    let mut events = Vec::new();
    loop {
        assert!(Instant::now() < deadline, "no reply to {request_id}");
        let (_, frame) = recv_typed(hub).await;
        match frame {
            FrameKind::CommandResult {
                request_id: got,
                result,
            } if got == request_id => return (result, events),
            FrameKind::Event { event, .. } => events.push(event),
            _ => {}
        }
    }
}

/// The next `EVENT` `matches` accepts, with every event read before it.
async fn until_event(
    hub: &mut UnixStream,
    matches: impl Fn(&AgentEvent) -> bool,
) -> Vec<AgentEvent> {
    let deadline = Instant::now() + STEP_DEADLINE;
    let mut events = Vec::new();
    loop {
        assert!(
            Instant::now() < deadline,
            "the expected event never arrived"
        );
        let (_, frame) = recv_typed(hub).await;
        if let FrameKind::Event { event, .. } = frame {
            let done = matches(&event);
            events.push(event);
            if done {
                return events;
            }
        }
    }
}

async fn command(
    hub: &mut UnixStream,
    request_id: u32,
    command: Command,
) -> (CommandResult, Vec<AgentEvent>) {
    send_frame(
        hub,
        &FrameKind::Command {
            request_id,
            command,
        },
    )
    .await;
    reply(hub, request_id).await
}

const fn is_asked(event: &AgentEvent) -> bool {
    matches!(event, AgentEvent::Asked { .. })
}

const fn is_control(event: &AgentEvent) -> bool {
    matches!(event, AgentEvent::TerminalControl { .. })
}

#[test]
fn hub_consumers_with_different_filters_each_receive_exactly_their_set() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let (ws_port, sat_shutdown, sat_task) = spawn_satellite(tmp.path().join("sat.sock"));
        let sat_pane = discover_satellite_pane(ws_port).await;
        let sat_id = ResourceId::satellite("sat", sat_pane);
        let hub_path = tmp.path().join("hub.sock");
        let (hub_shutdown, hub_task) =
            spawn_hub(hub_path.clone(), vec![satellite_entry("sat", ws_port)]);

        // The unfiltered consumer subscribes first; the filtered one after,
        // so a forwarded filter would be the one the satellite kept.
        let mut unfiltered = wait_for_socket(&hub_path, STEP_DEADLINE).await;
        let _ = get_screen_until_ok(&mut unfiltered, sat_pane).await;
        let subscribe = |event_types| Command::SubscribeResourceEvents {
            terminal_id: sat_id.clone(),
            event_types,
        };
        let (result, _) = command(&mut unfiltered, 1, subscribe(Vec::new())).await;
        assert_eq!(result, CommandResult::Ok);
        let mut filtered = wait_for_socket(&hub_path, STEP_DEADLINE).await;
        let (result, _) = command(
            &mut filtered,
            1,
            subscribe(vec![
                phux_protocol::wire::frame::ResourceEventType::CwdChanged,
            ]),
        )
        .await;
        assert_eq!(result, CommandResult::Ok);

        let ask = Command::ReportAsked {
            terminal_id: sat_id.clone(),
            id: "for-the-unfiltered".to_owned(),
            question: "only one consumer admits this?".to_owned(),
            suggestions: Vec::new(),
            elapsed_seconds: None,
        };
        // The satellite emits `asked` before it answers, so the event may
        // arrive ahead of the reply; only wait for it if it has not.
        let (result, before) = command(&mut unfiltered, 2, ask).await;
        assert_eq!(result, CommandResult::Ok);
        if !before.iter().any(is_asked) {
            let _ = until_event(&mut unfiltered, is_asked).await;
        }

        // A lease change bypasses every filter, so it reaches both; the
        // filtered consumer must see it without the asked event first.
        let take = Command::AcquireInput {
            terminal_id: sat_id.clone(),
            mode: phux_protocol::wire::frame::InputMode::Cooperative,
            ttl_ms: 0,
        };
        let (result, mut unfiltered_saw) = command(&mut unfiltered, 3, take).await;
        assert_eq!(result, CommandResult::Ok);
        if !unfiltered_saw.iter().any(is_control) {
            unfiltered_saw.extend(until_event(&mut unfiltered, is_control).await);
        }
        let filtered_saw = until_event(&mut filtered, is_control).await;
        assert!(unfiltered_saw.iter().any(is_control));
        assert!(
            !filtered_saw.iter().any(is_asked),
            "the cwd-only consumer never receives asked: {filtered_saw:?}"
        );

        drop((unfiltered, filtered));
        let _ = hub_shutdown.send(());
        let _ = sat_shutdown.send(());
        let _ = tokio::time::timeout(STEP_DEADLINE, hub_task).await;
        let _ = tokio::time::timeout(STEP_DEADLINE, sat_task).await;
    });
}

#[test]
fn a_lost_link_owes_its_consumers_a_journal_gap() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let (ws_port, sat_shutdown, sat_task) = spawn_satellite(tmp.path().join("sat.sock"));
        let sat_pane = discover_satellite_pane(ws_port).await;
        let sat_id = ResourceId::satellite("sat", sat_pane);
        let hub_path = tmp.path().join("hub.sock");
        let (hub_shutdown, hub_task) =
            spawn_hub(hub_path.clone(), vec![satellite_entry("sat", ws_port)]);
        let mut hub = wait_for_socket(&hub_path, STEP_DEADLINE).await;
        let _ = get_screen_until_ok(&mut hub, sat_pane).await;
        send_frame(
            &mut hub,
            &FrameKind::SubscribeEvents {
                terminal: Some(sat_id),
                after_seq: None,
            },
        )
        .await;
        let _ = get_screen_until_ok(&mut hub, sat_pane).await;

        drop(sat_shutdown);
        let _ = tokio::time::timeout(STEP_DEADLINE, sat_task).await;
        let _ = until_event(&mut hub, |event| {
            matches!(event, AgentEvent::JournalGap { .. })
        })
        .await;

        drop(hub);
        let _ = hub_shutdown.send(());
        let _ = tokio::time::timeout(STEP_DEADLINE, hub_task).await;
    });
}
