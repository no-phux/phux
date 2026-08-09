//! `phux agent send-keys` — send keys to a pane **only if it is still hosting
//! the agent you think it is**.
//!
//! This verb differs from top-level `phux send-keys` in exactly one way, and
//! the difference is the whole reason it exists: `phux send-keys` addresses a
//! *pane* and deliberately checks no identity — a pane is a pane, and typing
//! into it is the user's business. `phux agent send-keys` addresses an
//! *agent*, so it re-reads the pane's `phux.agent/v1` record immediately
//! before writing and refuses if the occupant is not the agent the caller
//! named. An orchestrator that resolved `@7` to "the reviewer" a minute ago,
//! and whose reviewer has since exited leaving a bare shell, must not have its
//! prompt land in that shell.
//!
//! Three contracts, all hard:
//!
//! 1. **All keys are validated before any byte is written.** Translation
//!    happens up front, on the whole argument vector; a typo in the third key
//!    cannot leave the first two delivered. There is no partial send.
//! 2. **The identity check and the write ride one connection.** The server
//!    handles a connection's frames in order, so nothing this client sends
//!    interleaves between the `GET_METADATA` answer and the `APPLY_INPUT`.
//!    See [`STALENESS`] for what that bound does *not* cover.
//! 3. **The whole batch is one acknowledged operation** (phux-w7z2.36,
//!    ADR-0053). Since the receipt landed, all-or-nothing covers *delivery*
//!    as well as validation, and a caller that loses the answer can ask again
//!    under the same operation id instead of guessing.
//!
//! # What changed when this verb moved off `ROUTE_INPUT`
//!
//! It shipped fire-and-forget, one `ROUTE_INPUT` per event, because `agent
//! prompt` did not exist yet and nothing else called `APPLY_INPUT`. That
//! version had a real hole, documented in its own help text: a transport
//! failure part-way through the sequence left the caller unable to tell
//! whether the keys landed, and its remedy deliberately did **not** say
//! "retry", because once a `ROUTE_INPUT` is acked those bytes are in the tty
//! input queue and no client can take them back.
//!
//! One `APPLY_INPUT` under an operation id closes both halves of that. The
//! server encodes the whole batch against one mode snapshot into one byte
//! vector and writes it as a single PTY job, so there is no interior seam to
//! fail at; and a same-id resubmission is answered from the server's dedupe
//! cache rather than written twice.
//!
//! The error contract is **inherited from ADR-0076 rather than re-derived**,
//! including the two readings that matter: `OK` is a *kernel-queue* receipt
//! (bytes accepted by `write(2)` on the PTY master and flushed — strictly
//! more than `ROUTE_INPUT` states, strictly less than consumption), and
//! `INPUT_DELIVERY_UNKNOWN` is **terminal**, exit 1, never retried under any
//! id.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::agent_meta::AgentRecord;
use phux_client::agent_prompt::{
    PromptError, PromptOutcome, Refusal, deliver_acknowledged, validate_batch,
};
use phux_client::attach::AttachError;
use phux_protocol::ids::InputOperationId;
use phux_server::runtime::default_socket_path;

use crate::commands::json_err::codes;
use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target};

use super::prompt::refusal_code;

/// The staleness bound this verb accepts, stated once so a reader does not
/// have to infer it from the code.
///
/// **Re-verified against the `APPLY_INPUT` frame ordering (phux-w7z2.36).**
/// The identity read (`GET_METADATA`, correlation id 1) and the write
/// (`APPLY_INPUT`, correlation id 2) are consecutive frames on one
/// connection, and `phux-server` handles a connection's frames in arrival
/// order, so **no frame from this client** can interleave between them. The
/// move made that bound *tighter*, not looser: the batch used to be N
/// `ROUTE_INPUT` frames with N-1 interior windows a concurrent writer could
/// slip into, and it is now one frame with none.
///
/// What still can change inside the window between the read and the write:
///
/// - another client's `SET_METADATA` / `DELETE_METADATA` on the same key;
/// - the server-side detector's next tick (300 ms, ADR-0046) republishing a
///   derived record;
/// - the pane's own foreground process exec'ing something else, which no
///   metadata read observes at all until the detector catches up.
///
/// So the bound is "one server frame-handling turn on this connection", not
/// atomicity. It closes the minute-wide window between resolving a selector
/// and typing into it, which is the window that actually bites; it does not
/// close a race against a concurrent writer, and this verb does not claim to.
const STALENESS: &str = "one server frame-handling turn on this connection";

