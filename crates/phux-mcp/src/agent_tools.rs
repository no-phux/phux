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

use phux_client::agent_meta::{AgentAttention, AgentMetaState, AgentRecord};
use phux_client::attach::connection::Connection;
use phux_client::selector::{Selector, format_terminal_id};
use phux_protocol::ids::ResourceId;
use serde_json::{Value, json};

use crate::cli_adapter::{
    CliAdapter, DEFAULT_CALL_TIMEOUT, bounded_string, bounded_strings, enum_string, push_socket,
};
use crate::cli_tools::{push_option, schema, string_schema};
use crate::tools::{ToolError, parse_selector, resolve_one, socket_arg, strict_object};

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
    (`@N`, `host/@N`, `session:window.pane`, `#tag`, or `.`). Omit for the \
    focused pane.";

/// A bounded string `target` property carrying `description`.
fn target_schema(description: &str) -> Value {
    json!({ "type": "string", "minLength": 1, "maxLength": 4096, "description": description })
}

/// Every schema in this family, in catalog order.
#[must_use]
pub(crate) fn schemas() -> Vec<Value> {
    vec![
        list_schema(),
        show_schema(),
        explain_schema(),
        set_schema(),
        clear_schema(),
        wait_schema(),
        send_keys_schema(),
        prompt_schema(),
        answer_schema(),
        start_schema(),
        session_open_schema(),
        session_close_schema(),
        emit_schema(),
        log_schema(),
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
        "phux_agent_set" => set(args).await,
        "phux_agent_clear" => clear(args).await,
        residue => call_with_adapter(residue, args, &CliAdapter::for_residue(residue)?).await,
    }
}

async fn call_with_adapter(
    name: &str,
    args: &Value,
    adapter: &CliAdapter,
) -> Result<Value, ToolError> {
    match name {
        "phux_agent_list" => list(args, adapter).await,
        "phux_agent_show" => projection(args, "show", adapter).await,
        "phux_agent_explain" => projection(args, "explain", adapter).await,
        "phux_agent_wait" => wait(args, adapter).await,
        "phux_agent_send_keys" => send_keys(args, adapter).await,
        "phux_agent_prompt" => prompt(args, adapter).await,
        "phux_agent_answer" => answer(args, adapter).await,
        "phux_agent_start" => start(args, adapter).await,
        "phux_agent_session_open" => session_open(args, adapter).await,
        "phux_agent_session_close" => session_close(args, adapter).await,
        "phux_agent_emit" => emit(args, adapter).await,
        "phux_agent_log" => log(args, adapter).await,
        other => Err(ToolError::new(format!("unknown agent tool: {other}"))),
    }
}

// -----------------------------------------------------------------------------
// list / show / explain — the level reads.
// -----------------------------------------------------------------------------

fn list_schema() -> Value {
    schema(
        "phux_agent_list",
        "List every pane's projected agent state (declared phux.agent/v1 record where present, \
         otherwise the server's detector). This is a LEVEL read: it asserts only the absence of \
         contrary evidence, so a pane reported `idle` may equally be finished, crashed, or \
         running a program with no detection manifest. Use phux_agent_wait for a completion gate.",
        json!({ "socket": string_schema() }),
        &[],
    )
}

fn show_schema() -> Value {
    schema(
        "phux_agent_show",
        "Show the projected agent state for one pane. LEVEL read — see phux_agent_list for what \
         that does and does not assert.",
        json!({ "target": target_schema(TARGET_DESC), "socket": string_schema() }),
        &[],
    )
}

fn explain_schema() -> Value {
    schema(
        "phux_agent_explain",
        "Show one pane's projected agent state together with the evidence behind it: every \
         detection source that fired, its confidence, and what it observed. Use this when a \
         state looks wrong. The offline capture-file mode of the CLI verb is deliberately not \
         exposed here — it is a manifest-authoring debugger over a local file, not a \
         control-plane read.",
        json!({ "target": target_schema(TARGET_DESC), "socket": string_schema() }),
        &[],
    )
}

async fn list(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(args, &["socket"], &[])?;
    let mut argv = vec!["agent".to_owned(), "list".to_owned(), "--json".to_owned()];
    push_socket(&mut argv, args)?;
    adapter.run_json(argv, DEFAULT_CALL_TIMEOUT).await
}

async fn projection(args: &Value, verb: &str, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(args, &["target", "socket"], &[])?;
    let mut argv = vec!["agent".to_owned(), verb.to_owned(), "--json".to_owned()];
    push_socket(&mut argv, args)?;
    push_target(&mut argv, args)?;
    adapter.run_json(argv, DEFAULT_CALL_TIMEOUT).await
}

// -----------------------------------------------------------------------------
// set / clear — the declared record.
// -----------------------------------------------------------------------------

fn set_schema() -> Value {
    schema(
        "phux_agent_set",
        "Declare a pane's agent identity by writing its phux.agent/v1 L3 record. Only `name` is \
         required. A declared `state` OUTRANKS the server's detector for the record's lifetime, \
         so declare one only when the caller genuinely knows better than the detector does.",
        json!({
            "target": target_schema(TARGET_DESC),
            "name": string_schema(),
            "kind": string_schema(),
            "state": { "type": "string", "enum": DECLARED_STATES },
            "attention": { "type": "string", "enum": ATTENTION_LEVELS },
            "session": string_schema(),
            "socket": string_schema(),
        }),
        &["name"],
    )
}

fn clear_schema() -> Value {
    schema(
        "phux_agent_clear",
        "Delete a pane's declared phux.agent/v1 record. The pane's state reverts to whatever the \
         server's detector projects; anything waiting on that agent sees a departure, not a \
         completion.",
        json!({
            "target": target_schema(TARGET_DESC),
            "socket": string_schema(),
        }),
        &[],
    )
}

