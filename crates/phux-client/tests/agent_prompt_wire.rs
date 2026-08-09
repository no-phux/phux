//! Wire-level contract for acknowledged input delivery (ADR-0053, ADR-0076).
//!
//! # Why this is not `phux_client::testkit`
//!
//! The shared scripted server is the right harness for almost everything, and
//! this file does not fork it lightly. It cannot express the two facts this
//! contract turns on:
//!
//! 1. It negotiates `ServerCapabilities::new()` — **no** features — so
//!    `ACKNOWLEDGED_INPUT` is never advertised and every acknowledged submit
//!    is refused at the capability gate before a frame is sent.
//! 2. It acks every command `Ok`, so no `APPLY_INPUT` **refusal** can be
//!    scripted — and the refusals are the entire point: `RESOURCE_EXHAUSTED`
//!    (retry, same id) and `INPUT_DELIVERY_UNKNOWN` (never retry, ever) are
//!    exactly the two answers a caller must not confuse.
//!
//! So this file stands up a purpose-built server that advertises the
//! capability, answers `APPLY_INPUT` from a script, and **records the
//! operation id of every submit it saw**. The last of those is what makes the
//! idempotency claim testable rather than asserted: the test reads the ids off
//! the wire, not off the client's own bookkeeping.
//!
//! The capability-gate refusal itself is tested against the *shared* harness,
//! in `phux_client::agent_prompt`'s unit tests, precisely because an
//! unmodified reference server is the older server that gate exists for.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::BytesMut;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{UnixListener, UnixStream};

use phux_client::agent_meta::{AgentMetaState, AgentRecord, TERMINAL_AGENT_KEY};
use phux_client::agent_prompt::{
    Delivery, MAX_PROMPT_BYTES, PromptError, PromptWait, Refusal, deliver_acknowledged,
    prompt_agent,
};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, ServerCapabilities, ServerFeature, ServerFeatureSet,
    select_bootstrap_profile,
};
use phux_protocol::ids::{InputOperationId, TerminalId};
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
    /// `METADATA_CHANGED` payloads pushed after the first `APPLY_INPUT`
    /// result — i.e. strictly post-write, which is the only kind that may
    /// satisfy `prompt --wait`.
    post_result: Vec<Option<Vec<u8>>>,
    /// Whether to advertise `ACKNOWLEDGED_INPUT`.
    acknowledged: bool,
}

impl Script {
    fn new(record: Option<Vec<u8>>) -> Self {
        Self {
            record,
            apply: vec![CommandResult::Ok],
            post_result: Vec::new(),
            acknowledged: true,
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

/// Bind a socket and serve `script` to every client that dials it.
fn serve(script: Script) -> (tempfile::TempDir, PathBuf, SeenIds) {
    let dir = tempfile::tempdir().expect("temp dir");
    let socket = dir.path().join("phux.sock");
    let listener = UnixListener::bind(&socket).expect("bind");
    let seen: SeenIds = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let script = script.clone();
            let recorded = Arc::clone(&recorded);
            tokio::spawn(async move { session(stream, script, recorded).await });
        }
    });
    (dir, socket, seen)
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
                let features = if script.acknowledged {
                    ServerFeatureSet::from_wire(ServerFeature::AcknowledgedInput as u32)
                } else {
                    ServerFeatureSet::default()
                };
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
                let value = (key == TERMINAL_AGENT_KEY)
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
                // Strictly after the result, which is the only position from
                // which a transition may satisfy `prompt --wait`.
                for value in script.post_result.clone() {
                    link.send(&FrameKind::MetadataChanged {
                        scope: Scope::Terminal(terminal_id.clone()),
                        key: TERMINAL_AGENT_KEY.to_owned(),
                        value,
                    })
                    .await;
                }
            }
            // Subscriptions register and reply with nothing, like the server.
            // Spelled out rather than folded into the trailing wildcard
            // because "no reply" is the load-bearing fact: a fake that acked
            // them would teach the client to wait forever.
            #[allow(
                clippy::match_same_arms,
                reason = "documents an ordering fact, not a fallthrough"
            )]
            FrameKind::SubscribeEvents { .. } | FrameKind::SubscribeMetadata { .. } => {}
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