/// Why one key argument was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KeySpecError {
    /// Zero-based position of the offending argument.
    pub(super) index: usize,
    /// The argument as typed.
    pub(super) spec: String,
    /// What is wrong with it.
    pub(super) reason: &'static str,
}

/// Validate every key argument, refusing the whole batch on the first
/// problem.
///
/// `phux_client::send_keys::spec_to_bytes` is total by design — anything it
/// does not recognize as a named key is typed as literal text — which is
/// right for `phux send-keys`, where literal text is most of the traffic. It
/// is wrong for an agent surface: `C-cc` is not a plausible thing to type at
/// an agent, it is a typo for `C-c`, and typing it literally is a silent
/// wrong action inside someone's turn. So this rejects the argument shapes
/// that *look* like a named key and are not one, and accepts everything else
/// as literal text.
pub(super) fn validate_key_specs(keys: &[String]) -> Result<(), KeySpecError> {
    for (index, spec) in keys.iter().enumerate() {
        let refuse = |reason: &'static str| KeySpecError {
            index,
            spec: spec.clone(),
            reason,
        };
        if spec.is_empty() {
            return Err(refuse("an empty argument types nothing"));
        }
        let lower = spec.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("c-") {
            let mut chars = rest.chars();
            match (chars.next(), chars.next()) {
                (Some(first), None) if first.is_ascii() => {}
                (None, _) => return Err(refuse("`C-` names no key")),
                (Some(_), None) => {
                    return Err(refuse(
                        "a control chord takes one ASCII character (`C-c`, `C-a`)",
                    ));
                }
                (Some(_), Some(_)) => {
                    return Err(refuse(
                        "a control chord takes exactly one character; `C-cc` would be \
                         typed literally",
                    ));
                }
            }
        } else if lower.strip_prefix("m-").is_some_and(str::is_empty) {
            return Err(refuse("`M-` names no key"));
        }
    }
    Ok(())
}

/// `phux agent send-keys TARGET KEYS... [--expect-agent NAME]
/// [--expect-kind KIND] [--json]`.
pub(super) fn run_agent_send_keys(
    target: &str,
    keys: &[String],
    expect_agent: Option<&str>,
    expect_kind: Option<&str>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    if keys.is_empty() {
        return json_err::emit(
            json,
            &json_err::CliError::new(
                codes::INVALID_KEY_SPEC,
                "no keys to send",
                "pass at least one key spec, e.g. `phux agent send-keys @7 \"yes\" Enter`",
            ),
            crate::exit_codes::EXIT_USAGE,
        );
    }
    // Contract 1: the whole batch is validated before a connection is even
    // opened, so a bad argument cannot leave a prefix delivered.
    if let Err(err) = validate_key_specs(keys) {
        return json_err::emit(
            json,
            &json_err::CliError::new(
                codes::INVALID_KEY_SPEC,
                format!(
                    "key {} ('{}') is not a key spec: {}",
                    err.index.saturating_add(1),
                    err.spec,
                    err.reason
                ),
                "nothing was sent — all keys are validated before any byte is written",
            ),
            crate::exit_codes::EXIT_USAGE,
        );
    }
    // Translate up front too: the event vector is built once, from the whole
    // argument list, so the literal-run-before-Enter grouping is decided
    // before anything is on the wire.
    let events = phux_client::send_keys::events_for(keys);
    // A literal run *not* followed by Enter is one event per character, so
    // this verb — unlike `agent prompt`, which is always two events — can
    // genuinely reach the 256-event protocol cap. Refuse locally, naming the
    // count: splitting one logical batch across two operations would give up
    // exactly the all-or-nothing property the verb exists for.
    if let Err(refusal) = validate_batch(&events) {
        return json_err::emit(
            json,
            &json_err::CliError::new(
                refusal_code(&refusal),
                format!("{refusal}; nothing was sent"),
                "send fewer keys per invocation. phux will not split one batch across two \
                 operations: a half-delivered key sequence is the failure all-or-nothing \
                 exists to prevent",
            ),
            crate::exit_codes::EXIT_USAGE,
        );
    }

    let selector = match parse_selector(Some(target)) {
        Ok(selector) => selector,
        Err(code) => return code,
    };
    let Some(operation_id) = InputOperationId::new(uuid::Uuid::new_v4().into_bytes()) else {
        return json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::INTERNAL_ERROR,
                "could not generate an input operation id",
                "report this: a v4 UUID is all-zero with probability 2^-128",
            ),
            crate::exit_codes::EXIT_FAILURE,
        );
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let expect_agent = expect_agent.map(str::to_owned);
    let expect_kind = expect_kind.map(str::to_owned);
    let key_count = keys.len();

    rt.block_on(async move {
        let pane = match resolve_target(&socket_path, &selector, "agent send-keys", json).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let label = crate::selector::format_terminal_id(&pane);
        let verify = |record: &AgentRecord| {
            identity_mismatch(record, expect_agent.as_deref(), expect_kind.as_deref())
        };
        // Contracts 2 and 3: subscribe, identity read (id 1), and the whole
        // batch as ONE acknowledged operation (id 2), on one connection.
        // `deliver_acknowledged` owns that ordering and the ADR-0076 error
        // contract, so this verb and `agent prompt` cannot drift apart on
        // what an OK means or on when a retry is honest.
        let outcome = deliver_acknowledged(
            &socket_path,
            &pane,
            operation_id,
            events,
            &verify,
            // No completion gate: `send-keys` delivers keystrokes, which may
            // be a `C-c` or an arrow. Waiting for a lifecycle transition is
            // `agent prompt --wait`'s question, not this one's.
            None,
        )
        .await;

        match outcome {
            Ok(outcome) => report_delivered(json, &label, &outcome, key_count),
            Err(err) => report_failure(json, &label, &socket_path, &err),
        }
    })
}

