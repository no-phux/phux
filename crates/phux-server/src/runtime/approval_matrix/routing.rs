//! SIGNAL-class routing and idempotency-keyed holds.

use super::*;

/// Held commands are SIGNAL-class, and every SIGNAL-class command routes to
/// the handler, as an approved one does.
#[test]
fn every_signal_class_command_routes_to_the_handler() {
    use phux_protocol::wire::frame::TerminalSignal;
    let terminal = WireResourceId::local(3);
    for held in [
        Command::KillResource {
            terminal_id: terminal.clone(),
            operation_id: None,
        },
        Command::KillResources {
            ids: vec![terminal.clone()],
            operation_id: None,
        },
        Command::CloseTabResources {
            ids: vec![terminal.clone()],
        },
        Command::SignalTerminal {
            terminal_id: terminal,
            signal: TerminalSignal::Kill,
            operation_id: None,
        },
        Command::DetachClients { session: None },
        Command::Upgrade,
    ] {
        for lane in [false, true] {
            assert_eq!(
                crate::runtime::client::route(&held, lane),
                crate::runtime::client::Route::Handler,
                "{held:?}"
            );
        }
    }
}

/// A keyed `KILL_RESOURCES` of an id that is not here: it answers OK and
/// binds its key (L20), so it shows what a keyed retry replays.
fn keyed_kill(request_id: u32, key: u8) -> FrameKind {
    command(
        request_id,
        Command::KillResources {
            ids: vec![WireResourceId::local(999)],
            operation_id: phux_protocol::ids::IdempotencyKey::new([key; 16]),
        },
    )
}

fn requested_count(frames: &[FrameKind]) -> usize {
    approval_events(frames)
        .iter()
        .filter(|(event, _)| matches!(event, AgentEvent::ApprovalRequested { .. }))
        .count()
}

/// ADR-0128 with L20: an approved keyed kill retried with its key replays
/// the first answer, without a second hold or a second delivery.
#[tokio::test(flavor = "current_thread")]
async fn an_approved_keyed_kill_retried_with_its_key_replays_without_a_second_hold() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let watch = observer(&world.state).await;
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let approver = connect(&world.state, APPROVER, TransportType::Quic).await;

            requester.send(&keyed_kill(HELD, 0x5a));
            let id = the_pending_approval(&world.state).await;
            approver.send(&decision(1, id, APPROVAL_APPROVE));
            let first = requester.result(HELD).await;
            assert!(matches!(first, CommandResult::OkWith(_)), "{first:?}");

            requester.send(&keyed_kill(HELD + 1, 0x5a));
            let replayed = requester.result(HELD + 1).await;
            assert_eq!(replayed, first, "the retry replays the first answer");
            assert!(world.state.with(|s| s.pending_approval_ids().is_empty()));
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(requested_count(&watch.frames()), 1, "one hold only");
            requester.close().await;
            approver.close().await;
            watch.close().await;
        })
        .await;
}

/// ADR-0128 with L20: two identical keyed requests held at once share one
/// approval record; one decision answers both.
#[tokio::test(flavor = "current_thread")]
async fn two_identical_keyed_requests_held_concurrently_share_one_approval() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let watch = observer(&world.state).await;
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let approver = connect(&world.state, APPROVER, TransportType::Quic).await;

            requester.send(&keyed_kill(HELD, 0x6b));
            requester.send(&keyed_kill(HELD + 1, 0x6b));
            let id = the_pending_approval(&world.state).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(
                world.state.with(|s| s.pending_approval_ids().len()),
                1,
                "the repeat joined the pending hold"
            );
            approver.send(&decision(1, id, APPROVAL_APPROVE));
            let first = requester.result(HELD).await;
            let second = requester.result(HELD + 1).await;
            assert!(matches!(first, CommandResult::OkWith(_)), "{first:?}");
            assert_eq!(second, first, "both waiters get the one execution's answer");
            assert_eq!(requested_count(&watch.frames()), 1);
            requester.close().await;
            approver.close().await;
            watch.close().await;
        })
        .await;
}

/// A joined waiter counts against the per-connection bound like a hold, and
/// its count returns when the approval resolves.
#[tokio::test(flavor = "current_thread")]
async fn joins_past_the_per_connection_bound_are_refused_and_released_after() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            world
                .state
                .with_mut(|s| s.set_approval_limits(Duration::from_secs(120), 2, 1024));
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let approver = connect(&world.state, APPROVER, TransportType::Quic).await;

            requester.send(&keyed_kill(HELD, 0x7c));
            let id = the_pending_approval(&world.state).await;
            requester.send(&keyed_kill(HELD + 1, 0x7c));
            assert!(
                until_state(&world.state, |s| s.approvals_held_by(REQUESTER) == 2).await,
                "the join counts"
            );
            requester.send(&keyed_kill(HELD + 2, 0x7c));
            let refused = requester.result(HELD + 2).await;
            assert!(
                matches!(
                    &refused,
                    CommandResult::Error { code: ErrorCode::ResourceExhausted, message }
                        if message == "too many actions awaiting approval"
                ),
                "{refused:?}"
            );
            assert_eq!(world.state.with(|s| s.pending_approval_ids().len()), 1);

            approver.send(&decision(1, id, APPROVAL_APPROVE));
            let _ = requester.result(HELD).await;
            let _ = requester.result(HELD + 1).await;
            assert!(
                until_state(&world.state, |s| s.approvals_held_by(REQUESTER) == 0).await,
                "every count is released"
            );
            requester.close().await;
            approver.close().await;
        })
        .await;
}