/// The happy path: subscribe, re-verify the occupant, submit ONE batch, and
/// report the receipt with the operation id the caller can correlate on.
#[tokio::test]
async fn a_verified_pane_takes_one_batch_and_reports_the_receipt() {
    let (_dir, socket, seen) = serve(Script::new(Some(record("working"))));
    let outcome = prompt_agent(
        &socket,
        &TerminalId::local(7),
        "ship it",
        op_id(0x31),
        &always_ok,
        None,
    )
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

/// **The idempotency test, read off the wire.** A `RESOURCE_EXHAUSTED` wrote
/// nothing, so the CLI resubmits — and every resubmission carries the *same*
/// operation id. A fresh id would be the duplicate prompt this whole design
/// exists to prevent, and the server's dedupe cache is keyed on that id, so
/// the guarantee is only real if the id on the wire is stable.
#[tokio::test]
async fn a_resource_exhausted_retry_reuses_the_same_operation_id() {
    let script = Script::new(Some(record("idle"))).apply(vec![
        refused(ErrorCode::ResourceExhausted),
        refused(ErrorCode::ResourceExhausted),
        CommandResult::Ok,
    ]);
    let (_dir, socket, seen) = serve(script);
    let outcome = prompt_agent(
        &socket,
        &TerminalId::local(7),
        "ship it",
        op_id(0x42),
        &always_ok,
        None,
    )
    .await
    .expect("the lane freed on the third attempt");

    assert_eq!(outcome.delivery, Delivery::Acked);
    assert_eq!(outcome.attempts, 3);
    let ids = seen.lock().expect("ids").clone();
    assert_eq!(ids.len(), 3, "the retries actually happened");
    assert!(
        ids.iter().all(|id| *id == op_id(0x42)),
        "every attempt must carry the id generated once for this invocation"
    );
}

/// A lane that never frees is a **failure**, not a refusal, and nothing was
/// written on any attempt — so re-running the command is safe. Every attempt
/// still carried one id.
#[tokio::test]
async fn a_lane_that_never_frees_fails_without_writing_anything() {
    let script =
        Script::new(Some(record("idle"))).apply(vec![refused(ErrorCode::ResourceExhausted)]);
    let (_dir, socket, seen) = serve(script);
    let outcome = prompt_agent(
        &socket,
        &TerminalId::local(7),
        "ship it",
        op_id(0x43),
        &always_ok,
        None,
    )
    .await;

    match outcome {
        Err(PromptError::LaneBusy { attempts, .. }) => {
            assert!(attempts > 1, "the backoff schedule must be spent");
            let ids = seen.lock().expect("ids").clone();
            assert_eq!(usize::try_from(attempts).unwrap_or(0), ids.len());
            assert!(ids.iter().all(|id| *id == op_id(0x43)));
        }
        other => panic!("a permanently busy lane is a failure: {other:?}"),
    }
}

/// **The rule an agent can violate catastrophically.**
/// `INPUT_DELIVERY_UNKNOWN` is terminal: the CLI submits exactly once, reports
/// the operation id, and stops. A same-id retry would replay the server's
/// cached unknown; a new-id retry would duplicate the prompt.
#[tokio::test]
async fn input_delivery_unknown_is_reported_once_and_never_retried() {
    let script =
        Script::new(Some(record("working"))).apply(vec![refused(ErrorCode::InputDeliveryUnknown)]);
    let (_dir, socket, seen) = serve(script);
    let outcome = prompt_agent(
        &socket,
        &TerminalId::local(7),
        "ship it",
        op_id(0x44),
        &always_ok,
        None,
    )
    .await;

    match outcome {
        Err(PromptError::DeliveryUnknown { operation_id, .. }) => {
            assert_eq!(operation_id.len(), 32);
        }
        other => panic!("an unknown delivery must be reported as such: {other:?}"),
    }
    assert_eq!(
        seen.lock().expect("ids").len(),
        1,
        "an indeterminate delivery must be submitted exactly once"
    );
}

/// A pane that refuses the batch before writing anything (canonical-mode
/// limit) is a refusal the caller can act on, and it is NOT retried: the
/// identical payload cannot succeed.
#[tokio::test]
async fn a_canonical_limit_refusal_is_not_retried() {
    let script =
        Script::new(Some(record("idle"))).apply(vec![refused(ErrorCode::CanonicalLimitExceeded)]);
    let (_dir, socket, seen) = serve(script);
    let outcome = prompt_agent(
        &socket,
        &TerminalId::local(7),
        "ship it",
        op_id(0x45),
        &always_ok,
        None,
    )
    .await;

    assert!(
        matches!(
            outcome,
            Err(PromptError::Refused(Refusal::CanonicalLimitExceeded(_)))
        ),
        "{outcome:?}"
    );
    assert_eq!(seen.lock().expect("ids").len(), 1);
}

/// Ownership re-verification, on the connection that carries the submit: a
/// pane hosting somebody else is refused with nothing written. This is the
/// check that stops a prompt landing in the bare shell an exited agent left
/// behind — and being executed there, since a readline shell inserts a
/// bracketed paste and our Enter then runs it.
#[tokio::test]
async fn a_mismatched_occupant_refuses_before_any_byte_is_written() {
    let (_dir, socket, seen) = serve(Script::new(Some(record("idle"))));
    let outcome = prompt_agent(
        &socket,
        &TerminalId::local(7),
        "ship it",
        op_id(0x46),
        &|found| Some(format!("'{}', not 'builder'", found.name)),
        None,
    )
    .await;

    match outcome {
        Err(PromptError::Refused(Refusal::AgentMismatch(who))) => {
            assert!(who.contains("reviewer"), "{who}");
        }
        other => panic!("a mismatched occupant must refuse: {other:?}"),
    }
    assert!(
        seen.lock().expect("ids").is_empty(),
        "the refusal must precede the submit"
    );
}

/// A pane with no `phux.agent/v1` record has no identity the gate could pass.
#[tokio::test]
async fn a_pane_with_no_record_is_refused_before_the_submit() {
    let (_dir, socket, seen) = serve(Script::new(None));
    let outcome = prompt_agent(
        &socket,
        &TerminalId::local(7),
        "ship it",
        op_id(0x47),
        &always_ok,
        None,
    )
    .await;

    assert!(
        matches!(outcome, Err(PromptError::Refused(Refusal::NoAgentRecord))),
        "{outcome:?}"
    );
    assert!(seen.lock().expect("ids").is_empty());
}

/// An oversized prompt is refused **client-side, before a socket is opened**,
/// naming the measured size — never split across operations (which
/// `docs/spec/input.md` forbids) and never quietly truncated.
#[tokio::test]
async fn an_oversized_prompt_never_reaches_the_wire() {
    let (_dir, socket, seen) = serve(Script::new(Some(record("idle"))));
    let outcome = prompt_agent(
        &socket,
        &TerminalId::local(7),
        &"x".repeat(MAX_PROMPT_BYTES + 1),
        op_id(0x48),
        &always_ok,
        None,
    )
    .await;

    match outcome {
        Err(PromptError::Refused(Refusal::TooLarge {
            measured, limit, ..
        })) => {
            assert_eq!(measured, MAX_PROMPT_BYTES + 1);
            assert_eq!(limit, MAX_PROMPT_BYTES);
        }
        other => panic!("an oversized prompt must be refused: {other:?}"),
    }
    assert!(
        seen.lock().expect("ids").is_empty(),
        "nothing may reach the wire"
    );
}

/// `prompt --wait` is satisfied by a transition observed **after** the
/// result, on the connection that carried the submit. That single-connection
/// ordering is the whole argument: the server writes to the PTY before
/// replying and frames on one connection are ordered, so no sequence counter
/// is needed.
#[tokio::test]
async fn a_post_result_transition_satisfies_prompt_wait() {
    let script = Script::new(Some(record("idle")))
        .post_result(vec![Some(record("working")), Some(record("idle"))]);
    let (_dir, socket, _seen) = serve(script);
    let outcome = prompt_agent(
        &socket,
        &TerminalId::local(7),
        "ship it",
        op_id(0x49),
        &always_ok,
        Some(&PromptWait {
            targets: vec![AgentMetaState::Idle],
            timeout: Some(Duration::from_secs(5)),
            poll_interval: Duration::from_millis(30),
        }),
    )
    .await
    .expect("an observed transition is a success");

    assert_eq!(outcome.delivery, Delivery::Acked);
    assert!(outcome.transition_observed(), "{outcome:?}");
    let wait = outcome.wait.expect("--wait carries a result");
    let edge = wait.edge.expect("a satisfied wait names its edge");
    assert_eq!(edge.from, AgentMetaState::Working);
    assert_eq!(edge.to, AgentMetaState::Idle);
}

/// **The corpse rule, on the prompt path.** A pane resting at `idle` that
/// never transitions times out rather than reporting a finished turn — even
/// though the pre-submit level was already `idle` and the target set is
/// `idle`. Delivery succeeded and the result says so: "the bytes landed" and
/// "the turn finished" are separate answers, and only the first is a receipt.
#[tokio::test]
async fn a_resting_level_never_satisfies_prompt_wait() {
    let (_dir, socket, _seen) = serve(Script::new(Some(record("idle"))));
    let outcome = prompt_agent(
        &socket,
        &TerminalId::local(7),
        "ship it",
        op_id(0x4a),
        &always_ok,
        Some(&PromptWait {
            targets: vec![AgentMetaState::Idle],
            timeout: Some(Duration::from_millis(300)),
            poll_interval: Duration::from_millis(30),
        }),
    )
    .await
    .expect("a timed-out wait is not an error");

    assert_eq!(outcome.delivery, Delivery::Acked);
    assert!(
        !outcome.transition_observed(),
        "a level read of idle must never satisfy a completion gate: {outcome:?}"
    );
    let wait = outcome.wait.expect("--wait carries a result");
    assert_eq!(wait.baseline, AgentMetaState::Idle);
    assert_eq!(wait.edges, 0);
}

/// `phux agent send-keys` (phux-w7z2.36) rides the same path with a
/// multi-event batch, and gets the same guarantee: **one** `APPLY_INPUT` for
/// the whole key sequence, and a retry that reuses the operation id.
///
/// The old fire-and-forget shape sent one `ROUTE_INPUT` per event, so a
/// failure part-way left the caller unable to say whether the keys landed and
/// unable to retry safely. This asserts both halves of the fix: the batch is
/// one frame (no interior seam to fail at), and the resubmission carries the
/// same id (so the server answers from its dedupe cache rather than typing the
/// keys twice).
#[tokio::test]
async fn a_multi_event_key_batch_is_one_operation_and_retries_under_one_id() {
    let script = Script::new(Some(record("idle"))).apply(vec![
        refused(ErrorCode::ResourceExhausted),
        CommandResult::Ok,
    ]);
    let (_dir, socket, seen) = serve(script);
    // The shape `phux agent send-keys @7 "yes please" Enter` builds: a
    // submission-safe paste plus the real Enter key.
    let events = phux_client::send_keys::events_for(&["yes please".to_owned(), "Enter".to_owned()]);
    assert_eq!(events.len(), 2, "{events:?}");

    let outcome = deliver_acknowledged(
        &socket,
        &TerminalId::local(7),
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
    assert!(
        ids.iter().all(|id| *id == op_id(0x4c)),
        "a send-keys retry must reuse its operation id too"
    );
}

/// A record that goes away after the write is delivery to an unknown
/// occupant, not a completion.
#[tokio::test]
async fn a_tombstone_after_the_write_is_a_departure_not_a_completion() {
    let script = Script::new(Some(record("working"))).post_result(vec![None]);
    let (_dir, socket, _seen) = serve(script);
    let outcome = prompt_agent(
        &socket,
        &TerminalId::local(7),
        "ship it",
        op_id(0x4b),
        &always_ok,
        Some(&PromptWait {
            targets: vec![AgentMetaState::Idle],
            timeout: Some(Duration::from_secs(5)),
            poll_interval: Duration::from_millis(30),
        }),
    )
    .await;

    assert!(
        matches!(
            outcome,
            Err(PromptError::Departed { .. } | PromptError::OccupantChanged { .. })
        ),
        "{outcome:?}"
    );
}
