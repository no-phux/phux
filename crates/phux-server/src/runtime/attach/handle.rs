//! The `ATTACH` handler and viewport application.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;
use phux_terminal::actor::TerminalResource as _;

/// Handle `ATTACH`: resolve the target, prepare the snapshot, capture every
/// pane's bounded bootstrap, and publish `ATTACHED`, the bootstraps, and
/// `ATTACH_READY` atomically. Any failure sends `ERROR`; nothing is
/// partially attached.
#[allow(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    reason = "linear orchestration over the decomposed ATTACH payload; the rollback macro must return from this frame"
)]
#[tracing::instrument(
    level = "info",
    name = "handle_attach",
    skip_all,
    fields(?client_id, target = ?target, cols = viewport.cols, rows = viewport.rows),
)]
pub(crate) async fn handle_attach(
    state: &SharedState,
    client_id: ClientId,
    attach_id: u32,
    target: AttachTarget,
    viewport: phux_protocol::wire::frame::ViewportInfo,
    request_scrollback: bool,
    scrollback_limit_lines: u32,
    // ADR-0127: the declared intent for every Terminal this attach returns.
    role_policy: Option<phux_protocol::wire::frame::RolePolicy>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    client_caps: ClientCapabilities,
    negotiated_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
    root_token: &CancellationToken,
    output_pumps: &mut JoinSet<()>,
    connection_token: &CancellationToken,
    // QUIC multi-stream (docs/protocol.md#quic-stream-routing): publish the snapshot only; per-pane
    // registration, pumps, and bootstraps start at `STREAM_BIND`.
    defer_subscription: bool,
) {
    // L1 §8.1: a viewer cannot take over; refused before anything changes.
    let role = role_policy.unwrap_or_default();
    if !role.is_valid() {
        send_error(
            out_tx,
            ErrorCode::MalformedMessage,
            "ATTACH role_policy { VIEWER, DELIBERATE } is invalid: a viewer cannot take over",
        )
        .await;
        return;
    }
    // Pin the session the dispatch guard authorized (nothing has awaited
    // since); from here the attach follows the id, never the name.
    let pinned = state.with(|s| crate::policy::resolve_attach_session(s, &target));
    let Some(stream_profile) = bootstrap_stream_profile(negotiated_profile) else {
        send_error(
            out_tx,
            ErrorCode::CodecUnavailable,
            "ATTACH selected an unsupported bootstrap profile",
        )
        .await;
        return;
    };
    // `scrollback_limit_lines == 0` means all retained history.
    let scrollback_req: Option<u32> = request_scrollback.then_some(scrollback_limit_lines);

    let Some(session) = resolve_attach_target(
        state,
        target,
        pinned,
        out_tx,
        root_token,
        client_caps.default_colors,
    )
    .await
    else {
        return;
    };

    refresh_registry_cwds(state).await;

    let same_session_reattach = is_same_session_reattach(state, client_id, session);

    let Some((snapshot, initial_client_id, panes_to_snapshot, closed_before_ready)) =
        prepare_attach_or_refuse(
            state,
            client_id,
            session,
            out_tx,
            client_caps,
            negotiated_profile,
            bootstrap_limits,
        )
        .await
    else {
        return;
    };
    let wire_client_id = super::super::wire_client(client_id);
    // For publication only: routing already followed the id.
    let session_name = state
        .with(|s| {
            s.registry()
                .session(session)
                .map(|found| found.name.clone())
        })
        .unwrap_or_default();
    if same_session_reattach {
        detach_prior_state_sync_consumers(&panes_to_snapshot, wire_client_id, client_caps).await;
    }

    apply_client_default_colors(&panes_to_snapshot, client_caps.default_colors).await;

    apply_attach_viewport(state, client_id, &panes_to_snapshot, viewport);

    if defer_subscription {
        let publication = AttachPublication {
            state,
            out_tx,
            connection_token,
            client_id,
            attach_id,
            stream_id: stream_id_from(u64::from(attach_id)),
            bootstrap_id: initial_bootstrap_id(),
        };
        // `STREAM_BIND` carries no role, so the session's role applies here.
        if publication
            .publish(snapshot, initial_client_id, Vec::new(), &session_name)
            .await
        {
            apply_session_role(
                state,
                client_id,
                &panes_to_snapshot,
                role,
                same_session_reattach,
            )
            .await;
        }
        return;
    }

    let stream_id = stream_id_from(u64::from(attach_id));
    let bootstrap_id = initial_bootstrap_id();
    let mut staging = AttachStaging::default();
    macro_rules! fail_prepublication {
        ($reason:expr) => {{
            fail_aggregate_attach_prepublication(
                state,
                client_id,
                attach_id,
                out_tx,
                connection_token,
                &staging.handles,
                &mut staging.pumps,
                output_pumps,
                $reason,
            )
            .await;
            return;
        }};
    }
    if staging
        .handles
        .try_reserve(panes_to_snapshot.len())
        .is_err()
    {
        fail_prepublication!("host allocation failed");
    }

    let (live_gate_tx, live_gate_rx) = tokio::sync::watch::channel(false);
    let Ok(aggregate_chunk_bytes) = usize::try_from(bootstrap_limits.max_chunk_bytes()) else {
        fail_prepublication!("bootstrap chunk bound cannot fit host");
    };
    let capture = PaneCaptureContext {
        state,
        out_tx,
        connection_token,
        live_gate_rx,
        client_id,
        wire_client_id,
        client_caps,
        stream_id,
        bootstrap_id,
        profile: stream_profile,
        limits: bootstrap_limits,
        scrollback: scrollback_req,
        chunk_bytes: aggregate_chunk_bytes,
    };
    // ADR-0127: roles apply only once the attach has published.
    let role_panes = panes_to_snapshot.clone();
    let capture_started = std::time::Instant::now();
    let captured = capture
        .capture_panes(&mut staging, panes_to_snapshot, closed_before_ready)
        .await;
    crate::perf::ATTACH_CAPTURE_WALL.record_elapsed(capture_started);
    if let Err(reason) = captured {
        fail_prepublication!(reason.as_str());
    }

    // Only now, with every bootstrap staged, retire the prior generation.
    if same_session_reattach {
        super::super::client::abort_output_pumps(output_pumps, client_id, "replacement ATTACH")
            .await;
    }
    let mut committed_output_pumps = staging.pumps;
    output_pumps
        .spawn_local(async move { while committed_output_pumps.join_next().await.is_some() {} });

    let publication = AttachPublication {
        state,
        out_tx,
        connection_token,
        client_id,
        attach_id,
        stream_id,
        bootstrap_id,
    };
    let publication_started = std::time::Instant::now();
    let published = publication
        .publish(snapshot, initial_client_id, staging.frames, &session_name)
        .await;
    crate::perf::ATTACH_PUBLISH_WALL.record_elapsed(publication_started);
    if !published {
        return;
    }
    apply_session_role(state, client_id, &role_panes, role, same_session_reattach).await;
    let _ = live_gate_tx.send(true);
    publication.release_gates(staging.gates).await;
}

