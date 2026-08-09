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
//! Two contracts, both hard:
//!
//! 1. **All keys are validated before any byte is written.** Translation
//!    happens up front, on the whole argument vector; a typo in the third key
//!    cannot leave the first two delivered. There is no partial send.
//! 2. **The identity check and the first write ride one connection.** The
//!    server handles a connection's frames in order, so nothing this client
//!    sends interleaves between the `GET_METADATA` answer and the first
//!    `ROUTE_INPUT`. See [`STALENESS`] for what that bound does *not* cover.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::agent_meta::{AgentRecord, TERMINAL_AGENT_KEY, parse_agent_record};
use phux_client::attach::AttachError;
use phux_client::attach::connection::Connection;
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{Command, CommandResult, Scope};
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target};

/// The staleness bound this verb accepts, stated once so a reader does not
/// have to infer it from the code.
///
/// The identity read and the first input write are consecutive frames on one
/// connection, and `phux-server` handles a connection's frames in arrival
/// order, so **no frame from this client** can interleave between them. What
/// still can, in that window:
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

/// Stable error codes this verb reports.
///
/// These belong in `crate::commands::json_err::codes` alongside the rest of
/// the closed vocabulary; they are declared here only because that module is
/// owned by a concurrent change in this wave. Fold them in when the two land.
mod codes {
    /// A key argument would not translate to the key the caller clearly
    /// meant, so the whole batch is refused before anything is written.
    pub(super) const INVALID_KEY_SPEC: &str = "invalid_key_spec";
    /// The pane declares no `phux.agent/v1` record, so there is no agent
    /// identity to verify against.
    pub(super) const NO_AGENT_RECORD: &str = "no_agent_record";
    /// The pane is hosting a different agent than the caller named.
    pub(super) const AGENT_MISMATCH: &str = "agent_mismatch";
}

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

    let selector = match parse_selector(Some(target)) {
        Ok(selector) => selector,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    rt.block_on(async move {
        let pane = match resolve_target(&socket_path, &selector, "agent send-keys", json).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let mut conn = match Connection::connect(&socket_path).await {
            Ok(conn) => conn,
            Err(err) => {
                return json_err::report_no_server(json, &err, &socket_path, "agent send-keys");
            }
        };
        let label = crate::selector::format_terminal_id(&pane);
        // Contract 2: identity read and input write on ONE connection.
        let record = match verify_occupant(&mut conn, &pane, expect_agent, expect_kind).await {
            Ok(record) => record,
            Err(VerifyFailure::Transport(err)) => {
                return json_err::report_no_server(json, &err, &socket_path, "agent send-keys");
            }
            Err(VerifyFailure::NoRecord) => {
                return json_err::emit(
                    json,
                    &json_err::CliError::new(
                        codes::NO_AGENT_RECORD,
                        format!(
                            "{label} declares no phux.agent/v1 record, so there is no agent \
                             identity to verify"
                        ),
                        "`phux send-keys` addresses the pane and checks no identity — use \
                         it when that is what you mean. To make this pane addressable as \
                         an agent, run `phux agent set` or `phux agent install-claude`",
                    ),
                    crate::exit_codes::EXIT_USAGE,
                );
            }
            Err(VerifyFailure::Mismatch(mismatch)) => {
                return json_err::emit(
                    json,
                    &json_err::CliError::new(
                        codes::AGENT_MISMATCH,
                        format!("{label} is hosting {mismatch}; nothing was sent"),
                        format!(
                            "the occupant is re-read immediately before the write \
                             (staleness bound: {STALENESS}); re-resolve the target, or use \
                             `phux send-keys` if you meant the pane rather than the agent"
                        ),
                    ),
                    crate::exit_codes::EXIT_USAGE,
                );
            }
        };

        if let Err(err) = route_events(&mut conn, &pane, events).await {
            return report_route_failure(json, &label, &socket_path, err);
        }

        report_delivered(json, &label, &record, keys.len())
    })
}

/// Report a batch that stopped part-way.
///
/// The remedy is deliberately not "retry": the all-or-nothing contract covers
/// *validation*, not delivery — once the first `ROUTE_INPUT` is acked those
/// bytes are in the pane's tty input queue and no client can take them back.
fn report_route_failure(
    json: bool,
    label: &str,
    socket_path: &std::path::Path,
    err: RouteFailure,
) -> ExitCode {
    match err {
        RouteFailure::Transport(err) => {
            json_err::report_no_server(json, &err, socket_path, "agent send-keys")
        }
        RouteFailure::Refused(message) => json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::TRANSPORT,
                format!("{label} refused input: {message}"),
                "some keys may already have been delivered; re-read the pane with \
                 `phux snapshot` before retrying",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
    }
}