/// `phux_agent_set` — write the whole `phux.agent/v1` record (last writer
/// wins) and return it as the server confirmed it, in-process.
async fn set(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &[
            "target",
            "name",
            "kind",
            "state",
            "attention",
            "session",
            "socket",
        ],
        &["name"],
    )?;
    let record = declared_record(args)?;
    let socket = socket_arg(args)?;
    let (mut conn, pane) = target_pane(&socket, args).await?;
    // The trailing GET inside `set_record` is load-bearing: SET_METADATA has
    // no reply frame, so the confirmed value is what the server read back.
    let (answer, _) = phux_client::agent_record::set_record(&mut conn, 100, &pane, &record).await?;
    drop(conn);
    match answer {
        Ok(Some(confirmed)) => record_document("set", &pane, Some(&confirmed)),
        Ok(None) => Err(ToolError::new("agent record did not persist")),
        Err(refusal) => Err(ToolError::new(format!(
            "agent record could not be confirmed: {refusal}"
        ))),
    }
}

/// The record `set` declares, validated the way `phux agent set` validates
/// its flags.
fn declared_record(args: &Value) -> Result<AgentRecord, ToolError> {
    let state = args
        .get("state")
        .map(|_| enum_string(args, "state", DECLARED_STATES, None))
        .transpose()?;
    let attention = args
        .get("attention")
        .map(|_| enum_string(args, "attention", ATTENTION_LEVELS, None))
        .transpose()?;
    let name = bounded_string(args, "name", true)?.unwrap_or_default();
    if name.trim().is_empty() {
        return Err(ToolError::new("`name` must not be empty"));
    }
    Ok(AgentRecord {
        name: name.trim().to_owned(),
        kind: bounded_string(args, "kind", false)?,
        state: state.map(AgentMetaState::from).unwrap_or_default(),
        attention: attention.map(AgentAttention::from),
        session: bounded_string(args, "session", false)?,
    })
}

/// `phux_agent_clear` — delete the record and confirm it is gone.
async fn clear(args: &Value) -> Result<Value, ToolError> {
    strict_object(args, &["target", "socket"], &[])?;
    let socket = socket_arg(args)?;
    let (mut conn, pane) = target_pane(&socket, args).await?;
    let (answer, _) = phux_client::agent_record::clear_record(&mut conn, 100, &pane).await?;
    drop(conn);
    match answer {
        Ok(None) => record_document("clear", &pane, None),
        Ok(Some(_)) => Err(ToolError::new("agent record was not cleared")),
        Err(refusal) => Err(ToolError::new(format!(
            "agent clear could not be confirmed: {refusal}"
        ))),
    }
}

/// Connect and resolve the optional `target` (the focused pane when absent)
/// against that connection's own snapshot, as `phux agent set` / `clear` do.
async fn target_pane(
    socket: &std::path::Path,
    args: &Value,
) -> Result<(Connection, ResourceId), ToolError> {
    let selector = bounded_string(args, "target", false)?
        .map(|raw| parse_selector(&raw))
        .transpose()?
        .unwrap_or(Selector::Current);
    let mut conn = Connection::connect(socket).await?;
    let view = phux_client::state::get_state_on(&mut conn).await?;
    let pane = resolve_one(socket, &selector, &view).await?;
    Ok((conn, pane))
}

/// The versioned `{schema_version, action, terminal, record}` document;
/// `record` is `null` for a cleared record.
fn record_document(
    action: &str,
    pane: &ResourceId,
    record: Option<&AgentRecord>,
) -> Result<Value, ToolError> {
    let record = match record {
        None => Value::Null,
        Some(record) => serde_json::from_slice(&record.encode())
            .map_err(|err| ToolError::new(format!("agent record is malformed JSON: {err}")))?,
    };
    Ok(json!({
        "schema_version": 1,
        "action": action,
        "terminal": format_terminal_id(pane),
        "record": record,
    }))
}

// -----------------------------------------------------------------------------
// wait — the EDGE read.
// -----------------------------------------------------------------------------

fn wait_schema() -> Value {
    schema(
        "phux_agent_wait",
        "Block until a pane's agent TRANSITIONS into one of `until` (default: idle, blocked, \
         done — the three ways a turn ends), then return the result document. Set `any` to wait \
         for the first matching transition from any local agent in the fleet; `target` and `any` \
         are mutually exclusive. \
         EDGE-TRIGGERED, and that is the whole point of the verb: a pane ALREADY RESTING in a \
         target state does not satisfy it and the call ends at the deadline with \
         `satisfied: false`. Read that as \"no transition was observed\", never as \"still \
         working\" — `idle` is the detector's fail-safe fallthrough, equally true of a finished \
         agent and a crashed one, so a gate satisfied by that level would pass on a corpse. \
         `phux_agent_show` is the level read. \
         SOUND USE: waiting on an agent someone else is driving. UNSOUND USE: \"did the prompt I \
         just sent finish?\" — a send and a wait are two tool calls on two connections with no \
         shared ordering point, so a fast agent can start and finish before this call subscribes \
         (ADR-0076 point 6). Use phux_agent_prompt for that question: it fuses delivery and the \
         transition wait in one process. \
         Result: the canonical document — `satisfied`, `baseline`, `state`, `edge` \
         {from, to, via}, `agent`, `observations` {edges, pushes, polls}, and `detection` with \
         the detector's confidence and sources.",
        json!({
            "target": target_schema(TARGET_DESC),
            "any": { "type": "boolean", "description": "Wait across every local agent in the fleet. Mutually exclusive with target." },
            "until": {
                "type": "array",
                "maxItems": 4,
                "items": { "type": "string", "enum": WAITABLE_STATES },
                "description": "States to wait for, OR-ed. Omit for idle/blocked/done. `unknown` is not spellable: a withdrawn record is a departure, reported as an error.",
            },
            "timeout_secs": {
                "type": "number",
                "minimum": 1,
                "maximum": 3600,
                "description": "Give up after this many seconds and report `satisfied: false`. Default 600; bounded to 1..=3600. Unlike the CLI there is no unbounded mode — a tool call that never returns wedges the host.",
            },
            "socket": string_schema(),
        }),
        &[],
    )
}

