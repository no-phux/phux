//! Hub consumers of one satellite Terminal keep their own event filters, and
//! lose their scope with a `journal_gap` when the link goes (ADR-0123). The
//! link is one client on the satellite, where the latest filter wins, so the
//! hub subscribes it unfiltered and filters per consumer.

use super::*;

fn events(frames: &[FrameKind]) -> impl Iterator<Item = &AgentEvent> {
    frames.iter().filter_map(|frame| match frame {
        FrameKind::Event { event, .. } => Some(event),
        _ => None,
    })
}

/// Every `EVENT` up to and including the first `pred` accepts.
async fn events_until(hub: &mut UnixStream, pred: impl Fn(&AgentEvent) -> bool) -> Vec<FrameKind> {
    let mut seen = Vec::new();
    next_event(hub, |frame| {
        seen.push(frame.clone());
        matches!(frame, FrameKind::Event { event, .. } if pred(event))
    })
    .await;
    seen
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
        let fed = Fed::boot().await;
        let sat_id = fed.sat_id();
        let subscribe = |event_types| Command::SubscribeResourceEvents {
            terminal_id: sat_id.clone(),
            event_types,
        };
        // Unfiltered first, filtered second: a forwarded filter would win.
        let mut unfiltered = fed.linked_hub().await;
        assert_eq!(
            command_via_hub(&mut unfiltered, 1, subscribe(Vec::new()))
                .await
                .0,
            CommandResult::Ok
        );
        let mut filtered = fed.hub().await;
        let cwd_only = vec![phux_protocol::wire::frame::ResourceEventType::CwdChanged];
        assert_eq!(
            command_via_hub(&mut filtered, 1, subscribe(cwd_only))
                .await
                .0,
            CommandResult::Ok
        );

        let ask = Command::ReportAsked {
            terminal_id: sat_id.clone(),
            id: "for-the-unfiltered".to_owned(),
            question: "only one consumer admits this?".to_owned(),
            suggestions: Vec::new(),
            elapsed_seconds: None,
        };
        let (result, before) = command_via_hub(&mut unfiltered, 2, ask).await;
        assert_eq!(result, CommandResult::Ok);
        if !events(&before).any(is_asked) {
            events_until(&mut unfiltered, is_asked).await;
        }

        // A lease change bypasses every filter, so both see it; the filtered
        // consumer must see it without the asked event first.
        let take = Command::AcquireInput {
            terminal_id: sat_id.clone(),
            mode: InputMode::Cooperative,
            ttl_ms: 0,
        };
        let (result, frames) = command_via_hub(&mut unfiltered, 3, take).await;
        assert_eq!(result, CommandResult::Ok);
        if !events(&frames).any(is_control) {
            events_until(&mut unfiltered, is_control).await;
        }
        let filtered_saw = events_until(&mut filtered, is_control).await;
        assert!(
            !events(&filtered_saw).any(is_asked),
            "the cwd-only consumer never receives asked: {filtered_saw:?}"
        );

        drop((unfiltered, filtered));
        fed.shutdown().await;
    });
}

#[test]
fn a_lost_link_owes_its_consumers_a_journal_gap() {
    phux_server_testkit::run_local(async {
        let mut fed = Fed::boot().await;
        let mut hub = fed.linked_hub().await;
        send_frame(
            &mut hub,
            &FrameKind::SubscribeEvents {
                terminal: Some(fed.sat_id()),
                after_seq: None,
            },
        )
        .await;
        let _ = get_screen_until_ok(&mut hub, fed.seed).await;

        fed.kill_satellite().await;
        next_event(&mut hub, |frame| {
            matches!(
                frame,
                FrameKind::Event {
                    event: AgentEvent::JournalGap { .. },
                    ..
                }
            )
        })
        .await;

        drop(hub);
        fed.shutdown().await;
    });
}
