//! Terminal resize handling and attach preparation.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use super::*;

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
            ResolvedOwned::Remote(route) => {
                if !relay_satellite_frame(
                    client_id,
                    wire_terminal_id,
                    &route,
                    "RESIZE_TERMINAL",
                    |id| FrameKind::ResizeTerminal {
                        terminal_id: id,
                        cols,
                        rows,
                        cell_px,
                    },
                ) {
                    warn!(
                        ?client_id,
                        ?wire_terminal_id,
                        cols,
                        rows,
                        "RESIZE_TERMINAL: SATELLITE-routed pane id rejected on non-federation-hub server",
                    );
                }
                return;
            }
            ResolvedOwned::Unknown => {
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
        let terminal = local.id;
        // ADR-0124: a retained pane's grid records how its process ended.
        if s.retained_exit(terminal).is_some() {
            debug!(?client_id, ?wire_terminal_id, "RESIZE_TERMINAL: pane exited; ignored");
            return;
        }
        // Match the actor's clamp. Record this target only after admission;
        // the replacement bootstrap carries the actor's settled geometry.
        let cols = cols.max(1);
        let rows = rows.max(1);
        let terminal = match local.handle.terminal() {
            Ok(terminal) => terminal,
            Err(error) => {
                debug!(?client_id, ?terminal, %error, "RESIZE_TERMINAL: not a Terminal; dropping");
                // L1 §1.1: no reply frame, so an uncorrelated ERROR.
                send_wrong_kind_error(s, client_id, error);
                return;
            }
        };
        // Without a cell size (ADR-0145) the actor keeps its last one.
        if try_resize(terminal, (cols, rows), cell_px, "RESIZE_TERMINAL", client_id)
            && let Some(pane) = s.registry_mut().terminal_mut(local.id)
        {
            pane.dims = (cols, rows);
        }
    });
}

/// Refuse a reply-less frame aimed at the wrong resource kind with an
/// uncorrelated `ERROR` to its sender.
pub(super) fn send_wrong_kind_error(
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
pub(super) fn try_resize(
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

/// Perform the attach mutation in one critical section: attach, build the
/// snapshot, and collect the panes that can bootstrap plus the focused-session
/// panes without an actor, which must be closed before `ATTACH_READY` rather
/// than stranding the client's attach barrier.
pub(crate) fn prepare_attach(
    state: &SharedState,
    client_id: ClientId,
    session: phux_core::ids::SessionId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    client_caps: ClientCapabilities,
    bootstrap_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
) -> Result<AttachPrepared, crate::state::AttachError> {
    state.with_mut(|s| {
        // The caller pinned the session by id; its name is read here, in
        // the same critical section as the attach, never earlier.
        let session_name = s
            .registry()
            .session(session)
            .map(|found| found.name.clone())
            .ok_or_else(|| crate::state::AttachError::UnknownSession(format!("{session:?}")))?;
        let session_name = session_name.as_str();
        let pane_count = s.legacy_panes_of(session).len();
        if pane_count > crate::runtime::attach::MAX_AGGREGATE_BOOTSTRAP_PANES {
            return Err(crate::state::AttachError::ResourceLimit);
        }
        let sid = s.attach(
            client_id,
            session_name,
            out_tx.clone(),
            client_caps,
            bootstrap_profile,
            bootstrap_limits,
        )?;
        s.touch_session(sid);
        let snapshot = s
            .build_session_snapshot(sid)
            .ok_or_else(|| crate::state::AttachError::UnknownSession(session_name.to_owned()))?;
        // workload-auth §6: every returned Terminal is filtered by OBSERVE.
        let snapshot = crate::policy::filter::filter_snapshot(
            s,
            client_id,
            phux_protocol::kinds::Verb::Observe,
            snapshot,
        );
        let panes_to_snapshot = s.attach_snapshot_panes(sid);
        let bootstrapped: HashSet<_> = panes_to_snapshot
            .iter()
            .map(|pane| pane.wire_terminal_id.clone())
            .collect();
        let focused_windows: HashSet<_> = snapshot
            .windows
            .iter()
            .filter(|window| window.session_id == snapshot.focused_session)
            .map(|window| window.id)
            .collect();
        let closed_before_ready = snapshot
            .resources
            .iter()
            .filter(|pane| {
                focused_windows.contains(&pane.window_id) && !bootstrapped.contains(&pane.id)
            })
            .map(|pane| pane.id.clone())
            .collect();
        let initial_client_id = super::super::wire_client(client_id);
        Ok((
            snapshot,
            initial_client_id,
            panes_to_snapshot,
            closed_before_ready,
        ))
    })
}