async fn wait(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "any", "until", "timeout_secs", "socket"],
        &[],
    )?;
    let any = bool_arg(args, "any")?;
    if any && args.get("target").is_some() {
        return Err(ToolError::new("`target` and `any` are mutually exclusive"));
    }
    let until = validated_until(args)?;
    let timeout_secs = timeout_secs(args, WAIT_DEFAULT_TIMEOUT_SECS)?;

    let mut argv = vec!["agent".to_owned(), "wait".to_owned()];
    if any {
        argv.push("--any".to_owned());
    }
    for state in until {
        argv.extend(["--until".to_owned(), state]);
    }
    argv.extend([
        "--timeout".to_owned(),
        timeout_secs.to_string(),
        "--json".to_owned(),
    ]);
    push_socket(&mut argv, args)?;
    push_target(&mut argv, args)?;
    run_until_transition(adapter, argv, timeout_secs, "phux agent wait").await
}

// -----------------------------------------------------------------------------
// send_keys — the identity-checked write.
// -----------------------------------------------------------------------------

fn send_keys_schema() -> Value {
    schema(
        "phux_agent_send_keys",
        "Send keys to a pane, but only if it still hosts the expected agent. The \
         agent-addressed sibling of phux_send_keys: it re-reads the pane's phux.agent/v1 record \
         immediately before writing and REFUSES if the occupant changed, so a prompt cannot land \
         in a shell that replaced the agent it was meant for. Use phux_send_keys when a pane, \
         not an agent, is what you mean. Validation is all-or-nothing: every key spec is checked \
         before a single byte is written, so a typo in the third key cannot leave the first two \
         delivered. The whole batch uses acknowledged APPLY_INPUT. Success means write and flush \
         completed on the PTY master (bytes reached the kernel tty queue), not that the agent \
         consumed them. INPUT_DELIVERY_UNKNOWN is terminal: inspect the pane and do not resend.",
        json!({
            "target": target_schema("Target selector resolving to one pane. Required — an agent write is never aimed at the focused pane by default."),
            "keys": {
                "type": "array",
                "minItems": 1,
                "maxItems": 64,
                "items": string_schema(),
                "description": "Key specs: named keys (`Enter`, `C-c`, `M-x`, `Up`) or literal text. A literal run immediately before `Enter` is delivered as one submission-safe paste.",
            },
            "expect_agent": string_schema(),
            "expect_kind": string_schema(),
            "socket": string_schema(),
        }),
        &["target", "keys"],
    )
}

async fn send_keys(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "keys", "expect_agent", "expect_kind", "socket"],
        &["target", "keys"],
    )?;
    let target = bounded_string(args, "target", true)?.unwrap_or_default();
    let keys = bounded_strings(args, "keys", true)?;
    let mut argv = vec![
        "agent".to_owned(),
        "send-keys".to_owned(),
        "--json".to_owned(),
    ];
    push_option(
        &mut argv,
        "--expect-agent",
        bounded_string(args, "expect_agent", false)?,
    );
    push_option(
        &mut argv,
        "--expect-kind",
        bounded_string(args, "expect_kind", false)?,
    );
    push_socket(&mut argv, args)?;
    // Key specs are caller text; a literal starting with `-` must stay positional.
    argv.push("--".to_owned());
    argv.push(target);
    argv.extend(keys);
    adapter.run_json(argv, DEFAULT_CALL_TIMEOUT).await
}

// -----------------------------------------------------------------------------
// prompt — fused acknowledged submit and transition wait.
// -----------------------------------------------------------------------------

fn prompt_schema() -> Value {
    schema(
        "phux_agent_prompt",
        "Deliver one single-line prompt through acknowledged, idempotent APPLY_INPUT and wait \
         in the same process for a post-delivery lifecycle transition. This is the sound MCP \
         completion primitive: splitting delivery and phux_agent_wait across calls can miss a \
         fast turn. The server has one acknowledged input lane, so serialize fleet prompting. \
         Success is a kernel tty-queue receipt, not proof the agent consumed the prompt. A \
         timeout returns the canonical document with transition_observed=false; delivery still \
         occurred. INPUT_DELIVERY_UNKNOWN is terminal: inspect the pane and do not resend.",
        json!({
            "target": target_schema("Target selector resolving to one pane. Required."),
            "text": string_schema(),
            "expect_agent": string_schema(),
            "expect_kind": string_schema(),
            "until": {
                "type": "array",
                "maxItems": 4,
                "items": { "type": "string", "enum": WAITABLE_STATES },
                "description": "Post-delivery states to wait for, OR-ed. Default: idle, blocked, done.",
            },
            "timeout_secs": {
                "type": "number",
                "minimum": 1,
                "maximum": 3600,
                "description": "Completion deadline in seconds. Default 600; bounded to 1..=3600.",
            },
            "socket": string_schema(),
        }),
        &["target", "text"],
    )
}

async fn prompt(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(
        args,
        &[
            "target",
            "text",
            "expect_agent",
            "expect_kind",
            "until",
            "timeout_secs",
            "socket",
        ],
        &["target", "text"],
    )?;
    let target = bounded_string(args, "target", true)?.unwrap_or_default();
    let text = bounded_string(args, "text", true)?.unwrap_or_default();
    if text.contains(['\n', '\r']) {
        return Err(ToolError::new("`text` must be a single line"));
    }
    let until = validated_until(args)?;
    let timeout_secs = timeout_secs(args, WAIT_DEFAULT_TIMEOUT_SECS)?;
    let mut argv = vec!["agent".to_owned(), "prompt".to_owned(), "--wait".to_owned()];
    push_option(
        &mut argv,
        "--expect-agent",
        bounded_string(args, "expect_agent", false)?,
    );
    push_option(
        &mut argv,
        "--expect-kind",
        bounded_string(args, "expect_kind", false)?,
    );
    for state in until {
        argv.extend(["--until".to_owned(), state]);
    }
    argv.extend([
        "--timeout".to_owned(),
        timeout_secs.to_string(),
        "--json".to_owned(),
    ]);
    push_socket(&mut argv, args)?;
    argv.extend(["--".to_owned(), target, text]);
    run_until_transition(adapter, argv, timeout_secs, "phux agent prompt").await
}

