//! Wire-level contract for acknowledged input delivery (ADR-0053, ADR-0076).
//!
//! Not `phux_client::testkit`: that server advertises no features and acks
//! every command, so it can neither offer `ACKNOWLEDGED_INPUT` nor script an
//! `APPLY_INPUT` refusal. This one does both and records every submit's
//! operation id, so idempotency is read off the wire.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::future_not_send,
    reason = "tests; the prompt futures are !Send by design"
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::BytesMut;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::{JoinHandle, JoinSet};

use phux_client::agent_meta::{AgentMetaState, AgentRecord, RESOURCE_AGENT_KEY};
use phux_client::agent_prompt::{
    Delivery, MAX_PROMPT_BYTES, PromptError, PromptOutcome, PromptWait, Refusal,
    deliver_acknowledged, prompt_agent,
};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, ServerCapabilities, ServerFeature, ServerFeatureSet,
    select_bootstrap_profile,
};
use phux_protocol::ids::{InputOperationId, ResourceId};
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind, Scope};

/// A frame link speaking the same length-prefixed framing `Connection` does.
struct Link {
    stream: UnixStream,
    out: BytesMut,
}

impl Link {
    async fn recv(&mut self) -> Option<FrameKind> {
        let mut header = [0_u8; 4];
        match self.stream.read_exact(&mut header).await {
            Ok(_) => {}
            Err(_) => return None,
        }
        let body = usize::try_from(u32::from_be_bytes(header)).expect("length fits usize");
        let mut encoded = Vec::with_capacity(4 + body);
        encoded.extend_from_slice(&header);
        encoded.resize(4 + body, 0);
        self.stream
            .read_exact(&mut encoded[4..])
            .await
            .expect("frame body");
        let (frame, tail) = FrameKind::decode(&encoded).expect("client sent a decodable frame");
        assert!(tail.is_empty(), "trailing bytes after a client frame");
        Some(frame)
    }

    async fn send(&mut self, frame: &FrameKind) {
        self.out.clear();
        frame.encode(&mut self.out);
        if self.stream.write_all(&self.out).await.is_err() {
            return;
        }
        let _ = self.stream.flush().await;
    }
}

/// What one scripted server answers with.
#[derive(Clone)]
struct Script {
    /// The `phux.agent/v1` payload every `GET_METADATA` answers with.
    record: Option<Vec<u8>>,
    /// Answers for successive `APPLY_INPUT` submits; the last one repeats.
    apply: Vec<CommandResult>,
    /// `METADATA_CHANGED` payloads pushed strictly after each result.
    post_result: Vec<Option<Vec<u8>>>,
}

impl Script {
    fn new(record: Option<Vec<u8>>) -> Self {
        Self {
            record,
            apply: vec![CommandResult::Ok],
            post_result: Vec::new(),
        }
    }

    fn apply(mut self, results: Vec<CommandResult>) -> Self {
        self.apply = results;
        self
    }

    fn post_result(mut self, pushes: Vec<Option<Vec<u8>>>) -> Self {
        self.post_result = pushes;
        self
    }
}

/// Every operation id the server saw, in submit order.
type SeenIds = Arc<Mutex<Vec<InputOperationId>>>;

/// Owns the listener directory and the accept task; aborting it drops the
/// [`JoinSet`] and with it every session.
struct Server {
    _dir: tempfile::TempDir,
    accept: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

/// Bind a socket and serve `script` to every client that dials it.
fn serve(script: Script) -> (Server, PathBuf, SeenIds) {
    let dir = tempfile::tempdir().expect("temp dir");
    let socket = dir.path().join("phux.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let seen: SeenIds = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let accept = tokio::spawn(async move {
        let mut sessions = JoinSet::new();
        while let Ok((stream, _)) = listener.accept().await {
            let script = script.clone();
            let recorded = Arc::clone(&recorded);
            sessions.spawn(async move { session(stream, script, recorded).await });
        }
    });
    (Server { _dir: dir, accept }, socket, seen)
}

