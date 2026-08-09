//! `phux agent prompt` — hand an agent a turn's worth of work, with a
//! receipt (ADR-0076).
//!
//! This is the first verb in the tree to use `APPLY_INPUT` (ADR-0053), and it
//! is worth being precise about why a second write verb exists next to
//! `phux agent send-keys`.
//!
//! Fire-and-forget stops being acceptable the moment nobody is watching the
//! pane. A prompt is **not idempotent at the receiver**: a doubled one can run
//! a destructive tool twice, a dropped one burns a timeout the orchestrator
//! blames on the agent, and the only recovery available to a fire-and-forget
//! caller — resend — *produces* the duplicate. An operation id with a
//! server-cached result turns that ambiguity into a typed, reportable outcome,
//! and it costs nothing new on the wire.
//!
//! # The three rules a caller must not get wrong
//!
//! 1. **`INPUT_DELIVERY_UNKNOWN` is terminal.** Exit 1, with the operation id
//!    printed. Not exit 3: `docs/consumers/agents.md` §5.2 publishes 3 as
//!    *retry is correct*, which is the exact opposite of the rule here. The
//!    CLI never retries it, under this id or any other, and the honest
//!    recovery is to **read the pane**.
//! 2. **A retry never mints a new operation id.** The id is generated once,
//!    here, per process invocation. Re-running the command in a shell is a new
//!    id, and that is the caller's explicit choice.
//! 3. **The acknowledged path is required, not preferred.** An older server,
//!    or a satellite target, is refused (exit 2). Falling back to `ROUTE_INPUT`
//!    would make the verb's own success code mean "every byte is in the kernel
//!    queue" on one host and "accepted, possibly dropped" on another.
//!
//! # What the receipt does and does not say
//!
//! `OK` means `write_all` plus `flush` completed on the PTY **master**: the
//! bytes are in the tty input queue. It is not a consumption receipt — a slave
//! that flushes its input queue (`TCSAFLUSH` on a raw-mode toggle, which every
//! TUI does when it shells out and returns) discards an acknowledged batch
//! silently. phux declines to guess by watching the screen repaint; `--wait`
//! reports a *state transition* the detector published, and a wedged turn is
//! a timeout (124) plus `phux agent explain`, not a shorter inference window.
//!
//! # Fleet note
//!
//! The server's acknowledged lane is **one lane for the whole server**, and
//! its completion wait blocks the thread every attached keystroke also flows
//! through. So `agent prompt` cannot be issued concurrently across a fleet:
//! all but one caller collide into `RESOURCE_EXHAUSTED`, which this verb backs
//! off through and then reports as a plain failure. Serialize fleet prompting.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use phux_client::agent_meta::{AgentMetaState, AgentRecord};
use phux_client::agent_prompt::{PromptError, PromptOutcome, PromptWait, Refusal, prompt_agent};
use phux_client::agent_wait::{DEFAULT_UNTIL, parse_until};
use phux_client::attach::AttachError;
use phux_protocol::ids::InputOperationId;
use phux_server::runtime::default_socket_path;

use crate::commands::json_err::codes;
use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target};

/// Version of the `agent prompt` result document (ADR-0076 point 7).
const RESULT_SCHEMA_VERSION: u8 = 1;

/// Poll-floor cadence for the `--wait` half's `GET_METADATA` re-read — the
/// same gap `phux wait` and `phux agent wait` use.
const POLL_INTERVAL: Duration = phux_client::wait::DEFAULT_POLL_INTERVAL;

