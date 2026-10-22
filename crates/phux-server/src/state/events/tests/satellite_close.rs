//! A relayed satellite close withdrawing the approval holds that name it.

use super::*;

fn kill(terminal: WireResourceId) -> Command {
    Command::KillResource {
        terminal_id: terminal,
        operation_id: None,
    }
}

/// A relayed satellite close withdraws holds first.
#[test]
fn a_relayed_satellite_close_withdraws_holds_naming_it_before_the_close() {
    let satellite = WireResourceId::satellite("sat", 3);
    let mut state = ServerState::new();
    let requester = state.new_client_id();
    let mut opened = state
        .open_approval(requester, &kill(satellite.clone()))
        .expect("held");
    let id = opened.id;
    let observer = state.new_client_id();
    let (tx, mut rx) = mpsc::channel(8);
    state.subscribe_satellite_events(observer, satellite.clone(), EventFilter::all(), None, tx);

    let close_seq = state
        .record_relayed_event(
            satellite,
            AgentEvent::ResourceClosed { exit_status: None },
            None,
        )
        .expect("stamped");

    assert!(state.pending_approval_ids().is_empty());
    assert_eq!(
        opened.decision.try_recv(),
        Ok(Decision::TerminalGone),
        "the requester is refused ('terminal gone')"
    );
    assert!(
        state
            .close_approval(id, ApprovalOutcome::Expired, None)
            .is_none(),
        "a later TTL cannot take the id"
    );

    let stream = drain_stamped(&mut rx);
    let decided = stream.iter().position(|(_, event)| {
        matches!(
            event,
            AgentEvent::ApprovalDecided {
                outcome: ApprovalOutcome::Withdrawn,
                ..
            }
        )
    });
    let closed = stream
        .iter()
        .position(|(_, event)| matches!(event, AgentEvent::ResourceClosed { .. }));
    assert!(
        decided.is_some() && decided < closed,
        "approval_decided precedes relayed RESOURCE_CLOSED: {stream:?}"
    );
    assert!(
        stream[decided.expect("withdrawn")]
            .0
            .is_some_and(|seq| seq < close_seq),
        "withdrawn seq precedes the close seq {close_seq}: {stream:?}"
    );
    let replay = state.journal.replay_after(0, |_| true);
    let retained: Vec<_> = replay.entries.iter().map(|entry| &entry.event).collect();
    assert!(
        retained.iter().any(|event| matches!(
            event,
            AgentEvent::ApprovalDecided {
                outcome: ApprovalOutcome::Withdrawn,
                ..
            }
        )),
        "{retained:?}"
    );
    assert!(
        retained.iter().all(|event| !matches!(
            event,
            AgentEvent::ResourceClosed { .. }
                | AgentEvent::ApprovalDecided {
                    outcome: ApprovalOutcome::Expired,
                    ..
                }
        )),
        "the close is not retained, and must not expire after it: {retained:?}"
    );
}

#[test]
fn a_relayed_satellite_close_does_not_withdraw_holds_naming_another_terminal() {
    let satellite = WireResourceId::satellite("sat", 3);
    let local = pane(1);
    let mut state = ServerState::new();
    let requester = state.new_client_id();
    let mut local_hold = state
        .open_approval(requester, &kill(local))
        .expect("held locally");
    let _satellite_hold = state
        .open_approval(requester, &kill(satellite.clone()))
        .expect("held on the satellite");
    let _ = state
        .record_relayed_event(
            satellite,
            AgentEvent::ResourceClosed { exit_status: None },
            None,
        )
        .expect("stamped");
    assert_eq!(state.pending_approval_ids(), vec![local_hold.id]);
    assert!(local_hold.decision.try_recv().is_err());
}

#[test]
fn a_relayed_non_close_does_not_withdraw_holds_naming_the_satellite() {
    let satellite = WireResourceId::satellite("sat", 3);
    let mut state = ServerState::new();
    let requester = state.new_client_id();
    let mut opened = state
        .open_approval(requester, &kill(satellite.clone()))
        .expect("held");
    let _ = state
        .record_relayed_event(satellite, AgentEvent::Bell, None)
        .expect("stamped");
    assert_eq!(state.pending_approval_ids(), vec![opened.id]);
    assert!(opened.decision.try_recv().is_err());
}

#[test]
fn a_relayed_satellite_close_withdraws_a_batch_that_names_it() {
    let satellite = WireResourceId::satellite("sat", 3);
    let local = pane(1);
    let mut state = ServerState::new();
    let requester = state.new_client_id();
    let mut opened = state
        .open_approval(
            requester,
            &Command::KillResources {
                ids: vec![local, satellite.clone()],
                operation_id: None,
            },
        )
        .expect("held");
    let _ = state
        .record_relayed_event(
            satellite,
            AgentEvent::ResourceClosed { exit_status: None },
            None,
        )
        .expect("stamped");
    assert!(state.pending_approval_ids().is_empty());
    assert_eq!(opened.decision.try_recv(), Ok(Decision::TerminalGone));
}
