//! Wire-level host consumer regressions, with no provider process or production socket.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use bytes::BytesMut;
use phux_client::native_agent::{ClientError, NativeController};
use phux_protocol::caps::{
    BootstrapCapabilities, ServerCapabilities, ServerFeatureExt, ServerFeatureExtSet,
    select_bootstrap_profile,
};
use phux_protocol::native_agent::{
    MAX_NATIVE_PENDING, NativeCommand, NativeCommandResult, NativeDisposition, NativeIntent,
    NativePublicationReceipt, NativeRequest, NativeUpdate,
};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode, FrameKind};
use phux_protocol::{PROTOCOL_VERSION, ResourceId};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, Notify};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::timeout;

const DEADLINE: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Broker {
    generation: AtomicU64,
    advertises_native: AtomicBool,
    lose_acks: AtomicUsize,
    publishes: Mutex<Vec<(u64, u64, NativeUpdate)>>,
    accepted: Mutex<HashMap<u64, (NativeUpdate, NativePublicationReceipt)>>,
    completions: Mutex<Vec<NativeCommandResult>>,
    receive_started: Notify,
    release_receive: Notify,
    fenced: Notify,
    fence_sent: Notify,
    delay_registration_for_fence: AtomicBool,
    refuse_publish: Mutex<Option<ErrorCode>>,
    refuse_complete: Mutex<Option<ErrorCode>>,
    refuse_close: Mutex<Option<ErrorCode>>,
    closes: AtomicUsize,
}

struct Fixture {
    dir: tempfile::TempDir,
    task: JoinHandle<()>,
    broker: Arc<Broker>,
}

impl Fixture {
    fn start(lost_acks: usize) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(dir.path().join("native.sock")).unwrap();
        let broker = Arc::new(Broker::default());
        broker.advertises_native.store(true, Ordering::Release);
        broker.lose_acks.store(lost_acks, Ordering::Release);
        let owned = Arc::clone(&broker);
        let task = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    result = listener.accept() => {
                        let (stream, _) = result.unwrap();
                        let broker = Arc::clone(&owned);
                        clients.spawn(async move { serve(stream, broker).await; });
                    }
                    _ = clients.join_next(), if !clients.is_empty() => {}
                }
            }
        });
        Self { dir, task, broker }
    }

    async fn controller(&self) -> NativeController {
        NativeController::connect(
            self.dir.path().join("native.sock"),
            ResourceId::local(22),
            "activation-A".into(),
            "private-binding".into(),
        )
        .await
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn read_frame(stream: &mut UnixStream) -> Option<FrameKind> {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length).await.ok()?;
    let body = u32::from_be_bytes(length) as usize;
    let mut bytes = vec![0_u8; body + 4];
    bytes[..4].copy_from_slice(&length);
    stream.read_exact(&mut bytes[4..]).await.ok()?;
    let (frame, rest) = FrameKind::decode(&bytes).unwrap();
    assert!(rest.is_empty());
    Some(frame)
}

async fn send(stream: &mut UnixStream, frame: FrameKind) {
    let mut bytes = BytesMut::new();
    frame.encode(&mut bytes);
    let _ = stream.write_all(&bytes).await;
}

fn json<T: serde::Serialize>(value: &T) -> CommandResult {
    CommandResult::OkWith(CommandValue::Json(serde_json::to_string(value).unwrap()))
}

enum PeerResponse {
    Reply(CommandResult),
    Fenced,
    LostAck,
}

async fn handshake(stream: &mut UnixStream, broker: &Broker) {
    let Some(FrameKind::Hello { client_caps, .. }) = read_frame(stream).await else {
        panic!("HELLO required")
    };
    let (selected_profile, bootstrap_limits) =
        select_bootstrap_profile(&client_caps, &BootstrapCapabilities::new()).unwrap();
    send(
        stream,
        FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new().with_features_ext(
                if broker.advertises_native.load(Ordering::Acquire) {
                    ServerFeatureExtSet::with(&[ServerFeatureExt::NativeAgent])
                } else {
                    ServerFeatureExtSet::new()
                },
            ),
            server_id: vec![],
            selected_profile,
            bootstrap_limits,
        },
    )
    .await;
}