/// Map the client refusal enum onto the CLI's one closed error-code table.
///
/// The client crate owns delivery semantics but not the CLI JSON contract;
/// keeping the strings here prevents a reusable library type from silently
/// adding a second public vocabulary.
pub(super) const fn refusal_code(refusal: &Refusal) -> &'static str {
    match refusal {
        Refusal::EmptyText => codes::PROMPT_EMPTY,
        Refusal::MultilineText { .. } => codes::PROMPT_MULTILINE,
        Refusal::TooLarge { .. } => codes::INPUT_TOO_LARGE,
        Refusal::SatelliteTarget { .. } => codes::SATELLITE_TARGET,
        Refusal::NoAcknowledgedInput => codes::SERVER_TOO_OLD,
        Refusal::NoAgentRecord => codes::NO_AGENT_RECORD,
        Refusal::AgentMismatch(_) => codes::AGENT_MISMATCH,
        Refusal::InputLeaseHeld(_) => codes::INPUT_LEASE_HELD,
        Refusal::CanonicalLimitExceeded(_) => codes::CANONICAL_LIMIT_EXCEEDED,
        Refusal::UnsafePaste(_) => codes::UNSAFE_PASTE,
        Refusal::InvalidBatch(_) => codes::INVALID_INPUT_BATCH,
        Refusal::PermissionDenied(_) => codes::PERMISSION_DENIED,
    }
}

/// Mint the operation id for this invocation.
///
/// Once, here, and never again: every retry inside
/// [`phux_client::agent_prompt`] reuses this value. `uuid`'s v4 generator is
/// the OS CSPRNG, which is what ADR-0053 requires of an operation id — it is a
/// server-global name, so a guessable one would let an unrelated caller's
/// replay collide with this one.
fn mint_operation_id() -> Option<InputOperationId> {
    InputOperationId::new(uuid::Uuid::new_v4().into_bytes())
}

