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
use phux_client::attach::connection::Connection;
use phux_client::selector::{self, Selector};
use phux_client::state::{self, StateView};
use phux_client::wait::{Condition, DEFAULT_IDLE_DWELL, DEFAULT_POLL_INTERVAL, WaitOutcome};
use phux_client::watch::WatchItem;
use phux_protocol::ids::TerminalId;
use phux_protocol::input::paste::PasteTrust;
use phux_protocol::wire::frame::{Command as WireCommand, CommandResult, CommandValue};
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
    let mut tools = json!([
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
                    "tail": { "type": "number", "description": "Return only the last N rendered rows (history, then viewport). 0 = all, capped at 10000. The viewport is a floor — a grid is never returned in part — so a window narrower than the viewport returns more rows than asked, never fewer, and `truncated` reports what was dropped." },
                    "unwrap": { "type": "boolean", "description": "Join soft-wrapped rows into logical lines (rows as written, not as painted), so a match straddling a wrap is findable. Cannot be combined with `cells`: cell coordinates are grid coordinates and do not survive the join." },
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
            "name": "phux_detach",
            "description": "Force-detach every client attached to a session, or every client attached anywhere on the server if `session` is omitted, from outside any attach UI. This is the control-plane counterpart to the interactive `C-a d` self-detach, not a self-detach: it never attaches, and there is no live view of this MCP connection's own to end. Use it to reclaim a session that is attached (or wedged) elsewhere so it is free for the next attach. The session and its panes are unaffected — only the viewing clients are disconnected, each cleanly (a `DETACHED` frame, clean TUI exit). Returns `detached`, the number of clients actually disconnected; `0` means nobody was attached there, which is success, not a miss. Requires confirm=true: this forcibly ejects whatever human or agent is currently attached, without their say-so.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "session": { "type": "string", "minLength": 1, "maxLength": 4096, "description": "Session to detach clients from. Omit to detach every attached client on the server." },
                    "confirm": { "type": "boolean", "const": true, "description": "Required explicit confirmation: this forcibly disconnects whoever is attached." },
                    "socket": { "type": "string" }
                },
                "required": ["confirm"]
            }
        },
        {
            "name": "phux_watch",
            "description": "Collect server-pushed events (command_started/finished, title_changed, asked, bell, pane_spawned/closed, dirty, idle) plus agent_state changes for a pane, or events alone server-wide. Bounded one-shot: returns after max_events or timeout_secs. An agent_state item reports one change to a pane's phux.agent/v1 record — name, kind, session, the new state, effective attention, and `from` when this call already saw a prior record; a present-and-null `state` is the tombstone (the record went away). agent_state items appear ONLY when `target` names a pane: the metadata subscription addresses one Terminal and L3 has no wildcard scope, so a server-wide watch carries events only. Observing an agent reach a state here is not a completion gate — see phux_agent_wait.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": "Pane selector to watch. Omit to watch server-wide events (no agent_state items)." },
                    "max_events": { "type": "number", "description": "Return after collecting this many items, agent_state included. Omit for no count cap." },
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
        crate::cli_tools::insert_schema(),
        crate::cli_tools::move_schema(),
        crate::cli_tools::swap_schema(),
        crate::cli_tools::workspace_schema(),
        crate::plugin_action::schema(),
        crate::plugin_workspace::schema(),
    ]);
    // The `phux_agent_*` family is appended rather than inlined because it
    // is a list that grows one entry per agent verb, and each entry freezes
    // its own argument shape (ADR-0071 point 7(b)). The diagnostics follow
    // for the same reason.
    if let Value::Array(entries) = &mut tools {
        entries.extend(crate::agent_tools::schemas());
        entries.extend(crate::diagnostic_tools::schemas());
    }
    tools
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
        "phux_detach" => phux_detach(args).await,
        "phux_watch" => phux_watch(args).await,
        "phux_ask" => crate::ask_tool::call(args).await,
        "phux_launch" | "phux_spawn" | "phux_signal" | "phux_tag" | "phux_rename"
        | "phux_insert_pane" | "phux_move_pane" | "phux_swap_pane" | "phux_workspace" => {
            crate::cli_tools::call(name, args).await
        }
        "phux_plugin_action" => crate::plugin_action::call(args).await,
        "phux_plugin_workspace" => crate::plugin_workspace::call(args),
        agent if crate::agent_tools::owns(agent) => crate::agent_tools::call(agent, args).await,
        diagnostic if crate::diagnostic_tools::owns(diagnostic) => {
            crate::diagnostic_tools::call(diagnostic, args).await
        }
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
/// `tail` and `unwrap` are the ADR-0077 read modifiers.
///
/// The read modifiers are the only reason this tool has two paths. They are
/// **client-side projections** of the same `GET_SCREEN` reply, and the
/// projection is subtle — unwrap-then-window ordering, the viewport floor,
/// re-basing `soft_wrap.scrollback` indices after a clip, `truncated_reason`
/// — so this delegates to the canonical `phux snapshot --json` rather than
/// keeping a second copy of it here. ADR-0022 §5: MCP is a thin adapter over
/// the surface the CLI uses, not a separate core. Without a modifier the
/// direct in-process read stays, because a subprocess for the common read
/// would be a real cost for no gain; the emitted document is the same
/// `ScreenState` either way.
async fn phux_snapshot(args: &Value) -> Result<Value, ToolError> {
    let socket = socket::resolve(str_arg(args, "socket"));
    let selector = parse_target(args)?;
    let scrollback = u32_arg(args, "scrollback");
    let cells = bool_arg(args, "cells").unwrap_or(false);
    let tail = u32_arg(args, "tail");
    let unwrap = bool_arg(args, "unwrap").unwrap_or(false);

    if unwrap && cells {
        return Err(ToolError::new(
            "`unwrap` cannot be combined with `cells`: cell coordinates are grid \
             coordinates and do not survive the join",
        ));
    }
    if tail.is_some() || unwrap {
        return snapshot_projected(args, scrollback, cells, tail, unwrap).await;
    }

    let view = state::get_state(&socket).await?;
    let terminal_id = resolve_one(&socket, &selector, &view).await?;
    let screen =
        phux_client::snapshot::get_screen_scrollback(&socket, terminal_id, scrollback, cells)
            .await?;
    serde_json::to_value(&screen)
        .map_err(|err| ToolError::new(format!("failed to serialize screen: {err}")))
}

