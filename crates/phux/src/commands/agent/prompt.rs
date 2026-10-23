//! `phux agent prompt` — hand an agent a turn's worth of work, with a
//! receipt (ADR-0076), over the acknowledged `APPLY_INPUT` lane (ADR-0053).
//!
//! A prompt is not idempotent at the receiver, so resend-on-doubt produces the
//! duplicate. The rules callers rely on:
//!
//! 1. **`INPUT_DELIVERY_UNKNOWN` is terminal**: exit 1 (not 3, which means
//!    "retry is correct"), never retried; the recovery is to read the pane.
//! 2. **`INPUT_NOT_WRITTEN` is provably nothing-written**: also exit 1, but
//!    resubmitting is safe.
//! 3. **One operation id per invocation**, reused by every internal retry.
//! 4. **The acknowledged path is required**: an older server or a satellite
//!    target is refused (exit 2), never downgraded to `ROUTE_INPUT`.
//!
//! `OK` means the bytes are in the tty input queue, not that they were
//! consumed; `--wait` reports an observed detector transition. Admission is
//! per pane (L1 §6.2.1): prompts to different panes run concurrently, and only
//! a second prompt to the same pane collides as busy.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use phux_client::agent_meta::AgentRecord;
use phux_client::agent_prompt::{PromptError, PromptOutcome, PromptWait, Refusal, prompt_agent};
use phux_client::attach::AttachError;
use phux_protocol::ids::InputOperationId;
use phux_server::runtime::default_socket_path;

use crate::commands::json_err::codes;
use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target_for_input};

/// Version of the `agent prompt` result document (ADR-0076 point 7).
const RESULT_SCHEMA_VERSION: u8 = 1;

/// Poll-floor cadence for the `--wait` half's `GET_METADATA` re-read — the
/// same gap `phux wait` and `phux agent wait` use.
const POLL_INTERVAL: Duration = phux_client::wait::DEFAULT_POLL_INTERVAL;

mod error_projection;
pub(super) use error_projection::refusal_code;
use error_projection::report_failure;

/// Mint the operation id for this invocation from the OS CSPRNG (ADR-0053);
/// every retry inside [`phux_client::agent_prompt`] reuses it.
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
    // `--until`/`--timeout` without `--wait` would be misread as a completion gate.
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
    let targets = match super::wait::resolve_until(until) {
        Ok(targets) => targets,
        Err(err) => return json_err::emit(json, &err, crate::exit_codes::EXIT_USAGE),
    };

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
        let pane =
            match resolve_target_for_input(&socket_path, &selector, "agent prompt", json).await {
                Ok(id) => id,
                Err(code) => return code,
            };
        let label = phux_client::selector::format_terminal_id(&pane);
        // ADR-0076 point 4: the same identity comparison `agent send-keys` does.
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
            Err(err) => report_failure(
                json,
                &label,
                &socket_path,
                &err,
                &phux_client_core::input_replay::operation_id_hex(&operation_id),
            ),
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
            "agent": {
                "name": outcome.agent.name,
                "kind": outcome.agent.kind,
                "state": outcome.agent.state.as_str(),
                "session": outcome.agent.session,
            },
            "pre_submit_state": outcome.pre_submit_state.as_str(),
            // Null: L3 cannot tell detector-owned from declared records, so
            // no freshness bound can be claimed.
            "staleness_bound_ms": serde_json::Value::Null,
            "attempts": outcome.attempts,
            "submit_ms": outcome.submit_ms,
            "transition_observed": satisfied,
            // "level" is reserved but never emitted: only a transition satisfies.
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
    // The prompt landed; the turn did not visibly end. Say both.
    let last = outcome
        .wait
        .as_ref()
        .map_or(outcome.pre_submit_state, |wait| wait.last);
    error!(
        "prompt delivered to {label} (operation {}), but no transition into a target \
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
