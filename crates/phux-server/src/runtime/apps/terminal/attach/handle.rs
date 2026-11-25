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
        host: &capture.host,
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
    let host = capture.host.clone();
    let token = capture.connection_token.clone();
    let publication = terminal_staging.publication;
    staging.park_owner(&capture, Box::pin(publication.run(host, token)));
    Ok(())
}

/// Queue host-resolved geometry on one exact Terminal engine. Host policy and
/// registry updates stay with the host; the app only owns its resize effect.
pub(crate) fn apply_viewport(
    handle: &ResourceHandle,
    cols: u16,
    rows: u16,
    cell_px: Option<(u16, u16)>,
) -> bool {
    let Ok(terminal) = handle.terminal() else { return false; };
    match terminal.resize.try_send(ResizeRequest {
        cols, rows, cell_px, resync_clients: false, resync_only: false, resync_for: None,
    }) {
        Ok(()) => true,
        Err(error) => { debug!(%error, "ATTACH viewport apply: actor gone"); false }
    }
}