async fn register_response(broker: &Broker) -> CommandResult {
    let generation = broker.generation.fetch_add(1, Ordering::AcqRel) + 1;
    broker.fenced.notify_waiters();
    if generation > 1 && broker.delay_registration_for_fence.load(Ordering::Acquire) {
        // Deliver the old receive's refusal before Register's JSON,
        // forcing the client's cross-connection generation race.
        broker.fence_sent.notified().await;
    }
    json(&serde_json::json!({"generation": generation}))
}

async fn receive_response(broker: &Broker, activation: String, generation: u64) -> PeerResponse {
    assert_eq!(generation, broker.generation.load(Ordering::Acquire));
    broker.receive_started.notify_one();
    tokio::select! {
        () = broker.release_receive.notified() => PeerResponse::Reply(json(&NativeCommand {
            operation_id: 91,
            activation,
            intent: NativeIntent::Prompt {
                text: "one actual prompt".into(),
            },
        })),
        () = broker.fenced.notified() => PeerResponse::Fenced,
    }
}

async fn publication_response(
    broker: &Broker,
    generation: u64,
    publication_id: u64,
    update: NativeUpdate,
) -> PeerResponse {
    assert_eq!(generation, broker.generation.load(Ordering::Acquire));
    broker
        .publishes
        .lock()
        .await
        .push((generation, publication_id, update.clone()));
    let refusal = *broker.refuse_publish.lock().await;
    if let Some(code) = refusal {
        return PeerResponse::Reply(CommandResult::Error {
            code,
            message: "native source unavailable".into(),
        });
    }
    let receipt = {
        let mut accepted = broker.accepted.lock().await;
        let seq = accepted.len() as u64 + 1;
        let (original, receipt) = accepted.entry(publication_id).or_insert_with(|| {
            (
                update.clone(),
                NativePublicationReceipt {
                    publication_id,
                    first_seq: seq,
                    last_seq: seq,
                },
            )
        });
        assert_eq!(
            &update, original,
            "changed payload must not reuse an identity"
        );
        let receipt = receipt.clone();
        drop(accepted);
        receipt
    };
    if broker
        .lose_acks
        .try_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
            remaining.checked_sub(1)
        })
        .is_ok()
    {
        return PeerResponse::LostAck; // Acceptance happened; only its acknowledgement was lost.
    }
    PeerResponse::Reply(json(&receipt))
}

async fn completion_response(
    broker: &Broker,
    generation: u64,
    result: NativeCommandResult,
) -> CommandResult {
    assert_eq!(generation, broker.generation.load(Ordering::Acquire));
    broker.completions.lock().await.push(result);
    let refusal = *broker.refuse_complete.lock().await;
    refusal.map_or(CommandResult::Ok, |code| CommandResult::Error {
        code,
        message: "server upgrading; retry completion".into(),
    })
}

async fn close_response(broker: &Broker, generation: u64) -> CommandResult {
    assert_eq!(generation, broker.generation.load(Ordering::Acquire));
    broker.closes.fetch_add(1, Ordering::AcqRel);
    let refusal = *broker.refuse_close.lock().await;
    refusal.map_or(CommandResult::Ok, |code| CommandResult::Error {
        code,
        message: "server upgrading; retry close".into(),
    })
}

