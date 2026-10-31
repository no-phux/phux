//! Screen and terminal-state queries.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use super::*;
use phux_terminal::actor::TerminalResource as _;

/// Highest `GET_SCREEN.format` selector (low 7 bits) this build renders:
/// `1` HTML, `2` VT.
const MAX_SCREEN_FORMAT: u8 = 2;

/// Resolve `terminal_id` to its Terminal facet and ask the actor for a screen
/// projection; `verb` names the command in failure messages.
async fn request_screen(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    scrollback: Option<u32>,
    cells: bool,
    format: u8,
    verb: &str,
) -> Result<(TerminalHandle, ScreenReply), CommandResult> {
    let (_, handle) = state.with(|s| terminal_handle(s, terminal_id))?;
    let terminal = handle.terminal().map_err(wrong_resource_kind)?.clone();
    let internal = |message: String| CommandResult::Error {
        code: ErrorCode::InternalError,
        message,
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    terminal
        .screen
        .send(ScreenRequest {
            pane: terminal_id.local_id().unwrap_or(0),
            scrollback,
            cells,
            format,
            reply: reply_tx,
        })
        .await
        .map_err(|_| internal(format!("pane actor unavailable for {verb}")))?;
    let reply = reply_rx
        .await
        .map_err(|_| internal(format!("pane actor dropped the {verb} reply")))?;
    Ok((terminal, reply))
}

/// `GET_SCREEN`: the pane's [`phux_core::screen::ScreenState`] as JSON
/// (ADR-0022 §2), without attaching or resizing. `format` optionally adds a
/// rendering (D9); an unknown selector is `INVALID_COMMAND` and one over the
/// byte budget is `RESOURCE_EXHAUSTED`.
pub(crate) async fn handle_get_screen(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    request_scrollback: Option<u32>,
    cells: bool,
    format: u8,
) -> CommandResult {
    let selector = format & phux_protocol::wire::frame::GET_SCREEN_FORMAT_SELECTOR_MASK;
    if selector > MAX_SCREEN_FORMAT {
        return CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message: format!("unknown GET_SCREEN format: {format}"),
        };
    }
    match request_screen(
        state,
        terminal_id,
        request_scrollback,
        cells,
        format,
        "GET_SCREEN",
    )
    .await
    {
        Ok((_, reply)) => screen_reply_result(reply),
        Err(refusal) => refusal,
    }
}

/// Turn a [`ScreenReply`] into the `GET_SCREEN` `CommandResult`.
fn screen_reply_result(reply: ScreenReply) -> CommandResult {
    match reply {
        ScreenReply::TooLarge {
            required_bytes,
            budget_bytes,
        } => CommandResult::Error {
            code: ErrorCode::ResourceExhausted,
            message: format!(
                "rendered capture would be {required_bytes} bytes, over the \
                 {budget_bytes}-byte budget; retry with a narrower --tail/--scrollback"
            ),
        },
        ScreenReply::Projection(screen) => serde_json::to_string(&screen).map_or_else(
            |err| CommandResult::Error {
                code: ErrorCode::InternalError,
                message: format!("screen serialization failed: {err}"),
            },
            |json| CommandResult::OkWith(CommandValue::Json(json)),
        ),
    }
}