/// The `tail`/`unwrap` half of [`phux_snapshot`], executed as canonical
/// `phux snapshot --json` so the ADR-0077 projection has exactly one
/// implementation.
async fn snapshot_projected(
    args: &Value,
    scrollback: Option<u32>,
    cells: bool,
    tail: Option<u32>,
    unwrap: bool,
) -> Result<Value, ToolError> {
    let mut argv = vec!["snapshot".to_owned(), "--json".to_owned()];
    if let Some(scrollback) = scrollback {
        argv.extend(["--scrollback".to_owned(), scrollback.to_string()]);
    }
    if cells {
        argv.push("--cells".to_owned());
    }
    if let Some(tail) = tail {
        argv.extend(["--tail".to_owned(), tail.to_string()]);
    }
    if unwrap {
        argv.push("--unwrap".to_owned());
    }
    crate::cli_adapter::push_socket(&mut argv, args)?;
    // `--` before the selector: it is caller-supplied text, and a value
    // beginning with `-` must reach the selector parser as a bad selector
    // rather than be read as a flag.
    if let Some(target) = crate::cli_adapter::bounded_string(args, "target", false)? {
        argv.extend(["--".to_owned(), target]);
    }
    crate::cli_adapter::CliAdapter::discover()
        .run_json(argv, crate::cli_adapter::DEFAULT_CALL_TIMEOUT)
        .await
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
    let view = state::get_state(&socket).await?;
    let pane = resolve_one(&socket, &selector, &view).await?;
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
    let view = state::get_state(&socket).await?;
    let pane = resolve_one(&socket, &selector, &view).await?;
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

    let view = state::get_state(&socket).await?;
    let terminal_id = resolve_one(&socket, &selector, &view).await?;
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

/// `phux_detach` — force-detach clients from *outside* the attach UI.
///
/// Direct `DETACH_CLIENTS` over the wire (`phux_client::attach::connection`)
/// rather than a `phux detach` subprocess: the CLI verb has no `--json`, and
/// the one fact this tool exists to report — how many clients were actually
/// disconnected — only rides the wire reply
/// (`phux_protocol::wire::frame::Command::DetachClients`'s doc comment:
/// `OkWith(Json(count))`, unconditionally; the server never refuses this
/// command). This is the bounded, request/response half of "attach or
/// detach" (bead phux-fwwa): unlike `phux attach`, it does not open a live
/// terminal stream, so it fits the one-text-content-block `tools/call`
/// envelope the way `phux_agent_*`'s header comment says a raw ANSI stream
/// cannot.
async fn phux_detach(args: &Value) -> Result<Value, ToolError> {
    strict_object(args, &["session", "confirm", "socket"], &["confirm"])?;
    if args.get("confirm") != Some(&Value::Bool(true)) {
        return Err(ToolError::new(
            "phux_detach forcibly disconnects attached clients; pass `confirm: true`",
        ));
    }
    let socket = socket::resolve(str_arg(args, "socket"));
    let session = crate::cli_adapter::bounded_string(args, "session", false)?;
    let mut conn = Connection::connect(&socket).await?;
    let (result, _interleaved) = conn
        .request(
            1,
            WireCommand::DetachClients {
                session: session.clone(),
            },
        )
        .await?
        .into_parts();
    match result {
        CommandResult::OkWith(CommandValue::Json(count)) => {
            let detached = count.trim().parse::<u64>().map_err(|_| {
                ToolError::new(format!("phux detach returned a malformed count: {count:?}"))
            })?;
            Ok(json!({ "schema_version": 1, "detached": detached, "session": session }))
        }
        other => Err(ToolError::new(phux_client::explain::explain_unexpected(
            "detach", &other,
        ))),
    }
}

/// `phux_watch` — collect server-pushed watch items, bounded.
///
/// The streaming `phux watch` doesn't fit a request/response tool call, so
/// this returns a finite batch: it stops at `max_events`, after
/// `timeout_secs`, or when the server closes, whichever comes first, and
/// returns the collected items as structured JSON. Omitting both bounds
/// blocks until the server exits — callers SHOULD pass `timeout_secs`.
///
/// It collects [`WatchItem`]s rather than calling
/// `phux_client::watch::collect_events`, which filters agent-state changes
/// out on purpose: that collector's contract is the `EVENT` taxonomy, and
/// widening it is a decision about *this* document, not about the streaming
/// layer. Both kinds ride one connection, so the returned array preserves
/// the order the server pushed them in — an `agent_state` line sits exactly
/// where it happened relative to the surrounding events.
async fn phux_watch(args: &Value) -> Result<Value, ToolError> {
    let socket = socket::resolve(str_arg(args, "socket"));
    // `target` is optional: absent ⇒ server-wide subscription, which carries
    // no agent-state items (`SUBSCRIBE_METADATA` names one Terminal and L3
    // has no wildcard scope).
    let terminal = match str_arg(args, "target") {
        None => None,
        Some(raw) => {
            let selector = selector::parse(raw)
                .map_err(|err| ToolError::new(format!("invalid target '{raw}': {err}")))?;
            let view = state::get_state(&socket).await?;
            Some(resolve_one(&socket, &selector, &view).await?)
        }
    };
    let max_items = num_arg(args, "max_events").and_then(|n| usize::try_from(n).ok());
    let timeout = num_arg(args, "timeout_secs").map(Duration::from_secs);

    let items = collect_watch_items(&socket, terminal, max_items, timeout).await?;
    let rendered: Vec<Value> = items.iter().map(watch_item_json).collect();
    // Versioned, unlike the CLI's `phux watch --json`. That surface is an
    // unbounded NDJSON stream a consumer may join mid-flight, so it is
    // versioned by the binary instead; this one is an ordinary bounded
    // request/response document, so it carries `schema_version` like every
    // other MCP result.
    //
    // `2` because `events[].event` gained the `agent_state` value. Adding a
    // *key* would not move the number — `docs/consumers/agents.md` §4.1 is
    // explicit that consumers ignore unknown keys — but this widens the
    // value domain of an existing discriminant a consumer branches on, and
    // that is the case the version exists to signal. The CLI stream answers
    // the same question by having no version at all and declaring the event
    // name to *be* the contract; this document has a version, so it uses it.
    Ok(json!({ "schema_version": 2, "events": rendered, "count": rendered.len() }))
}

/// Bounded one-shot collection of both watch item kinds.
///
/// The MCP twin of `phux_client::watch::collect_events`, differing only in
/// that it keeps `WatchItem::AgentState`. `timeout` elapsing is success —
/// the collected prefix is returned, not an error.
async fn collect_watch_items(
    socket: &std::path::Path,
    terminal: Option<TerminalId>,
    max_items: Option<usize>,
    timeout: Option<Duration>,
) -> Result<Vec<WatchItem>, AttachError> {
    let mut collected: Vec<WatchItem> = Vec::new();
    {
        let sink = |item: WatchItem| {
            collected.push(item);
            max_items.is_none_or(|max| collected.len() < max)
        };
        let fut = phux_client::watch::watch_events(socket, terminal, sink);
        match timeout {
            // Timeout is a clean stop: drop the future, keep the prefix.
            Some(deadline) => {
                if let Ok(result) = tokio::time::timeout(deadline, fut).await {
                    result?;
                }
            }
            None => fut.await?,
        }
    }
    Ok(collected)
}

/// Project one [`WatchItem`] to its JSON line.
fn watch_item_json(item: &WatchItem) -> Value {
    match item {
        WatchItem::Event(event) => agent_event_json(event),
        WatchItem::AgentState(update) => agent_state_json(update),
    }
}

/// Project one `AgentStateUpdate` to the same `agent_state` shape the
/// CLI's `phux watch --json` emits.
///
/// Three choices are load-bearing and mirror the CLI exactly, because a
/// consumer should be able to read either stream with one parser:
///
/// - **Identity survives the tombstone.** `name`/`kind` fall back to the
///   record last seen, so a consumer filtering by agent name still
///   recognizes the line that says its agent went away.
/// - **`state` is present-and-null on a deletion**, not absent, so a
///   consumer keyed on `state` sees the record go rather than reading the
///   previous value forever.
/// - **`attention` is the *effective* level.** The ADR-0046 detector never
///   writes the field and L3 §3.7 derives it from `state`, so the declared
///   value would be absent on every server-derived line.
fn agent_state_json(update: &phux_client::watch::AgentStateUpdate) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("event".to_owned(), Value::from("agent_state"));
    if let Some(terminal) = &update.terminal {
        obj.insert(
            "terminal".to_owned(),
            Value::from(selector::format_terminal_id(terminal)),
        );
    }
    if let Some(record) = update.record.as_ref().or(update.previous.as_ref()) {
        obj.insert("name".to_owned(), Value::from(record.name.clone()));
        if let Some(kind) = &record.kind {
            obj.insert("kind".to_owned(), Value::from(kind.clone()));
        }
    }
    match &update.record {
        Some(record) => {
            obj.insert("state".to_owned(), Value::from(record.state.as_str()));
            obj.insert(
                "attention".to_owned(),
                Value::from(record.effective_attention().as_str()),
            );
            if let Some(session) = &record.session {
                obj.insert("session".to_owned(), Value::from(session.clone()));
            }
        }
        None => {
            obj.insert("state".to_owned(), Value::Null);
        }
    }
    if let Some(previous) = &update.previous {
        obj.insert("from".to_owned(), Value::from(previous.state.as_str()));
    }
    Value::Object(obj)
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

/// Resolve `selector` against `view` to a single pane, exactly as the
/// CLI does (ADR-0021): resolve to the candidate terminals, then narrow via
/// [`selector::pick_target_pane`] (prefer the focused pane, else the first).
///
/// Takes the whole [`StateView`] rather than its snapshot because the miss
/// message depends on the other half. A federation hub that could not reach a
/// satellite still answers `GET_STATE`, with that satellite's panes simply
/// absent from the merge — so "no such target" against a degraded view is a
/// guess, and one an agent will act on. Every MCP tool below funnels its
/// resolution through here, which is why this is the only place that has to
/// know the difference.
///
/// # Errors
///
/// Returns [`ToolError`] when the selector matches no pane, saying which of
/// the two reasons it was.
async fn resolve_one(
    socket: &std::path::Path,
    selector: &Selector,
    view: &StateView,
) -> Result<TerminalId, ToolError> {
    let snapshot = view.snapshot();
    let candidates = state::resolve_targets(socket, selector, snapshot).await;
    selector::pick_target_pane(&candidates, &snapshot.focused_pane).ok_or_else(|| {
        if view.is_complete() {
            ToolError::new("no such target")
        } else {
            ToolError::new(format!(
                "could not resolve the target: this server's view of the fleet is \
                 incomplete ({}), so a miss here does not mean the target is gone",
                view.degradation().notices().join("; ")
            ))
        }
    })
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
    use phux_client::state::Degradation;
    use phux_client::testkit::{ScriptSpec, ScriptedServer};
    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::frame::{ErrorCode, FrameKind, Scope, TERMINAL_TAGS_KEY};
    use phux_protocol::wire::info::{SessionInfo, SessionSnapshot, TerminalInfo, WindowInfo};
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn bounded_watch_propagates_an_error_completed_before_timeout() {
        let dir = tempfile::tempdir().expect("temp dir");
        let missing_socket = dir.path().join("missing.sock");

        let result =
            collect_watch_items(&missing_socket, None, None, Some(Duration::from_secs(1))).await;

        assert!(
            matches!(result, Err(AttachError::Io(_))),
            "the MCP collector must not turn a transport error into an empty success: {result:?}"
        );
    }

    /// The fixture as a server that reached everything it knows about.
    fn whole_fleet() -> StateView {
        StateView::new(fixture(), Degradation::default())
    }

    /// The fixture as a hub that could not reach one satellite: the panes it
    /// would have contributed are simply not in the merge.
    fn partial_fleet() -> StateView {
        StateView::new(
            fixture(),
            Degradation::from_interleaved(&[FrameKind::Error {
                request_id: None,
                code: ErrorCode::SatelliteUnreachable,
                message: "satellite build-box is unreachable: link is down".to_owned(),
            }]),
        )
    }

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
                "phux_detach",
                "phux_watch",
                "phux_ask",
                "phux_launch",
                "phux_spawn",
                "phux_signal",
                "phux_tag",
                "phux_rename",
                "phux_insert_pane",
                "phux_move_pane",
                "phux_swap_pane",
                "phux_workspace",
                "phux_plugin_action",
                "phux_plugin_workspace",
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
                "phux_status",
                "phux_doctor",
            ]
        );
        for tool in arr {
            assert_eq!(tool["inputSchema"]["type"], json!("object"));
            assert!(tool["description"].is_string());
        }
    }

    #[test]
    fn compiled_skill_names_every_tool_and_load_bearing_rule() {
        let skill = crate::SKILL;
        for tool in catalog().as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            assert!(skill.contains(name), "compiled MCP skill omits {name}");
        }
        for rule in [
            "tools/list",
            "phux mcp --schema",
            "finite timeout",
            "level read",
            "observed transition",
            "delivery_unknown",
            "confirm: true",
            "human's client-local focus",
        ] {
            assert!(skill.contains(rule), "compiled MCP skill omits {rule:?}");
        }
        assert!(!skill.contains("ADR-"));
        assert!(!skill.contains("docs/"));
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
        // Same shape as `kill`: rejected before any socket connection, and a
        // present-but-false `confirm` is a distinct, equally-rejected case
        // from an absent one.
        let detach_error = dispatch("phux_detach", &json!({})).await.unwrap_err();
        assert_eq!(detach_error.0, "missing required argument `confirm`");
        assert!(
            dispatch("phux_detach", &json!({ "confirm": false }))
                .await
                .unwrap_err()
                .0
                .contains("confirm: true"),
        );
        assert!(
            dispatch(
                "phux_detach",
                &json!({ "confirm": true, "session": "work", "target": "@1" })
            )
            .await
            .is_err(),
            "phux_detach has no `target` — it addresses a session, not a pane",
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
        // The split agent family routes through `dispatch` by name, and the
        // retired multiplexer spelling is now an unknown tool rather than a
        // silently-accepted legacy shape.
        assert_eq!(
            dispatch("phux_agent", &json!({ "action": "list" }))
                .await
                .unwrap_err()
                .0,
            "unknown tool: phux_agent",
        );
        assert_eq!(
            dispatch("phux_agent_set", &json!({ "target": "@1" }))
                .await
                .unwrap_err()
                .0,
            "missing required argument `name`",
        );
        assert_eq!(
            dispatch("phux_agent_wait", &json!({ "until": ["unknown"] }))
                .await
                .unwrap_err()
                .0,
            "`until` must contain only: idle, working, blocked, done (got \"unknown\"; \
             'unknown' is a departure, not a waitable state)",
        );
        // `unwrap` + `cells` is refused before any read: the join destroys
        // the grid coordinates `cells` is expressed in.
        assert_eq!(
            dispatch(
                "phux_snapshot",
                &json!({ "target": "@1", "unwrap": true, "cells": true })
            )
            .await
            .unwrap_err()
            .0,
            "`unwrap` cannot be combined with `cells`: cell coordinates are grid \
             coordinates and do not survive the join",
        );
    }

    /// `phux_snapshot` grew the ADR-0077 read modifiers, and `phux_watch`
    /// documents that agent-state items need a named target.
    #[test]
    fn catalog_exposes_the_read_modifiers_and_the_agent_state_scope_caveat() {
        let cat = catalog();
        let arr = cat.as_array().expect("catalog is an array");
        let tool = |name: &str| {
            arr.iter()
                .find(|t| t["name"] == json!(name))
                .unwrap_or_else(|| panic!("missing tool {name}"))
                .clone()
        };

        let snapshot = tool("phux_snapshot");
        assert_eq!(
            snapshot["inputSchema"]["properties"]["tail"]["type"],
            json!("number"),
        );
        assert_eq!(
            snapshot["inputSchema"]["properties"]["unwrap"]["type"],
            json!("boolean"),
        );

        let watch = tool("phux_watch");
        let description = watch["description"].as_str().expect("a description");
        assert!(description.contains("agent_state"), "{description}");
        assert!(
            description.contains("ONLY when `target` names a pane"),
            "a server-wide watch carries no agent_state; that must be stated: {description}",
        );
    }

    /// `agent_state` items ride the same array as events, in push order,
    /// with the same field shape the CLI's `phux watch --json` emits.
    #[test]
    fn agent_state_json_matches_the_cli_line_shape() {
        use phux_client::agent_meta::{AgentAttention, AgentMetaState, AgentRecord};
        use phux_client::watch::AgentStateUpdate;

        let record = |state: AgentMetaState| AgentRecord {
            name: "bot".to_owned(),
            kind: Some("codex".to_owned()),
            state,
            attention: None,
            session: Some("fleet".to_owned()),
        };

        // A transition carries identity, the new state, the derived
        // attention, and where it came from.
        let moved = AgentStateUpdate {
            terminal: Some(TerminalId::local(7)),
            record: Some(record(AgentMetaState::Blocked)),
            previous: Some(record(AgentMetaState::Working)),
        };
        let value = agent_state_json(&moved);
        assert_eq!(value["event"], json!("agent_state"));
        assert_eq!(value["terminal"], json!("@7"));
        assert_eq!(value["name"], json!("bot"));
        assert_eq!(value["kind"], json!("codex"));
        assert_eq!(value["session"], json!("fleet"));
        assert_eq!(value["state"], json!("blocked"));
        assert_eq!(value["from"], json!("working"));
        assert_eq!(
            value["attention"],
            json!(AgentAttention::High.as_str()),
            "attention is the effective level derived from `blocked`, not the \
             (absent) declared one",
        );

        // The tombstone: `state` is present-and-null so a consumer keyed on
        // it sees the record go away instead of reading the last value
        // forever, and the identity it had survives so that consumer still
        // recognizes whose line it is.
        let cleared = AgentStateUpdate {
            terminal: Some(TerminalId::local(7)),
            record: None,
            previous: Some(record(AgentMetaState::Working)),
        };
        let value = agent_state_json(&cleared);
        assert_eq!(value["event"], json!("agent_state"));
        assert!(
            value.get("state").is_some_and(Value::is_null),
            "the tombstone's `state` must be present-and-null, got {value}",
        );
        assert_eq!(value["name"], json!("bot"));
        assert_eq!(value["from"], json!("working"));
        assert!(value.get("attention").is_none());

        // Both kinds project through one function, so one array can carry
        // them in the order the server pushed them.
        let event = WatchItem::Event(phux_client::watch::WatchEvent {
            terminal: None,
            event: phux_protocol::wire::frame::AgentEvent::Bell,
        });
        assert_eq!(watch_item_json(&event)["event"], json!("bell"));
        assert_eq!(
            watch_item_json(&WatchItem::AgentState(moved))["event"],
            json!("agent_state"),
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
        let whole = whole_fleet();
        let socket = std::path::Path::new("unused-for-non-tag-selectors");

        // Bare session → focused-or-first pane of the session.
        assert_eq!(
            resolve_one(socket, &selector::parse("work").unwrap(), &whole)
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
                resolve_one(socket, &selector::parse(raw).unwrap(), &whole)
                    .await
                    .unwrap(),
                expected,
            );
        }
        // `.` targets the focused session's focused pane; headless `=` is
        // rejected during parsing because MCP has no attached-client MRU.
        assert_eq!(
            resolve_one(socket, &Selector::Current, &whole)
                .await
                .unwrap(),
            TerminalId::local(100),
        );
        // Misses error.
        assert!(
            resolve_one(socket, &selector::parse("ghost").unwrap(), &whole)
                .await
                .is_err()
        );
        assert!(
            resolve_one(socket, &selector::parse("@999").unwrap(), &whole)
                .await
                .is_err()
        );
    }

    /// A miss against a hub that could not reach a satellite must not be
    /// reported as "no such target": an agent reading that will conclude the
    /// pane is gone and act on it (recreate it, fail the run), when the pane
    /// is alive on a link that is merely down.
    #[tokio::test]
    async fn a_miss_against_a_partial_fleet_does_not_claim_the_target_is_absent() {
        let socket = std::path::Path::new("unused-for-non-tag-selectors");
        let selector = selector::parse("@999").unwrap();

        let complete = resolve_one(socket, &selector, &whole_fleet())
            .await
            .expect_err("@999 is in neither fixture");
        assert_eq!(complete.0, "no such target");

        let degraded = resolve_one(socket, &selector, &partial_fleet())
            .await
            .expect_err("@999 is in neither fixture");
        assert!(
            !degraded.0.contains("no such target"),
            "a partial view cannot assert absence, got {:?}",
            degraded.0
        );
        assert!(
            degraded.0.contains("satellite build-box is unreachable"),
            "the message must name what could not be seen, got {:?}",
            degraded.0
        );
    }

    /// MCP `#tag` resolution fetches the shared L3 tag index and retains
    /// snapshot ordering before applying focused-pane preference.
    #[tokio::test]
    async fn resolve_one_fetches_tag_fixture() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("mcp-tag.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        // The shared harness owns the correlation (one METADATA_VALUE per
        // request id, in order); this closure supplies only the values.
        let spec = ScriptSpec::new().metadata(|scope, key| {
            assert_eq!(key, TERMINAL_TAGS_KEY);
            let Scope::Terminal(terminal_id) = scope else {
                panic!("tag lookup must be terminal-scoped, got {scope:?}");
            };
            matches!(terminal_id.local_id(), Some(100 | 200))
                .then(|| serde_json::to_vec(&vec!["build"]).unwrap())
        });
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

        let pane = resolve_one(&socket, &selector::parse("#build").unwrap(), &whole_fleet())
            .await
            .unwrap();
        assert_eq!(pane, TerminalId::local(100));
        let seen = server.await.unwrap();
        // One per pane, satellite panes included. The hand-written fake this
        // replaced served exactly four and then dropped the socket, so the
        // fifth (satellite-scoped) lookup was answered by a transport EOF
        // rather than by the server — `fetch_tag_index`'s best-effort
        // degradation was masking the gap instead of the test covering it.
        assert!(
            matches!(seen.first(), Some(FrameKind::Hello { .. })),
            "the client must negotiate before metadata lookup; got {seen:?}"
        );
        let metadata = &seen[1..];
        assert_eq!(
            metadata.len(),
            fixture().panes.len(),
            "one GET_METADATA per pane in the snapshot, pipelined; got {seen:?}"
        );
        assert!(
            metadata
                .iter()
                .all(|frame| matches!(frame, FrameKind::GetMetadata { .. })),
            "HELLO must be followed only by GET_METADATA; got {seen:?}"
        );
    }

    /// `phux_detach` sends `DetachClients { session }` over the wire (no CLI
    /// subprocess — the CLI verb has no `--json`) and reports the real
    /// count the server acked, not an echo of the input.
    #[tokio::test]
    async fn phux_detach_reports_the_server_acked_count() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("mcp-detach.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let spec = ScriptSpec::new().detach_result(2);
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

        let result = dispatch(
            "phux_detach",
            &json!({ "session": "work", "confirm": true, "socket": socket.to_string_lossy() }),
        )
        .await
        .unwrap();
        assert_eq!(result["schema_version"], json!(1));
        assert_eq!(result["detached"], json!(2));
        assert_eq!(result["session"], json!("work"));

        let seen = server.await.unwrap();
        assert!(
            matches!(seen.first(), Some(FrameKind::Hello { .. })),
            "the client must negotiate before DETACH_CLIENTS; got {seen:?}"
        );
        assert!(
            matches!(
                seen.get(1),
                Some(FrameKind::Command {
                    command: WireCommand::DetachClients { session },
                    ..
                }) if session.as_deref() == Some("work")
            ),
            "the named session must ride the wire command verbatim; got {seen:?}"
        );
    }

    /// Omitting `session` detaches server-wide — `None` rides the wire as
    /// `None`, not the empty string, and the JSON result's `session` is
    /// `null` rather than absent, so a consumer keyed on the field sees the
    /// server-wide call distinctly from a per-session one.
    #[tokio::test]
    async fn phux_detach_omits_session_for_a_server_wide_call() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("mcp-detach-all.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let spec = ScriptSpec::new().detach_result(0);
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

        let result = dispatch(
            "phux_detach",
            &json!({ "confirm": true, "socket": socket.to_string_lossy() }),
        )
        .await
        .unwrap();
        assert_eq!(
            result["detached"],
            json!(0),
            "0 detached is success, not a miss"
        );
        assert!(result["session"].is_null());

        let seen = server.await.unwrap();
        assert!(
            matches!(
                seen.get(1),
                Some(FrameKind::Command {
                    command: WireCommand::DetachClients { session: None },
                    ..
                })
            ),
            "no session named must ride as None, not an empty string; got {seen:?}"
        );
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
