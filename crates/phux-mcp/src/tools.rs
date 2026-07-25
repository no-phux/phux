//! The phux tool catalog and `tools/call` dispatch.
//!
//! Each tool is a thin wrapper over the `phux-client` agent surface
//! (`snapshot`, `send_keys`, `run`, `wait`) or a direct control-plane
//! command (`GET_STATE`). MCP is a thin adapter over the same structured
//! surface the CLI uses — not a separate core (ADR-0022 §5).
//!
//! A tool either returns a JSON `Value` (serialized into the MCP
//! `content[0].text` field) or a [`ToolError`] carrying a readable message
//! that becomes a `tools/call` result with `isError: true`. Tool failures
//! never crash the JSON-RPC loop.

#![allow(
    clippy::similar_names,
    reason = "argv and parsed args are deliberately adjacent in canonical CLI adapters"
)]

use std::time::Duration;

use phux_client::attach::AttachError;
use phux_client::selector::{self, Selector};
use phux_client::state;
use phux_client::wait::{Condition, DEFAULT_IDLE_DWELL, DEFAULT_POLL_INTERVAL, WaitOutcome};
use phux_protocol::ids::TerminalId;
use phux_protocol::input::paste::PasteTrust;
use phux_protocol::wire::info::SessionSnapshot;
use serde_json::{Value, json};

use crate::socket;

/// A tool-level failure: surfaced to the caller as a `tools/call` result
/// with `isError: true`, never as a process crash.
#[derive(Debug)]
pub(crate) struct ToolError(pub(crate) String);

impl ToolError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl From<AttachError> for ToolError {
    fn from(err: AttachError) -> Self {
        Self(err.to_string())
    }
}

/// Shared `inputSchema` description for the `target` selector argument.
///
/// The four targeted tools accept the CLI's full `TARGET` grammar
/// (`docs/consumers/tui.md` §3), resolved client-side against a `GET_STATE`
/// snapshot exactly as the CLI resolves it (ADR-0021).
const TARGET_DESC: &str = "Target selector: session, session:window, \
    session:window.pane, @paneid, or `.` for the focused session. `=` is \
    unsupported because MCP has no attached-client focus history. Omit for \
    the focused session.";

/// The MCP tool catalog: name, description, and JSON-Schema input shape.
///
/// Returned verbatim by `tools/list`. Schemas are minimal but valid JSON
/// Schema (`type: object` with `properties`/`required`).
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one flat JSON literal — the tool catalog; splitting it hurts readability"
)]
pub(crate) fn catalog() -> Value {
    json!([
        {
            "name": "phux_ls",
            "description": "List phux sessions on the running server (names, window counts, attached-client counts).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "socket": { "type": "string", "description": "Override the UDS path. Defaults to PHUX_SOCKET or the daemon default." }
                }
            }
        },
        {
            "name": "phux_snapshot",
            "description": "Capture a pane as structured screen data (side-effect-free; does not attach or resize).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": TARGET_DESC },
                    "scrollback": { "type": "number", "description": "Include scrollback history. 0 = all retained history; N = the most-recent N rows. Omit for the viewport only." },
                    "cells": { "type": "boolean", "description": "When true, include per-cell OSC-133 marks and styles. Default false." },
                    "socket": { "type": "string" }
                }
            }
        },
        {
            "name": "phux_send_keys",
            "description": "Send input to a pane. Each key is a named key (Enter, Tab, C-c, ...) or a literal string, tmux-style.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": TARGET_DESC },
                    "keys": { "type": "array", "items": { "type": "string" } },
                    "socket": { "type": "string" }
                },
                "required": ["target", "keys"]
            }
        },
        {
            "name": "phux_paste",
            "description": "Paste text into a pane as one paste event. The server adds bracketed-paste markers when the pane has DEC mode 2004 on, so multiline text lands intact (no per-character auto-indent). A paste inserts without submitting; follow with phux_send_keys Enter to run it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": TARGET_DESC },
                    "text": { "type": "string", "description": "The payload to paste, verbatim (newlines included)." },
                    "untrusted": { "type": "boolean", "description": "Mark the payload untrusted: the pane's untrusted-paste policy (reject by default) may silently drop an unsafe payload. Default false — the caller vouches for content it composed." },
                    "socket": { "type": "string" }
                },
                "required": ["target", "text"]
            }
        },
        {
            "name": "phux_run",
            "description": "Run a command in a pane and report its exit code, output, and duration. Assumes a POSIX shell.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": TARGET_DESC },
                    "command": { "type": "string" },
                    "timeout_secs": { "type": "number", "minimum": 1, "maximum": 3600, "description": "Give up after this many seconds. Default 600; bounded to 1..=3600." },
                    "socket": { "type": "string" }
                },
                "required": ["target", "command"]
            }
        },
        {
            "name": "phux_wait",
            "description": "Poll a pane until it contains text (`until`) or settles (`idle_ms`). Returns whether the condition was met.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": TARGET_DESC },
                    "until": { "type": "string", "description": "Succeed once any visible line contains this substring." },
                    "idle_ms": { "type": "number", "description": "Succeed once the screen holds still this long. Default when `until` is absent." },
                    "timeout_secs": { "type": "number", "description": "Give up after this many seconds. Default: wait forever." },
                    "socket": { "type": "string" }
                }
            }
        },
        {
            "name": "phux_new",
            "description": "Create a named session without attaching, returning its name and seed pane id through the canonical phux new --json surface.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Name for the new session. Required; a name already in use is rejected." },
                    "command": { "type": "array", "items": { "type": "string" }, "description": "Initial command (argv) for the seed pane. Omit or pass an empty array to use the server's default shell." },
                    "cwd": { "type": "string", "description": "Working directory for the seed pane." },
                    "socket": { "type": "string" }
                },
                "required": ["name"]
            }
        },
        {
            "name": "phux_kill",
            "description": "Kill the Terminal(s) a selector resolves to (a whole session, a window, a pane, or `#tag`). Requires confirm=true before executing the canonical CLI.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "target": { "type": "string", "description": TARGET_DESC },
                    "confirm": { "type": "boolean", "const": true, "description": "Required explicit destructive-operation confirmation." },
                    "socket": { "type": "string" }
                },
                "required": ["target", "confirm"]
            }
        },
        {
            "name": "phux_watch",
            "description": "Collect server-pushed events (command_started/finished, title_changed, asked, bell, pane_spawned/closed, dirty, idle) for a pane or server-wide. Bounded one-shot: returns after max_events or timeout_secs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": "Pane selector to watch. Omit to watch server-wide events." },
                    "max_events": { "type": "number", "description": "Return after collecting this many events. Omit for no count cap." },
                    "timeout_secs": { "type": "number", "description": "Return after this many seconds regardless of count. Strongly recommended — without it the call blocks until the server exits." },
                    "socket": { "type": "string" }
                }
            }
        },
        crate::ask_tool::schema(),
        crate::cli_tools::launch_schema(),
        crate::cli_tools::spawn_schema(),
        crate::cli_tools::signal_schema(),
        crate::cli_tools::tag_schema(),
        crate::cli_tools::rename_schema(),
        crate::cli_tools::agent_schema(),
        crate::cli_tools::insert_schema(),
        crate::cli_tools::move_schema(),
        crate::cli_tools::swap_schema(),
        crate::cli_tools::workspace_schema(),
        crate::plugin_action::schema(),
        crate::plugin_workspace::schema(),
    ])
}