// -----------------------------------------------------------------------------
// answer — correlate one live ask and deliver a validated answer.
// -----------------------------------------------------------------------------

fn answer_schema() -> Value {
    schema(
        "phux_agent_answer",
        "Answer one exact live agent ask. The ask id must still match, and the answer must be \
         either a 1-based published choice or explicit text. Text outside the published \
         suggestions requires allow_unlisted=true. Delivery is one acknowledged, idempotent \
         paste-plus-Enter batch; stale or unidentified asks write nothing.",
        json!({
            "target": target_schema("Target selector resolving to one pane. Required."),
            "id": string_schema(),
            "choice": { "type": "integer", "minimum": 1 },
            "text": string_schema(),
            "allow_unlisted": { "type": "boolean", "description": "Allow text not present in the ask's published suggestions. Valid only with text." },
            "socket": string_schema(),
        }),
        &["target", "id"],
    )
}

async fn answer(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "id", "choice", "text", "allow_unlisted", "socket"],
        &["target", "id"],
    )?;
    let target = bounded_string(args, "target", true)?.unwrap_or_default();
    let id = bounded_string(args, "id", true)?.unwrap_or_default();
    let choice = args.get("choice");
    let answer_text = bounded_string(args, "text", false)?;
    if choice.is_some() == answer_text.is_some() {
        return Err(ToolError::new("provide exactly one of `choice` or `text`"));
    }
    let allow_unlisted = bool_arg(args, "allow_unlisted")?;
    if allow_unlisted && answer_text.is_none() {
        return Err(ToolError::new("`allow_unlisted` requires `text`"));
    }
    let mut argv = vec![
        "agent".to_owned(),
        "answer".to_owned(),
        "--id".to_owned(),
        id,
    ];
    if let Some(value) = choice {
        let value = value
            .as_u64()
            .filter(|value| *value > 0)
            .ok_or_else(|| ToolError::new("`choice` must be a positive integer"))?;
        argv.extend(["--choice".to_owned(), value.to_string()]);
    } else if let Some(value) = answer_text {
        argv.extend(["--text".to_owned(), value]);
    }
    if allow_unlisted {
        argv.push("--allow-unlisted".to_owned());
    }
    argv.push("--json".to_owned());
    push_socket(&mut argv, args)?;
    argv.extend(["--".to_owned(), target]);
    adapter.run_json(argv, DEFAULT_CALL_TIMEOUT).await
}

// -----------------------------------------------------------------------------
// start — launch into an existing shell pane and await readiness.
// -----------------------------------------------------------------------------

fn start_schema() -> Value {
    schema(
        "phux_agent_start",
        "Start an agent inside an existing shell pane without creating, splitting, or moving \
         layout, then wait for the first detector publication proving readiness. The kind must \
         have a detection manifest. force skips the shell-at-prompt precondition but does not \
         weaken readiness; a timeout means the command was typed but readiness was not observed.",
        json!({
            "name": string_schema(),
            "kind": string_schema(),
            "target": target_schema("Existing pane selector. Required; this tool never creates layout."),
            "integration": string_schema(),
            "timeout_secs": { "type": "number", "minimum": 1, "maximum": 3600, "description": "Readiness deadline in seconds. Default 60; bounded to 1..=3600." },
            "force": { "type": "boolean", "description": "Skip the available-shell check and type into whatever occupies the pane." },
            "args": { "type": "array", "maxItems": 64, "items": string_schema(), "description": "Extra arguments appended to the integration launch command." },
            "socket": string_schema(),
        }),
        &["name", "kind", "target"],
    )
}

async fn start(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(
        args,
        &[
            "name",
            "kind",
            "target",
            "integration",
            "timeout_secs",
            "force",
            "args",
            "socket",
        ],
        &["name", "kind", "target"],
    )?;
    let name = bounded_string(args, "name", true)?.unwrap_or_default();
    let kind = bounded_string(args, "kind", true)?.unwrap_or_default();
    let target = bounded_string(args, "target", true)?.unwrap_or_default();
    let extra = bounded_strings(args, "args", false)?;
    let timeout_secs = timeout_secs(args, START_DEFAULT_TIMEOUT_SECS)?;
    let mut argv = vec!["agent".to_owned(), "start".to_owned()];
    argv.extend(["--kind".to_owned(), kind, "--target".to_owned(), target]);
    push_option(
        &mut argv,
        "--integration",
        bounded_string(args, "integration", false)?,
    );
    argv.extend(["--timeout".to_owned(), timeout_secs.to_string()]);
    if bool_arg(args, "force")? {
        argv.push("--force".to_owned());
    }
    argv.push("--json".to_owned());
    push_socket(&mut argv, args)?;
    argv.push(name);
    if !extra.is_empty() {
        argv.push("--".to_owned());
        argv.extend(extra);
    }
    adapter.run_json(argv, graced(timeout_secs)).await
}

// -----------------------------------------------------------------------------
// session open / close, emit, log — the AgentSession resource.
// -----------------------------------------------------------------------------

/// The closed `AgentEventsJsonlV1` record `type` vocabulary, checked before
/// a subprocess runs (the CLI and server enforce the same set).
const EVENT_TYPES: &[&str] = &[
    "session_start",
    "prompt",
    "tool_start",
    "tool_end",
    "notification",
    "ask",
    "stop",
    "session_end",
    "state",
    "provider_raw",
];