/// Report a delivered batch: silence without `--json` (the prose `send-keys`
/// says nothing on success either), one document with it.
///
/// The document gained `operation_id` and `delivery` when the verb moved onto
/// `APPLY_INPUT`: a caller that wants to correlate this invocation with a
/// server log line, or to record what its own success code attested, now can.
fn report_delivered(json: bool, label: &str, outcome: &PromptOutcome, keys: usize) -> ExitCode {
    if !json {
        return ExitCode::SUCCESS;
    }
    let document = serde_json::json!({
        "schema_version": 1,
        "terminal": label,
        "agent": { "name": outcome.agent.name, "kind": outcome.agent.kind },
        "keys": keys,
        "verified": true,
        "delivery": outcome.delivery.as_str(),
        "operation_id": outcome.operation_id,
        "attempts": outcome.attempts,
    });
    match serde_json::to_string_pretty(&document) {
        Ok(rendered) => {
            outln!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(err) => json_err::emit(
            true,
            &json_err::CliError::new(
                json_err::codes::JSON_SERIALIZE,
                format!("could not render agent send-keys JSON: {err}"),
                "report this: a document of strings and numbers cannot fail to serialize",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
    }
}

/// Map a delivery failure onto its published reading.
///
/// The readings are ADR-0076's, inherited rather than re-derived — see the
/// module docs. The one that changed shape when this verb moved off
/// `ROUTE_INPUT` is the old "some keys may already have been delivered;
/// re-read the pane before retrying": with an operation id, most failures are
/// now *provably* nothing-written and honestly retryable, and the residue
/// that is not is named exactly.
#[allow(
    clippy::too_many_lines,
    reason = "the exit-code and remedy table is the contract; splitting it \
              hides which readings sit next to each other"
)]
fn report_failure(
    json: bool,
    label: &str,
    socket_path: &std::path::Path,
    err: &PromptError,
) -> ExitCode {
    match err {
        PromptError::Refused(refusal @ Refusal::AgentMismatch(_)) => json_err::emit(
            json,
            &json_err::CliError::new(
                refusal_code(refusal),
                format!("{label}: {refusal}; nothing was sent"),
                format!(
                    "the occupant is re-read immediately before the write (staleness \
                     bound: {STALENESS}); re-resolve the target, or use `phux send-keys` \
                     if you meant the pane rather than the agent"
                ),
            ),
            crate::exit_codes::EXIT_USAGE,
        ),
        PromptError::Refused(refusal @ Refusal::NoAgentRecord) => json_err::emit(
            json,
            &json_err::CliError::new(
                refusal_code(refusal),
                format!(
                    "{label} declares no phux.agent/v1 record, so there is no agent \
                     identity to verify"
                ),
                "`phux send-keys` addresses the pane and checks no identity — use it when \
                 that is what you mean. To make this pane addressable as an agent, run \
                 `phux agent set` or `phux agent install-claude`",
            ),
            crate::exit_codes::EXIT_USAGE,
        ),
        PromptError::Refused(refusal @ Refusal::NoAcknowledgedInput) => json_err::emit(
            json,
            &json_err::CliError::new(
                refusal_code(refusal),
                format!("{label}: {refusal}; nothing was sent"),
                "upgrade the server (`phux update`), then retry. This verb will not fall \
                 back to fire-and-forget input, because its all-or-nothing promise now \
                 covers delivery and `ROUTE_INPUT` cannot keep it — `phux send-keys` is \
                 the fire-and-forget verb, and it says so",
            ),
            crate::exit_codes::EXIT_USAGE,
        ),
        PromptError::Refused(refusal @ Refusal::SatelliteTarget { .. }) => json_err::emit(
            json,
            &json_err::CliError::new(
                refusal_code(refusal),
                format!("{label}: {refusal}; nothing was sent"),
                "the receipt is owned by the machine that owns the PTY, so dial that \
                 server directly (`phux host add`) rather than routing through the hub. \
                 Plain `phux send-keys` still crosses a hub — with no receipt",
            ),
            crate::exit_codes::EXIT_USAGE,
        ),
        PromptError::Refused(refusal) => json_err::emit(
            json,
            &json_err::CliError::new(
                refusal_code(refusal),
                format!("{label}: {refusal}; nothing was sent"),
                "nothing was written, so the pane is unchanged",
            ),
            crate::exit_codes::EXIT_USAGE,
        ),
        PromptError::LaneBusy {
            attempts,
            operation_id,
            ..
        } => json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::TRANSPORT,
                format!(
                    "{label}: the server-wide acknowledged input lane stayed busy across \
                     {attempts} attempts (operation {operation_id}); nothing was written"
                ),
                "nothing was written on any attempt, so re-running this command is safe",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
        PromptError::NotFound(message) => json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::NO_SUCH_TARGET,
                format!("{label} is gone: {message}"),
                "re-resolve the target with `phux agent list`",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
        PromptError::DeliveryUnknown {
            operation_id,
            message,
        } => json_err::emit(
            json,
            &json_err::CliError::new(
                codes::DELIVERY_UNKNOWN,
                format!("{label}: delivery is unknown (operation {operation_id}): {message}"),
                "DO NOT RESEND. Some, all, or none of the keys reached the pane, and a \
                 batch reported unknown can still complete a moment later. Resending \
                 under a new operation id types them twice; resending under the same one \
                 replays this same answer. Read the pane instead: `phux snapshot`",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
        PromptError::OccupantChanged {
            detail,
            operation_id,
            ..
        } => json_err::emit(
            json,
            &json_err::CliError::new(
                codes::UNKNOWN_OCCUPANT,
                format!(
                    "{label}: {detail} while operation {operation_id} was in flight, so \
                     the keys went to an unknown occupant"
                ),
                "read the pane before resending",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
        // `send-keys` opens no completion gate, so a departure cannot arise.
        PromptError::Departed { .. } => json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::INTERNAL_ERROR,
                format!("{label}: unexpected lifecycle departure with no wait in progress"),
                "report this: `agent send-keys` passes no wait spec",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
        PromptError::Transport(err @ AttachError::Io(_)) => {
            json_err::report_no_server(json, err, socket_path, "agent send-keys")
        }
        PromptError::Transport(err) => json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::TRANSPORT,
                format!("agent send-keys failed: {err}"),
                "the operation was not acknowledged, so delivery is unknown; read the \
                 pane before resending. Run `phux doctor` for a health check",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
    }
}

/// Describe how `record` fails the caller's expectation, or `None` if it
/// meets it.
///
/// With neither `--expect-agent` nor `--expect-kind`, the requirement is
/// simply that the pane host *an* identified agent — which the caller has
/// already established by getting a record back.
pub(super) fn identity_mismatch(
    record: &AgentRecord,
    expect_agent: Option<&str>,
    expect_kind: Option<&str>,
) -> Option<String> {
    if let Some(expected) = expect_agent
        && !record.name.trim().eq_ignore_ascii_case(expected.trim())
    {
        return Some(format!("'{}', not '{expected}'", record.name));
    }
    if let Some(expected) = expect_kind {
        let actual = record.kind.as_deref().unwrap_or("");
        if !actual.trim().eq_ignore_ascii_case(expected.trim()) {
            return Some(format!(
                "'{}' of kind '{}', not kind '{expected}'",
                record.name, actual
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "tests")]

    use super::*;

    fn keys(specs: &[&str]) -> Vec<String> {
        specs.iter().map(|s| (*s).to_owned()).collect()
    }

    fn record(name: &str, kind: Option<&str>) -> AgentRecord {
        AgentRecord {
            name: name.to_owned(),
            kind: kind.map(str::to_owned),
            ..AgentRecord::default()
        }
    }

    /// Ordinary specs — named keys, chords, and literal text — all pass.
    #[test]
    fn valid_specs_pass_validation() {
        assert_eq!(
            validate_key_specs(&keys(&["Enter", "C-c", "M-x", "yes please", "Up"])),
            Ok(())
        );
    }

    /// The all-or-nothing contract: a typo in the third key refuses the whole
    /// batch, and the error names *which* argument so the caller can fix it
    /// without guessing.
    #[test]
    fn a_bad_spec_refuses_the_whole_batch_and_names_its_position() {
        let err = validate_key_specs(&keys(&["approve", "Enter", "C-cc"]))
            .expect_err("`C-cc` must not be accepted as literal text");
        assert_eq!(err.index, 2);
        assert_eq!(err.spec, "C-cc");
        assert!(err.reason.contains("one character"), "{}", err.reason);
    }

    /// The shapes that would otherwise be silently typed as literal text
    /// inside someone's turn.
    #[test]
    fn near_miss_chord_shapes_are_refused() {
        for spec in ["C-", "M-", "c-esc", "C-\u{e9}"] {
            assert!(
                validate_key_specs(&keys(&[spec])).is_err(),
                "'{spec}' must be refused rather than typed literally"
            );
        }
        assert!(validate_key_specs(&keys(&[""])).is_err());
    }

    /// With no expectation flags, any identified agent is acceptable — the
    /// check is "this pane still hosts an agent", which is already more than
    /// `phux send-keys` asserts.
    #[test]
    fn no_expectation_accepts_any_identified_agent() {
        assert_eq!(
            identity_mismatch(&record("reviewer", Some("claude")), None, None),
            None
        );
    }

    /// The check that stops a prompt landing in a bare shell that inherited
    /// the pane: the name must still match.
    #[test]
    fn a_different_occupant_is_a_mismatch() {
        let mismatch = identity_mismatch(&record("builder", Some("codex")), Some("reviewer"), None)
            .expect("a different name must be refused");
        assert!(mismatch.contains("builder"), "{mismatch}");
        assert!(mismatch.contains("reviewer"), "{mismatch}");
    }

    /// The event cap this verb can actually reach. A literal run *not*
    /// followed by `Enter` types one event per character, so a long enough
    /// argument crosses the 256-event protocol cap — and when it does the
    /// batch is refused whole, naming the count. It is never split across two
    /// operations: a half-typed key sequence is the failure the all-or-nothing
    /// contract exists to prevent, and splitting would also give up the
    /// single-operation receipt.
    #[test]
    fn a_batch_past_the_event_cap_is_refused_whole_and_never_split() {
        let long = "x".repeat(phux_protocol::wire::frame::MAX_APPLY_INPUT_EVENTS + 1);
        let events = phux_client::send_keys::events_for(&[long]);
        assert!(
            events.len() > phux_protocol::wire::frame::MAX_APPLY_INPUT_EVENTS,
            "a literal run types one event per character: got {}",
            events.len()
        );
        match validate_batch(&events) {
            Err(Refusal::TooLarge {
                unit,
                limit,
                wire: true,
                ..
            }) => {
                assert_eq!(unit, "events");
                assert_eq!(limit, phux_protocol::wire::frame::MAX_APPLY_INPUT_EVENTS);
            }
            other => panic!("an over-cap batch must be refused: {other:?}"),
        }

        // The submission-safe shape stays comfortably inside the cap: a
        // literal run immediately before `Enter` collapses to one paste plus
        // one key, which is the same two-event batch `agent prompt` sends.
        let submitted = phux_client::send_keys::events_for(&keys(&["yes please", "Enter"]));
        assert_eq!(submitted.len(), 2);
        assert_eq!(validate_batch(&submitted), Ok(()));
    }

    /// Names are compared trimmed and case-insensitively; kinds too. A shell
    /// quoting artifact must not read as a different agent.
    #[test]
    fn names_and_kinds_compare_trimmed_and_case_insensitively() {
        assert_eq!(
            identity_mismatch(
                &record("Reviewer", Some("Claude")),
                Some(" reviewer "),
                None
            ),
            None
        );
        assert_eq!(
            identity_mismatch(&record("reviewer", Some("claude")), None, Some("CLAUDE")),
            None
        );
        assert!(
            identity_mismatch(&record("reviewer", None), None, Some("claude")).is_some(),
            "a record with no kind cannot satisfy --expect-kind"
        );
    }
}