/// Dispatch a `tools/call` by tool name. Returns the tool's JSON result on
/// success, or a [`ToolError`] (rendered as `isError: true`) on failure.
///
/// # Errors
///
/// Returns [`ToolError`] for an unknown tool, a malformed/missing required
/// argument, or any failure from the underlying agent surface (no server,
/// unknown session, transport error).
pub(crate) async fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "phux_ls" => phux_ls(args).await,
        "phux_snapshot" => phux_snapshot(args).await,
        "phux_send_keys" => phux_send_keys(args).await,
        "phux_paste" => phux_paste(args).await,
        "phux_run" => phux_run(args).await,
        "phux_wait" => phux_wait(args).await,
        "phux_new" => phux_new(args).await,
        "phux_kill" => phux_kill(args).await,
        "phux_watch" => phux_watch(args).await,
        "phux_ask" => crate::ask_tool::call(args).await,
        "phux_launch" | "phux_spawn" | "phux_signal" | "phux_tag" | "phux_rename"
        | "phux_agent" | "phux_insert_pane" | "phux_move_pane" | "phux_swap_pane"
        | "phux_workspace" => crate::cli_tools::call(name, args).await,
        "phux_plugin_action" => crate::plugin_action::call(args).await,
        "phux_plugin_workspace" => crate::plugin_workspace::call(args),
        other => Err(ToolError::new(format!("unknown tool: {other}"))),
    }
}

/// `phux_ls` — execute and parse canonical `phux ls --json`.
async fn phux_ls(args: &Value) -> Result<Value, ToolError> {
    strict_object(args, &["socket"], &[])?;
    let mut argv = vec!["ls".to_owned(), "--json".to_owned()];
    crate::cli_adapter::push_socket(&mut argv, args)?;
    crate::cli_adapter::CliAdapter::discover()
        .run_json(argv, crate::cli_adapter::DEFAULT_CALL_TIMEOUT)
        .await
}

/// `phux_snapshot` — read a pane as structured data.
///
/// `scrollback` is tri-state, matching `phux snapshot --scrollback`:
/// absent ⇒ viewport only; `0` ⇒ all retained history; `N` ⇒ the
/// most-recent `N` rows. `cells` adds per-cell OSC-133 marks + styles.
async fn phux_snapshot(args: &Value) -> Result<Value, ToolError> {
    let socket = socket::resolve(str_arg(args, "socket"));
    let selector = parse_target(args)?;
    let scrollback = u32_arg(args, "scrollback");
    let cells = bool_arg(args, "cells").unwrap_or(false);
    let snapshot = state::get_state(&socket).await?;
    let terminal_id = resolve_one(&socket, &selector, &snapshot).await?;
    let screen =
        phux_client::snapshot::get_screen_scrollback(&socket, terminal_id, scrollback, cells)
            .await?;
    serde_json::to_value(&screen)
        .map_err(|err| ToolError::new(format!("failed to serialize screen: {err}")))
}

