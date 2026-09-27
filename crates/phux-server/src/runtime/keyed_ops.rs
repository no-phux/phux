//! Keyed supervisory commands (`docs/spec/L1.md` §5.1.1): `KILL_RESOURCE`,
//! `KILL_RESOURCE_IF`, `KILL_RESOURCES`, and `SIGNAL_TERMINAL` with an
//! `operation_id`, on the shared dedupe record (ADR-0126).
//!
//! The first admission runs the command; a same-command repeat answers the
//! first result, so a lost reply never kills or signals twice. A different
//! command under the key is refused, and a failed command binds nothing.
//! Satellite targets are deduped by the satellite, except `KILL_RESOURCES`,
//! whose batch the hub admits whole and forwards per host under the key.

use std::time::Duration;

use bytes::BytesMut;
use phux_protocol::ids::{IdempotencyKey, ResourceId, SatelliteHost};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode, FrameKind};
use sha2::{Digest, Sha256};

use super::operation_dedupe::{
    Admission, CachedOutcome, OperationClaim, OperationDomain, OperationKey,
};
use crate::state::SharedState;

/// How long a repeat (in its connection's read loop) waits for an unresolved
/// owner before it is refused.
const REPEAT_WAIT: Duration = Duration::from_secs(10);

/// How a keyed supervisory command is admitted.
#[derive(Debug)]
pub(crate) enum KeyedAdmission {
    /// No key: the command runs without dedupe, as it always did.
    Unkeyed,
    /// This request runs the command and settles the key with its result.
    Owner(OperationClaim),
    /// This request is answered without running anything.
    Answer(CommandResult),
}

/// The dedupe key of a supervisory `operation_id`.
pub(crate) const fn signal_key(key: IdempotencyKey) -> OperationKey {
    OperationKey::new(OperationDomain::Signal, *key.as_bytes())
}

/// Admit `command` if it carries a key.
pub(crate) async fn admit(state: &SharedState, command: &Command) -> KeyedAdmission {
    admit_within(state, command, REPEAT_WAIT).await
}

async fn admit_within(state: &SharedState, command: &Command, wait: Duration) -> KeyedAdmission {
    let Some(key) = command.idempotency_key().copied() else {
        return KeyedAdmission::Unkeyed;
    };
    let dedupe = state.with(|s| s.operation_dedupe().clone());
    let answer = match dedupe.admit(signal_key(key), digest(command), wait).await {
        Admission::Owner(claim) => return KeyedAdmission::Owner(claim),
        Admission::Final(outcome) => replayed(&outcome),
        Admission::InFlight => in_flight(),
        Admission::Conflict => conflict(),
        Admission::Full => CommandResult::Error {
            code: ErrorCode::ResourceExhausted,
            message: "the server's idempotency record is full; retry later".to_owned(),
        },
    };
    KeyedAdmission::Answer(answer)
}

/// Before a keyed command is held for approval (ADR-0128): the answer its
/// key already has, so a retry is never held twice. A free key is released
/// at once; the approved command admits it when it runs.
pub(crate) async fn prior_answer(state: &SharedState, command: &Command) -> Option<CommandResult> {
    match admit_within(state, command, Duration::ZERO).await {
        KeyedAdmission::Unkeyed => None,
        KeyedAdmission::Owner(claim) => {
            drop(claim);
            None
        }
        KeyedAdmission::Answer(result) => Some(result),
    }
}

/// Settle an owned key: a result that did the whole job binds; a refusal or
/// a partly failed `KILL_RESOURCES` binds nothing and releases the key.
pub(crate) fn settle(claim: Option<OperationClaim>, result: &CommandResult) {
    let Some(claim) = claim else {
        return;
    };
    if binds(result) {
        claim.bind(&CachedOutcome::Signal(result.clone()));
    }
}

fn binds(result: &CommandResult) -> bool {
    match result {
        CommandResult::Error { .. } => false,
        CommandResult::OkWith(CommandValue::Json(document)) => !KillResults::has_failures(document),
        _ => true,
    }
}

/// The digest a key is bound to: the whole command without its key, so a
/// repeat must name the same verb, targets, and arguments.
fn digest(command: &Command) -> [u8; 32] {
    let mut unkeyed = command.clone();
    if let Command::KillResource { operation_id, .. }
    | Command::KillResourceIf { operation_id, .. }
    | Command::KillResources { operation_id, .. }
    | Command::SignalTerminal { operation_id, .. } = &mut unkeyed
    {
        *operation_id = None;
    }
    let mut encoded = BytesMut::new();
    FrameKind::Command {
        request_id: 0,
        command: unkeyed,
    }
    .encode(&mut encoded);
    Sha256::digest(&encoded).into()
}

