//! Kind-neutral SESSION ATTACH transaction. Apps capture unpublished runs;
//! the host alone publishes the aggregate barrier and releases live output.
#![allow(
    clippy::redundant_pub_crate,
    reason = "private host module exposes crate-scoped app staging contracts"
)]

use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ClientCapabilities};
use phux_protocol::ids::{BootstrapId, StreamId};
use phux_protocol::wire::frame::{AttachTarget, ErrorCode, FrameKind, RolePolicy, ViewportInfo};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::{apps, send_error, stream_id_from};
use crate::state::{AttachSnapshotPane, ClientId, Outbound, SharedState};
use apps::terminal::attach::{
    apply_attach_viewport, apply_client_default_colors, apply_session_role,
    bootstrap_stream_profile, detach_prior_state_sync_consumers,
    fail_aggregate_attach_prepublication, initial_bootstrap_id, refresh_registry_cwds,
    resolve_attach_target,
};
use prepare::{is_same_session_reattach, prepare_attach_or_refuse};
use publication::AttachPublication;
use staging::{CaptureFailure, SessionStaging};

pub(crate) mod prepare;
mod publication;
pub(crate) mod staging;
#[cfg(test)]
mod tests;

/// The immutable generation and remaining resource ceilings lent to an app.
#[derive(Clone)]
pub(crate) struct SessionCapture<'a> {
    pub(crate) state: &'a SharedState,
    pub(crate) out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    pub(crate) connection_token: &'a CancellationToken,
    pub(crate) live_gate: tokio::sync::watch::Receiver<bool>,
    pub(crate) client_id: ClientId,
    pub(crate) client_caps: ClientCapabilities,
    pub(crate) stream_id: StreamId,
    pub(crate) bootstrap_id: BootstrapId,
    pub(crate) profile: Option<BootstrapProfile>,
    pub(crate) limits: BootstrapLimits,
    pub(crate) scrollback: Option<u32>,
}

impl SessionCapture<'_> {
    async fn capture(
        &self,
        staging: &mut SessionStaging,
        panes: Vec<AttachSnapshotPane>,
        mut closed: Vec<phux_protocol::ids::ResourceId>,
    ) -> Result<(), String> {
        staging
            .handles
            .try_reserve(panes.len())
            .map_err(|_| "host allocation failed".to_owned())?;
        closed
            .try_reserve(panes.len())
            .map_err(|_| "host allocation failed".to_owned())?;
        for pane in panes {
            staging.handles.push(pane.handle.clone());
            let wire = pane.wire_terminal_id.clone();
            match apps::app_for(pane.handle.kind)
                .stage_session(self.clone(), staging, pane)
                .await
            {
                Ok(()) => {}
                Err(CaptureFailure::Failed(reason)) => return Err(reason),
                Err(CaptureFailure::Closed(reason)) => {
                    tracing::debug!(resource = %wire, %reason, "resource closed during ATTACH capture");
                    closed.push(wire);
                }
            }
        }
        let closures = staging
            .collect_frames(
                closed
                    .into_iter()
                    .map(|terminal_id| FrameKind::ResourceClosed {
                        terminal_id,
                        exit_status: None,
                        reason: phux_protocol::wire::frame::CloseReason::Unknown,
                        signal: None,
                    }),
            )
            .map_err(|failure| format!("{failure:?}"))?;
        staging
            .append(closures, 0)
            .map_err(|failure| format!("{failure:?}"))
    }
}

/// Refuse a missing Terminal profile before resolving a target that could seed
/// a Terminal, or changing subscription, palette, geometry or input state.
fn requires_terminal_profile(
    state: &SharedState,
    pinned: Option<phux_core::ids::SessionId>,
) -> bool {
    pinned.is_none_or(|session| state.with(|s| !s.session_terminals(session).is_empty()))
}

fn attach_refusal(
    state: &SharedState,
    pinned: Option<phux_core::ids::SessionId>,
    role: RolePolicy,
    profile: Option<BootstrapProfile>,
) -> Option<(ErrorCode, &'static str)> {
    if !role.is_valid() {
        return Some((
            ErrorCode::MalformedMessage,
            "ATTACH role_policy { VIEWER, DELIBERATE } is invalid: a viewer cannot take over",
        ));
    }
    if requires_terminal_profile(state, pinned)
        && profile.and_then(bootstrap_stream_profile).is_none()
    {
        return Some((
            ErrorCode::CodecUnavailable,
            apps::terminal::commands::NO_TERMINAL_PROFILE,
        ));
    }
    None
}