/// `GET_TERMINAL_STATE` (ADR-0022, ADR-0015 L2): a structured JSON snapshot
/// of the grid, cursor, optional scrollback, and process facet, without an
/// attach. An actor that cannot report its process degrades to
/// `process: null`.
pub(crate) async fn handle_get_terminal_state(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    include_scrollback: bool,
    max_scrollback_lines: u16,
) -> CommandResult {
    let scrollback = include_scrollback.then_some(u32::from(max_scrollback_lines));
    // Cells are always requested for their semantic marks; no rendering.
    let (terminal, reply) = match request_screen(
        state,
        terminal_id,
        scrollback,
        true,
        0,
        "GET_TERMINAL_STATE",
    )
    .await
    {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    // `format: 0` never yields `TooLarge`.
    let ScreenReply::Projection(screen_state) = reply else {
        return CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "unexpected oversized-capture refusal for GET_TERMINAL_STATE".to_owned(),
        };
    };

    let viewport_cells = viewport_cells_json(&screen_state);
    let cursor = screen_state.cursor.map(|cs| {
        serde_json::json!({
            "x": cs.x,
            "y": cs.y,
            "visible": cs.visible,
        })
    });
    #[allow(clippy::cast_possible_truncation)]
    let scrollback_count_total = screen_state.scrollback.len() as u32;
    let scrollback_lines: Vec<_> = if include_scrollback {
        screen_state
            .scrollback
            .iter()
            .map(|line_text| serde_json::json!({ "text": line_text, "cells": [] }))
            .collect()
    } else {
        Vec::new()
    };

    let mut process = query_process_facet(&terminal).await;
    reconcile_process_exit(state, terminal_id, process.as_mut());
    let shell_state = process
        .as_ref()
        .and_then(|process| serde_json::to_value(process.prompt).ok());
    let timestamp_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());

    let terminal_state_json = serde_json::json!({
        "schema_version": phux_core::process::TERMINAL_STATE_SCHEMA_VERSION,
        "process": process,
        "cols": screen_state.cols,
        "rows": screen_state.rows,
        "cells": viewport_cells,
        "cursor": cursor,
        "scrollback": scrollback_lines,
        "scrollback_count_total": scrollback_count_total,
        "shell_state": shell_state,
        "pending_command": serde_json::Value::Null,
        "timestamp_secs": timestamp_secs,
        // No per-terminal logical clock exists yet.
        "seq": 0u64,
    });
    match serde_json::to_string(&terminal_state_json) {
        Ok(json) => CommandResult::OkWith(CommandValue::Json(json)),
        Err(err) => CommandResult::Error {
            code: ErrorCode::InternalError,
            message: format!("terminal state serialization failed: {err}"),
        },
    }
}

/// The viewport as JSON cells, one per char of each right-trimmed line. Width
/// is approximated: ASCII is one column, anything else two.
fn viewport_cells_json(screen_state: &phux_core::screen::ScreenState) -> Vec<serde_json::Value> {
    let mut viewport_cells = Vec::new();
    #[allow(clippy::cast_possible_truncation)]
    for (row_idx, line_text) in screen_state.lines.iter().enumerate() {
        let row = row_idx as u16;
        let mut col = 0u16;
        for ch in line_text.chars() {
            let width = if ch.is_ascii() { 1u16 } else { 2u16 };
            viewport_cells.push(serde_json::json!({
                "col": col,
                "row": row,
                "text": ch.to_string(),
                "width": width as u8,
                "selected": false,
            }));
            col += width;
            if col >= screen_state.cols {
                break;
            }
        }
    }
    viewport_cells
}

/// Make `process.exit` the record `GET_STATE` reports (ADR-0124, L1 §1.1):
/// a retained pane's exit wins, and a pane still closing (a shutdown, a
/// retained eviction) takes its ledger reason. A killed pane is reaped at
/// its commit (L1 §5.2), so it never reaches here.
fn reconcile_process_exit(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    process: Option<&mut phux_core::process::TerminalProcessState>,
) {
    let Some(process) = process else {
        return;
    };
    let (retained, pending) = state.with(|s| {
        let pane = s.terminal_from_wire(terminal_id);
        (
            pane.and_then(|pane| s.retained_process_exit(pane)),
            pane.and_then(|pane| s.pending_close_reason(pane)),
        )
    });
    if let Some(exit) = retained {
        process.exit = Some(exit);
        return;
    }
    if let (Some(exit), Some(reason)) = (process.exit.as_mut(), pending) {
        crate::state::close_reason_name(reason).clone_into(&mut exit.reason);
    }
}

/// Ask a Terminal's actor for its typed process facet; `None` when it cannot
/// answer.
async fn query_process_facet(
    terminal: &phux_terminal::actor::TerminalHandle,
) -> Option<phux_core::process::TerminalProcessState> {
    let (reply, reply_rx) = oneshot::channel();
    terminal
        .process
        .send(phux_terminal::actor::ProcessFacetRequest { reply })
        .await
        .ok()?;
    reply_rx.await.ok()
}