fn replayed(outcome: &CachedOutcome) -> CommandResult {
    match outcome {
        CachedOutcome::Signal(result) => result.clone(),
        _ => CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "the idempotency record holds no supervisory result under this key".to_owned(),
        },
    }
}

fn in_flight() -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::ResourceExhausted,
        message: "a command under this idempotency key is still in flight; retry".to_owned(),
    }
}

/// The refusal for a key reused with another command: the same code
/// `APPLY_INPUT` answers a reused operation id with (L1 §6.2.1).
fn conflict() -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::InvalidCommand,
        message: "idempotency conflict: the key was already used for a different command; \
                  nothing was run"
            .to_owned(),
    }
}

/// The per-id outcome of a `KILL_RESOURCES` that named a satellite id
/// (`docs/spec/L1.md` §5.2): one entry per requested id, so a batch split
/// across hosts never reports one `OK` for parts that failed.
#[derive(Debug, Default)]
pub(crate) struct KillResults {
    killed: Vec<String>,
    not_found: Vec<String>,
    failed: Vec<serde_json::Value>,
}

impl KillResults {
    /// `id` was killed (or, on a satellite, accepted by its batch).
    pub(crate) fn killed(&mut self, id: &ResourceId) {
        self.killed.push(selector_form(id));
    }

    /// `id` named nothing live on this server.
    pub(crate) fn not_found(&mut self, id: &ResourceId) {
        self.not_found.push(selector_form(id));
    }

    /// `id` was not killed, for the reason its host gave.
    pub(crate) fn failed(&mut self, id: &ResourceId, code: ErrorCode, message: &str) {
        self.failed.push(serde_json::json!({
            "id": selector_form(id),
            "code": code.as_wire(),
            "message": message,
        }));
    }

    /// Fold one satellite's per-id answer to its part of a keyed batch into
    /// these outcomes (L1 §5.2), re-spelling its `@N` ids as `host/@N`. An
    /// error, a missing document, or an unmentioned id fails the ids it
    /// covers, so a batch never binds past an id it cannot account for.
    pub(crate) fn merge_host(
        &mut self,
        host: &SatelliteHost,
        ids: &[ResourceId],
        result: &CommandResult,
    ) {
        let document = match result {
            CommandResult::Error { code, message } => {
                for id in ids {
                    self.failed(id, *code, message);
                }
                return;
            }
            CommandResult::OkWith(CommandValue::Json(document)) => {
                serde_json::from_str::<serde_json::Value>(document).ok()
            }
            _ => None,
        };
        match document {
            Some(document) => self.merge_document(host, ids, &document),
            None => {
                for id in ids {
                    self.failed(
                        id,
                        ErrorCode::InternalError,
                        "the satellite answered without per-id outcomes",
                    );
                }
            }
        }
    }

    fn merge_document(
        &mut self,
        host: &SatelliteHost,
        ids: &[ResourceId],
        document: &serde_json::Value,
    ) {
        let respell = |id: &serde_json::Value| id.as_str().map(|id| format!("{host}/{id}"));
        let list = |field: &str| {
            document
                .get(field)
                .and_then(serde_json::Value::as_array)
                .map(|ids| ids.iter().filter_map(respell).collect::<Vec<_>>())
                .unwrap_or_default()
        };
        let killed = list("killed");
        let not_found = list("not_found");
        let mut mentioned: std::collections::HashSet<String> =
            killed.iter().chain(&not_found).cloned().collect();
        self.killed.extend(killed);
        self.not_found.extend(not_found);
        let failed = document
            .get("failed")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten();
        for entry in failed {
            // An entry that names no id accounts for nothing; the id it
            // stood for is caught below as unmentioned.
            let Some(id) = entry.get("id").and_then(respell) else {
                continue;
            };
            let mut entry = entry.clone();
            entry["id"] = serde_json::Value::String(id.clone());
            mentioned.insert(id);
            self.failed.push(entry);
        }
        for id in ids {
            if !mentioned.contains(&selector_form(id)) {
                self.failed(
                    id,
                    ErrorCode::InternalError,
                    "the satellite's answer did not account for this id",
                );
            }
        }
    }

    /// The `COMMAND_RESULT` carrying these outcomes.
    pub(crate) fn into_result(self) -> CommandResult {
        let document = serde_json::json!({
            "schema_version": 1,
            "killed": self.killed,
            "not_found": self.not_found,
            "failed": self.failed,
        });
        CommandResult::OkWith(CommandValue::Json(document.to_string()))
    }

    fn has_failures(document: &str) -> bool {
        serde_json::from_str::<serde_json::Value>(document)
            .ok()
            .and_then(|value| value.get("failed").and_then(|f| f.as_array()).map(Vec::len))
            .is_some_and(|failed| failed > 0)
    }
}

