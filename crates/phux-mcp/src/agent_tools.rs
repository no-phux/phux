//! The `phux_agent_*` MCP tool family: **one tool per agent verb**.
//!
//! This used to be a single `phux_agent` tool multiplexed on an `action`
//! string over `list`/`show`/`explain`/`set`/`clear`. It is split here
//! because [ADR-0071](../../../ADR/0071-what-phux-1-0-commits-to.md) point 1
//! freezes the MCP tool *names and arguments* at 1.0, and point 7(b) names
//! this split as a precondition of that freeze.
//!
//! The reason is the argument union, not the aesthetics. A multiplexer's
//! frozen schema is the union of every action it will ever carry, and the
//! agent verbs do not share an argument shape: the five read/write-identity
//! verbs take `name`/`kind`/`state`/`attention`/`session`; `wait` takes a
//! repeatable `until` list and a deadline; `send_keys` takes a key array and
//! two occupant assertions; `prompt`, `answer`, and `start` each have another
//! distinct shape. Folding those into one object freezes an argument set
//! designed for five read verbs, and after 1.0 nothing can leave it. Distinct
//! tools each freeze only their own shape, and a verb added later adds a tool
//! rather than widening a union.
//!
//! Every tool here executes the canonical `phux` CLI with argv (never a
//! shell) through [`crate::cli_adapter`], with the same caps the rest of the
//! parity surface uses.
//!
//! ## What this family deliberately does NOT contain
//!
//! Recorded here so the decision is not re-litigated, extending the
//! `take`/`give` and headless-focus reasoning in `docs/consumers/mcp.md`
//! §3.13–3.22:
//!
//! - **No `phux_agent_attach` / `observe`.** A raw live ANSI stream has no
//!   request/response shape, and the one-text-content-block `tools/call`
//!   envelope cannot carry it. `phux_watch` is the bounded observation
//!   surface; a host that wants the live stream shells out.
//! - **No blind auto-answer tool.** [`answer_schema`] requires the exact live
//!   ask id and either one of the agent's published suggestions or an explicit
//!   `allow_unlisted` override. It cannot type into a pane after that question
//!   has gone away.
//! - **No notification tool.** `phux answer` is hook-shaped, and hooks run on
//!   the human's machine, not inside a model's tool call.
//! - **No `phux_agent_read`.** [ADR-0077](../../../ADR/0077-agent-read-surface.md)
//!   point 1 refuses a read-source vocabulary: the read modifiers `--tail`
//!   and `--unwrap` hang off `phux_snapshot`, where the rest of the read
//!   knobs already live. A second read tool would be exactly the vocabulary
//!   that ADR declined.
//! - **No offline `explain --file`.** That flag evaluates a capture file on
//!   local disk for manifest authoring; it is a debugging surface for a human
//!   writing a `.toml`, not a control-plane read, and exposing it would make
//!   an MCP tool a file reader.
//!
//! `phux_agent_prompt` is deliberately the fused submit-and-wait operation
//! [ADR-0076](../../../ADR/0076-agent-prompt-and-lifecycle-wait.md) point 6
//! admits. Two MCP calls are two CLI processes and two connections with no
//! shared ordering point; the fused tool holds one process across delivery
//! and observation so a fast turn cannot finish between calls.

#![allow(
    clippy::similar_names,
    reason = "argv and parsed args are deliberately adjacent in thin CLI wrappers"
)]

use serde_json::{Value, json};

use crate::cli_adapter::{
    CliAdapter, DEFAULT_CALL_TIMEOUT, bounded_string, bounded_strings, enum_string, push_socket,
};
use crate::cli_tools::{push_option, schema, string_schema};
use crate::tools::{ToolError, strict_object};

/// Lifecycle states a `phux.agent/v1` record can declare (L3 §3.7).
const DECLARED_STATES: &[&str] = &["unknown", "idle", "working", "blocked", "done"];

/// Attention levels a record can declare (L3 §3.7).
const ATTENTION_LEVELS: &[&str] = &["none", "low", "normal", "high"];

/// States `agent wait` accepts. `unknown` is absent on purpose: withdrawing
/// a record is a *departure*, reported as an error, not a state to wait for.
const WAITABLE_STATES: &[&str] = &["idle", "working", "blocked", "done"];

/// Default deadline for `phux_agent_wait`, in seconds.
///
/// The CLI's `--timeout` is optional and unbounded; a tool call's is not.
/// An MCP `tools/call` that can block forever wedges the host, so this tool
/// always passes a deadline, bounded exactly the way `phux_run` bounds its
/// own (`docs/consumers/mcp.md` §3.4).
const WAIT_DEFAULT_TIMEOUT_SECS: u64 = 600;

