//! Exercise the shared producer without a socket, actor, or payload logging.
#![allow(
    clippy::wildcard_imports,
    reason = "the attach test topics share the parent namespace"
)]
use super::*;
use phux_terminal_api::actor::TerminalResource as _;
use std::sync::{Arc, Mutex};

pub(super) type Captured = Arc<Mutex<Vec<u8>>>;

struct Writer(Captured);

impl std::io::Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn capture() -> (Captured, tracing::subscriber::DefaultGuard) {
    let bytes = Captured::default();
    let writer = bytes.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter("off,phux_server::publication_diagnostics=debug")
        .with_ansi(false)
        .without_time()
        .json()
        .with_writer(move || Writer(writer.clone()))
        .finish();
    (bytes, tracing::subscriber::set_default(subscriber))
}

pub(super) fn assert_event(bytes: &Captured, phase: &str, outcome: &str) {
    let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    let rows: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(!text.contains("private-context-sentinel"));
    assert!(!text.contains("private-address-sentinel"));
    assert!(
        rows.iter()
            .any(|row| row["fields"]["phase"] == phase && row["fields"]["outcome"] == outcome),
        "missing {phase}/{outcome}: {text}"
    );
    for row in &rows {
        let fields = row["fields"].as_object().unwrap();
        assert_eq!(
            fields.len(),
            2,
            "no payload, identity, or raw errors in diagnostic events"
        );
        assert!(fields.values().all(serde_json::Value::is_string));
    }
}