/// `@N` for a local id, `host/@N` for a satellite one: the selector a user
/// would type for it.
fn selector_form(id: &ResourceId) -> String {
    match id {
        ResourceId::Local { id } => format!("@{id}"),
        ResourceId::Satellite { host, id } => format!("{host}/@{id}"),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use phux_protocol::wire::frame::TerminalSignal;

    fn key(byte: u8) -> Option<IdempotencyKey> {
        IdempotencyKey::new([byte; 16])
    }

    fn kill(id: u32, byte: u8) -> Command {
        Command::KillResource {
            terminal_id: ResourceId::local(id),
            operation_id: key(byte),
        }
    }

    #[test]
    fn the_digest_ignores_the_key_and_covers_verb_and_target() {
        assert_eq!(digest(&kill(3, 1)), digest(&kill(3, 2)));
        assert_ne!(digest(&kill(3, 1)), digest(&kill(4, 1)));
        let signal = Command::SignalTerminal {
            terminal_id: ResourceId::local(3),
            signal: TerminalSignal::Kill,
            operation_id: key(1),
        };
        assert_ne!(
            digest(&kill(3, 1)),
            digest(&signal),
            "one namespace, two verbs"
        );
    }

    /// A failed command binds nothing: its key admits the next repeat.
    #[tokio::test]
    async fn a_refusal_releases_the_key_and_a_success_binds_it() {
        let state = SharedState::new();
        let KeyedAdmission::Owner(claim) = admit(&state, &kill(3, 7)).await else {
            panic!("the first admission owns the key");
        };
        settle(
            Some(claim),
            &CommandResult::Error {
                code: ErrorCode::TerminalNotFound,
                message: "gone".to_owned(),
            },
        );
        let KeyedAdmission::Owner(claim) = admit(&state, &kill(3, 7)).await else {
            panic!("a refused command's repeat runs again");
        };
        settle(Some(claim), &CommandResult::Ok);
        assert!(matches!(
            admit(&state, &kill(3, 7)).await,
            KeyedAdmission::Answer(CommandResult::Ok)
        ));
    }

    /// A repeat behind an owner that never resolves is refused once the
    /// bound passes, instead of holding its connection's read loop.
    #[tokio::test]
    async fn a_repeat_behind_a_stalled_owner_is_refused_after_the_bound() {
        let state = SharedState::new();
        let KeyedAdmission::Owner(_stalled) = admit(&state, &kill(3, 8)).await else {
            panic!("the stalled owner admits first");
        };
        let KeyedAdmission::Answer(CommandResult::Error { code, message }) =
            admit_within(&state, &kill(3, 8), Duration::from_millis(20)).await
        else {
            panic!("the repeat is refused, not admitted");
        };
        assert_eq!(code, ErrorCode::ResourceExhausted);
        assert!(message.contains("in flight"), "{message}");
    }

    /// A satellite answer that omits a forwarded id, or names none on a
    /// failure, cannot make that id vanish: it is failed, and the batch does
    /// not bind.
    #[test]
    fn a_satellite_id_the_answer_does_not_mention_is_failed() {
        let host = SatelliteHost::new("up");
        let ids = [
            ResourceId::satellite("up", 3),
            ResourceId::satellite("up", 4),
            ResourceId::satellite("up", 5),
        ];
        let answer = CommandResult::OkWith(CommandValue::Json(
            r#"{"schema_version":1,"killed":["@3"],"not_found":[],
                "failed":[{"code":107,"message":"no id"}]}"#
                .to_owned(),
        ));
        let mut results = KillResults::default();
        results.merge_host(&host, &ids, &answer);
        assert_eq!(results.killed, ["up/@3"]);
        let failed: Vec<_> = results
            .failed
            .iter()
            .map(|entry| {
                (
                    entry["id"].as_str().map(str::to_owned),
                    entry["code"].clone(),
                )
            })
            .collect();
        assert_eq!(
            failed,
            [
                (
                    Some("up/@4".to_owned()),
                    ErrorCode::InternalError.as_wire().into()
                ),
                (
                    Some("up/@5".to_owned()),
                    ErrorCode::InternalError.as_wire().into()
                ),
            ]
        );
        assert!(!binds(&results.into_result()));
    }

    #[test]
    fn a_partial_kill_does_not_bind() {
        let mut results = KillResults::default();
        results.killed(&ResourceId::local(1));
        assert!(binds(&results.into_result()));
        let mut results = KillResults::default();
        results.killed(&ResourceId::local(1));
        results.failed(
            &ResourceId::satellite("devbox", 2),
            ErrorCode::SatelliteUnreachable,
            "link is down",
        );
        assert!(!binds(&results.into_result()));
    }
}