/// Largest `tail` `phux_agent_log` forwards: a tool result is one text block.
const LOG_MAX_TAIL: u64 = 10_000;

/// Shared `target` description for the session verbs.
const SESSION_TARGET_DESC: &str = "Target selector: the pane hosting the agent (`@N`, `%name`, \
    `session:window.pane`) resolves to its unique live AgentSession child; an AgentSession \
    resource id (`@N`) names itself. Required.";

fn session_open_schema() -> Value {
    schema(
        "phux_agent_session_open",
        "Open an AgentSession resource bound to a pane: the server-side, producer-fed event \
         log an agent harness appends to with phux_agent_emit. The session is a child of the \
         Terminal it names — closing the pane closes it — and it is what `%name` targets, \
         phux_agent_log reads, and the sidebar shows under the pane. Refused with \
         `unsupported_server` on a server that does not advertise `resource_kinds` (check \
         phux_status's `features`). Returns {schema_version, resource, parent, provider, \
         native_id}.",
        json!({
            "target": target_schema("The parent pane: a selector resolving to one Terminal-kind resource. Required."),
            "provider": { "type": "string", "minLength": 1, "maxLength": 4096, "description": "Agent provider slug, e.g. `claude`." },
            "native_id": { "type": "string", "minLength": 1, "maxLength": 4096, "description": "Opaque provider-native session id, when the provider has one." },
            "socket": string_schema(),
        }),
        &["target", "provider"],
    )
}

fn session_close_schema() -> Value {
    schema(
        "phux_agent_session_close",
        "Close a pane's AgentSession resource. Closing a child never affects the parent pane. \
         Returns {schema_version, resource, closed}.",
        json!({
            "target": target_schema(SESSION_TARGET_DESC),
            "socket": string_schema(),
        }),
        &["target"],
    )
}

fn emit_schema() -> Value {
    schema(
        "phux_agent_emit",
        "Append one AgentEventsJsonlV1 record to a pane's AgentSession log. The server stamps \
         `seq` and `ts_ms`; the caller supplies `type` (closed vocabulary) and an optional \
         `data` object (default `{}`). The server derives the agent's lifecycle state from the \
         stream (`prompt`/`tool_start` -> working, `ask` -> blocked, `stop` -> done, \
         `session_end` -> retract). Returns the stamped header {schema_version, resource, \
         seq, ts_ms, type}.",
        json!({
            "target": target_schema(SESSION_TARGET_DESC),
            "type": { "type": "string", "enum": EVENT_TYPES, "description": "Record type." },
            "data": { "type": "object", "description": "Record payload, a JSON object. Omit for `{}`." },
            "socket": string_schema(),
        }),
        &["target", "type"],
    )
}

fn log_schema() -> Value {
    schema(
        "phux_agent_log",
        "Read the retained AgentEventsJsonlV1 records of a pane's AgentSession: one bounded \
         read of the server-side ring, never a live follow (a tool result is one text block; \
         the CLI's `phux agent log --follow` is the streaming form). Returns \
         {schema_version, resource, parent, provider, native_id, records: [{seq, ts_ms, \
         type, data}]}.",
        json!({
            "target": target_schema(SESSION_TARGET_DESC),
            "tail": { "type": "number", "minimum": 1, "maximum": LOG_MAX_TAIL, "description": "Return only the last N records. Omit for every retained record." },
            "socket": string_schema(),
        }),
        &["target"],
    )
}

async fn session_open(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "provider", "native_id", "socket"],
        &["target", "provider"],
    )?;
    let target = bounded_string(args, "target", true)?.unwrap_or_default();
    let provider = bounded_string(args, "provider", true)?.unwrap_or_default();
    let mut argv = vec![
        "agent".to_owned(),
        "session".to_owned(),
        "open".to_owned(),
        "--provider".to_owned(),
        provider,
    ];
    push_option(
        &mut argv,
        "--native-id",
        bounded_string(args, "native_id", false)?,
    );
    argv.push("--json".to_owned());
    push_socket(&mut argv, args)?;
    argv.extend(["--".to_owned(), target]);
    adapter.run_json(argv, DEFAULT_CALL_TIMEOUT).await
}

async fn session_close(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(args, &["target", "socket"], &["target"])?;
    let target = bounded_string(args, "target", true)?.unwrap_or_default();
    let mut argv = vec!["agent".to_owned(), "session".to_owned(), "close".to_owned()];
    push_socket(&mut argv, args)?;
    argv.extend(["--".to_owned(), target]);
    // `session close` has no `--json`: it prints `@N<TAB>closed`.
    let output = adapter.run(argv, DEFAULT_CALL_TIMEOUT).await?;
    parse_closed_line(&output.stdout)
}

/// Parse the `@N<TAB>closed` line `phux agent session close` prints into the
/// small documented projection.
fn parse_closed_line(stdout: &str) -> Result<Value, ToolError> {
    let line = stdout.trim();
    match line.split_once('\t') {
        Some((resource, "closed")) if !resource.is_empty() => {
            Ok(json!({ "schema_version": 1, "resource": resource, "closed": true }))
        }
        _ => Err(ToolError::new(format!(
            "phux agent session close returned malformed output: {line:?}"
        ))),
    }
}