/// Default readiness deadline for `phux_agent_start`.
const START_DEFAULT_TIMEOUT_SECS: u64 = 60;

/// Grace added to the tool's own subprocess deadline on top of the CLI
/// timeout, so the CLI reports its own 124 rather than being killed first.
const WAIT_DEADLINE_GRACE_SECS: u64 = 5;

/// `phux agent wait`'s timeout exit code: no transition was observed. The
/// result document still rides out on stdout, so this is a *result*, not a
/// failure (see [`crate::cli_adapter::CliAdapter::run_allowing`]).
const EXIT_WAIT_TIMEOUT: i32 = 124;

/// Shared `target` description for the agent tools.
const TARGET_DESC: &str = "Target selector resolving to one pane \
    (`@N`, `host/@N`, `session:window.pane`, `#tag`, or `.`). Omit for the \
    focused pane.";

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
    )
}

/// Dispatch one `phux_agent_*` call.
///
/// # Errors
///
/// Returns [`ToolError`] for an unknown name, a malformed argument, or a
/// canonical-CLI failure.
pub(crate) async fn call(name: &str, args: &Value) -> Result<Value, ToolError> {
    call_with_adapter(name, args, &CliAdapter::discover()).await
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
        "phux_agent_set" => set(args, adapter).await,
        "phux_agent_clear" => clear(args, adapter).await,
        "phux_agent_wait" => wait(args, adapter).await,
        "phux_agent_send_keys" => send_keys(args, adapter).await,
        "phux_agent_prompt" => prompt(args, adapter).await,
        "phux_agent_answer" => answer(args, adapter).await,
        "phux_agent_start" => start(args, adapter).await,
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
        json!({ "target": { "type": "string", "minLength": 1, "maxLength": 4096, "description": TARGET_DESC }, "socket": string_schema() }),
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
        json!({ "target": { "type": "string", "minLength": 1, "maxLength": 4096, "description": TARGET_DESC }, "socket": string_schema() }),
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
            "target": { "type": "string", "minLength": 1, "maxLength": 4096, "description": TARGET_DESC },
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
            "target": { "type": "string", "minLength": 1, "maxLength": 4096, "description": TARGET_DESC },
            "socket": string_schema(),
        }),
        &[],
    )
}

async fn set(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
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
    if args.get("state").is_some() {
        let _ = enum_string(args, "state", DECLARED_STATES, None)?;
    }
    if args.get("attention").is_some() {
        let _ = enum_string(args, "attention", ATTENTION_LEVELS, None)?;
    }
    let name = bounded_string(args, "name", true)?.unwrap_or_default();
    let mut argv = vec![
        "agent".to_owned(),
        "set".to_owned(),
        "--name".to_owned(),
        name,
    ];
    for (key, flag) in [
        ("kind", "--kind"),
        ("state", "--state"),
        ("attention", "--attention"),
        ("session", "--session"),
    ] {
        push_option(&mut argv, flag, bounded_string(args, key, false)?);
    }
    push_socket(&mut argv, args)?;
    push_target(&mut argv, args)?;
    let output = adapter.run(argv, DEFAULT_CALL_TIMEOUT).await?;
    parse_agent_record("set", &output.stdout)
}

async fn clear(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(args, &["target", "socket"], &[])?;
    let mut argv = vec!["agent".to_owned(), "clear".to_owned()];
    push_socket(&mut argv, args)?;
    push_target(&mut argv, args)?;
    let output = adapter.run(argv, DEFAULT_CALL_TIMEOUT).await?;
    parse_agent_record("clear", &output.stdout)
}

// -----------------------------------------------------------------------------
// wait — the EDGE read.
// -----------------------------------------------------------------------------

