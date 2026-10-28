use serde_json::{Value, json};

use super::{
    START_DEFAULT_TIMEOUT_SECS, TARGET_DESC, WAIT_DEFAULT_TIMEOUT_SECS, WAITABLE_STATES, bool_arg,
    graced, push_target, run_until_transition, target_schema, timeout_secs, validated_until,
};
use crate::cli_adapter::{
    CliAdapter, DEFAULT_CALL_TIMEOUT, bounded_string, bounded_strings, push_socket,
};
use crate::cli_tools::{push_option, schema, string_schema};
use crate::tools::{ToolError, strict_object};

#[cfg(test)]
mod tests;

// -----------------------------------------------------------------------------
// wait — the EDGE read.
// -----------------------------------------------------------------------------

pub(super) fn wait_schema() -> Value {
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

pub(super) async fn wait(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
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

pub(super) fn send_keys_schema() -> Value {
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

pub(super) async fn send_keys(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
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

pub(super) fn prompt_schema() -> Value {
    schema(
        "phux_agent_prompt",
        "Deliver one single-line prompt through acknowledged, idempotent APPLY_INPUT and wait \
         in the same process for a post-delivery lifecycle transition. This is the sound MCP \
         completion primitive: splitting delivery and phux_agent_wait across calls can miss a \
         fast turn. Admission is per pane: prompts to different panes run in parallel; serialize prompts to one pane. \
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

pub(super) async fn prompt(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
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

pub(super) fn answer_schema() -> Value {
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

pub(super) async fn answer(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
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

pub(super) fn start_schema() -> Value {
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

pub(super) async fn start(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
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