async fn emit(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "type", "data", "socket"],
        &["target", "type"],
    )?;
    let target = bounded_string(args, "target", true)?.unwrap_or_default();
    let event_type = enum_string(args, "type", EVENT_TYPES, None)?;
    let data = match args.get("data") {
        None => None,
        Some(Value::Object(object)) => Some(
            serde_json::to_string(&Value::Object(object.clone()))
                .map_err(|err| ToolError::new(format!("`data` could not be encoded: {err}")))?,
        ),
        Some(_) => return Err(ToolError::new("`data` must be a JSON object")),
    };
    let mut argv = vec![
        "agent".to_owned(),
        "emit".to_owned(),
        "--type".to_owned(),
        event_type,
    ];
    push_option(&mut argv, "--data", data);
    argv.push("--json".to_owned());
    push_socket(&mut argv, args)?;
    argv.extend(["--".to_owned(), target]);
    adapter.run_json(argv, DEFAULT_CALL_TIMEOUT).await
}

async fn log(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(args, &["target", "tail", "socket"], &["target"])?;
    let target = bounded_string(args, "target", true)?.unwrap_or_default();
    let tail = match args.get("tail") {
        None => None,
        Some(value) => Some(
            value
                .as_u64()
                .filter(|value| (1..=LOG_MAX_TAIL).contains(value))
                .ok_or_else(|| {
                    ToolError::new(format!("`tail` must be an integer in 1..={LOG_MAX_TAIL}"))
                })?,
        ),
    };
    let mut argv = vec!["agent".to_owned(), "log".to_owned(), "--json".to_owned()];
    push_option(&mut argv, "--tail", tail.map(|tail| tail.to_string()));
    push_socket(&mut argv, args)?;
    argv.extend(["--".to_owned(), target]);
    // Never `--follow`: a tool result is one bounded text block.
    adapter.run_json(argv, DEFAULT_CALL_TIMEOUT).await
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::*;
    use crate::cli_adapter::fake;

    /// A fake `phux` answering each agent verb with the real CLI's shape,
    /// including `wait`'s document-then-exit-124 timeout.
    fn fake_cli() -> (TempDir, CliAdapter, PathBuf) {
        fake::cli(
            r#"case "$2" in
  set) printf '@1\t{"name":"bot"}\n' ;;
  clear) printf '@1\t-\n' ;;
  wait) printf '{"schema_version":1,"satisfied":false,"baseline":"working","state":"working"}\n'
        exit 124 ;;
  send-keys) printf '{"schema_version":1,"verified":true}\n' ;;
  prompt) printf '{"schema_version":1,"delivery":"ok","transition_observed":false}\n'
          exit 124 ;;
  answer) printf '{"schema_version":1,"delivered":true}\n' ;;
  start) printf '{"schema_version":1,"started":true,"ready":true}\n' ;;
  session) case "$3" in
    open) printf '{"schema_version":1,"resource":"@9","parent":"@7","provider":"claude","native_id":null}\n' ;;
    close) printf '@9\tclosed\n' ;;
  esac ;;
  emit) printf '{"schema_version":1,"resource":"@9","seq":42,"ts_ms":1757404800123,"type":"prompt"}\n' ;;
  log) printf '{"schema_version":1,"resource":"@9","parent":"@7","provider":"claude","native_id":null,"records":[{"seq":1,"ts_ms":10,"type":"session_start","data":{}},{"seq":2,"ts_ms":20,"type":"prompt","data":{"chars":4}}]}\n' ;;
  *) printf '{"schema_version":1,"agents":[]}\n' ;;
