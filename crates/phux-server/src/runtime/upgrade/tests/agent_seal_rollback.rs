#![allow(clippy::unwrap_used, reason = "tests")]

use super::super::*;
use super::support::*;
use crate::resource::agent_session::BootstrapRequest;
use crate::runtime::apps::agent_session::upgrade::AGENT_ROLLBACK_TIMEOUT;
use tokio::sync::oneshot;

/// phux-twft regression: the live server refused every upgrade with
/// `pane ResourceId(..) did not provide an upgrade handoff: actor mailbox
/// closed` because the capture asked every handle in the resource table,
/// including `AgentSession` engines (built with a closed upgrade
/// mailbox) and handles that had outlived their registry entry. Only
/// live tree panes are asked now, and the reversible preparation
/// completes with exactly the live pane.
#[tokio::test(flavor = "current_thread")]
async fn upgrade_capture_ignores_agent_sessions_and_orphan_handles() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let state = SharedState::new();
            let (live_pane, orphan, agent) = state.with_mut(|s| {
                let (sid, live_pane) = s.seed_session("main");
                let live =
                    phux_terminal::actor::TerminalActor::new_with_seed(20, 5, b"live").unwrap();
                let live_token = live.token.clone();
                let _ =
                    s.spawn_resource_actor(live_pane, live.handle, live_token, live.actor.run());

                // A leaked handle: its engine is gone and its record was
                // removed before the handle was registered.
                let orphan = s.add_pane_to_session(sid).unwrap();
                s.retire_record(orphan, phux_core::process::ExitOutcome::UNKNOWN);
                let dead = phux_terminal::actor::TerminalActor::new_with_seed(20, 5, b"").unwrap();
                let _ = s.register_resource_handle(orphan, dead.handle, dead.token);
                drop(dead.actor);

                // An agent session bound to the live pane.
                let facet = phux_core::resource::AgentFacet {
                    provider: "opencode".to_owned(),
                    native_id: None,
                    state: None,
                };
                let agent = s.new_agent_session(live_pane, &facet).unwrap();
                let token = tokio_util::sync::CancellationToken::new();
                let bundle = crate::resource::agent_session::AgentSessionActor::build(
                    live_pane,
                    "opencode",
                    None,
                    token.clone(),
                    4096,
                );
                let _ = s.spawn_resource_actor(agent, bundle.handle, token, bundle.actor.run());

                let _ = s.build_session_snapshot(sid);
                let _ = s.intern_window_wire(sid);
                s.set_upgrade_context(
                    7,
                    PathBuf::from("/nonexistent/phux.sock"),
                    RuntimeFlags::default(),
                );
                (live_pane, orphan, agent)
            });

            let context = capture_upgrade_context(&state).unwrap();
            let asked: Vec<_> = context.pane_senders.iter().map(|(id, _)| *id).collect();
            assert_eq!(asked, vec![live_pane], "only the live tree pane is asked");
            assert!(
                !asked.contains(&agent),
                "an agent session has no PTY to hand off"
            );
            assert!(!asked.contains(&orphan), "an orphan handle is not a pane");

            let panes = collect_pane_handoffs(context.pane_senders, Duration::from_secs(2))
                .await
                .expect("an agent session or orphan handle must not abort");
            assert_eq!(panes.len(), 1);
            assert!(panes.contains_key(&live_pane));

            let blob = reassemble_unchanged_tree(
                &state,
                context.listener_fd,
                context.listener_fd,
                &context.tree_identity,
                &UpgradeHandoffs {
                    panes,
                    agent_sessions: HashMap::new(),
                },
            )
            .expect("the tree did not change");
            assert_eq!(blob.panes.len(), 1, "only the live pane crosses");
            assert!(
                blob.panes[0]
                    .vt_replay_bytes
                    .windows(4)
                    .any(|w| w == b"live"),
                "the live pane carries its own handoff"
            );
        })
        .await;
}

