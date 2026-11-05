//! Real exit-watch and committed-close fanout cannot precede aggregate readiness.

use super::*;

fn watched_tree(
    state: &SharedState,
    parent: phux_core::ids::ResourceId,
    session: phux_core::ids::SessionId,
) -> (phux_core::ids::ResourceId, ResourceHandle) {
    let core = state.with_mut(|s| {
        let core = s
            .create_document(
                parent,
                LaunchSpec::Tree(phux_core::resource::TreeLaunch {
                    target: "watched".to_owned(),
                }),
                &Actor::owner(),
            )
            .unwrap();
        s.move_resource_to(core, session, &Actor::owner()).unwrap();
        core
    });
    let token = CancellationToken::new();
    let bundle = DocumentActor::<TreeDocument>::build(parent, "watched", token.clone());
    let handle = bundle.handle;
    state.with_mut(|s| {
        s.register_resource_handle(core, handle.clone(), token.clone())
            .unwrap();
    });
    crate::runtime::connections::spawn_resource_exit_watcher(
        state.clone(),
        core,
        Some(bundle.exit_notify),
        CancellationToken::new(),
        None,
    );
    tokio::task::spawn_local(bundle.actor.run());
    let mut output = handle.output.subscribe();
    // A producer chooses to exit after publishing its final document. The
    // second real connection triggers that publish through production dispatch.
    tokio::task::spawn_local(async move {
        while let Ok(event) = output.recv().await {
            if let ResourceOutput::Live { bytes, .. } = event
                && bytes.as_ref() == b"{\"close\":true}"
            {
                token.cancel();
                return;
            }
        }
    });
    (core, handle)
}

async fn close_from(producer: &mut Connection, wire: &phux_protocol::ids::ResourceId, kill: bool) {
    let command = if kill {
        phux_protocol::wire::frame::Command::KillResource {
            terminal_id: wire.clone(),
            operation_id: None,
        }
    } else {
        phux_protocol::wire::frame::Command::PublishResourceState {
            terminal_id: wire.clone(),
            bytes: b"{\"close\":true}".to_vec(),
        }
    };
    producer
        .send(FrameKind::Command {
            request_id: 77,
            command,
        })
        .await;
    assert!(matches!(
        producer.next().await,
        FrameKind::CommandResult {
            request_id: 77,
            result: phux_protocol::wire::frame::CommandResult::Ok
                | phux_protocol::wire::frame::CommandResult::OkWith(_),
            ..
        }
    ));
}

async fn wait_reaped(state: &SharedState, core: phux_core::ids::ResourceId) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while state.with(|s| s.resource_handle(core).is_some()) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual watcher/kill reaped the captured resource");
}

async fn close_during_capture(kill: bool, cancel: bool) {
    let state = SharedState::new();
    let (session, parent) = parent(&state, false);
    let (core, handle) = watched_tree(&state, parent, session);
    publish::<TreeDocument>(&handle, Bytes::from_static(b"{\"initial\":true}")).await;
    let (_, parked, actor) = parked_tree(&state, parent, session);
    let wire = state.with_mut(|s| s.intern_resource_wire(core));
    let mut viewer = Connection::start(&state, false).await;
    let mut producer = Connection::start(&state, false).await;
    viewer.attach(1).await;
    wait_for_capture(&parked).await;
    close_from(&mut producer, &wire, kill).await;
    wait_reaped(&state, core).await;
    assert!(
        viewer.rx.try_recv().is_err(),
        "CLOSE must not escape ahead of ATTACHED"
    );
    if cancel {
        let viewer_client = viewer.client;
        viewer.finish().await; // must not wait for the later actor's reply
        assert!(state.with(|s| !s.attached().contains_key(&viewer_client)));
        drop(actor);
    } else {
        tokio::task::spawn_local(actor.run());
        prefix(&mut viewer, 2, 1).await;
        assert!(
            matches!(viewer.next().await, FrameKind::ResourceClosed { terminal_id, .. } if terminal_id == wire)
        );
        viewer.send(FrameKind::Ping { nonce: 99 }).await;
        assert!(matches!(viewer.next().await, FrameKind::Pong { nonce: 99 }));
        viewer.finish().await;
    }
    producer.finish().await;
}

#[tokio::test(flavor = "current_thread")]
async fn actual_exit_watcher_waits_for_all_bootstraps_and_attach_ready() {
    LocalSet::new()
        .run_until(close_during_capture(false, false))
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn committed_kill_waits_for_aggregate_publication_without_delaying_its_reply() {
    LocalSet::new()
        .run_until(close_during_capture(true, false))
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn connection_cancellation_releases_close_fences_while_later_capture_is_parked() {
    LocalSet::new()
        .run_until(close_during_capture(false, true))
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_during_a_full_bootstrap_mailbox_releases_publication_fence() {
    LocalSet::new()
        .run_until(async {
            let state = SharedState::new();
            let (session, parent) = parent(&state, false);
            let (_, handle) = watched_tree(&state, parent, session);
            let payload = format!("\"{}\"", "x".repeat(8 * 1024 * 1024));
            publish::<TreeDocument>(&handle, Bytes::from(payload)).await;
            let viewer = Connection::start_bounded(&state, false, 4).await;
            viewer.attach(1).await;
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while viewer.rx.capacity() > 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("bootstrap filled the transport writer mailbox");
            let publication = state.with(|s| {
                s.attached()[&viewer.client]
                    .publication_gate
                    .clone()
                    .unwrap()
            });
            assert!(!publication.is_cancelled());
            viewer.finish().await;
            assert!(
                publication.is_cancelled(),
                "cancellation released the lifecycle publication fence"
            );
        })
        .await;
}