async fn fence_previous(
    state: &SharedState,
    client: ClientId,
    panes: &[AttachSnapshotPane],
    caps: ClientCapabilities,
    same_session: bool,
) {
    // Single-resource pumps are not owned by the aggregate JoinSet. Fence all
    // prior paths, not just Terminal ticks, before staging their replacements.
    for pane in panes {
        super::pump::stop_output(state, client, pane.terminal_id).await;
    }
    if same_session {
        detach_prior_state_sync_consumers(panes, super::wire_client(client), caps).await;
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "ordered session transaction with an existing wire payload and rollback boundary"
)]
#[tracing::instrument(
    level = "info", name = "handle_attach", skip_all,
    fields(?client_id, target = ?target, cols = viewport.cols, rows = viewport.rows),
)]
pub(crate) async fn handle_attach(
    state: &SharedState,
    client_id: ClientId,
    attach_id: u32,
    target: AttachTarget,
    viewport: ViewportInfo,
    request_scrollback: bool,
    scrollback_limit_lines: u32,
    role_policy: Option<RolePolicy>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    client_caps: ClientCapabilities,
    negotiated_profile: impl Into<Option<BootstrapProfile>>,
    bootstrap_limits: BootstrapLimits,
    root_token: &CancellationToken,
    output_pumps: &mut JoinSet<()>,
    connection_token: &CancellationToken,
    defer_subscription: bool,
) -> bool {
    let profile = negotiated_profile.into();
    let role = role_policy.unwrap_or_default();
    let pinned = state.with(|s| crate::policy::resolve_attach_session(s, &target));
    if let Some((code, message)) = attach_refusal(state, pinned, role, profile) {
        send_error(out_tx, code, message).await;
        return false;
    }
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
        return false;
    };
    refresh_registry_cwds(state).await;
    let same_session = is_same_session_reattach(state, client_id, session);
    let Some((snapshot, initial_client_id, panes, closed, _publication_guard)) =
        prepare_attach_or_refuse(
            state,
            client_id,
            session,
            out_tx,
            client_caps,
            profile,
            bootstrap_limits,
        )
        .await
    else {
        return false;
    };
    fence_previous(state, client_id, &panes, client_caps, same_session).await;
    apply_client_default_colors(&panes, client_caps.default_colors).await;
    apply_attach_viewport(state, client_id, &panes, viewport);
    let publication = AttachPublication {
        state,
        out_tx,
        cancellation: connection_token,
        client_id,
        attach_id,
    };
    let session_name = state
        .with(|s| s.registry().session(session).map(|s| s.name.clone()))
        .unwrap_or_default();
    let (release, gate) = tokio::sync::watch::channel(false);
    let mut staging = SessionStaging::default();
    // Only Terminal supports deferred STREAM_BIND. Other apps still capture in
    // this transaction and their control-stream output shares ATTACH_READY.
    let captured_panes = panes
        .iter()
        .filter(|pane| {
            !defer_subscription || !apps::app_for(pane.handle.kind).defers_stream_bootstrap()
        })
        .cloned()
        .collect();
    let capture = SessionCapture {
        state,
        out_tx,
        connection_token,
        live_gate: gate,
        client_id,
        client_caps,
        stream_id: stream_id_from(u64::from(attach_id)),
        bootstrap_id: initial_bootstrap_id(),
        profile,
        limits: bootstrap_limits,
        scrollback: request_scrollback.then_some(scrollback_limit_lines),
    };
    let started = std::time::Instant::now();
    let result = tokio::select! {
        biased;
        () = connection_token.cancelled() => Err("connection cancelled during ATTACH capture".to_owned()),
        result = capture.capture(&mut staging, captured_panes, closed) => result,
    };
    crate::perf::ATTACH_CAPTURE_WALL.record_elapsed(started);
    if let Err(reason) = result {
        fail_aggregate_attach_prepublication(
            state,
            client_id,
            attach_id,
            out_tx,
            connection_token,
            &staging.handles,
            &mut staging.pumps,
            output_pumps,
            &reason,
        )
        .await;
        return false;
    }
    if same_session {
        super::connections::abort_output_pumps(output_pumps, client_id, "replacement ATTACH").await;
    }
    let mut pumps = staging.pumps;
    output_pumps.spawn_local(async move { while pumps.join_next().await.is_some() {} });
    let started = std::time::Instant::now();
    let published = publication
        .publish(snapshot, initial_client_id, staging.frames, &session_name)
        .await;
    crate::perf::ATTACH_PUBLISH_WALL.record_elapsed(started);
    if published {
        apply_session_role(state, client_id, &panes, role, same_session).await;
        let _ = release.send(true);
    }
    published
}