esac
"#,
        )
    }

    async fn assert_argv(
        adapter: &CliAdapter,
        log: &Path,
        name: &str,
        args: Value,
        expected: &[&str],
    ) -> Value {
        let result = call_with_adapter(name, &args, adapter)
            .await
            .unwrap_or_else(|err| panic!("{name} failed: {err:?}"));
        assert_eq!(fake::logged(log), expected, "{name}");
        result
    }

    /// Every agent verb is its own tool: none carries an `action`
    /// discriminant, and the read and write shapes never merge.
    #[test]
    fn the_agent_family_is_distinct_tools_with_no_action_multiplexer() {
        let schemas = schemas();
        let names: Vec<&str> = schemas
            .iter()
            .filter_map(|schema| schema["name"].as_str())
            .collect();
        for schema in &schemas {
            assert!(
                schema["inputSchema"]["properties"].get("action").is_none(),
                "{} still multiplexes on `action`",
                schema["name"],
            );
            assert!(owns(schema["name"].as_str().unwrap()));
        }
        let props = |name: &str| schemas[names.iter().position(|n| *n == name).unwrap()].clone();
        for read in ["phux_agent_list", "phux_agent_show", "phux_agent_explain"] {
            let schema = props(read);
            for forbidden in ["name", "keys", "until", "state", "timeout_secs"] {
                assert!(
                    schema["inputSchema"]["properties"].get(forbidden).is_none(),
                    "{read} carries `{forbidden}` from another verb",
                );
            }
        }
        assert!(
            props("phux_agent_wait")["inputSchema"]["properties"]
                .get("name")
                .is_none(),
            "wait must not inherit the identity-declaration arguments",
        );
        assert!(
            props("phux_agent_set")["inputSchema"]["properties"]
                .get("until")
                .is_none(),
            "set must not inherit the wait arguments",
        );
    }

    /// The wait description must state the edge rule, the meaning of a
    /// timeout, and the two-connection caveat, or callers misread results.
    #[test]
    fn the_wait_description_states_the_edge_rule_and_the_two_connection_caveat() {
        let schema = wait_schema();
        let description = schema["description"].as_str().unwrap();
        assert!(description.contains("EDGE-TRIGGERED"), "{description}");
        assert!(
            description.contains("ALREADY RESTING"),
            "the resting-pane timeout must be stated: {description}",
        );
        assert!(
            description.contains("no transition was observed"),
            "the 124 reading must be stated: {description}",
        );
        assert!(
            description.contains("two connections"),
            "the ADR-0076 point 6 caveat must be stated: {description}",
        );
        assert!(
            description.contains("ADR-0076"),
            "the caveat must be attributable: {description}",
        );
    }

    /// Every tool executes the exact canonical argv, with the `--`
    /// separator ahead of any caller-supplied positional.
    #[tokio::test]
    async fn every_agent_tool_executes_the_exact_canonical_argv() {
        let (_temp, adapter, log) = fake_cli();

        assert_argv(
            &adapter,
            &log,
            "phux_agent_list",
            json!({ "socket": "/sock" }),
            &["agent", "list", "--json", "--socket", "/sock"],
        )
        .await;

        assert_argv(
            &adapter,
            &log,
            "phux_agent_show",
            json!({ "target": "@5", "socket": "/sock" }),
            &["agent", "show", "--json", "--socket", "/sock", "--", "@5"],
        )
        .await;
        assert_argv(
            &adapter,
            &log,
            "phux_agent_explain",
            json!({}),
            &["agent", "explain", "--json"],
        )
        .await;

        assert_argv(
            &adapter,
            &log,
            "phux_agent_send_keys",
            json!({
                "target": "@7", "keys": ["yes", "Enter"],
                "expect_agent": "bot", "expect_kind": "codex", "socket": "/sock"
            }),
            &[
                "agent",
                "send-keys",
                "--json",
                "--expect-agent",
                "bot",
                "--expect-kind",
                "codex",
                "--socket",
                "/sock",
                "--",
                "@7",
                "yes",
                "Enter",
            ],
        )
        .await;
    }

    /// The turn-driving tools execute the exact canonical argv.
    #[tokio::test]
    async fn turn_driving_tools_execute_the_exact_canonical_argv() {
        let (_temp, adapter, log) = fake_cli();
        let prompt = assert_argv(
            &adapter,
            &log,
            "phux_agent_prompt",
            json!({
                "target": "%bot", "text": "Review the patch",
                "expect_agent": "bot", "expect_kind": "codex",
                "until": ["blocked", "done"], "timeout_secs": 45,
                "socket": "/sock"
            }),
            &[
                "agent",
                "prompt",
                "--wait",
                "--expect-agent",
                "bot",
                "--expect-kind",
                "codex",
                "--until",
                "blocked",
                "--until",
                "done",
                "--timeout",
                "45",
                "--json",
                "--socket",
                "/sock",
                "--",
                "%bot",
                "Review the patch",
            ],
        )
        .await;
        assert_eq!(prompt["delivery"], "ok");
        assert_eq!(prompt["transition_observed"], false);

        assert_argv(
            &adapter,
            &log,
            "phux_agent_answer",
            json!({
                "target": "%bot", "id": "deploy", "text": "Proceed",
                "allow_unlisted": true, "socket": "/sock"
            }),
            &[
                "agent",
                "answer",
                "--id",
                "deploy",
                "--text",
                "Proceed",
                "--allow-unlisted",
                "--json",
                "--socket",
                "/sock",
                "--",
                "%bot",
            ],
        )
        .await;

        assert_argv(
            &adapter,
            &log,
            "phux_agent_start",
            json!({
                "name": "reviewer", "kind": "codex", "target": "@8",
                "integration": "codex-cli", "timeout_secs": 40, "force": true,
                "args": ["--model", "gpt-5"], "socket": "/sock"
            }),
            &[
                "agent",
                "start",
                "--kind",
                "codex",
                "--target",
                "@8",
                "--integration",
                "codex-cli",
                "--timeout",
                "40",
                "--force",
                "--json",
                "--socket",
                "/sock",
                "reviewer",
                "--",
                "--model",
                "gpt-5",
            ],
        )
        .await;
    }

    /// The wait tool always bounds the CLI (which defaults to unbounded),
    /// passes each `until` as its own flag, and returns the timeout
    /// document rather than collapsing exit 124 into an error.
    #[tokio::test]
    async fn wait_bounds_the_deadline_and_returns_the_timeout_document() {
        let (_temp, adapter, log) = fake_cli();

        let bounded = assert_argv(
            &adapter,
            &log,
            "phux_agent_wait",
            json!({ "target": "@7", "until": ["blocked", "done"], "timeout_secs": 30, "socket": "/sock" }),
            &[
                "agent", "wait", "--until", "blocked", "--until", "done", "--timeout", "30",
                "--json", "--socket", "/sock", "--", "@7",
            ],
        )
        .await;
        assert_eq!(
            bounded["satisfied"],
            json!(false),
            "exit 124 must arrive as `satisfied: false`, not as a tool error",
        );
        assert_eq!(bounded["baseline"], json!("working"));

        // Omitting `timeout_secs` still bounds the call.
        assert_argv(
            &adapter,
            &log,
            "phux_agent_wait",
            json!({}),
            &[
                "agent",
                "wait",
                "--timeout",
                &WAIT_DEFAULT_TIMEOUT_SECS.to_string(),
                "--json",
            ],
        )
        .await;

        assert_argv(
            &adapter,
            &log,
            "phux_agent_wait",
            json!({ "any": true, "until": ["blocked"], "timeout_secs": 20 }),
            &[
                "agent",
                "wait",
                "--any",
                "--until",
                "blocked",
                "--timeout",
                "20",
                "--json",
            ],
        )
        .await;
    }

    /// Validation happens before any subprocess: an adapter pointed at a
    /// program that cannot exist proves nothing was executed.
    #[tokio::test]
    async fn malformed_arguments_are_rejected_before_execution() {
        let adapter = CliAdapter::new("must-not-execute");
        for (name, args) in [
            // Unknown key.
            ("phux_agent_list", json!({ "target": "@1" })),
            // Missing required.
            ("phux_agent_set", json!({ "target": "@1" })),
            ("phux_agent_send_keys", json!({ "target": "@1" })),
            ("phux_agent_send_keys", json!({ "keys": ["a"] })),
            // Empty key batch: all-or-nothing has nothing to deliver.
            (
                "phux_agent_send_keys",
                json!({ "target": "@1", "keys": [] }),
            ),
            // Closed vocabularies.
            ("phux_agent_set", json!({ "name": "b", "state": "busy" })),
            (
                "phux_agent_set",
                json!({ "name": "b", "attention": "urgent" }),
            ),
            ("phux_agent_wait", json!({ "until": ["unknown"] })),
            ("phux_agent_wait", json!({ "timeout_secs": 0 })),
            ("phux_agent_wait", json!({ "timeout_secs": 3601 })),
            ("phux_agent_wait", json!({ "any": "yes" })),
            ("phux_agent_wait", json!({ "target": "@1", "any": true })),
            ("phux_agent_prompt", json!({ "target": "@1" })),
            (
                "phux_agent_prompt",
                json!({ "target": "@1", "text": "two\nlines" }),
            ),
            (
                "phux_agent_prompt",
                json!({ "target": "@1", "text": "hello", "until": ["unknown"] }),
            ),
            ("phux_agent_answer", json!({ "target": "@1", "id": "q" })),
            (
                "phux_agent_answer",
                json!({ "target": "@1", "id": "q", "choice": 0 }),
            ),
            (
                "phux_agent_answer",
                json!({ "target": "@1", "id": "q", "choice": 1, "text": "yes" }),
            ),
            (
                "phux_agent_answer",
                json!({ "target": "@1", "id": "q", "choice": 1, "allow_unlisted": true }),
            ),
            (
                "phux_agent_start",
                json!({ "name": "bot", "kind": "codex" }),
            ),
            (
                "phux_agent_start",
                json!({ "name": "bot", "kind": "codex", "target": "@1", "force": "yes" }),
            ),
            // The multiplexer's old shape is not silently accepted.
            ("phux_agent_list", json!({ "action": "list" })),
        ] {
            let result = match name {
                "phux_agent_set" => set(&args).await,
                _ => call_with_adapter(name, &args, &adapter).await,
            };
            assert!(result.is_err(), "{name} accepted {args}");
        }
    }

    /// The session verbs execute the exact canonical argv: flags first, the
    /// `--` separator, then the caller's selector.
    #[tokio::test]
    async fn session_tools_execute_the_exact_canonical_argv() {
        let (_temp, adapter, log) = fake_cli();

        let opened = assert_argv(
            &adapter,
            &log,
            "phux_agent_session_open",
            json!({ "target": "@7", "provider": "claude", "native_id": "abc-123", "socket": "/sock" }),
            &[
                "agent", "session", "open", "--provider", "claude", "--native-id", "abc-123",
                "--json", "--socket", "/sock", "--", "@7",
            ],
        )
        .await;
        assert_eq!(opened["resource"], "@9");
        assert_eq!(opened["parent"], "@7");

        let closed = assert_argv(
            &adapter,
            &log,
            "phux_agent_session_close",
            json!({ "target": "%reviewer" }),
            &["agent", "session", "close", "--", "%reviewer"],
        )
        .await;
        assert_eq!(closed["resource"], "@9");
        assert_eq!(closed["closed"], true);

        let emitted = assert_argv(
            &adapter,
            &log,
            "phux_agent_emit",
            json!({ "target": "@7", "type": "prompt", "data": { "length": 4 }, "socket": "/sock" }),
            &[
                "agent",
                "emit",
                "--type",
                "prompt",
                "--data",
                "{\"length\":4}",
                "--json",
                "--socket",
                "/sock",
                "--",
                "@7",
            ],
        )
        .await;
        assert_eq!(emitted["seq"], 42);
        assert_eq!(emitted["type"], "prompt");

        // No `data` means no `--data`: the CLI defaults the payload to `{}`.
        assert_argv(
            &adapter,
            &log,
            "phux_agent_emit",
            json!({ "target": "@9", "type": "stop" }),
            &["agent", "emit", "--type", "stop", "--json", "--", "@9"],
        )
        .await;

        let logged = assert_argv(
            &adapter,
            &log,
            "phux_agent_log",
            json!({ "target": "@7", "tail": 50, "socket": "/sock" }),
            &[
                "agent", "log", "--json", "--tail", "50", "--socket", "/sock", "--", "@7",
            ],
        )
        .await;
        assert_eq!(logged["schema_version"], 1);
        assert_eq!(logged["resource"], "@9");
        assert_eq!(logged["parent"], "@7");
        assert_eq!(logged["records"][1]["type"], "prompt");
        assert_eq!(logged["records"][1]["data"]["chars"], 4);
    }

    /// Session-verb argument validation happens before any subprocess.
    #[tokio::test]
    async fn session_tool_arguments_are_validated_before_execution() {
        let adapter = CliAdapter::new("must-not-execute");
        for (name, args) in [
            ("phux_agent_session_open", json!({ "target": "@7" })),
            ("phux_agent_session_open", json!({ "provider": "claude" })),
            ("phux_agent_session_close", json!({})),
            ("phux_agent_emit", json!({ "target": "@7" })),
            (
                "phux_agent_emit",
                json!({ "target": "@7", "type": "frobnicate" }),
            ),
            (
                "phux_agent_emit",
                json!({ "target": "@7", "type": "stop", "data": "text" }),
            ),
            ("phux_agent_log", json!({})),
            ("phux_agent_log", json!({ "target": "@7", "tail": 0 })),
            ("phux_agent_log", json!({ "target": "@7", "follow": true })),
        ] {
            assert!(
                call_with_adapter(name, &args, &adapter).await.is_err(),
                "{name} accepted {args}",
            );
        }
    }

    #[test]
    fn the_closed_line_parser_is_strict() {
        let doc = parse_closed_line("@9\tclosed\n").unwrap();
        assert_eq!(doc["resource"], "@9");
        assert_eq!(doc["closed"], true);
        assert!(parse_closed_line("@9\tgone").is_err());
        assert!(parse_closed_line("closed").is_err());
        assert!(parse_closed_line("").is_err());
    }
}
