use super::*;

/// ADR-0124 §6: a retained exited pane crosses an upgrade without a PTY
/// and the resumed image closes it with `SERVER_SHUTDOWN`.
#[tokio::test(flavor = "current_thread")]
async fn graceful_upgrade_drops_retained_resources_with_server_shutdown() {
    use phux_protocol::wire::frame::CloseReason;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(Box::pin(async {
            let (mut old, wire, running_wire, tokens) = old_image_with_retained_panes();
            let blob = old.build_upgrade_blob(7).await;
            let crossed = blob
                .panes
                .iter()
                .find(|p| Some(p.wire_id) == wire.local_id())
                .expect("the pane is in the blob");
            assert_eq!(
                crossed.retained_exit.map(|exit| exit.exit_status),
                Some(Some(5)),
                "marked, with its exit, for the new image to close"
            );
            let live_blob = blob
                .panes
                .iter()
                .find(|p| Some(p.wire_id) == running_wire.local_id())
                .expect("the running pane is in the blob");
            assert_eq!(
                (live_blob.retained_exit, live_blob.retain_secs),
                (None, Some(60)),
                "a running pane's retention request crosses"
            );
            assert_eq!(
                (crossed.master_fd, crossed.child_pid),
                (None, None),
                "no PTY crosses for a retained pane"
            );

            let resumed = SharedState::new();
            let root = CancellationToken::new();
            resume_session_tree(&resumed, &blob, &root).expect("resume");
            let pane = resumed
                .with(|s| s.resource_from_wire(&wire))
                .expect("rebuilt, to be closed");
            let running_pane = resumed
                .with(|s| s.resource_from_wire(&running_wire))
                .expect("the running pane is rebuilt");
            assert_eq!(
                resumed.with(|s| s.retain_request(running_pane)),
                Some(60),
                "its retention request survived the upgrade"
            );
            assert!(
                resumed
                    .with_mut(|s| s.retain_exited(
                        running_pane,
                        phux_core::process::ExitOutcome::exited(1),
                        2
                    ))
                    .is_some(),
                "so it is still retained at exit"
            );
            let (tx, mut rx) = tokio::sync::mpsc::channel(8);
            resumed.with_mut(|s| {
                let client = s.new_client_id();
                s.subscribe_resource(client, pane, Some(tx));
            });
            assert_eq!(
                next_close_of(&mut rx, &wire).await,
                (CloseReason::ServerShutdown, Some(5)),
                "closed as a shutdown, carrying the exit it was retained with"
            );
            assert!(
                resumed.with(|s| s.registry().resource(pane).is_none()),
                "the resumed image reaped it"
            );
            for token in &tokens {
                token.cancel();
            }
        }))
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn graceful_upgrade_retires_durable_screens_without_purging_results() {
    use phux_core::{Delta, ExitPolicy, Transition};
    use phux_protocol::wire::frame::CloseReason;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (mut old, wire, _, tokens) = old_image_with_retained_panes();
            let pane = old.resource_from_wire(&wire).unwrap();
            let mut record = old.registry().record(pane).unwrap().clone();
            let key = record.key;
            record.on_exit = ExitPolicy::Retain;
            record.durable_result = true;
            old.registry_mut().commit(&Transition {
                deltas: vec![Delta::PutResource(record)],
                ..Transition::default()
            });
            old.activation_ended(pane, phux_core::ExitOutcome::exited(5), false);
            let blob = old.build_upgrade_blob(7).await;
            let resumed = SharedState::new();
            let root = CancellationToken::new();
            resume_session_tree(&resumed, &blob, &root).unwrap();
            let pane = resumed
                .with(|state| state.resource_from_wire(&wire))
                .unwrap();
            let (tx, mut rx) = tokio::sync::mpsc::channel(8);
            resumed.with_mut(|state| {
                let client = state.new_client_id();
                state.subscribe_resource(client, pane, Some(tx));
            });
            assert_eq!(
                next_close_of(&mut rx, &wire).await,
                (CloseReason::ServerShutdown, Some(5))
            );
            resumed.with(|state| {
                let record = state
                    .org()
                    .resource(&key)
                    .expect("durable result survives handoff screen retirement");
                assert!(record.durable_result);
                assert_eq!(
                    record.activation.as_ref().unwrap().state,
                    phux_core::ExecState::Completed
                );
                assert!(state.resource_handle(pane).is_none());
            });
            root.cancel();
            for token in tokens {
                token.cancel();
            }
        })
        .await;
}