/// The captured streams are sealed until the re-exec: an append in the
/// window is refused, never acked and then lost. An upgrade that does not
/// happen drops the seal and the old image streams on.
#[tokio::test(flavor = "current_thread")]
async fn a_failed_upgrade_unseals_the_streams_it_cut() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let state = SharedState::new();
            let (_, _, append) = state.with_mut(old_image_with_agent_sessions);
            append_records(&append, "{\"type\":\"prompt\"}").await;
            let (blob, seal) = capture_blob(&state).await;
            assert_eq!(blob.agent_sessions[0].base_seq, 1);
            assert!(matches!(
                try_append(&append, "{\"type\":\"stop\"}").await,
                Err(crate::resource::agent_session::AppendRejection::Overflow(_))
            ));
            let bootstrap = seal.bootstraps[0].clone();
            drop(seal);
            // Drop queues unseal; appends use a different mailbox. An
            // ordinary bootstrap on the same FIFO acknowledges that the
            // queued unseal was applied, without depending on select order.
            let (reply, received) = oneshot::channel();
            bootstrap
                .send(BootstrapRequest {
                    reply,
                    seal: None,
                    after_seq: None,
                })
                .await
                .unwrap();
            assert_eq!(received.await.unwrap().base_seq, 1);
            let next = append_records(&append, "{\"type\":\"stop\"}").await;
            assert_eq!(next.first_seq, 2, "the refusal consumed no sequence");
        })
        .await;
}

/// This responsive actor acknowledges `lift` within its deadline, so the
/// next append is accepted with no FIFO barrier or retry. The completed
/// rollback leaves `Drop` nothing to do (#1035).
#[tokio::test(flavor = "current_thread")]
async fn a_lifted_seal_accepts_the_next_append_with_no_barrier() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let state = SharedState::new();
            let (_, _, append) = state.with_mut(old_image_with_agent_sessions);
            append_records(&append, "{\"type\":\"prompt\"}").await;
            let (blob, mut seal) = capture_blob(&state).await;
            assert_eq!(blob.agent_sessions[0].base_seq, 1);
            assert!(matches!(
                try_append(&append, "{\"type\":\"stop\"}").await,
                Err(crate::resource::agent_session::AppendRejection::Overflow(_))
            ));

            seal.lift().await;

            // Nothing between here and the append: if `lift` had only
            // queued the unseal, this is where it would race it.
            let next = append_records(&append, "{\"type\":\"stop\"}").await;
            assert_eq!(next.first_seq, 2, "the refusal consumed no sequence");

            drop(seal);
        })
        .await;
}

/// An empty seal lifts without touching the runtime, so the failure path
/// can call it unconditionally.
#[tokio::test(flavor = "current_thread")]
async fn lifting_a_seal_that_covers_no_sessions_is_a_no_op() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let mut seal = AgentSessionSeal {
                bootstraps: Vec::new(),
            };
            seal.lift().await;
        })
        .await;
}

