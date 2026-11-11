//! Terminal's aggregate capture adapter and viewport effects.
#![allow(clippy::wildcard_imports, reason = "shared attach namespace")]

use super::*;
use crate::runtime::session_attach::{SessionCapture, staging::SessionStaging};
use phux_terminal_api::actor::TerminalResource as _;

#[cfg(test)]
pub(crate) use crate::runtime::session_attach::handle_attach;

/// Capture Terminal state privately. The host sees only accounted frames and
/// a continuation; native cursors and actor publication gates never leave here.
pub(crate) async fn stage_session(
    capture: SessionCapture<'_>,
    staging: &mut SessionStaging,
    pane: AttachSnapshotPane,
) -> Result<(), CaptureFailure> {
    let profile = capture
        .profile
        .and_then(bootstrap_stream_profile)
        .ok_or_else(|| "Terminal bootstrap profile unavailable".to_owned())?;
    let mut terminal_staging = AttachStaging {
        budget: BootstrapStagingBudget::with_limits(
            staging.budget.remaining_bytes(),
            staging.budget.remaining_frames(),
        ),
        ..AttachStaging::default()
    };
    let context = PaneCaptureContext {
        state: capture.state,
        out_tx: capture.out_tx,
        connection_token: capture.connection_token,
        live_gate_rx: capture.live_gate.clone(),
        client_id: capture.client_id,
        wire_client_id: crate::runtime::wire_client(capture.client_id),
        client_caps: capture.client_caps,
        stream_id: capture.stream_id,
        bootstrap_id: capture.bootstrap_id,
        profile,
        limits: capture.limits,
        scrollback: capture.scrollback,
        chunk_bytes: usize::try_from(capture.limits.max_chunk_bytes())
            .map_err(|_| "bootstrap chunk bound cannot fit host".to_owned())?,
    };
    context
        .capture_pane(&mut terminal_staging, pane.clone())
        .await?;
    staging.append(
        terminal_staging.frames,
        terminal_staging.budget.staged_bytes,
    )?;
    let state = capture.state.clone();
    let token = capture.connection_token.clone();
    let client_id = capture.client_id;
    let stream_id = capture.stream_id;
    let bootstrap_id = capture.bootstrap_id;
    let mut pumps = terminal_staging.pumps;
    let gates = terminal_staging.gates;
    staging.park_owner(
        &capture,
        Box::pin(async move {
            release_gates(&state, &token, client_id, stream_id, bootstrap_id, gates).await;
            while pumps.join_next().await.is_some() {}
        }),
    );
    Ok(())
}

/// Size each Terminal of `panes` to `viewport` as its size policy resolves
/// it, in the lock that recorded the viewport.
pub(crate) fn apply_panes_viewport(
    state: &mut crate::state::ServerState,
    panes: &[AttachSnapshotPane],
    viewport: phux_protocol::wire::frame::ViewportInfo,
) {
    for pane in panes {
        apply_pane_viewport(state, pane, viewport);
    }
}

fn apply_pane_viewport(
    state: &mut crate::state::ServerState,
    pane: &AttachSnapshotPane,
    viewport: phux_protocol::wire::frame::ViewportInfo,
) {
    let Ok(terminal) = pane.handle.terminal() else {
        return;
    };
    if state.retained_exit(pane.terminal_id).is_some() {
        return;
    }
    let Some((cols, rows)) = state.resolve_terminal_geometry(pane.terminal_id, Some(viewport))
    else {
        return;
    };
    match terminal.resize.try_send(ResizeRequest {
        cols,
        rows,
        cell_px: state.resolve_terminal_cell_px(pane.terminal_id),
        resync_clients: false,
        resync_only: false,
        resync_for: None,
    }) {
        Ok(()) => {
            if let Some(terminal) = state.registry_mut().terminal_mut(pane.terminal_id) {
                terminal.dims = (cols, rows);
            }
        }
        Err(error) => {
            debug!(terminal_id = ?pane.terminal_id, %error, "ATTACH viewport apply: actor gone");
        }
    }
}
