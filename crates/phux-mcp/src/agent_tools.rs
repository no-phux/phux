//! The `phux_agent_*` MCP tool family: one tool per agent verb, so each
//! freezes only its own argument shape (ADR-0071 point 7(b)); an `action`
//! multiplexer would freeze the union of every verb's arguments.
//!
//! `phux_agent_set` / `phux_agent_clear` run in-process over
//! `phux_client::agent_record`; the rest run the canonical CLI through
//! [`crate::cli_adapter`] (reasons in `crate::tool_table`). There is
//! deliberately no attach/observe (a live stream has no request/response
//! shape), no blind auto-answer, no second read tool (ADR-0077), and no
//! offline `explain --file`. `phux_agent_prompt` fuses submit and wait in
//! one process (ADR-0076 point 6) so a fast turn cannot finish between calls.

#![allow(
    clippy::similar_names,
    reason = "argv and parsed args are deliberately adjacent in thin CLI wrappers"
)]

mod identity;
mod reads;
mod resource_sessions;
mod turn_driving;

#[cfg(test)]
mod tests;

use serde_json::{Value, json};

use crate::cli_adapter::{CliAdapter, bounded_string, bounded_strings};
use crate::tools::ToolError;

/// Lifecycle states a `phux.agent/v1` record can declare (L3 §3.7).
const DECLARED_STATES: &[&str] = &["unknown", "idle", "working", "blocked", "done"];

/// Attention levels a record can declare (L3 §3.7).
const ATTENTION_LEVELS: &[&str] = &["none", "low", "normal", "high"];

/// States `agent wait` accepts. `unknown` is absent on purpose: withdrawing
/// a record is a *departure*, reported as an error, not a state to wait for.
const WAITABLE_STATES: &[&str] = &["idle", "working", "blocked", "done"];

/// Default deadline for `phux_agent_wait`: unlike the CLI's, a tool call is
/// always bounded, since one that blocks forever wedges the host.
const WAIT_DEFAULT_TIMEOUT_SECS: u64 = 600;

/// Default readiness deadline for `phux_agent_start`.
const START_DEFAULT_TIMEOUT_SECS: u64 = 60;

/// Grace added to the tool's own subprocess deadline on top of the CLI
/// timeout, so the CLI reports its own 124 rather than being killed first.
const WAIT_DEADLINE_GRACE_SECS: u64 = 5;

/// `phux agent wait`'s "no transition observed" exit; its document still
/// rides on stdout, so it is a result, not a failure.
const EXIT_WAIT_TIMEOUT: i32 = 124;

/// Shared `target` description for the agent tools.
const TARGET_DESC: &str = "Target selector resolving to one pane \
    (`@N`, `host/@N`, `session:group.pane`, `#tag`, or `.`). Omit for the \
    focused pane.";

/// A bounded string `target` property carrying `description`.
fn target_schema(description: &str) -> Value {
    json!({ "type": "string", "minLength": 1, "maxLength": 4096, "description": description })
}

/// Every schema in this family, in catalog order.
#[must_use]
pub(crate) fn schemas() -> Vec<Value> {
    vec![
        reads::list_schema(),
        reads::show_schema(),
        reads::explain_schema(),
        identity::set_schema(),
        identity::clear_schema(),
        turn_driving::wait_schema(),
        turn_driving::send_keys_schema(),
        turn_driving::prompt_schema(),
        turn_driving::answer_schema(),
        turn_driving::start_schema(),
        resource_sessions::session_open_schema(),
        resource_sessions::session_close_schema(),
        resource_sessions::emit_schema(),
        resource_sessions::log_schema(),
    ]
}

/// Whether `name` belongs to this family (used by `tools/call` dispatch).
#[must_use]
pub(crate) fn owns(name: &str) -> bool {
    matches!(
        name,
        "phux_agent_list"
            | "phux_agent_show"
            | "phux_agent_explain"
            | "phux_agent_set"
            | "phux_agent_clear"
            | "phux_agent_wait"
            | "phux_agent_send_keys"
            | "phux_agent_prompt"
            | "phux_agent_answer"
            | "phux_agent_start"
            | "phux_agent_session_open"
            | "phux_agent_session_close"
            | "phux_agent_emit"
            | "phux_agent_log"
    )
}