fn wait_schema() -> Value {
    schema(
        "phux_agent_wait",
        "Block until a pane's agent TRANSITIONS into one of `until` (default: idle, blocked, \
         done — the three ways a turn ends), then return the result document. \
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
            "target": { "type": "string", "minLength": 1, "maxLength": 4096, "description": TARGET_DESC },
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
    strict_object(args, &["target", "until", "timeout_secs", "socket"], &[])?;
    let until = bounded_strings(args, "until", false)?;
    for state in &until {
        if !WAITABLE_STATES.contains(&state.as_str()) {
            return Err(ToolError::new(format!(
                "`until` must contain only: {} (got {state:?}; 'unknown' is a departure, \
                 not a waitable state)",
                WAITABLE_STATES.join(", "),
            )));
        }
    }
    let timeout_secs = match args.get("timeout_secs") {
        None => WAIT_DEFAULT_TIMEOUT_SECS,
        Some(value) => value
            .as_u64()
            .filter(|value| (1..=3600).contains(value))
            .ok_or_else(|| ToolError::new("`timeout_secs` must be an integer in 1..=3600"))?,
    };

    let mut argv = vec!["agent".to_owned(), "wait".to_owned()];
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

    // 124 is a result, not a failure: the CLI prints the whole document and
    // then exits 124 to say "no transition observed". The document's own
    // `satisfied: false` carries that, so the tool returns it verbatim
    // instead of collapsing it into an error string.
    let output = adapter
        .run_allowing(
            argv,
            std::time::Duration::from_secs(timeout_secs.saturating_add(WAIT_DEADLINE_GRACE_SECS)),
            &[EXIT_WAIT_TIMEOUT],
        )
        .await?;
    serde_json::from_str(&output.stdout).map_err(|err| {
        ToolError::new(format!(
            "phux agent wait returned malformed JSON: {err}; stdout={:?}",
            output.stdout
        ))
    })
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
            "target": { "type": "string", "minLength": 1, "maxLength": 4096, "description": "Target selector resolving to one pane. Required — an agent write is never aimed at the focused pane by default." },
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
    // Everything after `--` is positional. Key specs are caller-supplied
    // text and a literal beginning with `-` is legitimate input, so the
    // separator is load-bearing here rather than decorative.
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
            "target": { "type": "string", "minLength": 1, "maxLength": 4096, "description": "Target selector resolving to one pane. Required." },
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
    let output = adapter
        .run_allowing(
            argv,
            std::time::Duration::from_secs(timeout_secs.saturating_add(WAIT_DEADLINE_GRACE_SECS)),
            &[EXIT_WAIT_TIMEOUT],
        )
        .await?;
    parse_json_result("phux agent prompt", &output.stdout)
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
            "target": { "type": "string", "minLength": 1, "maxLength": 4096, "description": "Target selector resolving to one pane. Required." },
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
            "target": { "type": "string", "minLength": 1, "maxLength": 4096, "description": "Existing pane selector. Required; this tool never creates layout." },
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
    adapter
        .run_json(
            argv,
            std::time::Duration::from_secs(timeout_secs.saturating_add(WAIT_DEADLINE_GRACE_SECS)),
        )
        .await
}

// -----------------------------------------------------------------------------
// Shared helpers.
// -----------------------------------------------------------------------------

fn validated_until(args: &Value) -> Result<Vec<String>, ToolError> {
    let until = bounded_strings(args, "until", false)?;
    for state in &until {
        if !WAITABLE_STATES.contains(&state.as_str()) {
            return Err(ToolError::new(format!(
                "`until` must contain only: {}",
                WAITABLE_STATES.join(", ")
            )));
        }
    }
    Ok(until)
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

fn parse_json_result(context: &str, stdout: &str) -> Result<Value, ToolError> {
    serde_json::from_str(stdout).map_err(|err| {
        ToolError::new(format!(
            "{context} returned malformed JSON: {err}; stdout={stdout:?}"
        ))
    })
}

/// Append the optional `target` positional behind a `--` separator.
///
/// The separator is not decoration: a selector is caller-supplied text, and
/// without it a value beginning with `-` would be parsed as a flag by the
/// canonical CLI rather than rejected as a bad selector.
fn push_target(argv: &mut Vec<String>, args: &Value) -> Result<(), ToolError> {
    if let Some(target) = bounded_string(args, "target", false)? {
        argv.push("--".to_owned());
        argv.push(target);
    }
    Ok(())
}

/// Parse the `TERMINAL<TAB>RECORD` line `phux agent set` / `clear` prints
/// into a versioned document. `-` means "no record" (the cleared case).
fn parse_agent_record(action: &str, stdout: &str) -> Result<Value, ToolError> {
    let line = stdout.trim();
    let (terminal, record) = line
        .split_once('\t')
        .ok_or_else(|| ToolError::new("phux agent returned malformed output"))?;
    let record = if record == "-" {
        Value::Null
    } else {
        serde_json::from_str(record)
            .map_err(|err| ToolError::new(format!("phux agent returned malformed JSON: {err}")))?
    };
    Ok(json!({
        "schema_version": 1,
        "action": action,
        "terminal": terminal,
        "record": record,
    }))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::*;

    /// A fake `phux` that logs its argv one entry per line and answers each
    /// agent verb with the shape the real CLI produces — including `wait`'s
    /// document-then-exit-124 timeout.
    fn fake_cli() -> (TempDir, CliAdapter, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("argv");
        let executable = temp.path().join("phux");
        let script = format!(
            r#"#!/bin/sh
: > '{log}'
for arg in "$@"; do
  printf '%s\n' "$arg" >> '{log}'
done
case "$2" in
  set) printf '@1\t{{"name":"bot"}}\n' ;;
  clear) printf '@1\t-\n' ;;
  wait) printf '{{"schema_version":1,"satisfied":false,"baseline":"working","state":"working"}}\n'
        exit 124 ;;
  send-keys) printf '{{"schema_version":1,"verified":true}}\n' ;;
  prompt) printf '{{"schema_version":1,"delivery":"ok","transition_observed":false}}\n'
          exit 124 ;;
  answer) printf '{{"schema_version":1,"delivered":true}}\n' ;;
  start) printf '{{"schema_version":1,"started":true,"ready":true}}\n' ;;
  *) printf '{{"schema_version":1,"agents":[]}}\n' ;;
