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

use super::pump::initial_bootstrap_id;
use super::{apps, send_error, stream_id_from};
use crate::resource::ResourceHandle;
use crate::state::{AttachSnapshotPane, ClientId, Outbound, SharedState};
use apps::resolve_attach_target;
use prepare::{is_same_session_reattach, prepare_attach_or_refuse};
use publication::AttachPublication;
use staging::{CaptureFailure, SessionStaging};

pub(crate) mod prepare;
mod publication;
pub(crate) mod staging;
#[cfg(test)]
mod tests;

/// One session `ATTACH`, as the apps see it around staging.
#[derive(Clone, Copy)]
pub(crate) struct SessionAttach<'a> {
    pub(crate) state: &'a SharedState,
    pub(crate) client_id: ClientId,
    pub(crate) client_caps: ClientCapabilities,
    /// The client re-attaches the session it already had.
    pub(crate) same_session: bool,
}

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
    pinned.is_none_or(|session| state.with(|s| session_needs_terminal_profile(s, session)))
}

/// Whether some member of `session` streams in the connection's Terminal
/// profile, as its kind's catalog stream contract says.
pub(crate) fn session_needs_terminal_profile(
    s: &crate::state::ServerState,
    session: phux_core::ids::SessionId,
) -> bool {
    let registry = s.registry();
    registry.members(session).into_iter().any(|id| {
        registry.resource(id).is_some_and(|resource| {
            phux_protocol::kinds::needs_terminal_profile(crate::id_bridge::wire_kind(resource.kind))
        })
    })
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
    if requires_terminal_profile(state, pinned) && profile.is_none() {
        return Some((ErrorCode::CodecUnavailable, apps::NO_TERMINAL_PROFILE));
    }
    None
}

/// Fence the client's earlier streams of `panes`, then let each kind's app
/// fence its own consumers and apply the client's presentation and viewport.
async fn prepare_panes(
    attach: SessionAttach<'_>,
    panes: &[AttachSnapshotPane],
    viewport: ViewportInfo,
) {
    // Single-resource pumps are not owned by the aggregate JoinSet. Fence all
    // prior paths, not just Terminal ticks, before staging their replacements.
    for pane in panes {
        super::pump::stop_output(attach.state, attach.client_id, pane.terminal_id).await;
    }
    let by_kind = apps::by_kind(panes, |pane| pane.handle.kind);
    for (app, of_kind) in &by_kind {
        app.prepare_attach(attach, of_kind).await;
    }
    if viewport.cols == 0 || viewport.rows == 0 {
        return;
    }
    attach.state.with_mut(|s| {
        s.set_client_viewport(attach.client_id, viewport);
        for (app, of_kind) in &by_kind {
            app.attach_viewport(s, of_kind, viewport);
        }
    });
}

/// [`prepare_panes`] for a first attach with no palette, driven to
/// completion: the geometry tests' path to the production viewport steps.
#[cfg(test)]
pub(crate) fn prepare_panes_now(
    state: &SharedState,
    client_id: ClientId,
    panes: &[AttachSnapshotPane],
    viewport: ViewportInfo,
) {
    let attach = SessionAttach {
        state,
        client_id,
        client_caps: ClientCapabilities::default(),
        same_session: false,
    };
    // Nothing in this path awaits an actor: no pump to fence, no prior
    // consumer, no palette to apply.
    futures_util::FutureExt::now_or_never(prepare_panes(attach, panes, viewport))
        .expect("a first attach with no palette prepares without waiting");
}

/// Record the declared role, then let each kind's app apply it.
async fn apply_role(attach: SessionAttach<'_>, panes: &[AttachSnapshotPane], role: RolePolicy) {
    attach
        .state
        .with_mut(|s| s.set_attached_viewer(attach.client_id, role.is_viewer()));
    for (app, of_kind) in apps::by_kind(panes, |pane| pane.handle.kind) {
        app.after_attach(attach, &of_kind, role).await;
    }
}

/// Roll back a session `ATTACH` that failed before publication: stop every
/// staged and committed pump, let each kind release its consumers, drop the
/// client's consumer state, then end the connection with a fatal error.
#[allow(
    clippy::too_many_arguments,
    reason = "single rollback boundary deliberately receives every staged and committed resource so cancellation, producer detach, pump abortion, and the fatal sentinel remain strictly ordered"
)]
async fn fail_prepublication(
    state: &SharedState,
    client_id: ClientId,
    attach_id: u32,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    connection_token: &CancellationToken,
    staged_handles: &[ResourceHandle],
    staged_pumps: &mut JoinSet<()>,
    committed_pumps: &mut JoinSet<()>,
    reason: &str,
) {
    staged_pumps.abort_all();
    while staged_pumps.join_next().await.is_some() {}
    super::connections::abort_output_pumps(committed_pumps, client_id, "failed ATTACH").await;
    for (app, of_kind) in apps::by_kind(staged_handles, |handle| handle.kind) {
        app.abort_attach(client_id, &of_kind).await;
    }
    super::connections::detach_and_release_consumer_state(state, client_id);

    // The writer closes right after this terminal ERROR, discarding anything
    // a surviving producer races in behind it.
    let deadline = std::time::Duration::from_secs(1);
    if !matches!(
        tokio::time::timeout(
            deadline,
            out_tx.send(Outbound::FatalError {
                request_id: None,
                code: ErrorCode::CodecUnavailable,
                message: format!("ATTACH {attach_id} failed before publication: {reason}"),
            }),
        )
        .await,
        Ok(Ok(()))
    ) {
        tracing::warn!(client = ?client_id, attach_id, "failed to enqueue terminal ATTACH error");
    }
    connection_token.cancel();
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
    apps::refresh_for_attach(state).await;
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
    let attach = SessionAttach {
        state,
        client_id,
        client_caps,
        same_session,
    };
    prepare_panes(attach, &panes, viewport).await;
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
        fail_prepublication(
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
        apply_role(attach, &panes, role).await;
        let _ = release.send(true);
    }
    published
}