/// Report a fully delivered batch: silence without `--json` (the prose
/// `send-keys` says nothing on success either), one document with it.
fn report_delivered(json: bool, label: &str, record: &AgentRecord, keys: usize) -> ExitCode {
    if !json {
        return ExitCode::SUCCESS;
    }
    let document = serde_json::json!({
        "schema_version": 1,
        "terminal": label,
        "agent": { "name": record.name, "kind": record.kind },
        "keys": keys,
        "verified": true,
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

/// Why the occupant check refused the write.
#[derive(Debug)]
enum VerifyFailure {
    /// Connect/transport/protocol failure reading the record.
    Transport(AttachError),
    /// The pane declares no record, so there is no identity to check.
    NoRecord,
    /// The pane hosts a different agent than the caller named; the string
    /// describes the occupant.
    Mismatch(String),
}

/// Re-read the pane's occupant over `conn` and confirm it is the agent the
/// caller named, immediately before the first byte is written.
///
/// This is the one thing `phux agent send-keys` does that `phux send-keys`
/// does not. See [`STALENESS`] for exactly how fresh the answer is.
async fn verify_occupant(
    conn: &mut Connection,
    pane: &TerminalId,
    expect_agent: Option<&str>,
    expect_kind: Option<&str>,
) -> Result<AgentRecord, VerifyFailure> {
    let record = read_record(conn, pane)
        .await
        .map_err(VerifyFailure::Transport)?
        .ok_or(VerifyFailure::NoRecord)?;
    identity_mismatch(&record, expect_agent, expect_kind).map_or(Ok(record), |mismatch| {
        Err(VerifyFailure::Mismatch(mismatch))
    })
}

/// Why the input batch stopped part-way.
#[derive(Debug)]
enum RouteFailure {
    /// Connect/transport/protocol failure.
    Transport(AttachError),
    /// The server refused a `ROUTE_INPUT`.
    Refused(String),
}

/// Deliver `events` to `pane` over the already-verified `conn`.
///
/// Each `ROUTE_INPUT` is acked before the next is sent, so the events land on
/// the pane actor's input mailbox in send order. This mirrors
/// `phux_client::send_keys::route_keys` rather than calling it, because that
/// function opens its own connection — and the identity read has to share
/// this one for the ordering guarantee in [`STALENESS`] to hold.
async fn route_events(
    conn: &mut Connection,
    pane: &TerminalId,
    events: Vec<phux_protocol::input::InputEvent>,
) -> Result<(), RouteFailure> {
    for (index, event) in events.into_iter().enumerate() {
        // request_id 1 was the identity read; input acks start at 2 so this
        // connection never reuses a correlation id.
        let request_id = u32::try_from(index)
            .unwrap_or(u32::MAX - 1)
            .saturating_add(2);
        let reply = conn
            .request(
                request_id,
                Command::RouteInput {
                    terminal_id: pane.clone(),
                    event,
                },
            )
            .await
            .map_err(RouteFailure::Transport)?;
        // This connection subscribed to nothing and `handle_route_input`
        // emits no frame of its own before the ack, so there is nothing for
        // the server to interleave here (same argument as
        // `phux_client::send_keys::paste_to`).
        match reply.into_result_ignoring_interleaved() {
            CommandResult::Ok => {}
            CommandResult::Error { message, .. } => return Err(RouteFailure::Refused(message)),
            other => {
                return Err(RouteFailure::Refused(format!(
                    "unexpected reply to ROUTE_INPUT: {other:?}"
                )));
            }
        }
    }
    Ok(())
}

/// Read the pane's `phux.agent/v1` record over `conn` with correlation id 1.
async fn read_record(
    conn: &mut Connection,
    pane: &TerminalId,
) -> Result<Option<AgentRecord>, AttachError> {
    let (answer, interleaved) = conn
        .request_metadata(
            1,
            Scope::Terminal(pane.clone()),
            TERMINAL_AGENT_KEY.to_owned(),
        )
        .await?
        .into_parts();
    // This connection holds no subscription and `handle_get_metadata` pushes
    // nothing ahead of its answer, so an interleaved frame here would be a
    // server bug rather than a transition worth keeping.
    debug_assert!(
        interleaved.is_empty(),
        "unsubscribed connection received {interleaved:?} ahead of METADATA_VALUE",
    );
    let value = answer.map_err(|refusal| AttachError::Refused(refusal.to_string()))?;
    Ok(value.as_deref().and_then(parse_agent_record))
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