/// `phux_send_keys` — send input to the pane named by the selector.
async fn phux_send_keys(args: &Value) -> Result<Value, ToolError> {
    let socket = socket::resolve(str_arg(args, "socket"));
    let selector = required_target(args)?;
    let keys = string_array(args, "keys")?;
    if keys.is_empty() {
        return Err(ToolError::new(
            "`keys` must be a non-empty array of strings",
        ));
    }
    let snapshot = state::get_state(&socket).await?;
    let pane = resolve_one(&socket, &selector, &snapshot).await?;
    // `send_to` returns `()`; echo the pane we resolved ourselves.
    phux_client::send_keys::send_to(&socket, pane.clone(), &keys).await?;
    Ok(json!({ "sent": true, "pane": pane_value(&pane) }))
}

/// `phux_paste` — paste a payload into the pane named by the selector.
///
/// One `InputEvent::Paste` over `ROUTE_INPUT` (no attach, no resize); the
/// server brackets the payload when the pane's DEC mode 2004 is on.
/// Trusted by default, mirroring `phux paste`; `untrusted: true` opts
/// into the server-side safety gate.
async fn phux_paste(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "text", "untrusted", "socket"],
        &["target", "text"],
    )?;
    let socket = socket::resolve(str_arg(args, "socket"));
    let selector = required_target(args)?;
    let text = required_str(args, "text")?.to_owned();
    let untrusted = bool_arg(args, "untrusted").unwrap_or(false);
    let trust = if untrusted {
        PasteTrust::Untrusted
    } else {
        PasteTrust::Trusted
    };
    let snapshot = state::get_state(&socket).await?;
    let pane = resolve_one(&socket, &selector, &snapshot).await?;
    phux_client::send_keys::paste_to(&socket, pane.clone(), text.into_bytes(), trust).await?;
    Ok(json!({ "sent": true, "pane": pane_value(&pane), "untrusted": untrusted }))
}

/// `phux_run` — run a command in the pane named by the selector.
async fn phux_run(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "command", "timeout_secs", "socket"],
        &["target", "command"],
    )?;
    let target = crate::cli_adapter::bounded_string(args, "target", true)?.unwrap_or_default();
    let command = crate::cli_adapter::bounded_string(args, "command", true)?.unwrap_or_default();
    let timeout_secs = match args.get("timeout_secs") {
        None => RUN_DEFAULT_TIMEOUT_SECS,
        Some(value) => value
            .as_u64()
            .filter(|value| (1..=3600).contains(value))
            .ok_or_else(|| ToolError::new("`timeout_secs` must be an integer in 1..=3600"))?,
    };
    let mut argv = vec![
        "run".to_owned(),
        "--json".to_owned(),
        "--timeout".to_owned(),
        timeout_secs.to_string(),
    ];
    crate::cli_adapter::push_socket(&mut argv, args)?;
    argv.extend([target, command]);
    crate::cli_adapter::CliAdapter::discover()
        .run_json(argv, Duration::from_secs(timeout_secs.saturating_add(5)))
        .await
}

/// `phux_wait` — poll the pane named by the selector until a condition holds.
async fn phux_wait(args: &Value) -> Result<Value, ToolError> {
    let socket = socket::resolve(str_arg(args, "socket"));
    let selector = parse_target(args)?;
    // `until` takes precedence; otherwise settle on idle (explicit ms or
    // the default dwell). Mirrors `phux wait`.
    let condition = str_arg(args, "until").map_or_else(
        || {
            let dwell = num_arg(args, "idle_ms").map_or(DEFAULT_IDLE_DWELL, Duration::from_millis);
            Condition::Idle(dwell)
        },
        |needle| Condition::Contains(needle.to_owned()),
    );
    let timeout = num_arg(args, "timeout_secs").map(Duration::from_secs);

    let snapshot = state::get_state(&socket).await?;
    let terminal_id = resolve_one(&socket, &selector, &snapshot).await?;
    let result = phux_client::wait::poll_until(
        &socket,
        terminal_id,
        &condition,
        timeout,
        DEFAULT_POLL_INTERVAL,
    )
    .await?;
    let outcome = match result.outcome {
        WaitOutcome::Met => "met",
        WaitOutcome::TimedOut => "timed_out",
    };
    Ok(json!({ "outcome": outcome, "polls": result.polls }))
}

