use super::*;
use crate::hub::relay::{HubRelays, RelayHandle, RelayMailbox, RelayRequest};
use crate::runtime::keyed_ops::{KeyedAdmission, admit};
use phux_protocol::ids::{IdempotencyKey, ResourceId, SatelliteHost};
use phux_protocol::wire::frame::CommandValue;
use phux_terminal::ControlRequest;

/// A hub whose satellite `up` has a live link, answered by the test, and
/// whose satellite `down` has none.
fn hub() -> (SharedState, ClientId, RelayMailbox) {
    let state = SharedState::new();
    let client = state.with_mut(crate::state::ServerState::new_client_id);
    let relays = HubRelays::default();
    let (up, up_mailbox) = RelayHandle::new(SatelliteHost::new("up"));
    let (down, down_mailbox) = RelayHandle::new(SatelliteHost::new("down"));
    drop(down_mailbox);
    relays.insert(up);
    relays.insert(down);
    state.with_mut(|s| s.set_hub_relays(relays));
    (state, client, up_mailbox)
}

fn outcomes(result: CommandResult) -> serde_json::Value {
    let CommandResult::OkWith(CommandValue::Json(document)) = result else {
        panic!("a keyed batch answers per-id outcomes, got {result:?}");
    };
    serde_json::from_str(&document).expect("json")
}

/// L1 §5.2: a keyed batch split across hosts awaits every host, and is
/// keyed at every hop: the satellite's own per-id outcome is merged, so a
/// stale satellite id is `not_found`, not `killed`.
#[tokio::test(flavor = "current_thread")]
async fn kill_resources_across_hosts_merges_results_instead_of_fire_and_forget() {
    let (state, client, mut up) = hub();
    let satellite = tokio::spawn(async move {
        let Some(RelayRequest::Keyed { command, reply, .. }) = up.requests.recv().await else {
            panic!("the keyed batch reaches the satellite's link");
        };
        let _ = reply.send(CommandResult::OkWith(CommandValue::Json(
            r#"{"schema_version":1,"killed":["@3"],"not_found":["@5"],"failed":[]}"#.to_owned(),
        )));
        command
    });

    let key = IdempotencyKey::new([9; 16]);
    let ids = [
        ResourceId::local(77),
        ResourceId::satellite("up", 3),
        ResourceId::satellite("up", 5),
        ResourceId::satellite("down", 4),
    ];
    let result = handle_kill_terminals(&state, client, &ids, key).await;
    assert_eq!(
        satellite.await.expect("satellite task"),
        Command::KillResources {
            ids: vec![ResourceId::local(3), ResourceId::local(5)],
            operation_id: key,
        },
        "each host gets its own ids, under the batch's key"
    );
    let document = outcomes(result);
    assert_eq!(document["killed"], serde_json::json!(["up/@3"]));
    assert_eq!(
        document["not_found"],
        serde_json::json!(["@77", "up/@5"]),
        "a stale satellite id is not reported killed"
    );
    assert_eq!(document["failed"][0]["id"], "down/@4");
    assert_eq!(
        document["failed"][0]["code"],
        ErrorCode::SatelliteUnreachable.as_wire()
    );

    assert_eq!(
        outcomes(
            handle_kill_terminals(
                &state,
                client,
                &[ResourceId::local(78)],
                IdempotencyKey::new([10; 16]),
            )
            .await
        )["not_found"],
        serde_json::json!(["@78"]),
        "what a satellite answers a keyed batch naming a stale id"
    );
}

/// An unkeyed batch keeps the reply every older client understands: it
/// awaits every host, and answers `OK` even when one could not be reached.
#[tokio::test(flavor = "current_thread")]
async fn an_unkeyed_cross_host_batch_keeps_its_ok_reply() {
    let (state, client, mut up) = hub();
    let satellite = tokio::spawn(async move {
        let Some(RelayRequest::Command { command, reply, .. }) = up.requests.recv().await else {
            panic!("the unkeyed batch relays as a plain command");
        };
        let _ = reply.send(CommandResult::Ok);
        command
    });
    let ids = [
        ResourceId::satellite("up", 3),
        ResourceId::satellite("down", 4),
    ];
    assert_eq!(
        handle_kill_terminals(&state, client, &ids, None).await,
        CommandResult::Ok
    );
    assert_eq!(
        satellite.await.expect("satellite task"),
        Command::KillResources {
            ids: vec![ResourceId::local(3)],
            operation_id: None,
        }
    );
}

/// A keyed signal the first attempt owns, committed on its actor queue.
struct CommittedSignal {
    state: SharedState,
    command: Command,
    claim: crate::runtime::operation_dedupe::OperationClaim,
    actor: tokio::sync::mpsc::Receiver<ControlRequest>,
    reply_rx: oneshot::Receiver<Result<(), String>>,
}

async fn committed_signal() -> CommittedSignal {
    let state = SharedState::new();
    let key = IdempotencyKey::new([4; 16]);
    let command = Command::SignalTerminal {
        terminal_id: ResourceId::local(3),
        signal: phux_protocol::wire::frame::TerminalSignal::Interrupt,
        operation_id: key,
    };
    let KeyedAdmission::Owner(claim) = admit(&state, &command).await else {
        panic!("the first attempt owns the key");
    };
    let (control, actor) = tokio::sync::mpsc::channel(4);
    let (reply, reply_rx) = oneshot::channel();
    let request = ControlRequest::Signal {
        signal: phux_protocol::wire::frame::TerminalSignal::Interrupt,
        input_holder: None,
        by: phux_protocol::ClientId::new(1),
        operation_id: key,
        reply,
    };
    assert!(commit_signal(&control, request, Some(&claim)).await);
    CommittedSignal {
        state,
        command,
        claim,
        actor,
        reply_rx,
    }
}

/// L1 §5.1.1: a connection cancelled before the reply does not release
/// the key, so the retry answers the first result and nothing is
/// delivered twice.
#[tokio::test(flavor = "current_thread")]
async fn a_keyed_signal_cancelled_before_its_reply_is_delivered_once() {
    let CommittedSignal {
        state,
        command,
        claim,
        mut actor,
        reply_rx,
    } = committed_signal().await;
    drop(reply_rx);
    drop(claim);
    assert!(
        matches!(
            admit(&state, &command).await,
            KeyedAdmission::Answer(CommandResult::Ok)
        ),
        "the retry answers the committed signal instead of sending it again"
    );
    assert!(actor.try_recv().is_ok(), "the first attempt was queued");
    assert!(actor.try_recv().is_err(), "and nothing else was");
}

/// A committed signal whose delivery failed answers that failure on
/// retry, never the committed `OK`.
#[tokio::test(flavor = "current_thread")]
async fn a_keyed_signal_that_was_not_delivered_answers_its_failure_on_retry() {
    let CommittedSignal {
        state,
        command,
        claim,
        ..
    } = committed_signal().await;
    let failure = CommandResult::Error {
        code: ErrorCode::InternalError,
        message: "no PTY child to signal".to_owned(),
    };
    record_signal_failure(Some(&claim), &failure);
    crate::runtime::keyed_ops::settle(Some(claim), &failure);
    assert!(
        matches!(
            admit(&state, &command).await,
            KeyedAdmission::Answer(CommandResult::Error {
                code: ErrorCode::InternalError,
                ..
            })
        ),
        "the retry answers the failure, not a false OK"
    );
}