#[allow(
    clippy::too_many_lines,
    reason = "one server, one match; splitting it hides the frame ordering"
)]
async fn session(stream: UnixStream, script: Script, seen: SeenIds) {
    let mut link = Link {
        stream,
        out: BytesMut::new(),
    };
    let mut applies = 0_usize;
    while let Some(frame) = link.recv().await {
        match frame {
            FrameKind::Hello { client_caps, .. } => {
                let (selected_profile, bootstrap_limits) =
                    select_bootstrap_profile(&client_caps, &BootstrapCapabilities::new())
                        .expect("shared bootstrap profile");
                let features = ServerFeatureSet::from_wire(ServerFeature::AcknowledgedInput as u32);
                link.send(&FrameKind::HelloOk {
                    protocol_major: PROTOCOL_VERSION.major,
                    protocol_minor: PROTOCOL_VERSION.minor,
                    protocol_patch: PROTOCOL_VERSION.patch,
                    server_caps: ServerCapabilities::new().with_features(features),
                    server_id: Vec::new(),
                    selected_profile,
                    bootstrap_limits,
                })
                .await;
            }
            FrameKind::GetMetadata {
                request_id, key, ..
            } => {
                let value = (key == RESOURCE_AGENT_KEY)
                    .then(|| script.record.clone())
                    .flatten();
                link.send(&FrameKind::MetadataValue { request_id, value })
                    .await;
            }
            FrameKind::Command {
                request_id,
                command:
                    Command::ApplyInput {
                        operation_id,
                        terminal_id,
                        ..
                    },
            } => {
                seen.lock().expect("ids lock").push(operation_id);
                let result = script
                    .apply
                    .get(applies)
                    .or_else(|| script.apply.last())
                    .cloned()
                    .unwrap_or(CommandResult::Ok);
                applies = applies.saturating_add(1);
                link.send(&FrameKind::CommandResult { request_id, result })
                    .await;
                for value in script.post_result.clone() {
                    link.send(&FrameKind::MetadataChanged {
                        scope: Scope::Resource(terminal_id.clone()),
                        key: RESOURCE_AGENT_KEY.to_owned(),
                        value,
                        actor: None,
                    })
                    .await;
                }
            }
            // Subscriptions get no reply, like the real server.
            FrameKind::Command { request_id, .. } => {
                link.send(&FrameKind::CommandResult {
                    request_id,
                    result: CommandResult::Ok,
                })
                .await;
            }
            _ => {}
        }
    }
}

const fn op_id(fill: u8) -> InputOperationId {
    match InputOperationId::new([fill; 16]) {
        Some(id) => id,
        None => panic!("the fills used here are all non-zero"),
    }
}