/// `phux_new` — create a named session without attaching.
///
/// Mirrors canonical `phux new --json`: `name` is required (the create-only
/// path never auto-names), while `command` and `cwd` are optional. The CLI owns
/// server startup and the returned `{session, terminal_id}` JSON contract.
async fn phux_new(args: &Value) -> Result<Value, ToolError> {
    strict_object(args, &["name", "command", "cwd", "socket"], &["name"])?;
    let name = crate::cli_adapter::bounded_string(args, "name", true)?.unwrap_or_default();
    let mut argv = vec!["new".to_owned(), "-s".to_owned(), name, "--json".to_owned()];
    if let Some(cwd) = crate::cli_adapter::bounded_string(args, "cwd", false)? {
        argv.extend(["-c".to_owned(), cwd]);
    }
    crate::cli_adapter::push_socket(&mut argv, args)?;
    let command = crate::cli_adapter::bounded_strings(args, "command", false)?;
    if !command.is_empty() {
        argv.push("--".to_owned());
        argv.extend(command);
    }
    crate::cli_adapter::CliAdapter::discover()
        .run_json(argv, crate::cli_adapter::DEFAULT_CALL_TIMEOUT)
        .await
}

/// `phux_kill` — tear down the Terminal(s) a selector resolves to.
///
/// Executes canonical `phux kill`, preserving its tag-aware resolution,
/// whole-session atomic teardown, per-pane fallback, and clean-disconnect
/// handling instead of maintaining a second MCP implementation.
async fn phux_kill(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "confirm", "socket"],
        &["target", "confirm"],
    )?;
    if args.get("confirm") != Some(&Value::Bool(true)) {
        return Err(ToolError::new(
            "phux_kill is destructive; pass `confirm: true`",
        ));
    }
    let target = crate::cli_adapter::bounded_string(args, "target", true)?.unwrap_or_default();
    let mut argv = vec!["kill".to_owned(), target.clone()];
    crate::cli_adapter::push_socket(&mut argv, args)?;
    crate::cli_adapter::CliAdapter::discover()
        .run(argv, crate::cli_adapter::DEFAULT_CALL_TIMEOUT)
        .await?;
    Ok(json!({ "schema_version": 1, "killed": true, "target": target }))
}

/// `phux_watch` — collect server-pushed agent events, bounded.
///
/// The streaming `phux watch` doesn't fit a request/response tool call, so
/// this returns a finite batch: it stops at `max_events`, after
/// `timeout_secs`, or when the server closes, whichever comes first, and
/// returns the collected events as structured JSON. Omitting both bounds
/// blocks until the server exits — callers SHOULD pass `timeout_secs`.
async fn phux_watch(args: &Value) -> Result<Value, ToolError> {
    let socket = socket::resolve(str_arg(args, "socket"));
    // `target` is optional: absent ⇒ server-wide subscription.
    let terminal = match str_arg(args, "target") {
        None => None,
        Some(raw) => {
            let selector = selector::parse(raw)
                .map_err(|err| ToolError::new(format!("invalid target '{raw}': {err}")))?;
            let snapshot = state::get_state(&socket).await?;
            Some(resolve_one(&socket, &selector, &snapshot).await?)
        }
    };
    let max_events = num_arg(args, "max_events").and_then(|n| usize::try_from(n).ok());
    let timeout = num_arg(args, "timeout_secs").map(Duration::from_secs);

    let events = phux_client::watch::collect_events(&socket, terminal, max_events, timeout).await?;
    let rendered: Vec<Value> = events.iter().map(agent_event_json).collect();
    Ok(json!({ "events": rendered, "count": rendered.len() }))
}

/// Project one [`phux_client::watch::WatchEvent`] to the stable JSON shape the
/// CLI's `phux watch --json` emits (a `event` name plus the payload field).
fn agent_event_json(ev: &phux_client::watch::WatchEvent) -> Value {
    use phux_protocol::wire::frame::AgentEvent;
    let (kind, mut obj) = match &ev.event {
        AgentEvent::CommandStarted => ("command_started", json!({})),
        AgentEvent::CommandFinished { exit_code } => {
            ("command_finished", json!({ "exit_code": exit_code }))
        }
        AgentEvent::TitleChanged { title } => ("title_changed", json!({ "title": title })),
        AgentEvent::Bell => ("bell", json!({})),
        AgentEvent::PaneSpawned => ("pane_spawned", json!({})),
        AgentEvent::PaneClosed { exit_status } => {
            ("pane_closed", json!({ "exit_status": exit_status }))
        }
        AgentEvent::Dirty => ("dirty", json!({})),
        AgentEvent::Idle => ("idle", json!({})),
        AgentEvent::Asked {
            id,
            question,
            suggestions,
            elapsed_seconds,
        } => (
            "asked",
            json!({
                "id": id,
                "question": question,
                "suggestions": suggestions,
                "elapsed_seconds": elapsed_seconds,
            }),
        ),
        AgentEvent::Unknown { tag, .. } => ("unknown", json!({ "tag": tag })),
        // `AgentEvent` is `#[non_exhaustive]`: a future minor may add a kind
        // this build predates. Surface it generically rather than failing.
        _ => ("unknown", json!({})),
    };
    if let Value::Object(map) = &mut obj {
        map.insert("event".to_owned(), Value::from(kind));
        if let Some(t) = &ev.terminal {
            map.insert(
                "terminal".to_owned(),
                Value::from(selector::format_terminal_id(t)),
            );
        }
    }
    obj
}

// -----------------------------------------------------------------------------
// Shared helpers.
// -----------------------------------------------------------------------------