#[test]
fn normal_info_logging_does_not_emit_extra_publication_events() {
    let bytes = Captured::default();
    let writer = bytes.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || Writer(writer.clone()))
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        super::super::bootstrap::publication_phase("native_activation", "started");
        PumpFault::PublicationNotActivated.record();
    });
    assert!(bytes.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn activation_refusal_and_actor_loss_are_not_capture_deadlines() {
    let (bytes, _guard) = capture();
    let _context = tracing::info_span!(
        target: "phux_server::publication_diagnostics",
        "private-context-sentinel",
        endpoint = "private-address-sentinel"
    )
    .entered();
    let (handle, _, _, mut requests) = native_attach_handle();
    let terminal = handle.terminal().unwrap();
    let activate = activate_native_publication(
        terminal,
        7,
        phux_protocol::ids::ResourceId::local(1),
        two_pump_stream(),
        two_pump_initial_generation(),
        [9; 32],
    );
    tokio::pin!(activate);
    let request = tokio::select! {
        result = &mut activate => panic!("unexpected completion: {result:?}"),
        request = requests.recv() => request.unwrap(),
    };
    // No new activation timer: a held reply remains pending past 18 seconds.
    tokio::time::advance(std::time::Duration::from_secs(18)).await;
    assert!(futures_util::poll!(&mut activate).is_pending());
    request
        .reply
        .send(Err(
            phux_terminal_api::native_state::NativeStateError::InvalidHandle,
        ))
        .unwrap();
    assert!(matches!(
        activate.await,
        Err(NativeRequestFailure::Refused(_))
    ));
    assert_event(&bytes, "native_activation", "refused");
    drop(requests);
    let result = activate_native_publication(
        terminal,
        7,
        phux_protocol::ids::ResourceId::local(1),
        two_pump_stream(),
        two_pump_initial_generation(),
        [9; 32],
    )
    .await;
    assert!(matches!(result, Err(NativeRequestFailure::Unsent)));
    assert_event(&bytes, "native_activation", "actor_unsent");
    assert!(
        !std::str::from_utf8(&bytes.lock().unwrap())
            .unwrap()
            .contains("deadline")
    );
}

#[tokio::test]
async fn activation_success_and_dropped_reply_have_distinct_static_outcomes() {
    let (bytes, _guard) = capture();
    let (handle, _, _, mut requests) = native_attach_handle();
    let terminal = handle.terminal().unwrap();
    for answer in [true, false] {
        let activate = activate_native_publication(
            terminal,
            7,
            phux_protocol::ids::ResourceId::local(1),
            two_pump_stream(),
            two_pump_initial_generation(),
            [9; 32],
        );
        tokio::pin!(activate);
        let request = tokio::select! {
            result = &mut activate => panic!("unexpected completion: {result:?}"),
            request = requests.recv() => request.unwrap(),
        };
        if answer {
            request
                .reply
                .send(Ok(phux_terminal_api::actor::NativePublicationReply {
                    replay: Vec::new(),
                    live: handle.output.subscribe(),
                }))
                .unwrap();
            assert!(activate.await.is_ok());
        } else {
            drop(request);
            assert!(matches!(activate.await, Err(NativeRequestFailure::Dropped)));
        }
    }
    assert_event(&bytes, "native_activation", "activated");
    assert_event(&bytes, "native_activation", "actor_dropped");
}

#[tokio::test(flavor = "current_thread")]
async fn initial_session_queue_reports_success_and_outbound_close() {
    crate::runtime::test_support::local(async {
        for close_outbound in [false, true] {
            let (bytes, _guard) = capture();
            let state = SharedState::new();
            let (_, pane) = state.with_mut(|s| s.seed_session("queue-private-sentinel"));
            let (handle, mut consumers, mut captures, mut activations) = native_attach_handle();
            state.with_mut(|s| {
                s.register_resource_handle(pane, handle.clone(), CancellationToken::new())
                    .unwrap()
            });
            let client = state.with_mut(|s| s.clients_mut().new_client_id());
            let (out_tx, mut out_rx) =
                tokio::sync::mpsc::channel(if close_outbound { 1 } else { 32 });
            let token = CancellationToken::new();
            let mut pumps = JoinSet::new();
            let publishing = attach(
                &state,
                client,
                1,
                "queue-private-sentinel",
                None,
                &out_tx,
                native_profile(),
                &mut pumps,
                &token,
                false,
            );
            tokio::pin!(publishing);
            let consumer = tokio::select! {
                () = &mut publishing => panic!("attach ended before registration"),
                consumer = consumers.recv() => consumer.unwrap(),
            };
            consumer
                .reply
                .send(Ok(phux_terminal_api::actor::ConsumerAttachOutcome {
                    tick_managed: false,
                    state_sync_bootstrap: None,
                }))
                .unwrap();
            let request = tokio::select! {
                () = &mut publishing => panic!("attach ended before capture"),
                request = captures.recv() => request.unwrap(),
            };
            let checkpoint = empty_native_checkpoint(&request);
            request.reply.send(Ok(checkpoint)).unwrap();
            if close_outbound {
                let frame = tokio::select! {
                    () = &mut publishing => panic!("attach ended before its reply"),
                    frame = out_rx.recv() => frame.unwrap(),
                };
                assert!(matches!(frame, Outbound::Frame(FrameKind::Attached { .. })));
                drop(out_rx);
                publishing.await;
            } else {
                // ATTACH_READY releases the parked producer; activation
                // happens after the aggregate publication returns.
                publishing.await;
                let request = activations.recv().await.unwrap();
                request
                    .reply
                    .send(Ok(phux_terminal_api::actor::NativePublicationReply {
                        replay: Vec::new(),
                        live: handle.output.subscribe(),
                    }))
                    .unwrap();
                tokio::task::yield_now().await;
            }
            assert_event(&bytes, "session_bootstrap_queue", "started");
            assert_event(
                &bytes,
                "session_bootstrap_queue",
                if close_outbound {
                    "outbound_closed"
                } else {
                    "queued"
                },
            );
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn initial_spawn_queue_reports_success_and_outbound_close() {
    crate::runtime::test_support::local(async {
        for close_outbound in [false, true] {
            let (bytes, _guard) = capture();
            let state = SharedState::new();
            let (_, pane) = state.with_mut(|s| s.seed_session("spawn-private-sentinel"));
            let wire = state.with_mut(|s| s.idspace.intern_resource(pane));
            let client = state.with_mut(|s| s.clients_mut().new_client_id());
            let (handle, _, mut captures, mut activations) = native_attach_handle();
            let terminal = handle.terminal().unwrap().clone();
            state.with_mut(|s| {
                s.register_resource_handle(pane, handle.clone(), CancellationToken::new())
                    .unwrap()
            });
            let (out_tx, mut out_rx) =
                tokio::sync::mpsc::channel(if close_outbound { 1 } else { 32 });
            let publication = super::super::spawn_publication::SpawnPublication {
                state: &state,
                out_tx: &out_tx,
                request_id: 1,
                client_id: client,
                core_terminal_id: pane,
                wire_terminal_id: wire,
                handle: handle.clone(),
                terminal,
                client_caps: ClientCapabilities::default(),
                stream_id: two_pump_stream(),
                profile: BootstrapStreamProfile::NativeState {
                    codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1,
                },
                limits: BootstrapLimits::default(),
                bind_instance: false,
                idempotency_key: None,
            };
            let token = CancellationToken::new();
            let mut pumps = JoinSet::new();
            let publishing = publication.publish(&mut pumps, &token, false);
            tokio::pin!(publishing);
            let request = tokio::select! {
                () = &mut publishing => panic!("spawn ended before capture"),
                request = captures.recv() => request.unwrap(),
            };
            let checkpoint = empty_native_checkpoint(&request);
            request.reply.send(Ok(checkpoint)).unwrap();
            if close_outbound {
                let frame = tokio::select! {
                    () = &mut publishing => panic!("spawn ended before its reply"),
                    frame = out_rx.recv() => frame.unwrap(),
                };
                assert!(matches!(
                    frame,
                    Outbound::Frame(FrameKind::ResourceSpawned { .. })
                ));
                drop(out_rx);
            } else {
                let request = tokio::select! {
                    () = &mut publishing => panic!("spawn ended before activation"),
                    request = activations.recv() => request.unwrap(),
                };
                request
                    .reply
                    .send(Ok(phux_terminal_api::actor::NativePublicationReply {
                        replay: Vec::new(),
                        live: handle.output.subscribe(),
                    }))
                    .unwrap();
            }
            publishing.await;
            assert_event(&bytes, "spawn_bootstrap_queue", "started");
            assert_event(
                &bytes,
                "spawn_bootstrap_queue",
                if close_outbound {
                    "outbound_closed"
                } else {
                    "queued"
                },
            );
        }
    })
    .await;
}
