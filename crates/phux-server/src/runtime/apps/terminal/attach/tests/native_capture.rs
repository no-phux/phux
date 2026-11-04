//! Native capture invalidation, vanished panes, and capacity failures.
#![allow(
    clippy::wildcard_imports,
    reason = "the attach test topics share the parent test namespace"
)]

use super::*;
use phux_terminal::actor::TerminalResource as _;

/// A native capture the actor invalidates mid-flight (the last shell's
/// in-place respawn, a reflow) is superseded, not refused: the pump asks
/// for the resync and republishes, and the client is never told its
/// generation was lost or disconnected.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn an_invalidated_native_capture_waits_for_the_resync_instead_of_failing() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (handle, _consumer_attach_rx, mut bootstrap_rx, mut publication_rx) =
                native_attach_handle();
            let (output, _keepalive) = tokio::sync::broadcast::channel(16);
            let (resize, mut resize_rx) = ResizeSender::channel(4);
            let terminal = handle.terminal().expect("terminal facet").clone();
            let mut consumer = spawn_native_pump(terminal, &output, resize);
            let resync = |reason| ResourceOutput::Resync {
                cols: 80,
                rows: 24,
                reason,
                audience: ResyncAudience::Everyone,
                base_seq: 0,
                bytes: bytes::Bytes::new(),
            };

            output
                .send(resync(crate::resource::ResyncReason::Exit))
                .expect("pump subscribed");
            let invalidated = bootstrap_rx.recv().await.expect("exit capture request");
            invalidated
                .reply
                .send(Err(phux_terminal::native_state::NativeStateError::Resize))
                .expect("pump awaits its capture");
            let request = resync_request_from(&mut resize_rx, &mut consumer).await;
            assert_eq!(
                request.resync_for.map(|pump| pump.bootstrap_id),
                Some(two_pump_initial_generation()),
                "the pump asks for its own replacement generation"
            );
            assert!(
                !consumer.task.is_finished(),
                "the pump must outlive the invalidation"
            );

            output
                .send(resync(crate::resource::ResyncReason::Resize))
                .expect("pump subscribed");
            let capture = bootstrap_rx
                .recv()
                .await
                .expect("replacement capture request");
            let replacement = capture.bootstrap_id;
            let reply = empty_native_checkpoint(&capture);
            capture
                .reply
                .send(Ok(reply))
                .expect("pump awaits its capture");
            let publication = publication_rx.recv().await.expect("publication request");
            publication
                .reply
                .send(Ok(phux_terminal::actor::NativePublicationReply {
                    replay: Vec::new(),
                    live: output.subscribe(),
                }))
                .expect("pump awaits its publication");

            assert_eq!(
                frames_seen(&mut consumer, 3).await,
                vec![
                    Seen::Tombstone,
                    Seen::Begin {
                        generation: replacement,
                        base_seq: 0,
                    },
                    Seen::Ready {
                        generation: replacement,
                    },
                ],
                "one tombstone, then the replacement; no error frame"
            );
            assert_eq!(
                replacement,
                next_bootstrap_id(two_pump_initial_generation()),
                "the replacement follows the tombstoned generation"
            );
            assert_quiet(&mut consumer, "the resynced pump").await;
            assert!(!consumer.task.is_finished(), "the client stays attached");
            consumer.task.abort();
        })
        .await;
}