/// Default `phux_run` timeout when `timeout_secs` is unset (seconds).
/// Matches `phux run`'s default so the surfaces agree.
const RUN_DEFAULT_TIMEOUT_SECS: u64 = 600;

/// Enforce the runtime half of a strict JSON object schema.
///
/// JSON Schema is advisory at the MCP boundary, so handlers also reject
/// non-object arguments, unknown keys, and absent required keys before any
/// subprocess or wire side effect.
pub(crate) fn strict_object(
    args: &Value,
    allowed: &[&str],
    required: &[&str],
) -> Result<(), ToolError> {
    let object = args
        .as_object()
        .ok_or_else(|| ToolError::new("tool arguments must be an object"))?;
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(ToolError::new(format!("unknown argument `{key}`")));
    }
    if let Some(key) = required.iter().find(|key| !object.contains_key(**key)) {
        return Err(ToolError::new(format!("missing required argument `{key}`")));
    }
    Ok(())
}

/// Parse the optional `target` argument into a [`Selector`], defaulting to
/// the focused session when absent. Mirrors the CLI's `parse_selector`
/// front door (phux-n95).
///
/// # Errors
///
/// Returns [`ToolError`] when an explicit `target` is present but malformed
/// (e.g. `@nope`, `work:1.x`).
fn parse_target(args: &Value) -> Result<Selector, ToolError> {
    str_arg(args, "target").map_or(Ok(Selector::Current), |raw| {
        selector::parse(raw).map_err(|err| ToolError::new(format!("invalid target '{raw}': {err}")))
    })
}

/// Parse a required `target` argument into a [`Selector`]. Used by tools
/// where the target is not optional (`send_keys`, `run`).
///
/// # Errors
///
/// Returns [`ToolError`] when `target` is missing/not a string, or present
/// but malformed.
fn required_target(args: &Value) -> Result<Selector, ToolError> {
    let raw = required_str(args, "target")?;
    selector::parse(raw).map_err(|err| ToolError::new(format!("invalid target '{raw}': {err}")))
}

/// Resolve `selector` against `snapshot` to a single pane, exactly as the
/// CLI does (ADR-0021): resolve to the candidate terminals, then narrow via
/// [`selector::pick_target_pane`] (prefer the focused pane, else the first).
///
/// # Errors
///
/// Returns [`ToolError`] when the selector matches no pane.
async fn resolve_one(
    socket: &std::path::Path,
    selector: &Selector,
    snapshot: &SessionSnapshot,
) -> Result<TerminalId, ToolError> {
    let candidates = state::resolve_targets(socket, selector, snapshot).await;
    selector::pick_target_pane(&candidates, &snapshot.focused_pane)
        .ok_or_else(|| ToolError::new("no such target"))
}

/// A JSON rendering of a `TerminalId` using the canonical direct selector.
fn pane_value(id: &TerminalId) -> Value {
    json!(selector::format_terminal_id(id))
}

/// Read an optional string argument from a tool's params object.
fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

/// Read a required string argument, erroring with a readable message when
/// it is missing or not a string.
fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    str_arg(args, key).ok_or_else(|| ToolError::new(format!("missing required string `{key}`")))
}

