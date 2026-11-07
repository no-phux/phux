//! Terminal resize handling.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use super::*;
use phux_terminal_api::actor::TerminalResource as _;

/// Handle a client's `RESIZE_TERMINAL` (L1 §3.1): set one Terminal's exact
/// size (and, when carried, its cell pixel size), bypassing the window-size
/// policy, so a headless caller or a layout-owning client can size a pane. A later view-derived recompute may supersede it (docs/architecture.md).
/// There is no reply frame, so not-found paths log and drop.
pub(crate) fn handle_terminal_resize(
    state: &SharedState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    (cols, rows): (u16, u16),
    cell_px: Option<(u16, u16)>,
) {
    // ADR-0145: a degenerate cell is no cell size; keep the last one.
    let cell_px = cell_px.filter(|&(w, h)| w > 0 && h > 0);
    state.with_mut(|s| {
        let local = match s.resolve_resource(wire_terminal_id).into_owned() {
            ResolvedOwned::Remote(_) | ResolvedOwned::Unknown => {
                debug!(
                    ?client_id,
                    ?wire_terminal_id,
                    cols,
                    rows,
                    "RESIZE_TERMINAL: unknown pane; dropping (no-reply per wire frame design)",
                );
                return;
            }
            ResolvedOwned::Local(local) => local,
        };
        let Some(terminal) = resize_destination(s, client_id, local.id, &local.handle) else {
            return;
        };
        // Match the actor's clamp. Record this target only after admission;
        // the replacement bootstrap carries the actor's settled geometry.
        let cols = cols.max(1);
        let rows = rows.max(1);
        // Without a cell size (ADR-0145) the actor keeps its last one.
        if try_resize(
            terminal,
            (cols, rows),
            cell_px,
            "RESIZE_TERMINAL",
            client_id,
        ) && let Some(pane) = s.registry_mut().terminal_mut(local.id)
        {
            pane.dims = (cols, rows);
        }
    });
}

fn resize_destination<'a>(
    s: &crate::state::ServerState,
    client_id: ClientId,
    resource: phux_core::ids::ResourceId,
    handle: &'a ResourceHandle,
) -> Option<&'a TerminalHandle> {
    // ADR-0124: a retained pane's grid records how its process ended.
    if s.retained_exit(resource).is_some() {
        return None;
    }
    match handle.terminal() {
        Ok(terminal) => Some(terminal),
        Err(error) => {
            // L1 §1.1: no reply frame, so an uncorrelated ERROR.
            send_wrong_kind_error(s, client_id, error);
            None
        }
    }
}

/// Refuse a reply-less frame aimed at the wrong resource kind with an
/// uncorrelated `ERROR` to its sender.
pub(in crate::runtime) fn send_wrong_kind_error(
    s: &crate::state::ServerState,
    client_id: ClientId,
    error: WrongResourceKind,
) {
    if let Some(mailbox) = s.client_mailbox(client_id) {
        let _ = mailbox.try_send(Outbound::Frame(FrameKind::Error {
            request_id: None,
            code: ErrorCode::WrongResourceKind,
            message: error.to_string(),
        }));
    }
}

/// Admit a live resize into the actor's latest-geometry slot. A closed
/// actor refuses it; mailbox pressure coalesces rather than losing it.
pub(in crate::runtime) fn try_resize(
    terminal: &TerminalHandle,
    (cols, rows): (u16, u16),
    cell_px: Option<(u16, u16)>,
    verb: &str,
    client_id: ClientId,
) -> bool {
    let request = ResizeRequest {
        cols,
        rows,
        cell_px,
        resync_clients: true,
        resync_only: false,
        resync_for: None,
    };
    match terminal.resize.try_send(request) {
        Ok(()) => true,
        Err(error) => {
            debug!(?client_id, %error, "{verb}: pane actor gone; resize not accepted");
            false
        }
    }
}