/// Record the ATTACH viewport and resize every pane to the window-size
/// policy across its subscribers, so full-screen programs fill the client's
/// terminal. Zero dimensions are a no-op (SPEC §10.5); accepted geometry is
/// coalesced until the actor can apply it before capturing the bootstrap.
pub(crate) fn apply_attach_viewport(
    state: &SharedState,
    client_id: ClientId,
    panes_to_snapshot: &[AttachSnapshotPane],
    viewport: phux_protocol::wire::frame::ViewportInfo,
) {
    let cols = viewport.cols;
    let rows = viewport.rows;
    if cols == 0 || rows == 0 {
        return;
    }
    state.with_mut(|s| {
        s.set_client_viewport(client_id, viewport);
        for pane in panes_to_snapshot {
            if s.retained_exit(pane.terminal_id).is_some() {
                continue;
            }
            let Some((cols, rows)) = s.resolve_terminal_geometry(pane.terminal_id, Some(viewport))
            else {
                continue;
            };
            // No resync: the attach bootstrap is authoritative, and a resync
            // would race ahead of it.
            let Ok(terminal) = pane.handle.terminal() else {
                continue;
            };
            match terminal.resize.try_send(ResizeRequest {
                cols,
                rows,
                cell_px: s.resolve_terminal_cell_px(pane.terminal_id),
                resync_clients: false,
                resync_only: false,
                resync_for: None,
            }) {
                Ok(()) => {
                    if let Some(pane_entry) = s.registry_mut().terminal_mut(pane.terminal_id) {
                        pane_entry.dims = (cols, rows);
                    }
                }
                Err(error) => {
                    debug!(
                        terminal_id = ?pane.terminal_id, %error,
                        "ATTACH viewport apply: pane actor gone; dropping resize",
                    );
                }
            }
        }
    });
}