async fn serve(mut stream: UnixStream, broker: Arc<Broker>) {
    handshake(&mut stream, &broker).await;
    while let Some(frame) = read_frame(&mut stream).await {
        let FrameKind::Command {
            request_id,
            command: Command::NativeAgent { request },
        } = frame
        else {
            panic!("controller must use only dedicated native requests");
        };
        let response = match request.into_request() {
            NativeRequest::Register {
                agent_session_id,
                activation,
                binding_proof,
            } => {
                assert_binding(&agent_session_id, &activation, &binding_proof);
                PeerResponse::Reply(register_response(&broker).await)
            }
            NativeRequest::Receive {
                agent_session_id,
                activation,
                binding_proof,
                generation,
            } => {
                assert_binding(&agent_session_id, &activation, &binding_proof);
                receive_response(&broker, activation, generation).await
            }
            NativeRequest::Publish {
                agent_session_id,
                activation,
                binding_proof,
                generation,
                publication_id,
                update,
            } => {
                assert_binding(&agent_session_id, &activation, &binding_proof);
                publication_response(&broker, generation, publication_id, update).await
            }
            NativeRequest::Complete {
                agent_session_id,
                activation,
                binding_proof,
                generation,
                result,
            } => {
                assert_binding(&agent_session_id, &activation, &binding_proof);
                PeerResponse::Reply(completion_response(&broker, generation, result).await)
            }
            NativeRequest::Close {
                agent_session_id,
                activation,
                binding_proof,
                generation,
            } => {
                assert_binding(&agent_session_id, &activation, &binding_proof);
                PeerResponse::Reply(close_response(&broker, generation).await)
            }
            request => panic!("host must never create a resource or replay input: {request:?}"),
        };
        let result = match response {
            PeerResponse::Reply(result) => result,
            PeerResponse::Fenced => {
                send(
                    &mut stream,
                    FrameKind::CommandResult {
                        request_id,
                        result: CommandResult::Error {
                            code: ErrorCode::PreconditionFailed,
                            message: "native controller generation is fenced".into(),
                        },
                    },
                )
                .await;
                broker.fence_sent.notify_one();
                continue;
            }
            PeerResponse::LostAck => return,
        };
        send(&mut stream, FrameKind::CommandResult { request_id, result }).await;
    }
}

fn assert_binding(agent: &ResourceId, activation: &str, proof: &str) {
    assert_eq!(agent, &ResourceId::local(22));
    assert_eq!(activation, "activation-A");
    assert_eq!(proof, "private-binding");
}

fn notice(text: &str) -> NativeUpdate {
    NativeUpdate::Notice {
        level: "info".into(),
        text: text.into(),
    }
}

