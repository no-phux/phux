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

use super::apps::{self, terminal};
use super::pump::initial_bootstrap_id;
use super::{send_error, stream_id_from};
use crate::state::{AttachSnapshotPane, ClientId, Outbound, SharedState};
use prepare::{is_same_session_reattach, prepare_attach_or_refuse};
use publication::AttachPublication;
pub(in crate::runtime) use publication::publication_phase;
use staging::{CaptureFailure, SessionStaging};
use target::resolve_attach_target;

pub(crate) mod prepare;
mod publication;
pub(crate) mod staging;
mod target;
#[cfg(test)]
mod tests;

/// One session `ATTACH`'s context around staging.
#[derive(Clone, Copy)]
struct SessionAttach<'a> {
    state: &'a SharedState,
    client_id: ClientId,
    client_caps: ClientCapabilities,
    /// The client re-attaches the session it already had.
    same_session: bool,
}

/// The immutable generation and remaining resource ceilings lent to an app.
#[derive(Clone)]
pub(crate) struct SessionCapture<'a> {
    pub(crate) lease: apps::StreamLease,
    pub(crate) out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    pub(crate) live_gate: tokio::sync::watch::Receiver<bool>,
    pub(crate) client_id: ClientId,
    pub(crate) client_caps: ClientCapabilities,
    pub(crate) stream_id: StreamId,
    pub(crate) bootstrap_id: BootstrapId,
    pub(crate) profile: Option<BootstrapProfile>,
    pub(crate) limits: BootstrapLimits,
    pub(crate) scrollback: Option<u32>,
}

async fn capture_panes<'a>(
        make: impl Fn(apps::StreamLease) -> SessionCapture<'a>,
        staging: &mut SessionStaging,
        panes: Vec<(AttachSnapshotPane, apps::StreamLease)>,
        mut closed: Vec<phux_protocol::ids::ResourceId>,
    ) -> Result<(), String> {
        closed
            .try_reserve(panes.len())
            .map_err(|_| "host allocation failed".to_owned())?;
        for (pane, lease) in panes {
            let wire = pane.wire_terminal_id.clone();
            match apps::app_for(pane.handle.kind)
                .stage_session(make(lease), staging, pane)
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

/// Fence the client's earlier streams of `panes`, then prepare Terminal
/// consumers, palettes and sizes before staging.
async fn prepare_panes(
    attach: SessionAttach<'_>,
    panes: &[AttachSnapshotPane],
    viewport: ViewportInfo,
    reservations: &[apps::StreamReservation],
) -> bool {
    // Single-resource pumps are not owned by the aggregate JoinSet. Fence all
    // prior paths, not just Terminal ticks, before staging their replacements.
    for reservation in reservations {
        if !reservation.ready().await { return false; }
    }
    terminal::attach::prepare_consumers(
        panes,
        attach.client_caps,
    )
    .await;
    if viewport.cols == 0 || viewport.rows == 0 {
        return true;
    }
    apps::StreamHost::new(attach.state, attach.client_id).apply_viewport(panes, viewport);
    true
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
    futures_util::FutureExt::now_or_never(prepare_panes(attach, panes, viewport, &[]))
        .expect("a first attach with no palette prepares without waiting");
}

/// Record the declared session role and apply it to Terminal leases.
async fn apply_role(attach: SessionAttach<'_>, panes: &[AttachSnapshotPane], role: RolePolicy) {
    apps::StreamHost::new(attach.state, attach.client_id)
        .apply_session_role(panes, role, attach.same_session)
        .await;
}

/// Retire only this unpublished cohort. Mailbox capacity is awaited before
/// the final exact-token cleanup, so an older failure cannot detach a successor.
async fn fail_prepublication(
    host: &apps::StreamHost,
    attach_id: u32,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    connection: &CancellationToken,
    staged: &mut JoinSet<()>,
    reason: &str,
    publication: &CancellationToken,
) {
    staged.abort_all();
    while staged.join_next().await.is_some() {}
    let permit = tokio::select! {
        biased;
        () = publication.cancelled() => return,
        () = connection.cancelled() => return,
        result = tokio::time::timeout(std::time::Duration::from_secs(1), out_tx.reserve()) => result.ok().and_then(Result::ok),
    };
    let fatal = permit.map(|permit| (permit, Outbound::FatalError {
        request_id: None,
        code: ErrorCode::CodecUnavailable,
        message: format!("ATTACH {attach_id} failed before publication: {reason}"),
    }));
    host.abort_pending(publication, connection, fatal);
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
    terminal::attach::refresh_registry_cwds(state).await;
    let same_session = is_same_session_reattach(state, client_id, session);
    let Some((snapshot, initial_client_id, panes, closed, publication_guard)) =
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
    let publication_token = publication_guard.disarm();
    let _publication_guard = publication_token.clone().drop_guard();
    let host = apps::StreamHost::new(state, client_id);
    let Ok(reservations) = host.reserve_session(&panes, initial_bootstrap_id(), connection_token, &publication_token) else { return false; };
    let prepared = tokio::select! {
        biased;
        () = publication_token.cancelled() => false,
        () = connection_token.cancelled() => false,
        result = prepare_panes(attach, &panes, viewport, &reservations) => result,
    };
    if !prepared { return false; }
    let publication = AttachPublication {
        state,
        out_tx,
        cancellation: &publication_token,
        connection: connection_token,
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
        .iter().zip(&reservations)
        .filter(|(pane, _)| !defer_subscription || !apps::defers_stream_bootstrap(pane.handle.kind))
        .map(|(pane, reservation)| (pane.clone(), reservation.lease.clone()))
        .collect();
    let capture = |lease| SessionCapture {
        lease,
        out_tx,
        live_gate: gate.clone(),
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
        () = publication_token.cancelled() => Err("ATTACH superseded during capture".to_owned()),
        result = capture_panes(capture, &mut staging, captured_panes, closed) => result,
    };
    crate::perf::ATTACH_CAPTURE_WALL.record_elapsed(started);
    if let Err(reason) = result {
        if publication_token.is_cancelled() || connection_token.is_cancelled() { return false; }
        fail_prepublication(
            &host,
            attach_id,
            out_tx,
            connection_token,
            &mut staging.pumps,
            &reason,
            &publication_token,
        )
        .await;
        return false;
    }
    let started = std::time::Instant::now();
    let published = publication
        .publish(snapshot, initial_client_id, staging.frames, &session_name)
        .await;
    crate::perf::ATTACH_PUBLISH_WALL.record_elapsed(started);
    if published {
        tokio::select! {
            biased;
            () = publication_token.cancelled() => return false,
            () = connection_token.cancelled() => return false,
            () = apply_role(attach, &panes, role) => {},
        }
        if !host.publish_session(reservations, &publication_token, connection_token, || release.send(true).is_ok()) { return false; }
        let mut pumps = staging.pumps;
        while output_pumps.try_join_next().is_some() {}
        output_pumps.spawn_local(async move { while pumps.join_next().await.is_some() {} });
    } else { host.abort_pending(&publication_token, connection_token, None); }
    published
}