/// A pane that exits while its pump is blocked publishing a replacement
/// generation to a slow consumer ends that pump as `PaneGone`, not as a
/// connection fault: the client learns of the exit from
/// `RESOURCE_CLOSED` and keeps its connection. Hosted CI saw the old
/// mapping disconnect `a_consumer_that_falls_behind...` mid-flood.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn a_pane_gone_before_its_replacement_publication_is_not_a_connection_fault() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (handle, _consumer_attach_rx, mut bootstrap_rx, mut publication_rx) =
                native_attach_handle();
            let (output, _keepalive) = tokio::sync::broadcast::channel(16);
            let (resize, _resize_rx) = ResizeSender::channel(4);
            let terminal = handle.terminal().expect("terminal facet").clone();
            let mut consumer = spawn_native_pump(terminal, &output, resize);

            output
                .send(ResourceOutput::Resync {
                    cols: 80,
                    rows: 24,
                    reason: crate::resource::ResyncReason::Resize,
                    audience: ResyncAudience::Everyone,
                    base_seq: 0,
                    bytes: bytes::Bytes::new(),
                })
                .expect("pump subscribed");
            let capture = bootstrap_rx.recv().await.expect("replacement capture");
            let reply = empty_native_checkpoint(&capture);
            capture.reply.send(Ok(reply)).expect("pump awaits capture");
            assert_eq!(frames_seen(&mut consumer, 3).await.len(), 3);
            // The actor exits with the request in its mailbox.
            drop(publication_rx.recv().await.expect("publication request"));

            let fault = consumer.task.await.expect("pump task");
            assert!(
                matches!(fault, Some(PumpFault::PaneGone)),
                "a gone pane must not close the client: {fault:?}"
            );
        })
        .await;
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn fresh_native_capacity_failure_sends_error_then_closes_without_publication() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let state = SharedState::new();
            let (_session, terminal) = state.with_mut(|s| s.seed_session("fresh-failure"));
            let (
                handle,
                mut consumer_attach_rx,
                mut native_bootstrap_rx,
                mut native_publication_rx,
            ) = native_attach_handle();
            state.with_mut(|s| {
                let _ = s.register_resource_handle(terminal, handle, CancellationToken::new());
            });
            let client_id = state.with_mut(crate::state::ServerState::new_client_id);
            let (out_tx, mut out_rx) =
                tokio::sync::mpsc::channel(crate::state::DEFAULT_CLIENT_MAILBOX);
            let connection_token = CancellationToken::new();
            let mut output_pumps = JoinSet::new();
            tokio::join!(
                attach(
                    &state,
                    client_id,
                    41,
                    "fresh-failure",
                    None,
                    &out_tx,
                    native_profile(),
                    &mut output_pumps,
                    &connection_token,
                    false,
                ),
                answer_native_attach(
                    &mut consumer_attach_rx,
                    &mut native_bootstrap_rx,
                    &mut native_publication_rx,
                    false,
                )
            );

            assert!(matches!(
                out_rx.recv().await,
                Some(Outbound::FatalError {
                    code: ErrorCode::CodecUnavailable,
                    ..
                })
            ));
            assert!(out_rx.try_recv().is_err(), "no ATTACHED or BEGIN may leak");
            assert!(connection_token.is_cancelled());
            assert!(state.with(|s| !s.attached().contains_key(&client_id)));
            drop(out_tx);
            assert!(
                out_rx.recv().await.is_none(),
                "fatal fresh attach must reach EOF"
            );
        })
        .await;
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn replacement_native_capacity_failure_closes_but_preserves_terminal_state() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let state = SharedState::new();
            let (_session, terminal) = state.with_mut(|s| s.seed_session("replacement-failure"));
            let (
                handle,
                mut consumer_attach_rx,
                mut native_bootstrap_rx,
                mut native_publication_rx,
            ) = native_attach_handle();
            state.with_mut(|s| {
                let _ = s.register_resource_handle(terminal, handle, CancellationToken::new());
            });
            let client_id = state.with_mut(crate::state::ServerState::new_client_id);
            let (out_tx, mut out_rx) =
                tokio::sync::mpsc::channel(crate::state::DEFAULT_CLIENT_MAILBOX);
            let connection_token = CancellationToken::new();
            let mut output_pumps = JoinSet::new();

            for (attach_id, succeed) in [(51, true), (52, false)] {
                tokio::join!(
                    attach(
                        &state,
                        client_id,
                        attach_id,
                        "replacement-failure",
                        None,
                        &out_tx,
                        native_profile(),
                        &mut output_pumps,
                        &connection_token,
                        false,
                    ),
                    answer_native_attach(
                        &mut consumer_attach_rx,
                        &mut native_bootstrap_rx,
                        &mut native_publication_rx,
                        succeed,
                    )
                );
                if succeed {
                    for _ in 0..5 {
                        out_rx.recv().await.expect("initial attach publication");
                    }
                    assert!(state.with(|s| s.attached().contains_key(&client_id)));
                }
            }
            assert!(matches!(
                out_rx.recv().await,
                Some(Outbound::FatalError {
                    code: ErrorCode::CodecUnavailable,
                    ..
                })
            ));
            assert!(connection_token.is_cancelled());
            assert!(state.with(|s| !s.attached().contains_key(&client_id)));
            assert!(
                state.with(|s| s.registry().terminal(terminal).is_some()),
                "failed replacement must not reap canonical terminal state"
            );
            output_pumps.abort_all();
            while output_pumps.join_next().await.is_some() {}
            drop(out_tx);
            assert!(
                out_rx.recv().await.is_none(),
                "fatal replacement must close cleanly after ERROR"
            );
        })
        .await;
}
