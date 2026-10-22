//! A held request from hold to decision: approve, deny, single use, expiry,
//! and the requester leaving.

use super::*;

#[tokio::test(flavor = "current_thread")]
async fn held_signal_writes_an_approval_record_emits_requested_and_defers_the_result() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let watch = observer(&world.state).await;
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            requester.send(&kill(HELD, &world.alpha));
            let id = the_pending_approval(&world.state).await;

            let record = record(&world.state, id).expect("the record is written");
            let WireResourceId::Local { id: alpha } = world.alpha else {
                unreachable!()
            };
            assert_eq!(record["id"], id.to_string());
            assert_eq!(record["method"], "KILL_RESOURCE");
            assert_eq!(
                record["subjects"],
                serde_json::json!([format!("terminal:{alpha}")])
            );
            assert_eq!(record["requester"]["client"], REQUESTER.0);
            assert_eq!(
                record["requester"]["credential_id"],
                credential_id(REQUESTER)
            );
            let ttl = record["expires_at_ms"].as_u64().unwrap()
                - record["requested_at_ms"].as_u64().unwrap();
            assert_eq!(ttl, 120_000, "the default TTL");

            assert!(
                watch.until(|frames| saw_requested(frames, id)).await,
                "approval_requested is journaled: {:?}",
                watch.frames()
            );
            let (_, actor) = watch
                .approval_events()
                .into_iter()
                .find(|(event, _)| matches!(event, AgentEvent::ApprovalRequested { .. }))
                .unwrap();
            assert_eq!(
                actor,
                Some(u32::try_from(REQUESTER.0).unwrap()),
                "the requester is the actor"
            );

            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(requester.results(HELD).is_empty(), "the result is deferred");
            requester.close().await;
            watch.close().await;
        })
        .await;
}

fn saw_requested(frames: &[FrameKind], id: ApprovalId) -> bool {
    approval_events(frames)
        .iter()
        .any(|(event, _)| *event == AgentEvent::ApprovalRequested { id })
}