/// Dispatch one `phux_agent_*` call.
///
/// # Errors
///
/// Returns [`ToolError`] for an unknown name, a malformed argument, or a
/// canonical-CLI failure.
pub(crate) async fn call(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "phux_agent_set" => identity::set(args).await,
        "phux_agent_clear" => identity::clear(args).await,
        residue => call_with_adapter(residue, args, &CliAdapter::for_residue(residue)?).await,
    }
}

async fn call_with_adapter(
    name: &str,
    args: &Value,
    adapter: &CliAdapter,
) -> Result<Value, ToolError> {
    match name {
        "phux_agent_list" => reads::list(args, adapter).await,
        "phux_agent_show" => reads::projection(args, "show", adapter).await,
        "phux_agent_explain" => reads::projection(args, "explain", adapter).await,
        "phux_agent_wait" => turn_driving::wait(args, adapter).await,
        "phux_agent_send_keys" => turn_driving::send_keys(args, adapter).await,
        "phux_agent_prompt" => turn_driving::prompt(args, adapter).await,
        "phux_agent_answer" => turn_driving::answer(args, adapter).await,
        "phux_agent_start" => turn_driving::start(args, adapter).await,
        "phux_agent_session_open" => resource_sessions::session_open(args, adapter).await,
        "phux_agent_session_close" => resource_sessions::session_close(args, adapter).await,
        "phux_agent_emit" => resource_sessions::emit(args, adapter).await,
        "phux_agent_log" => resource_sessions::log(args, adapter).await,
        other => Err(ToolError::new(format!("unknown agent tool: {other}"))),
    }
}

// -----------------------------------------------------------------------------
// Shared helpers.
// -----------------------------------------------------------------------------

fn validated_until(args: &Value) -> Result<Vec<String>, ToolError> {
    let until = bounded_strings(args, "until", false)?;
    if let Some(state) = until
        .iter()
        .find(|state| !WAITABLE_STATES.contains(&state.as_str()))
    {
        return Err(ToolError::new(format!(
            "`until` must contain only: {} (got {state:?}; 'unknown' is a departure, \
             not a waitable state)",
            WAITABLE_STATES.join(", "),
        )));
    }
    Ok(until)
}

/// The subprocess deadline: the CLI timeout plus grace, so the CLI reports
/// its own 124 rather than being killed first.
const fn graced(timeout_secs: u64) -> std::time::Duration {
    std::time::Duration::from_secs(timeout_secs.saturating_add(WAIT_DEADLINE_GRACE_SECS))
}

/// Run a transition-waiting verb, returning its document even under the
/// "no transition observed" exit.
async fn run_until_transition(
    adapter: &CliAdapter,
    argv: Vec<String>,
    timeout_secs: u64,
    context: &str,
) -> Result<Value, ToolError> {
    let output = adapter
        .run_allowing(argv, graced(timeout_secs), &[EXIT_WAIT_TIMEOUT])
        .await?;
    serde_json::from_str(&output.stdout).map_err(|err| {
        ToolError::new(format!(
            "{context} returned malformed JSON: {err}; stdout={:?}",
            output.stdout
        ))
    })
}

fn timeout_secs(args: &Value, default: u64) -> Result<u64, ToolError> {
    args.get("timeout_secs").map_or(Ok(default), |value| {
        value
            .as_u64()
            .filter(|value| (1..=3600).contains(value))
            .ok_or_else(|| ToolError::new("`timeout_secs` must be an integer in 1..=3600"))
    })
}

fn bool_arg(args: &Value, key: &str) -> Result<bool, ToolError> {
    args.get(key).map_or(Ok(false), |value| {
        value
            .as_bool()
            .ok_or_else(|| ToolError::new(format!("`{key}` must be a boolean")))
    })
}

/// Append the optional `target` positional behind `--`, so a selector
/// starting with `-` is never parsed as a flag.
fn push_target(argv: &mut Vec<String>, args: &Value) -> Result<(), ToolError> {
    if let Some(target) = bounded_string(args, "target", false)? {
        argv.push("--".to_owned());
        argv.push(target);
    }
    Ok(())
}