fn record(state: &str) -> Vec<u8> {
    format!(r#"{{"name":"reviewer","kind":"claude","state":"{state}"}}"#).into_bytes()
}

fn refused(code: ErrorCode) -> CommandResult {
    CommandResult::Error {
        code,
        message: "scripted".to_owned(),
    }
}

const fn always_ok(_record: &AgentRecord) -> Option<String> {
    None
}

/// `prompt_agent` "ship it" on `@7` under `op_id(fill)`, verifying nothing.
async fn prompt(
    socket: &Path,
    fill: u8,
    wait: Option<&PromptWait>,
) -> Result<PromptOutcome, PromptError> {
    prompt_agent(
        socket,
        &ResourceId::local(7),
        "ship it",
        op_id(fill),
        &always_ok,
        wait,
    )
    .await
}

fn wait_for_idle(timeout: Duration) -> PromptWait {
    PromptWait {
        targets: vec![AgentMetaState::Idle],
        timeout: Some(timeout),
        poll_interval: Duration::from_millis(30),
    }
}

/// The happy path: one batch, and a receipt with a correlatable id.
#[tokio::test]
async fn a_verified_pane_takes_one_batch_and_reports_the_receipt() {
    let (_server, socket, seen) = serve(Script::new(Some(record("working"))));
    let outcome = prompt(&socket, 0x31, None)
        .await
        .expect("a verified pane accepts the batch");

    assert_eq!(outcome.delivery, Delivery::Acked);
    assert_eq!(outcome.attempts, 1);
    assert_eq!(outcome.agent.name, "reviewer");
    assert_eq!(outcome.pre_submit_state, AgentMetaState::Working);
    assert_eq!(outcome.operation_id.len(), 32, "16 bytes of lowercase hex");
    assert!(!outcome.transition_observed(), "no --wait was asked for");
    // One batch, not two: text and Enter never ride separate operations.
    assert_eq!(seen.lock().expect("ids").len(), 1);
}

/// The idempotency test, read off the wire: every `RESOURCE_EXHAUSTED`
/// resubmission carries the same operation id.
#[tokio::test]
async fn a_resource_exhausted_retry_reuses_the_same_operation_id() {
    let script = Script::new(Some(record("idle"))).apply(vec![
        refused(ErrorCode::ResourceExhausted),
        refused(ErrorCode::ResourceExhausted),
        CommandResult::Ok,
    ]);
    let (_server, socket, seen) = serve(script);
    let outcome = prompt(&socket, 0x42, None)
        .await
        .expect("the lane freed on the third attempt");

    assert_eq!(outcome.delivery, Delivery::Acked);
    assert_eq!(outcome.attempts, 3);
    let ids = seen.lock().expect("ids").clone();
    assert_eq!(ids.len(), 3, "the retries actually happened");
    assert!(ids.iter().all(|id| *id == op_id(0x42)));
}

/// A lane that never frees is a failure after the whole schedule, every
/// attempt under one id.
#[tokio::test]
async fn a_lane_that_never_frees_fails_without_writing_anything() {
    let script =
        Script::new(Some(record("idle"))).apply(vec![refused(ErrorCode::ResourceExhausted)]);
    let (_server, socket, seen) = serve(script);
    match prompt(&socket, 0x43, None).await {
        Err(PromptError::LaneBusy { attempts, .. }) => {
            assert!(attempts > 1, "the backoff schedule must be spent");
            let ids = seen.lock().expect("ids").clone();
            assert_eq!(usize::try_from(attempts).unwrap_or(0), ids.len());
            assert!(ids.iter().all(|id| *id == op_id(0x43)));
        }
        other => panic!("a permanently busy lane is a failure: {other:?}"),
    }
}

/// Unknown delivery (terminal), proven not-written (the caller's choice), and
/// a canonical-limit refusal are each reported distinctly and submitted
/// exactly once.
#[tokio::test]
async fn non_busy_answers_are_reported_distinctly_and_never_retried() {
    type Check = fn(&Result<PromptOutcome, PromptError>) -> bool;
    let cases: [(ErrorCode, Check); 3] = [
        (
            ErrorCode::InputDeliveryUnknown,
            |outcome| matches!(outcome, Err(PromptError::DeliveryUnknown { operation_id, .. }) if operation_id.len() == 32),
        ),
        (
            ErrorCode::InputNotWritten,
            |outcome| matches!(outcome, Err(PromptError::NotWritten { operation_id, .. }) if operation_id.len() == 32),
        ),
        (ErrorCode::CanonicalLimitExceeded, |outcome| {
            matches!(
                outcome,
                Err(PromptError::Refused(Refusal::CanonicalLimitExceeded(_)))
            )
        }),
    ];
    for (code, check) in cases {
        let script = Script::new(Some(record("working"))).apply(vec![refused(code)]);
        let (_server, socket, seen) = serve(script);
        let outcome = prompt(&socket, 0x44, None).await;
        assert!(check(&outcome), "{code:?}: {outcome:?}");
        assert_eq!(seen.lock().expect("ids").len(), 1, "{code:?} was retried");
    }
}

/// Refusals that must precede the submit: a mismatched occupant (which
/// would otherwise land in the shell an exited agent left behind), a pane
/// with no record, and an oversized prompt (never split or truncated).
#[tokio::test]
async fn pre_submit_refusals_write_nothing() {
    let (_server, socket, seen) = serve(Script::new(Some(record("idle"))));
    let mismatched = prompt_agent(
        &socket,
        &ResourceId::local(7),
        "ship it",
        op_id(0x46),
        &|found| Some(format!("'{}', not 'builder'", found.name)),
        None,
    )
    .await;
    assert!(
        matches!(&mismatched, Err(PromptError::Refused(Refusal::AgentMismatch(who))) if who.contains("reviewer")),
        "{mismatched:?}"
    );
    let oversized = prompt_agent(
        &socket,
        &ResourceId::local(7),
        &"x".repeat(MAX_PROMPT_BYTES + 1),
        op_id(0x48),
        &always_ok,
        None,
    )
    .await;
    assert!(
        matches!(
            oversized,
            Err(PromptError::Refused(Refusal::TooLarge { measured, limit, .. }))
                if measured == MAX_PROMPT_BYTES + 1 && limit == MAX_PROMPT_BYTES
        ),
        "{oversized:?}"
    );
    assert!(seen.lock().expect("ids").is_empty());

    let (_server, socket, seen) = serve(Script::new(None));
    let unrecorded = prompt(&socket, 0x47, None).await;
    assert!(
        matches!(
            unrecorded,
            Err(PromptError::Refused(Refusal::NoAgentRecord))
        ),
        "{unrecorded:?}"
    );
    assert!(seen.lock().expect("ids").is_empty());
}

/// `prompt --wait` is satisfied by a transition observed after the result,
/// on the connection that carried the submit.
#[tokio::test]
async fn a_post_result_transition_satisfies_prompt_wait() {
    let script = Script::new(Some(record("idle")))
        .post_result(vec![Some(record("working")), Some(record("idle"))]);
    let (_server, socket, _seen) = serve(script);
    let outcome = prompt(&socket, 0x49, Some(&wait_for_idle(Duration::from_secs(5))))
        .await
        .expect("an observed transition is a success");

    assert_eq!(outcome.delivery, Delivery::Acked);
    assert!(outcome.transition_observed(), "{outcome:?}");
    let edge = outcome
        .wait
        .and_then(|wait| wait.edge)
        .expect("a satisfied wait names its edge");
    assert_eq!(edge.from, AgentMetaState::Working);
    assert_eq!(edge.to, AgentMetaState::Idle);
}

/// The corpse rule on the prompt path: a pane resting at `idle` times out
/// with delivery acked, never reporting a finished turn.
#[tokio::test]
async fn a_resting_level_never_satisfies_prompt_wait() {
    let (_server, socket, _seen) = serve(Script::new(Some(record("idle"))));
    let outcome = prompt(
        &socket,
        0x4a,
        Some(&wait_for_idle(Duration::from_millis(300))),
    )
    .await
    .expect("a timed-out wait is not an error");

    assert_eq!(outcome.delivery, Delivery::Acked);
    assert!(!outcome.transition_observed(), "{outcome:?}");
    let wait = outcome.wait.expect("--wait carries a result");
    assert_eq!(wait.baseline, AgentMetaState::Idle);
    assert_eq!(wait.edges, 0);
}

/// `phux agent send-keys` rides the same path: one `APPLY_INPUT` for the
/// whole key sequence, and a retry under the same id.
#[tokio::test]
async fn a_multi_event_key_batch_is_one_operation_and_retries_under_one_id() {
    let script = Script::new(Some(record("idle"))).apply(vec![
        refused(ErrorCode::ResourceExhausted),
        CommandResult::Ok,
    ]);
    let (_server, socket, seen) = serve(script);
    let events = phux_client::send_keys::events_for(&["yes please".to_owned(), "Enter".to_owned()]);
    assert_eq!(events.len(), 2, "{events:?}");

    let outcome = deliver_acknowledged(
        &socket,
        &ResourceId::local(7),
        op_id(0x4c),
        events,
        &always_ok,
        None,
    )
    .await
    .expect("a verified pane accepts the key batch");

    assert_eq!(outcome.delivery, Delivery::Acked);
    assert_eq!(outcome.attempts, 2);
    let ids = seen.lock().expect("ids").clone();
    assert_eq!(ids.len(), 2, "one APPLY_INPUT per attempt, not per event");
    assert!(ids.iter().all(|id| *id == op_id(0x4c)));
}

/// A record that goes away after the write is not a completion.
#[tokio::test]
async fn a_tombstone_after_the_write_is_a_departure_not_a_completion() {
    let script = Script::new(Some(record("working"))).post_result(vec![None]);
    let (_server, socket, _seen) = serve(script);
    let outcome = prompt(&socket, 0x4b, Some(&wait_for_idle(Duration::from_secs(5)))).await;
    assert!(
        matches!(
            outcome,
            Err(PromptError::Departed { .. } | PromptError::OccupantChanged { .. })
        ),
        "{outcome:?}"
    );
}