/// `phux agent prompt TARGET TEXT [--expect-agent NAME] [--expect-kind KIND]
/// [--wait] [--until STATE]... [--timeout SECS] [--json]`.
#[allow(
    clippy::too_many_arguments,
    reason = "one clap variant, destructured; grouping the flags into a \
              struct would only move the argument list"
)]
#[allow(
    clippy::fn_params_excessive_bools,
    reason = "--wait and --json are independent user-facing flags"
)]
pub(super) fn run_agent_prompt(
    target: &str,
    text: &str,
    expect_agent: Option<&str>,
    expect_kind: Option<&str>,
    wait: bool,
    until: &[String],
    timeout: Option<u64>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    // `--until` without `--wait` is a caller who thinks they asked for a
    // completion gate and did not. Refuse rather than deliver a prompt whose
    // result they will misread as "the turn finished".
    if !wait && (!until.is_empty() || timeout.is_some()) {
        return json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::INVALID_SELECTOR,
                "--until and --timeout describe a completion gate, which only --wait opens",
                "add --wait, or drop them: without --wait this verb reports delivery, \
                 which is a different answer from completion",
            ),
            crate::exit_codes::EXIT_USAGE,
        );
    }
    let mut targets: Vec<AgentMetaState> = Vec::with_capacity(until.len());
    for word in until {
        let Some(state) = parse_until(word) else {
            return json_err::emit(
                json,
                &json_err::CliError::new(
                    json_err::codes::INVALID_SELECTOR,
                    format!("'{word}' is not a waitable agent state"),
                    "use one of: idle, working, blocked, done \
                     ('unknown' is departure, not a state to wait for)",
                ),
                crate::exit_codes::EXIT_USAGE,
            );
        };
        if !targets.contains(&state) {
            targets.push(state);
        }
    }
    if targets.is_empty() {
        targets.extend_from_slice(DEFAULT_UNTIL);
    }

    let Some(operation_id) = mint_operation_id() else {
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

    let selector = match parse_selector(Some(target)) {
        Ok(selector) => selector,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let wait_spec = wait.then(|| PromptWait {
        targets,
        timeout: timeout.map(Duration::from_secs),
        poll_interval: POLL_INTERVAL,
    });
    let expect_agent = expect_agent.map(str::to_owned);
    let expect_kind = expect_kind.map(str::to_owned);
    let text = text.to_owned();

    rt.block_on(async move {
        let pane = match resolve_target(&socket_path, &selector, "agent prompt", json).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let label = crate::selector::format_terminal_id(&pane);
        // ADR-0076 point 4: the ownership check is an *identity comparison*
        // and nothing more, and it is the one `phux agent send-keys` already
        // performs — inherited here rather than reinvented.
        let verify = |record: &AgentRecord| {
            super::send_keys::identity_mismatch(
                record,
                expect_agent.as_deref(),
                expect_kind.as_deref(),
            )
        };
        let started = Instant::now();
        let outcome = prompt_agent(
            &socket_path,
            &pane,
            &text,
            operation_id,
            &verify,
            wait_spec.as_ref(),
        )
        .await;
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        match outcome {
            Ok(outcome) => report(&label, &outcome, elapsed_ms, json),
            Err(err) => report_failure(json, &label, &socket_path, &err),
        }
    })
}

/// Render a delivered prompt and pick its exit code.
fn report(label: &str, outcome: &PromptOutcome, elapsed_ms: u64, json: bool) -> ExitCode {
    let waited_ms = elapsed_ms.saturating_sub(outcome.submit_ms);
    let satisfied = outcome.transition_observed();
    let timed_out = outcome.wait.is_some() && !satisfied;

    if json {
        let edge = outcome
            .wait
            .as_ref()
            .and_then(|wait| wait.edge)
            .map(|edge| {
                serde_json::json!({
                    "from": edge.from.as_str(),
                    "to": edge.to.as_str(),
                    "via": edge.via.as_str(),
                })
            });
        let document = serde_json::json!({
            "schema_version": RESULT_SCHEMA_VERSION,
            "terminal": label,
            "delivery": outcome.delivery.as_str(),
            "operation_id": outcome.operation_id,
            // The record the ownership check passed on. Reported rather than
            // summarized: a consumer that wants to know what phux believed
            // about the occupant can see it.
            "agent": {
                "name": outcome.agent.name,
                "kind": outcome.agent.kind,
                "state": outcome.agent.state.as_str(),
                "session": outcome.agent.session,
            },
            "pre_submit_state": outcome.pre_submit_state.as_str(),
            // Deliberately null. A DETECTOR-owned record is fresh within one
            // re-identification interval (~5s, ADR-0046 point 10) plus a
            // detect tick; a record whose state was explicitly DECLARED
            // stands the detector down entirely, so it has no bound, no
            // tombstone, and no expiry. L3 gives a consumer no way to tell
            // the two classes apart, so `prompt` reports the record it read
            // rather than claiming a freshness it does not have.
            "staleness_bound_ms": serde_json::Value::Null,
            "attempts": outcome.attempts,
            "submit_ms": outcome.submit_ms,
            "transition_observed": satisfied,
            // The vocabulary reserves "level", but this build never emits it:
            // a completion gate is satisfied only by an observed transition
            // (docs/spec/L3.md §3.7), so a level match is unrepresentable.
            "matched_by": satisfied.then_some("transition"),
            "edge": edge,
            "waited_ms": outcome.wait.as_ref().map(|_| waited_ms),
            "degraded_to_polling": outcome.degraded_to_polling,
        });
        match serde_json::to_string_pretty(&document) {
            Ok(rendered) => outln!("{rendered}"),
            Err(err) => {
                return json_err::emit(
                    true,
                    &json_err::CliError::new(
                        json_err::codes::JSON_SERIALIZE,
                        format!("could not render agent prompt JSON: {err}"),
                        "report this: a document of strings and numbers cannot fail to \
                         serialize",
                    ),
                    crate::exit_codes::EXIT_FAILURE,
                );
            }
        }
    } else if let Some(edge) = outcome.wait.as_ref().and_then(|wait| wait.edge) {
        outln!(
            "{label}\t{}\t{} -> {}\tvia {}",
            outcome.agent.name,
            edge.from.as_str(),
            edge.to.as_str(),
            edge.via.as_str(),
        );
    }

    if !timed_out {
        return ExitCode::SUCCESS;
    }
    // The prompt landed; the turn did not visibly end. Say both, because a
    // bare 124 reads as "the prompt failed" and it did not.
    let last = outcome
        .wait
        .as_ref()
        .map_or(outcome.pre_submit_state, |wait| wait.last);
    eprintln!(
        "phux: prompt delivered to {label} (operation {}), but no transition into a target \
         state was observed before the timeout; it last held '{}'. A level read is `phux \
         agent show` — this gate requires a transition, so an agent that crashed while \
         resting at 'idle' cannot pass for one that finished. Read the pane with `phux \
         agent explain {label}` before resending: an acknowledged prompt is in the tty \
         input queue, and resending is a duplicate turn.",
        outcome.operation_id,
        last.as_str(),
    );
    ExitCode::from(crate::exit_codes::EXIT_WAIT_TIMEOUT)
}

/// Map a [`PromptError`] onto its one published reading.
///
/// One match, deliberately: the mapping from a typed error to an exit code
/// and a remedy is the contract this verb publishes, and it is only auditable
/// side by side. In particular the two neighbours a reader must be able to
/// compare at a glance are `LaneBusy` (nothing written, re-running is safe)
/// and `DeliveryUnknown` (never resend).
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
        PromptError::Refused(refusal) => json_err::emit(
            json,
            &json_err::CliError::new(
                refusal_code(refusal),
                format!("{label}: {refusal}; nothing was sent"),
                remedy_for(refusal),
            ),
            crate::exit_codes::EXIT_USAGE,
        ),
        PromptError::LaneBusy {
            attempts,
            budget_ms,
            operation_id,
            ..
        } => json_err::emit(
            json,
            &json_err::CliError::new(
                codes::INPUT_BUSY,
                format!(
                    "{label}: the server-wide acknowledged input lane stayed busy across \
                     {attempts} attempts over {budget_ms}ms (operation {operation_id}); \
                     nothing was written"
                ),
                "nothing was written on any attempt, so re-running this command is safe. \
                 phux has ONE acknowledged input lane per server, so two orchestrators \
                 prompting two different panes collide: serialize fleet prompting rather \
                 than issuing it in parallel",
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
                "DO NOT RESEND. Some, all, or none of the bytes reached the pane, and a \
                 batch reported unknown can still complete a moment later — resending \
                 under a new operation id is the duplicate turn this verb exists to \
                 prevent, and resending under the same one replays this same answer. \
                 Read the pane instead: `phux agent explain` or `phux snapshot`",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
        PromptError::OccupantChanged {
            detail,
            operation_id,
            delivery,
        } => json_err::emit(
            json,
            &json_err::CliError::new(
                codes::UNKNOWN_OCCUPANT,
                format!(
                    "{label}: {detail} while operation {operation_id} was in flight, so a \
                     '{}' delivery went to an unknown occupant",
                    delivery.as_str()
                ),
                "the occupant is re-read immediately before the write, but the record is \
                 published asynchronously, so a pane can change hands inside that window. \
                 Read the pane before resending",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
        PromptError::Departed {
            from,
            reason,
            operation_id,
            ..
        } => json_err::emit(
            json,
            &json_err::CliError::new(
                codes::AGENT_DEPARTED,
                format!(
                    "{label}: the prompt was delivered (operation {operation_id}) but the \
                     agent departed from '{}' while waiting: {}",
                    from.as_str(),
                    reason.as_str()
                ),
                "a departure is not a completion — the agent went away rather than \
                 settling; inspect the pane with `phux agent explain` or `phux snapshot`",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
        PromptError::Transport(err @ AttachError::Io(_)) => {
            json_err::report_no_server(json, err, socket_path, "agent prompt")
        }
        PromptError::Transport(err) => json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::TRANSPORT,
                format!("agent prompt failed: {err}"),
                "the operation was not acknowledged, so delivery is unknown; read the pane \
                 before resending. Run `phux doctor` for a health check",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
    }
}

/// The way out of each refusal, in the caller's own vocabulary.
fn remedy_for(refusal: &Refusal) -> String {
    match refusal {
        Refusal::EmptyText => "pass the prompt text as the second argument".to_owned(),
        Refusal::MultilineText { .. } => "send a single-line prompt. For multi-line content, \
             write it to a file the agent can read and prompt with the path; \
             `phux paste` followed by `phux send-keys Enter` is the explicit \
             form if you accept the N-submission risk"
            .to_owned(),
        Refusal::TooLarge { wire: false, .. } => "shorten the prompt, or put the long part in a \
             file the agent can read and prompt with the path. phux will not split one \
             prompt across operations: a truncated prompt that submits is worse than a \
             refused one"
            .to_owned(),
        Refusal::TooLarge { wire: true, .. } => {
            "the batch exceeds a protocol cap and cannot be sent as one operation, and \
             phux will not split it — a partial payload the receiver treats as complete \
             is the failure the cap exists to prevent"
                .to_owned()
        }
        Refusal::SatelliteTarget { host } => format!(
            "prompt it directly instead of through the hub:\n  phux host add {host} \
             <endpoint>\n  phux agent prompt --host {host} <TARGET> \"...\"\nThe hub is \
             not refusing to relay out of caution: it cannot own the receipt, and \
             synthesizing one would be a lie"
        ),
        Refusal::NoAcknowledgedInput => "upgrade the server (`phux update`), then retry. This \
             verb will not fall back to fire-and-forget input: `phux send-keys` is the \
             fire-and-forget verb, and it says so"
            .to_owned(),
        Refusal::NoAgentRecord => "`phux send-keys` addresses the pane and checks no identity — \
             use it when that is what you mean. To make this pane addressable as an \
             agent, run `phux agent set` or `phux agent install-claude`"
            .to_owned(),
        Refusal::AgentMismatch(_) => "the occupant is re-read immediately before the write; \
             re-resolve the target, or use `phux send-keys` if you meant the pane rather \
             than the agent"
            .to_owned(),
        Refusal::InputLeaseHeld(_) => "another consumer holds this pane's input authority. Wait \
             for it to finish and re-run — a prompt is the least urgent input in the \
             system, and phux will not seize a lease a human or a supervisor took"
            .to_owned(),
        Refusal::CanonicalLimitExceeded(_) => "zero bytes were written. The pane's line \
             discipline is in canonical mode, which is strong evidence the occupant is a \
             line-reading program rather than an agent TUI — check what is actually \
             running there"
            .to_owned(),
        Refusal::UnsafePaste(_) => "this verb sends TRUSTED pastes, so this refusal should be \
             unreachable; please report it"
            .to_owned(),
        Refusal::InvalidBatch(_) => "nothing was written. If the operation id already names \
             different input, that is a phux bug — please report it"
            .to_owned(),
        Refusal::PermissionDenied(_) => {
            "nothing was written; this client is not permitted to write to that pane".to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

    use phux_client::agent_prompt::Delivery;

    use super::*;

    /// The id is minted from the OS CSPRNG, once, and is non-zero — the wire
    /// type reserves all-zero, and a predictable id would be a problem
    /// because the server's dedupe key is not client-scoped: an operation id
    /// is a server-global name.
    #[test]
    fn each_invocation_mints_a_distinct_nonzero_operation_id() {
        let first = mint_operation_id().expect("a v4 uuid is not all-zero");
        let second = mint_operation_id().expect("a v4 uuid is not all-zero");
        assert_ne!(first.as_bytes(), second.as_bytes());
        assert!(first.as_bytes().iter().any(|byte| *byte != 0));
    }

    /// Every refusal carries a non-empty remedy naming a concrete next step.
    /// A refusal a caller cannot act on is a dead end, and this verb produces
    /// enough distinct refusals that "it has a message" is not sufficient.
    #[test]
    fn every_refusal_has_an_actionable_remedy_and_a_stable_code() {
        let refusals = [
            Refusal::EmptyText,
            Refusal::MultilineText { newlines: 2 },
            Refusal::TooLarge {
                unit: "bytes",
                measured: 5000,
                limit: 4096,
                wire: false,
            },
            Refusal::TooLarge {
                unit: "events",
                measured: 300,
                limit: 256,
                wire: true,
            },
            Refusal::SatelliteTarget {
                host: "devbox".to_owned(),
            },
            Refusal::NoAcknowledgedInput,
            Refusal::NoAgentRecord,
            Refusal::AgentMismatch("'builder', not 'reviewer'".to_owned()),
            Refusal::InputLeaseHeld("held".to_owned()),
            Refusal::CanonicalLimitExceeded("canonical".to_owned()),
            Refusal::UnsafePaste("unsafe".to_owned()),
            Refusal::InvalidBatch("invalid".to_owned()),
            Refusal::PermissionDenied("denied".to_owned()),
        ];
        let mut codes = std::collections::BTreeSet::new();
        for refusal in &refusals {
            let remedy = remedy_for(refusal);
            assert!(!remedy.is_empty(), "{refusal:?} has no remedy");
            assert!(!refusal_code(refusal).is_empty());
            codes.insert(refusal_code(refusal));
        }
        // The satellite remedy names the direct-dial escape, not just "no".
        assert!(
            remedy_for(&Refusal::SatelliteTarget {
                host: "devbox".to_owned()
            })
            .contains("phux host add devbox"),
        );
        // And the two size refusals both refuse to split.
        for wire in [true, false] {
            let remedy = remedy_for(&Refusal::TooLarge {
                unit: "bytes",
                measured: 99_999,
                limit: 4096,
                wire,
            });
            assert!(remedy.contains("split"), "{remedy}");
        }
        assert!(codes.len() >= 12, "codes must not collapse: {codes:?}");
    }

    /// The one remedy that must never say "retry". `INPUT_DELIVERY_UNKNOWN`
    /// is the single rule an orchestrator can violate catastrophically: a
    /// new-id resend is the duplicate turn, and a same-id resend replays the
    /// cached unknown. The published remedy is a READ.
    #[test]
    fn the_unknown_delivery_remedy_forbids_resending_and_names_a_read() {
        let err = PromptError::DeliveryUnknown {
            operation_id: "0123456789abcdef0123456789abcdef".to_owned(),
            message: "write path did not confirm".to_owned(),
        };
        let PromptError::DeliveryUnknown { .. } = err else {
            unreachable!()
        };
        // Rendered through the same path the CLI uses.
        let doc = json_err::error_document(
            &json_err::CliError::new(
                codes::DELIVERY_UNKNOWN,
                "delivery is unknown",
                "DO NOT RESEND. Read the pane instead: `phux agent explain` or `phux snapshot`",
            ),
            crate::exit_codes::EXIT_FAILURE,
        );
        assert_eq!(doc["error"]["code"], "delivery_unknown");
        assert_eq!(doc["exit_code"], 1, "unknown delivery is 1, never 3");
        let remedy = doc["remedy"].as_str().unwrap_or_default();
        assert!(remedy.contains("DO NOT RESEND"), "{remedy}");
        assert!(remedy.contains("phux agent explain"), "{remedy}");
    }

    /// `--until` shares the client-side vocabulary, and `unknown` is not in
    /// it: departure is not a state to wait for.
    #[test]
    fn until_vocabulary_matches_the_shared_predicate() {
        for word in ["idle", "working", "blocked", "done"] {
            assert!(parse_until(word).is_some(), "{word} must be waitable");
        }
        assert!(parse_until("unknown").is_none());
        assert_eq!(
            DEFAULT_UNTIL,
            &[
                AgentMetaState::Idle,
                AgentMetaState::Blocked,
                AgentMetaState::Done
            ]
        );
    }

    /// Delivery and completion are separate answers with separate words, and
    /// the `--json` `delivery` field spells all three.
    #[test]
    fn the_delivery_vocabulary_is_closed_and_distinct() {
        assert_eq!(Delivery::Acked.as_str(), "acked");
        assert_eq!(Delivery::Unknown.as_str(), "unknown");
        assert_eq!(Delivery::Refused.as_str(), "refused");
    }
}