#[tokio::test]
async fn legacy_server_is_refused_before_sending_native_registration() {
    let fixture = Fixture::start(0);
    fixture
        .broker
        .advertises_native
        .store(false, Ordering::Release);
    let result = NativeController::connect(
        fixture.dir.path().join("native.sock"),
        ResourceId::local(22),
        "activation-A".into(),
        "private-binding".into(),
    )
    .await;
    assert!(matches!(result, Err(ClientError::Protocol(_))));
    drop(result);
    assert_eq!(fixture.broker.generation.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn oversized_payloads_are_local_errors_without_publication_reservations() {
    let fixture = Fixture::start(0);
    let controller = fixture.controller().await;
    let oversized = "x".repeat(16 * 1024);
    assert!(matches!(
        controller.publish(notice(&oversized)).await,
        Err(ClientError::Invalid(_))
    ));
    assert!(matches!(
        controller
            .complete(NativeCommandResult {
                operation_id: 91,
                disposition: NativeDisposition::Completed,
                detail: Some(oversized),
            })
            .await,
        Err(ClientError::Invalid(_))
    ));
    assert!(fixture.broker.publishes.lock().await.is_empty());
    assert!(fixture.broker.completions.lock().await.is_empty());
    assert_eq!(fixture.broker.generation.load(Ordering::Acquire), 1);
    let receipt = controller.publish(notice("valid output")).await.unwrap();
    assert_eq!(receipt.publication_id, 1);
    controller.close().await.unwrap();
    drop(controller);
}

#[tokio::test]
async fn held_receive_does_not_block_publish_complete_or_close() {
    let fixture = Fixture::start(0);
    let controller = Arc::new(fixture.controller().await);
    let receiver = {
        let controller = Arc::clone(&controller);
        tokio::spawn(async move { controller.receive().await })
    };
    timeout(DEADLINE, fixture.broker.receive_started.notified())
        .await
        .unwrap();
    let receipt = timeout(DEADLINE, controller.publish(notice("draining SDK output")))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.publication_id, 1);
    let result = NativeCommandResult {
        operation_id: 91,
        disposition: NativeDisposition::Queued,
        detail: Some("UI sent only".into()),
    };
    timeout(DEADLINE, controller.complete(result.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture.broker.completions.lock().await.as_slice(),
        &[result]
    );
    fixture.broker.release_receive.notify_one();
    let command = timeout(DEADLINE, receiver).await.unwrap().unwrap().unwrap();
    assert_eq!(command.operation_id, 91);
    let receiver = {
        let controller = Arc::clone(&controller);
        tokio::spawn(async move { controller.receive().await })
    };
    timeout(DEADLINE, fixture.broker.receive_started.notified())
        .await
        .unwrap();
    timeout(DEADLINE, controller.close())
        .await
        .unwrap()
        .unwrap();
    drop(controller);
    assert!(matches!(
        timeout(DEADLINE, receiver).await.unwrap().unwrap(),
        Err(ClientError::Closed { .. })
    ));
}

#[tokio::test]
async fn lost_publication_ack_reuses_payload_id_and_original_stamped_sequences() {
    let fixture = Fixture::start(1);
    let controller = fixture.controller().await;
    let update = notice("one transcript projection");
    let receipt = timeout(DEADLINE, controller.publish(update.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        receipt,
        NativePublicationReceipt {
            publication_id: 1,
            first_seq: 1,
            last_seq: 1
        }
    );
    assert_eq!(fixture.broker.generation.load(Ordering::Acquire), 2);
    assert_eq!(
        fixture.broker.publishes.lock().await.as_slice(),
        &[(1, 1, update.clone()), (2, 1, update)]
    );
    let next = controller.publish(notice("next output")).await.unwrap();
    drop(controller);
    assert_eq!(
        next,
        NativePublicationReceipt {
            publication_id: 2,
            first_seq: 2,
            last_seq: 2
        }
    );
    assert_eq!(fixture.broker.accepted.lock().await.len(), 2);
}

#[tokio::test]
async fn pending_output_is_bounded_and_failed_calls_keep_their_first_identity() {
    let fixture = Fixture::start(usize::MAX);
    let controller = fixture.controller().await;
    for index in 0..MAX_NATIVE_PENDING {
        let error = timeout(
            DEADLINE,
            controller.publish(notice(&format!("output {index}"))),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(error.is_retryable());
    }
    let before = fixture.broker.publishes.lock().await.len();
    assert!(
        matches!(controller.publish(notice("one beyond the bound")).await, Err(ClientError::Invalid(message)) if message.contains("queue is full"))
    );
    assert_eq!(fixture.broker.publishes.lock().await.len(), before);
    assert!(
        fixture
            .broker
            .publishes
            .lock()
            .await
            .iter()
            .all(|(_, id, update)| *id == 1 && *update == notice("output 0"))
    );
    fixture.broker.lose_acks.store(0, Ordering::Release);
    let receipt = controller.publish(notice("output 0")).await.unwrap();
    drop(controller);
    assert_eq!(receipt.publication_id, 1);
}

#[tokio::test]
async fn closed_resource_and_refusal_are_not_reconnected_as_transport_failures() {
    for code in [ErrorCode::PermissionDenied, ErrorCode::TerminalNotFound] {
        let fixture = Fixture::start(0);
        *fixture.broker.refuse_publish.lock().await = Some(code);
        let controller = fixture.controller().await;
        let error = controller
            .publish(notice("not accepted"))
            .await
            .unwrap_err();
        assert!(!error.is_retryable());
        match code {
            ErrorCode::TerminalNotFound => {
                assert!(matches!(error, ClientError::Closed { .. }));
                assert!(matches!(
                    controller.receive().await,
                    Err(ClientError::Closed { .. })
                ));
            }
            _ => assert!(
                matches!(error, ClientError::Refused { code: actual, .. } if actual == code)
            ),
        }
        drop(controller);
        assert_eq!(fixture.broker.generation.load(Ordering::Acquire), 1);
        assert_eq!(fixture.broker.publishes.lock().await.len(), 1);
    }
}

#[tokio::test]
async fn publication_reconnect_fences_receive_even_when_its_refusal_precedes_register_ack() {
    let fixture = Fixture::start(1);
    fixture
        .broker
        .delay_registration_for_fence
        .store(true, Ordering::Release);
    let controller = Arc::new(fixture.controller().await);
    let receiver = {
        let controller = Arc::clone(&controller);
        tokio::spawn(async move { controller.receive().await })
    };
    timeout(DEADLINE, fixture.broker.receive_started.notified())
        .await
        .unwrap();
    let receipt = timeout(
        DEADLINE,
        controller.publish(notice("ack lost while receiving")),
    )
    .await
    .unwrap()
    .unwrap();
    drop(controller);
    assert_eq!(receipt.publication_id, 1);
    timeout(DEADLINE, fixture.broker.receive_started.notified())
        .await
        .unwrap();
    fixture.broker.release_receive.notify_one();
    let command = timeout(DEADLINE, receiver).await.unwrap().unwrap().unwrap();
    assert_eq!(command.operation_id, 91);
    assert_eq!(fixture.broker.generation.load(Ordering::Acquire), 2);
}

#[tokio::test]
async fn sealed_actor_backpressure_retries_the_same_publication_without_rebinding() {
    let fixture = Fixture::start(0);
    let controller = fixture.controller().await;
    *fixture.broker.refuse_publish.lock().await = Some(ErrorCode::Overflow);
    let update = notice("output retained across actor seal");
    let error = controller.publish(update.clone()).await.unwrap_err();
    assert!(error.is_retryable());
    assert!(
        matches!(error, ClientError::Busy { message } if message == "native source unavailable")
    );
    assert_eq!(fixture.broker.generation.load(Ordering::Acquire), 1);
    assert!(fixture.broker.accepted.lock().await.is_empty());
    *fixture.broker.refuse_publish.lock().await = None;
    let receipt = controller.publish(update.clone()).await.unwrap();
    drop(controller);
    assert_eq!(
        receipt,
        NativePublicationReceipt {
            publication_id: 1,
            first_seq: 1,
            last_seq: 1
        }
    );
    assert_eq!(fixture.broker.generation.load(Ordering::Acquire), 1);
    assert_eq!(
        fixture.broker.publishes.lock().await.as_slice(),
        &[(1, 1, update.clone()), (1, 1, update)]
    );
}

#[tokio::test]
async fn sealed_cut_keeps_completion_identity_and_does_not_claim_busy_close_succeeded() {
    let fixture = Fixture::start(0);
    let controller = fixture.controller().await;
    let result = NativeCommandResult {
        operation_id: 91,
        disposition: NativeDisposition::Completed,
        detail: Some("actual provider completion".into()),
    };
    *fixture.broker.refuse_complete.lock().await = Some(ErrorCode::Overflow);
    let error = controller.complete(result.clone()).await.unwrap_err();
    assert!(matches!(&error, ClientError::Busy { .. }));
    assert!(error.is_retryable());
    *fixture.broker.refuse_complete.lock().await = None;
    controller.complete(result.clone()).await.unwrap();
    assert_eq!(
        fixture.broker.completions.lock().await.as_slice(),
        &[result.clone(), result]
    );
    assert_eq!(fixture.broker.generation.load(Ordering::Acquire), 1);
    *fixture.broker.refuse_close.lock().await = Some(ErrorCode::Overflow);
    let error = controller.close().await.unwrap_err();
    assert!(matches!(&error, ClientError::Busy { .. }));
    assert!(error.is_retryable());
    assert_eq!(fixture.broker.closes.load(Ordering::Acquire), 1);
    *fixture.broker.refuse_close.lock().await = None;
    controller.close().await.unwrap();
    controller.close().await.unwrap();
    drop(controller);
    assert_eq!(fixture.broker.closes.load(Ordering::Acquire), 2);
    assert_eq!(fixture.broker.generation.load(Ordering::Acquire), 1);
}