/// Read an optional non-negative integer argument (as `u64`). Values that
/// are not non-negative integers are treated as absent.
fn num_arg(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

/// Read an optional non-negative integer argument as `u32`. A value that is
/// not a non-negative integer, or that overflows `u32`, is treated as
/// absent. Used for `scrollback`, whose `None`/`Some(0)`/`Some(n)` triad is
/// load-bearing (viewport / all history / last-n rows).
fn u32_arg(args: &Value, key: &str) -> Option<u32> {
    num_arg(args, key).and_then(|n| u32::try_from(n).ok())
}

/// Read an optional boolean argument. Non-boolean values are treated as
/// absent.
fn bool_arg(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

/// Read a required array-of-strings argument.
fn string_array(args: &Value, key: &str) -> Result<Vec<String>, ToolError> {
    let arr = args
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| ToolError::new(format!("missing required array `{key}`")))?;
    arr.iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| ToolError::new(format!("`{key}` must contain only strings")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;
    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::frame::{FrameKind, Scope, TERMINAL_TAGS_KEY};
    use phux_protocol::wire::info::{SessionInfo, TerminalInfo, WindowInfo};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    #[test]
    fn catalog_lists_all_tools_with_object_schemas() {
        let cat = catalog();
        let arr = cat.as_array().expect("catalog is an array");
        let names: Vec<&str> = arr.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(
            names,
            vec![
                "phux_ls",
                "phux_snapshot",
                "phux_send_keys",
                "phux_paste",
                "phux_run",
                "phux_wait",
                "phux_new",
                "phux_kill",
                "phux_watch",
                "phux_ask",
                "phux_launch",
                "phux_spawn",
                "phux_signal",
                "phux_tag",
                "phux_rename",
                "phux_agent",
                "phux_insert_pane",
                "phux_move_pane",
                "phux_swap_pane",
                "phux_workspace",
                "phux_plugin_action",
                "phux_plugin_workspace",
            ]
        );
        for tool in arr {
            assert_eq!(tool["inputSchema"]["type"], json!("object"));
            assert!(tool["description"].is_string());
        }
    }

    /// `phux-mcp --schema` renders the catalog as a standalone document.
    /// The dump has to survive a round-trip, because its whole purpose is to
    /// be read by a tool that is not this binary.
    #[test]
    fn the_catalog_round_trips_as_a_standalone_json_document() {
        let rendered = serde_json::to_string_pretty(&catalog()).expect("catalog serializes");
        let parsed: Value = serde_json::from_str(&rendered).expect("dump re-parses");
        assert_eq!(parsed, catalog(), "the dump lost information");
        assert!(
            parsed.as_array().is_some_and(|a| !a.is_empty()),
            "the dump is not a non-empty array"
        );
    }

    /// `phux_new` exposes a required `name` and optional `cwd`/`command`/
    /// `socket` props, with `command` typed as a string array.
    #[test]
    fn catalog_phux_new_requires_name_and_object_schema() {
        let cat = catalog();
        let arr = cat.as_array().expect("catalog is an array");
        let new = arr
            .iter()
            .find(|t| t["name"] == json!("phux_new"))
            .expect("phux_new present");

        assert_eq!(new["inputSchema"]["type"], json!("object"));
        assert_eq!(new["inputSchema"]["required"], json!(["name"]));

        let props = &new["inputSchema"]["properties"];
        assert_eq!(props["name"]["type"], json!("string"));
        assert_eq!(props["cwd"]["type"], json!("string"));
        assert_eq!(props["socket"]["type"], json!("string"));
        assert_eq!(props["command"]["type"], json!("array"));
        assert_eq!(props["command"]["items"]["type"], json!("string"));
    }

    /// The grown snapshot surface: `scrollback` + `cells` params, plus the
    /// unified `target` selector (no more `session`-name-only `session`
    /// param) on all four targeted tools.
    #[test]
    fn catalog_exposes_scrollback_cells_and_target_selector() {
        let cat = catalog();
        let arr = cat.as_array().expect("catalog is an array");
        let tool = |name: &str| {
            arr.iter()
                .find(|t| t["name"] == json!(name))
                .unwrap_or_else(|| panic!("missing tool {name}"))
                .clone()
        };

        let snap = tool("phux_snapshot");
        let props = &snap["inputSchema"]["properties"];
        assert_eq!(props["scrollback"]["type"], json!("number"));
        assert_eq!(props["cells"]["type"], json!("boolean"));
        // snapshot's selector is optional (no `required`).
        assert!(snap["inputSchema"].get("required").is_none());

        // Every targeted tool documents the unified `target` selector and
        // dropped the old `session`-only param.
        for name in ["phux_snapshot", "phux_send_keys", "phux_run", "phux_wait"] {
            let t = tool(name);
            let p = &t["inputSchema"]["properties"];
            assert!(p["target"].is_object(), "{name} missing target");
            assert!(p.get("session").is_none(), "{name} still has session");
        }

        // send_keys/run now require `target` (not `session`).
        assert_eq!(
            tool("phux_send_keys")["inputSchema"]["required"],
            json!(["target", "keys"]),
        );
        // paste requires an explicit target and payload; `untrusted` is an
        // optional boolean (trusted is the default).
        assert_eq!(
            tool("phux_paste")["inputSchema"]["required"],
            json!(["target", "text"]),
        );
        assert_eq!(
            tool("phux_paste")["inputSchema"]["properties"]["untrusted"]["type"],
            json!("boolean"),
        );
        assert_eq!(
            tool("phux_run")["inputSchema"]["required"],
            json!(["target", "command"]),
        );

        // Destructive kill requires both an explicit target and confirmation;
        // watch's target is optional.
        assert_eq!(
            tool("phux_kill")["inputSchema"]["required"],
            json!(["target", "confirm"]),
        );
        assert_eq!(
            tool("phux_kill")["inputSchema"]["properties"]["confirm"]["const"],
            true,
        );
        assert!(tool("phux_watch")["inputSchema"].get("required").is_none());
    }

    #[tokio::test]
    async fn added_tool_dispatch_routes_to_strict_validation() {
        let kill_error = dispatch("phux_kill", &json!({ "target": "@1" }))
            .await
            .unwrap_err();
        assert_eq!(
            kill_error.0, "missing required argument `confirm`",
            "kill must reject before discovering or starting the CLI",
        );
        assert!(dispatch("phux_launch", &json!({})).await.is_err());
        // paste rejects a missing payload and unknown keys before any
        // socket resolution or wire side effect.
        let paste_error = dispatch("phux_paste", &json!({ "target": "@1" }))
            .await
            .unwrap_err();
        assert_eq!(paste_error.0, "missing required argument `text`");
        assert!(
            dispatch(
                "phux_paste",
                &json!({ "target": "@1", "text": "x", "keys": ["a"] })
            )
            .await
            .is_err()
        );
        assert!(
            dispatch("phux_signal", &json!({ "target": "@1", "signal": "kill" }))
                .await
                .is_err()
        );
        assert!(
            dispatch("phux_insert_pane", &json!({ "target": "@1" }))
                .await
                .is_err()
        );
        assert!(
            dispatch("phux_workspace", &json!({ "action": "delete" }))
                .await
                .is_err()
        );
    }

    /// `agent_event_json` projects each event kind to the same stable shape
    /// the CLI's `phux watch --json` emits (`event` name + payload field).
    #[test]
    fn agent_event_json_projects_kind_and_payload() {
        use phux_client::watch::WatchEvent;
        use phux_protocol::wire::frame::AgentEvent;

        let ev = WatchEvent {
            terminal: None,
            event: AgentEvent::CommandFinished {
                exit_code: Some(42),
            },
        };
        let v = agent_event_json(&ev);
        assert_eq!(v["event"], json!("command_finished"));
        assert_eq!(v["exit_code"], json!(42));

        let bell = WatchEvent {
            terminal: None,
            event: AgentEvent::Bell,
        };
        assert_eq!(agent_event_json(&bell)["event"], json!("bell"));

        let titled = WatchEvent {
            terminal: None,
            event: AgentEvent::TitleChanged {
                title: "vim".to_owned(),
            },
        };
        let tv = agent_event_json(&titled);
        assert_eq!(tv["event"], json!("title_changed"));
        assert_eq!(tv["title"], json!("vim"));

        let satellite = WatchEvent {
            terminal: Some(TerminalId::satellite("devbox", 7)),
            event: AgentEvent::Dirty,
        };
        assert_eq!(agent_event_json(&satellite)["terminal"], json!("devbox/@7"));
        assert_eq!(pane_value(&TerminalId::local(3)), json!("@3"));
        assert_eq!(
            pane_value(&TerminalId::satellite("devbox", 7)),
            json!("devbox/@7"),
        );

        let asked = WatchEvent {
            terminal: None,
            event: AgentEvent::Asked {
                id: "q1".to_owned(),
                question: "Deploy to prod?".to_owned(),
                suggestions: vec!["Yes".to_owned(), "No".to_owned()],
                elapsed_seconds: None,
            },
        };
        let av = agent_event_json(&asked);
        assert_eq!(av["event"], json!("asked"));
        assert_eq!(av["id"], json!("q1"));
        assert_eq!(av["question"], json!("Deploy to prod?"));
        assert_eq!(av["suggestions"], json!(["Yes", "No"]));
        assert!(av["elapsed_seconds"].is_null());
    }

    /// `scrollback`/`cells` arg plumbing: the tri-state scrollback and the
    /// optional bool map as documented.
    #[test]
    fn scrollback_and_cells_args_parse() {
        // Absent → None (viewport only); 0 → Some(0) (all history); N → N.
        assert_eq!(u32_arg(&json!({}), "scrollback"), None);
        assert_eq!(u32_arg(&json!({ "scrollback": 0 }), "scrollback"), Some(0));
        assert_eq!(
            u32_arg(&json!({ "scrollback": 25 }), "scrollback"),
            Some(25)
        );
        // Negative / overflowing values are treated as absent.
        assert_eq!(u32_arg(&json!({ "scrollback": -3 }), "scrollback"), None);
        assert_eq!(
            u32_arg(
                &json!({ "scrollback": u64::from(u32::MAX) + 1 }),
                "scrollback"
            ),
            None,
        );

        assert_eq!(bool_arg(&json!({}), "cells"), None);
        assert_eq!(bool_arg(&json!({ "cells": true }), "cells"), Some(true));
        assert_eq!(bool_arg(&json!({ "cells": false }), "cells"), Some(false));
        assert_eq!(bool_arg(&json!({ "cells": "yes" }), "cells"), None);
    }

    /// `parse_target` is the optional-selector front door (snapshot/wait):
    /// absent ⇒ `Current`, supported grammar parses, malformed/`=` ⇒ error.
    #[test]
    fn parse_target_defaults_and_accepts_grammar() {
        assert_eq!(parse_target(&json!({})).unwrap(), Selector::Current);
        assert_eq!(
            parse_target(&json!({ "target": "." })).unwrap(),
            Selector::Current,
        );
        assert_eq!(
            parse_target(&json!({ "target": "work:1.2" })).unwrap(),
            Selector::Pane("work".to_owned(), selector::WindowRef::Index(1), 2),
        );
        assert_eq!(
            parse_target(&json!({ "target": "@100" })).unwrap(),
            Selector::TerminalId(100),
        );
        // Malformed and headless `=` both error before any server round trip.
        assert!(parse_target(&json!({ "target": "@nope" })).is_err());
        let err = parse_target(&json!({ "target": "=" })).unwrap_err();
        assert!(err.0.contains("attached-TUI focus history"), "{err:?}");
    }

    /// `required_target` (the `send_keys`/`run` front door) rejects a
    /// missing target and a malformed one alike.
    #[test]
    fn required_target_demands_a_selector() {
        assert!(required_target(&json!({})).is_err());
        assert_eq!(
            required_target(&json!({ "target": "work" })).unwrap(),
            Selector::Session("work".to_owned()),
        );
        assert!(required_target(&json!({ "target": "work:1.x" })).is_err());
    }

    /// `resolve_one` maps each selector form to the expected pane against a
    /// multi-session/window/pane snapshot, exactly as the CLI does, and
    /// errors on a miss.
    #[tokio::test]
    async fn resolve_one_maps_every_selector_form() {
        let snap = fixture();
        let socket = std::path::Path::new("unused-for-non-tag-selectors");

        // Bare session → focused-or-first pane of the session.
        assert_eq!(
            resolve_one(socket, &selector::parse("work").unwrap(), &snap)
                .await
                .unwrap(),
            TerminalId::local(100),
        );
        // Window, exact pane, local id, and satellite id selectors.
        for (raw, expected) in [
            ("work:1", TerminalId::local(101)),
            ("work:editor", TerminalId::local(101)),
            ("work:1.1", TerminalId::local(102)),
            ("@200", TerminalId::local(200)),
            ("devbox/@7", TerminalId::satellite("devbox", 7)),
        ] {
            assert_eq!(
                resolve_one(socket, &selector::parse(raw).unwrap(), &snap)
                    .await
                    .unwrap(),
                expected,
            );
        }
        // `.` targets the focused session's focused pane; headless `=` is
        // rejected during parsing because MCP has no attached-client MRU.
        assert_eq!(
            resolve_one(socket, &Selector::Current, &snap)
                .await
                .unwrap(),
            TerminalId::local(100),
        );
        // Misses error.
        assert!(
            resolve_one(socket, &selector::parse("ghost").unwrap(), &snap)
                .await
                .is_err()
        );
        assert!(
            resolve_one(socket, &selector::parse("@999").unwrap(), &snap)
                .await
                .is_err()
        );
    }

    /// MCP `#tag` resolution fetches the shared L3 tag index and retains
    /// snapshot ordering before applying focused-pane preference.
    #[tokio::test]
    async fn resolve_one_fetches_tag_fixture() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("mcp-tag.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            for _ in 0..4 {
                let frame = read_frame(&mut stream).await;
                let FrameKind::GetMetadata {
                    request_id,
                    scope: Scope::Terminal(terminal_id),
                    key,
                } = frame
                else {
                    panic!("expected terminal GET_METADATA, got {frame:?}");
                };
                assert_eq!(key, TERMINAL_TAGS_KEY);
                let value = matches!(terminal_id.local_id(), Some(100 | 200))
                    .then(|| serde_json::to_vec(&vec!["build"]).unwrap());
                write_frame(&mut stream, &FrameKind::MetadataValue { request_id, value }).await;
            }
        });

        let pane = resolve_one(&socket, &selector::parse("#build").unwrap(), &fixture())
            .await
            .unwrap();
        assert_eq!(pane, TerminalId::local(100));
        server.await.unwrap();
    }

    async fn read_frame(stream: &mut tokio::net::UnixStream) -> FrameKind {
        let mut header = [0_u8; 4];
        stream.read_exact(&mut header).await.unwrap();
        let body_len = usize::try_from(u32::from_be_bytes(header)).unwrap();
        let mut encoded = Vec::with_capacity(4 + body_len);
        encoded.extend_from_slice(&header);
        encoded.resize(4 + body_len, 0);
        stream.read_exact(&mut encoded[4..]).await.unwrap();
        let (frame, tail) = FrameKind::decode(&encoded).unwrap();
        assert!(tail.is_empty());
        frame
    }

    async fn write_frame(stream: &mut tokio::net::UnixStream, frame: &FrameKind) {
        let mut encoded = BytesMut::new();
        frame.encode(&mut encoded);
        stream.write_all(&encoded).await.unwrap();
    }

    /// Build a snapshot: session "work" (id 1, focused, pane 100 focused)
    /// with two windows, plus a second session "play" (pane 200).
    fn fixture() -> SessionSnapshot {
        let work = SessionId::new(1);
        let play = SessionId::new(2);
        let w0 = WindowId::new(10);
        let w1 = WindowId::new(11);
        let p0 = WindowId::new(20);
        let sessions = vec![
            SessionInfo::new(work, "work"),
            SessionInfo::new(play, "play"),
        ];
        let windows = vec![
            WindowInfo::new(w0, work, "shell").with_index(0),
            WindowInfo::new(w1, work, "editor").with_index(1),
            WindowInfo::new(p0, play, "shell").with_index(0),
        ];
        let panes = vec![
            TerminalInfo::new(TerminalId::local(100), w0, 80, 24),
            TerminalInfo::new(TerminalId::local(101), w1, 80, 24),
            TerminalInfo::new(TerminalId::local(102), w1, 80, 24),
            TerminalInfo::new(TerminalId::local(200), p0, 80, 24),
            // Aggregated federation inventory carries satellite panes without
            // inventing hub-local session/window joins.
            TerminalInfo::new(
                TerminalId::satellite("devbox", 7),
                WindowId::new(999),
                80,
                24,
            ),
        ];
        SessionSnapshot::new(work, w0, TerminalId::local(100))
            .with_sessions(sessions)
            .with_windows(windows)
            .with_panes(panes)
    }
}