#[tokio::test(flavor = "current_thread")]
async fn approve_by_a_signal_holder_runs_the_held_command_exactly_once_under_the_requesters_grant()
{
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let watch = observer(&world.state).await;
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let approver = connect(&world.state, APPROVER, TransportType::Quic).await;

            requester.send(&detach_all(HELD));
            let id = the_pending_approval(&world.state).await;
            approver.send(&decision(1, id, APPROVAL_APPROVE));
            let result = requester.result(HELD).await;
            assert!(
                matches!(result, CommandResult::OkWith(_)),
                "the held command ran: {result:?}"
            );
            let approver_id = u32::try_from(APPROVER.0).unwrap();
            assert_eq!(
                watch.saw_decision(ApprovalOutcome::Approved).await,
                Some(approver_id),
                "approval_decided carries the approver"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(requester.results(HELD).len(), 1, "it ran exactly once");
            assert!(record(&world.state, id).is_none(), "the record is gone");
            assert!(
                approver
                    .frames()
                    .iter()
                    .all(|frame| !matches!(frame, FrameKind::Error { .. })),
                "the decision was accepted"
            );

            // The approval never lends the approver's grant: re-classified
            // under the requester's grant as it stands at decision time, a
            // requester that lost SIGNAL is refused though the approver
            // holds it at Global.
            requester.send(&detach_all(HELD + 1));
            let id = the_pending_approval(&world.state).await;
            let narrowed = ConnectionGrant::scoped(
                TerminalScopeSet::parse_all(&["observe@global"]).unwrap(),
                None,
            )
            .unwrap();
            world
                .state
                .with_mut(|s| s.set_connection_grant(REQUESTER, narrowed));
            approver.send(&decision(2, id, APPROVAL_APPROVE));
            let refused = requester.result(HELD + 1).await;
            assert!(is_denied(&refused, "permission denied"), "{refused:?}");

            requester.close().await;
            approver.close().await;
            watch.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn deny_is_a_normal_journaled_outcome_with_permission_denied_to_the_requester() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let watch = observer(&world.state).await;
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let approver = connect(&world.state, APPROVER, TransportType::Quic).await;

            requester.send(&kill(HELD, &world.alpha));
            let id = the_pending_approval(&world.state).await;
            approver.send(&decision(1, id, APPROVAL_DENY));
            let result = requester.result(HELD).await;
            assert!(is_denied(&result, "approval denied"), "{result:?}");
            assert_eq!(
                watch.saw_decision(ApprovalOutcome::Denied).await,
                Some(u32::try_from(APPROVER.0).unwrap())
            );
            assert!(record(&world.state, id).is_none());
            assert!(
                world.state.with(|s| s.registry().terminal_count()) == 2,
                "nothing was killed"
            );
            requester.close().await;
            approver.close().await;
            watch.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_second_approve_of_the_same_id_is_rejected_single_use() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let approver = connect(&world.state, APPROVER, TransportType::Quic).await;
            let owner = connect(&world.state, OBSERVER, TransportType::UnixSocket).await;

            requester.send(&detach_all(HELD));
            let id = the_pending_approval(&world.state).await;
            approver.send(&decision(1, id, APPROVAL_APPROVE));
            let _ = requester.result(HELD).await;

            // A scoped decider meets the guard: an id with no pending
            // approval resolves to no subject, refused like an absent one.
            approver.send(&decision(2, id, APPROVAL_APPROVE));
            assert_eq!(approver.refused(2).await, Some(ErrorCode::PermissionDenied));
            // The owner passes the guard and meets the table: still refused.
            owner.send(&decision(3, id, APPROVAL_APPROVE));
            assert_eq!(owner.refused(3).await, Some(ErrorCode::PermissionDenied));

            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(requester.results(HELD).len(), 1, "the command ran once");
            requester.close().await;
            approver.close().await;
            owner.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn expiry_denies_and_removes_the_record() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            world
                .state
                .with_mut(|s| s.set_approval_limits(Duration::from_millis(150), 64, 1024));
            let watch = observer(&world.state).await;
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let approver = connect(&world.state, APPROVER, TransportType::Quic).await;

            requester.send(&kill(HELD, &world.alpha));
            let id = the_pending_approval(&world.state).await;
            let result = requester.result(HELD).await;
            assert!(is_denied(&result, "approval expired"), "{result:?}");
            assert!(record(&world.state, id).is_none(), "the record is removed");
            assert!(world.state.with(|s| s.pending_approval_ids().is_empty()));
            assert_eq!(
                watch.saw_decision(ApprovalOutcome::Expired).await,
                None,
                "an expiry has no actor"
            );
            approver.send(&decision(1, id, APPROVAL_APPROVE));
            assert_eq!(approver.refused(1).await, Some(ErrorCode::PermissionDenied));
            requester.close().await;
            approver.close().await;
            watch.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn requester_disconnect_drops_its_holds() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let watch = observer(&world.state).await;
            let mut requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            requester.send(&kill(HELD, &world.alpha));
            requester.send(&detach_all(HELD + 1));
            assert!(until_state(&world.state, |s| s.pending_approval_ids().len() == 2).await);
            let ids = world
                .state
                .with(crate::state::ServerState::pending_approval_ids);

            requester.disconnect();
            assert!(
                until_state(&world.state, |s| s.pending_approval_ids().is_empty()).await,
                "a disconnect withdraws every hold"
            );
            for id in ids {
                assert!(record(&world.state, id).is_none());
            }
            assert_eq!(watch.saw_decision(ApprovalOutcome::Withdrawn).await, None);
            let withdrawn = watch
                .approval_events()
                .iter()
                .filter(|(event, _)| {
                    matches!(
                        event,
                        AgentEvent::ApprovalDecided {
                            outcome: ApprovalOutcome::Withdrawn,
                            ..
                        }
                    )
                })
                .count();
            assert_eq!(withdrawn, 2);
            requester.close().await;
            watch.close().await;
        })
        .await;
}