esac
"#,
            log = log.display(),
        );
        fs::write(&executable, script).unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();
        (temp, CliAdapter::new(executable), log)
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
        let actual = fs::read_to_string(log).unwrap();
        assert_eq!(actual.lines().collect::<Vec<_>>(), expected, "{name}");
        result
    }

    /// Bead phux-w7z2.30: the multiplexer is gone. Every agent verb is its
    /// own tool with its own frozen argument shape, and no schema anywhere
    /// in the family carries an `action` discriminant.
    #[test]
    fn the_agent_family_is_distinct_tools_with_no_action_multiplexer() {
        let schemas = schemas();
        let names: Vec<&str> = schemas
            .iter()
            .filter_map(|schema| schema["name"].as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "phux_agent_list",
                "phux_agent_show",
                "phux_agent_explain",
                "phux_agent_set",
                "phux_agent_clear",
                "phux_agent_wait",
                "phux_agent_send_keys",
                "phux_agent_prompt",
                "phux_agent_answer",
                "phux_agent_start",
            ],
        );
        for schema in &schemas {
            assert_eq!(schema["inputSchema"]["additionalProperties"], false);
            assert_eq!(schema["inputSchema"]["type"], "object");
            assert!(
                schema["inputSchema"]["properties"].get("action").is_none(),
                "{} still multiplexes on `action`",
                schema["name"],
            );
            assert!(schema["description"].is_string());
            assert!(owns(schema["name"].as_str().unwrap()));
        }
        // The union that must never re-form: no read tool may carry the
        // write verbs' arguments, and no write tool the read verbs'.
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

    /// `phux_agent_wait`'s description has to carry three things or it
    /// produces orchestrators that misread its result: that it is
    /// edge-triggered, that a timeout means no transition (not "still
    /// working"), and the two-connection caveat from ADR-0076 point 6.
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

        let set = assert_argv(
            &adapter,
            &log,
            "phux_agent_set",
            json!({
                "target": "@5", "name": "bot", "kind": "codex",
                "state": "working", "attention": "high", "session": "s", "socket": "/sock"
            }),
            &[
                "agent",
                "set",
                "--name",
                "bot",
                "--kind",
                "codex",
                "--state",
                "working",
                "--attention",
                "high",
                "--session",
                "s",
                "--socket",
                "/sock",
                "--",
                "@5",
            ],
        )
        .await;
        assert_eq!(set["record"]["name"], "bot");
        assert_eq!(set["action"], "set");

        let cleared = assert_argv(
            &adapter,
            &log,
            "phux_agent_clear",
            json!({ "target": "@5" }),
            &["agent", "clear", "--", "@5"],
        )
        .await;
        assert!(
            cleared["record"].is_null(),
            "a cleared record is null, not absent",
        );

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

    /// The three turn-driving tools are kept together because their exact
    /// argv is where fused waiting, ask correlation, and existing-pane start
    /// become enforceable rather than descriptive.
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

        // Omitting `timeout_secs` still bounds the call: the CLI's own
        // default is unbounded, which a tool call cannot afford.
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
            assert!(
                call_with_adapter(name, &args, &adapter).await.is_err(),
                "{name} accepted {args}",
            );
        }
    }

    #[test]
    fn the_record_parser_is_strict() {
        let set = parse_agent_record("set", "@2\t{\"name\":\"codex\"}\n").unwrap();
        assert_eq!(set["record"]["name"], "codex");
        assert_eq!(set["schema_version"], 1);
        assert!(parse_agent_record("set", "bad").is_err());
        assert!(parse_agent_record("set", "@2\tnot-json").is_err());
    }
}