/// A well-formed empty handoff rebuilds, then the adopted listener accepts.
#[tokio::test(flavor = "current_thread")]
async fn transactional_resume_serves_on_the_adopted_listener() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let (path, listener_fd) = bind_resume_listener(&dir);
            let blob = resume_handoff_blob(listener_fd, Vec::new(), Vec::new(), Vec::new());
            let resume_fd = write_resume_fd(&blob);
            let cfg = ServerConfig {
                socket_path: path.clone(),
                ..ServerConfig::with_default_socket()
            };
            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
            let server = tokio::task::spawn_local(async move {
                ServerRuntime::new(cfg)
                    .resume(resume_fd)
                    .overlay_detect(Vec::new)
                    .run_async(async move {
                        let _ = shutdown_rx.await;
                    })
                    .await
            });

            let mut connected = false;
            for _ in 0..50 {
                if tokio::net::UnixStream::connect(&path).await.is_ok() {
                    connected = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            assert!(
                connected,
                "a successful resume must accept on the adopted listener"
            );
            let _ = shutdown_tx.send(());
            server
                .await
                .expect("server task")
                .expect("resumed server shuts down cleanly");
        })
        .await;
}

/// An upgrade into a posture a cold start refuses (no mode beside a
/// remote listener, ADR-0179) keeps serving the adopted socket, binds no
/// remote listener, and reports why in `GET_STATE`; the same
/// configuration cold refuses to start.
#[tokio::test(flavor = "current_thread")]
async fn a_refused_posture_on_resume_keeps_the_panes_and_drops_the_remote_door() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let quic: SocketAddr = "127.0.0.1:0".parse().expect("addr");
            let dir = tempfile::tempdir().expect("tempdir");
            let cold = ServerConfig {
                socket_path: dir.path().join("cold.sock"),
                ..ServerConfig::with_default_socket()
            };
            let refused = ServerRuntime::new(cold)
                .listen_quic(quic)
                .overlay_detect(Vec::new)
                .run_async(std::future::pending())
                .await
                .expect_err("a cold start refuses no mode beside a listener");
            assert!(
                matches!(
                    refused,
                    ServerError::Policy(crate::policy::PostureError::UnsetWithRemoteListener)
                ),
                "{refused:?}"
            );

            let (path, listener_fd) = bind_resume_listener(&dir);
            let blob = resume_handoff_blob(listener_fd, Vec::new(), Vec::new(), Vec::new());
            let resume_fd = write_resume_fd(&blob);
            let cfg = ServerConfig {
                socket_path: path.clone(),
                ..ServerConfig::with_default_socket()
            };
            let (report_tx, report_rx) = tokio::sync::oneshot::channel();
            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
            let server = tokio::task::spawn_local(async move {
                ServerRuntime::new(cfg)
                    .resume(resume_fd)
                    .listen_quic(quic)
                    .overlay_detect(Vec::new)
                    .on_listening(move |report| {
                        let _ = report_tx.send(report.clone());
                    })
                    .run_async(async move {
                        let _ = shutdown_rx.await;
                    })
                    .await
            });

            let report = report_rx.await.expect("the resumed server reports");
            assert_eq!(
                report.listeners,
                vec![RemoteListenerSlot::disabled(
                    RemoteListenerTransport::Quic,
                    Some(quic.to_string()),
                    ListenerDisabledReason::PolicyModeRefused,
                )],
            );
            assert!(
                tokio::net::UnixStream::connect(&path).await.is_ok(),
                "the resumed server serves the adopted socket"
            );
            let _ = shutdown_tx.send(());
            server
                .await
                .expect("server task")
                .expect("resumed server shuts down cleanly");
        })
        .await;
}

/// A dangling topology must not start accept loops: `run_async` returns
/// the rebuild error and the adopted listener is gone.
#[tokio::test(flavor = "current_thread")]
async fn dangling_blob_refuses_to_serve_after_resume() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let (path, listener_fd) = bind_resume_listener(&dir);
            let blob = resume_handoff_blob(
                listener_fd,
                vec![crate::upgrade::blob::SessionBlob {
                    wire_id: 1,
                    name: "main".to_owned(),
                    window_wire_ids: vec![99],
                    active_window: None,
                    created_at_unix_nanos: 0,
                    last_touched: None,
                    root: None,
                    keep_empty: false,
                    key: None,
                }],
                Vec::new(),
                Vec::new(),
            );
            let resume_fd = write_resume_fd(&blob);
            let cfg = ServerConfig {
                socket_path: path.clone(),
                ..ServerConfig::with_default_socket()
            };
            let err = ServerRuntime::new(cfg)
                .resume(resume_fd)
                .overlay_detect(Vec::new)
                .run_async(std::future::pending())
                .await
                .expect_err("incomplete topology must fail closed");
            match err {
                ServerError::Rebuild(crate::state::RebuildError::DanglingRef { kind, id }) => {
                    assert_eq!(kind, "window");
                    assert_eq!(id, 99);
                }
                other => panic!("expected dangling rebuild error, got {other:?}"),
            }
            assert!(
                tokio::net::UnixStream::connect(&path).await.is_err(),
                "rebuild failure must not keep the adopted listener serving"
            );
        })
        .await;
}