#[tokio::test(start_paused = true)]
async fn rollback_has_one_deadline_and_does_not_block_a_healthy_neighbor() {
    let (blocked, _blocked_rx) = mpsc::channel(1);
    blocked
        .try_send(BootstrapRequest {
            reply: oneshot::channel().0,
            seal: None,
            after_seq: None,
        })
        .unwrap();
    let (healthy, mut healthy_rx) = mpsc::channel(1);
    let mut seal = AgentSessionSeal {
        bootstraps: vec![blocked.clone(), healthy],
    };
    {
        let lift = seal.lift();
        tokio::pin!(lift);
        assert!(futures_util::poll!(&mut lift).is_pending());
        let request = healthy_rx
            .try_recv()
            .expect("a blocked neighbor must not prevent an unseal request");
        assert_eq!(request.seal, Some(false));
        request.reply.send(empty_agent_cut()).unwrap();
        assert!(futures_util::poll!(&mut lift).is_pending());
        tokio::time::advance(AGENT_ROLLBACK_TIMEOUT).await;
        assert!(
            futures_util::poll!(&mut lift).is_ready(),
            "rollback must finish within one aggregate deadline"
        );
    }
    drop(seal);
    assert!(matches!(
        healthy_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
}

#[tokio::test(start_paused = true)]
async fn rollback_waits_for_a_full_mailbox_and_then_for_application() {
    let (bootstrap, mut receiver) = mpsc::channel(1);
    bootstrap
        .try_send(BootstrapRequest {
            reply: oneshot::channel().0,
            seal: None,
            after_seq: None,
        })
        .unwrap();
    let mut seal = AgentSessionSeal {
        bootstraps: vec![bootstrap],
    };
    {
        let lift = seal.lift();
        tokio::pin!(lift);
        assert!(futures_util::poll!(&mut lift).is_pending());
        assert_eq!(receiver.recv().await.unwrap().seal, None);
        assert!(futures_util::poll!(&mut lift).is_pending());
        let request = receiver.recv().await.unwrap();
        assert_eq!(request.seal, Some(false));
        assert!(futures_util::poll!(&mut lift).is_pending());
        request.reply.send(empty_agent_cut()).unwrap();
        assert!(futures_util::poll!(&mut lift).is_ready());
    }
    drop(seal);
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
}

#[tokio::test(start_paused = true)]
async fn rollback_is_bounded_when_an_actor_accepts_but_never_answers() {
    let (bootstrap, mut receiver) = mpsc::channel(1);
    let mut seal = AgentSessionSeal {
        bootstraps: vec![bootstrap],
    };
    let unanswered;
    {
        let lift = seal.lift();
        tokio::pin!(lift);
        assert!(futures_util::poll!(&mut lift).is_pending());
        unanswered = receiver.recv().await.unwrap();
        tokio::time::advance(AGENT_ROLLBACK_TIMEOUT).await;
        assert!(futures_util::poll!(&mut lift).is_ready());
    }
    drop(seal);
    assert_eq!(receiver.recv().await.unwrap().seal, Some(false));
    drop(unanswered);
}

#[tokio::test(start_paused = true)]
async fn lost_actors_do_not_delay_rollback_or_leave_pending_senders() {
    let (closed, receiver) = mpsc::channel(1);
    drop(receiver);
    let (lost_reply, mut reply_receiver) = mpsc::channel(1);
    let mut seal = AgentSessionSeal {
        bootstraps: vec![closed, lost_reply],
    };
    {
        let lift = seal.lift();
        tokio::pin!(lift);
        assert!(futures_util::poll!(&mut lift).is_pending());
        drop(reply_receiver.recv().await.unwrap());
        drop(reply_receiver);
        assert!(futures_util::poll!(&mut lift).is_ready());
    }
}

#[tokio::test(start_paused = true)]
async fn cancelling_rollback_keeps_drop_fallback_for_a_full_mailbox() {
    let (bootstrap, mut receiver) = mpsc::channel(1);
    let mut seal = AgentSessionSeal {
        bootstraps: vec![bootstrap],
    };
    {
        let lift = seal.lift();
        tokio::pin!(lift);
        assert!(futures_util::poll!(&mut lift).is_pending());
        // The queued request fills the mailbox. Cancel while its reply is
        // still outstanding; Drop must arrange a second attempt.
    }
    drop(seal);
    assert_eq!(receiver.recv().await.unwrap().seal, Some(false));
    assert_eq!(receiver.recv().await.unwrap().seal, Some(false));
}

#[tokio::test(start_paused = true)]
async fn cancelled_rollback_does_not_repeat_an_acknowledged_unseal() {
    let (healthy, mut healthy_rx) = mpsc::channel(1);
    let (blocked, mut blocked_rx) = mpsc::channel(1);
    let mut seal = AgentSessionSeal {
        bootstraps: vec![healthy, blocked],
    };
    {
        let lift = seal.lift();
        tokio::pin!(lift);
        assert!(futures_util::poll!(&mut lift).is_pending());
        let request = healthy_rx.recv().await.unwrap();
        assert_eq!(request.seal, Some(false));
        request.reply.send(empty_agent_cut()).unwrap();
        assert!(futures_util::poll!(&mut lift).is_pending());
        // The neighbor still owes its acknowledgement when we cancel.
    }
    drop(seal);
    assert!(matches!(
        healthy_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
    assert_eq!(blocked_rx.recv().await.unwrap().seal, Some(false));
    assert_eq!(blocked_rx.recv().await.unwrap().seal, Some(false));
}

#[tokio::test(start_paused = true)]
async fn failed_exec_restores_flags_before_waiting_for_rollback_application() {
    let (bootstrap, mut receiver) = mpsc::channel(1);
    bootstrap
        .try_send(BootstrapRequest {
            reply: oneshot::channel().0,
            seal: None,
            after_seq: None,
        })
        .unwrap();
    let (plan, listener, original) = unexecutable_plan(AgentSessionSeal {
        bootstraps: vec![bootstrap],
    });
    {
        let exec = plan.exec();
        tokio::pin!(exec);
        assert!(futures_util::poll!(&mut exec).is_pending());
        assert_eq!(fd_flags(listener.as_raw_fd()), original);
        assert_eq!(receiver.recv().await.unwrap().seal, None);
        assert!(futures_util::poll!(&mut exec).is_pending());
        let request = receiver.recv().await.unwrap();
        assert_eq!(request.seal, Some(false));
        assert!(futures_util::poll!(&mut exec).is_pending());
        request.reply.send(empty_agent_cut()).unwrap();
        let std::task::Poll::Ready(error) = futures_util::poll!(&mut exec) else {
            panic!("exec failure must return after rollback is applied");
        };
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
}
