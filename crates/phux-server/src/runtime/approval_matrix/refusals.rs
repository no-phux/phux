//! Who may decide, what is never held, the pending bounds, and holds
//! withdrawn by revocation or a reaped terminal.

use super::*;

#[tokio::test(flavor = "current_thread")]
async fn a_viewer_or_non_signal_holder_cannot_decide() {
    LocalSet::new()
        .run_until(async {
            let world = world(|_, beta| {
                vec![
                    (REQUESTER, vec!["observe,bind,?signal@global".to_owned()]),
                    // Un-held SIGNAL everywhere, but subscribed to the held
                    // Terminal as a VIEWER (ADR-0127).
                    (APPROVER, vec!["observe,bind,signal@global".to_owned()]),
                    // Un-held SIGNAL, on another Terminal only.
                    (
                        OUTSIDER,
                        vec![format!("observe,bind,signal@terminal:{beta}")],
                    ),
                    // No SIGNAL at all.
                    (UNSIGNALED, vec!["observe,bind@global".to_owned()]),
                ]
            });
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let viewer = connect(&world.state, APPROVER, TransportType::Quic).await;
            let outsider = connect(&world.state, OUTSIDER, TransportType::Quic).await;
            let unsignaled = connect(&world.state, UNSIGNALED, TransportType::Quic).await;
            world
                .state
                .with_mut(|s| s.set_viewer_mark(APPROVER, &world.alpha, true));

            requester.send(&kill(HELD, &world.alpha));
            let id = the_pending_approval(&world.state).await;
            for (peer, who) in [
                (&viewer, "an attach VIEWER, whatever its grant"),
                (&outsider, "SIGNAL on another Terminal"),
                (&unsignaled, "a grant without SIGNAL"),
                (&requester, "the requester, whose SIGNAL is held"),
            ] {
                peer.send(&decision(9, id, APPROVAL_APPROVE));
                assert_eq!(
                    peer.refused(9).await,
                    Some(ErrorCode::PermissionDenied),
                    "{who}"
                );
            }
            assert!(record(&world.state, id).is_some(), "still pending");
            assert!(requester.results(HELD).is_empty());

            // The viewer mark is what refused it: cleared, the same grant
            // decides.
            world
                .state
                .with_mut(|s| s.set_viewer_mark(APPROVER, &world.alpha, false));
            viewer.send(&decision(10, id, APPROVAL_DENY));
            let result = requester.result(HELD).await;
            assert!(is_denied(&result, "approval denied"), "{result:?}");
            let _ = world.beta;
            for peer in [requester, viewer, outsider, unsignaled] {
                peer.close().await;
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn local_mode_never_holds() {
    LocalSet::new()
        .run_until(async {
            let state = SharedState::new();
            state.with_mut(|s| {
                s.set_policy_engine(Arc::new(ScopedPolicy::local()));
                s.set_peer_identity(OBSERVER, owner_peer());
            });
            let owner = connect(&state, OBSERVER, TransportType::UnixSocket).await;
            owner.send(&detach_all(HELD));
            let result = owner.result(HELD).await;
            assert!(matches!(result, CommandResult::OkWith(_)), "{result:?}");
            assert!(state.with(|s| s.pending_approval_ids().is_empty()));
            owner.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn pending_bound_refuses_the_65th_hold_with_resource_exhausted() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            for request_id in 1..=65 {
                requester.send(&detach_all(request_id));
            }
            let refused = requester.result(65).await;
            assert!(
                matches!(
                    refused,
                    CommandResult::Error {
                        code: ErrorCode::ResourceExhausted,
                        ..
                    }
                ),
                "{refused:?}"
            );
            assert_eq!(world.state.with(|s| s.pending_approval_ids().len()), 64);
            assert!((1..=64).all(|id| requester.results(id).is_empty()));
            requester.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_held_signal_on_a_frame_is_refused_not_held() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            requester.send(&FrameKind::SetMetadata {
                request_id: 4,
                scope: Scope::Global,
                key: CONFIG_RELOAD_KEY.to_owned(),
                value: b"nonce".to_vec(),
            });
            assert_eq!(
                requester.refused(4).await,
                Some(ErrorCode::PermissionDenied)
            );
            assert!(world.state.with(|s| s.pending_approval_ids().is_empty()));
            requester.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn revocation_withdraws_the_requesters_holds() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let watch = observer(&world.state).await;
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            requester.send(&kill(HELD, &world.alpha));
            let id = the_pending_approval(&world.state).await;
            world.state.with_mut(|s| {
                let _ = crate::runtime::connections::release_revoked_consumer_state(s, REQUESTER);
            });
            assert!(world.state.with(|s| s.pending_approval_ids().is_empty()));
            assert!(record(&world.state, id).is_none());
            assert_eq!(watch.saw_decision(ApprovalOutcome::Withdrawn).await, None);
            requester.close().await;
            watch.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_viewer_cannot_decide_a_held_close_tab_and_its_record_lists_the_terminals() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let viewer = connect(&world.state, APPROVER, TransportType::Quic).await;
            world
                .state
                .with_mut(|s| s.set_viewer_mark(APPROVER, &world.alpha, true));
            requester.send(&command(
                HELD,
                Command::CloseTabResources {
                    ids: vec![world.alpha.clone(), world.beta.clone()],
                },
            ));
            let id = the_pending_approval(&world.state).await;
            let (WireResourceId::Local { id: alpha }, WireResourceId::Local { id: beta }) =
                (&world.alpha, &world.beta)
            else {
                unreachable!()
            };
            let written = record(&world.state, id).expect("the record is written");
            assert_eq!(written["method"], "CLOSE_TAB_RESOURCES");
            assert_eq!(
                written["subjects"],
                serde_json::json!([format!("terminal:{alpha}"), format!("terminal:{beta}")])
            );
            viewer.send(&decision(1, id, APPROVAL_APPROVE));
            assert_eq!(viewer.refused(1).await, Some(ErrorCode::PermissionDenied));
            assert!(record(&world.state, id).is_some(), "still pending");
            requester.close().await;
            viewer.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_server_wide_bound_refuses_with_resource_exhausted() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            world
                .state
                .with_mut(|s| s.set_approval_limits(Duration::from_secs(120), 64, 2));
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            for request_id in 1..=3 {
                requester.send(&detach_all(request_id));
            }
            let refused = requester.result(3).await;
            assert!(
                matches!(
                    &refused,
                    CommandResult::Error { code: ErrorCode::ResourceExhausted, message }
                        if message.contains("on this server")
                ),
                "{refused:?}"
            );
            assert_eq!(world.state.with(|s| s.pending_approval_ids().len()), 2);
            requester.close().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_reaped_terminal_withdraws_its_holds_before_its_close() {
    LocalSet::new()
        .run_until(async {
            let world = held_signal_world();
            let watch = observer(&world.state).await;
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            requester.send(&kill(HELD, &world.alpha));
            let _ = the_pending_approval(&world.state).await;
            world.state.with_mut(|s| {
                crate::runtime::connections::journal_pane_closed(
                    s,
                    &world.alpha,
                    None,
                    None,
                    crate::state::CloseAttribution::default(),
                );
            });
            let result = requester.result(HELD).await;
            assert!(is_denied(&result, "terminal gone"), "{result:?}");
            assert!(world.state.with(|s| s.pending_approval_ids().is_empty()));
            let closed = |frames: &[FrameKind]| {
                frames.iter().position(|frame| {
                    matches!(
                        frame,
                        FrameKind::Event {
                            event: AgentEvent::ResourceClosed { .. },
                            ..
                        }
                    )
                })
            };
            assert!(watch.until(|frames| closed(frames).is_some()).await);
            let frames = watch.frames();
            let decided = frames.iter().position(|frame| {
                matches!(
                    frame,
                    FrameKind::Event {
                        event: AgentEvent::ApprovalDecided {
                            outcome: ApprovalOutcome::Withdrawn,
                            ..
                        },
                        ..
                    }
                )
            });
            assert!(
                decided.is_some() && decided < closed(&frames),
                "approval_decided precedes pane_closed: {frames:?}"
            );
            requester.close().await;
            watch.close().await;
        })
        .await;
}
