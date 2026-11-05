//! Membership, policy filtering and preflight in one state transaction.

use super::{
    AttachSnapshotPane, BootstrapLimits, BootstrapProfile, ClientCapabilities, ClientId, ErrorCode,
    Outbound, SharedState, apps, bootstrap_stream_profile, send_error, staging,
};
use crate::state::AttachError;
use std::collections::HashSet;

pub(crate) type AttachPrepared = (
    phux_protocol::wire::info::SessionSnapshot,
    phux_protocol::ids::ClientId,
    Vec<AttachSnapshotPane>,
    Vec<phux_protocol::ids::ResourceId>,
    tokio_util::sync::DropGuard,
);

pub(super) fn is_same_session_reattach(
    state: &SharedState,
    client: ClientId,
    session: phux_core::ids::SessionId,
) -> bool {
    state.with(|s| {
        s.attached()
            .get(&client)
            .is_some_and(|attached| attached.session == session)
    })
}

/// Preflight before registration, then capture the authorized session membership
/// and authoritative closures, regardless of whether a resource has a window.
pub(crate) fn prepare_attach(
    state: &SharedState,
    client_id: ClientId,
    session: phux_core::ids::SessionId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    client_caps: ClientCapabilities,
    bootstrap_profile: impl Into<Option<BootstrapProfile>>,
    bootstrap_limits: BootstrapLimits,
) -> Result<AttachPrepared, AttachError> {
    let bootstrap_profile = bootstrap_profile.into();
    state.with_mut(|s| {
        let session_name = s
            .registry()
            .session(session)
            .map(|found| found.name.clone())
            .ok_or_else(|| AttachError::UnknownSession(format!("{session:?}")))?;
        if bootstrap_profile
            .and_then(bootstrap_stream_profile)
            .is_none()
            && !s.session_terminals(session).is_empty()
        {
            return Err(AttachError::TerminalProfileUnavailable);
        }
        if s.session_resources(session).len() > staging::MAX_AGGREGATE_BOOTSTRAP_PANES {
            return Err(AttachError::ResourceLimit);
        }
        let sid = s.attach(
            client_id,
            &session_name,
            out_tx.clone(),
            client_caps,
            bootstrap_profile,
            bootstrap_limits,
        )?;
        s.touch_session(sid);
        let snapshot = s
            .build_session_snapshot(sid)
            .ok_or_else(|| AttachError::UnknownSession(session_name.clone()))?;
        let snapshot = crate::policy::filter::filter_snapshot(
            s,
            client_id,
            phux_protocol::kinds::Verb::Observe,
            snapshot,
        );
        let authorized: HashSet<_> = snapshot
            .resources
            .iter()
            .map(|resource| resource.id.clone())
            .collect();
        let panes: Vec<_> = s
            .attach_snapshot_panes(sid)
            .into_iter()
            .filter(|pane| authorized.contains(&pane.wire_terminal_id))
            .collect();
        let bootstrapped: HashSet<_> = panes
            .iter()
            .map(|pane| pane.wire_terminal_id.clone())
            .collect();
        let closed = snapshot
            .resources
            .iter()
            .filter(|resource| {
                resource.session == Some(snapshot.focused_session)
                    && !bootstrapped.contains(&resource.id)
            })
            .map(|resource| resource.id.clone())
            .collect();
        let publication = s.begin_attach_publication(client_id);
        Ok((
            snapshot,
            super::super::wire_client(client_id),
            panes,
            closed,
            publication,
        ))
    })
}

pub(super) async fn prepare_attach_or_refuse(
    state: &SharedState,
    client: ClientId,
    session: phux_core::ids::SessionId,
    out: &tokio::sync::mpsc::Sender<Outbound>,
    caps: ClientCapabilities,
    profile: Option<BootstrapProfile>,
    limits: BootstrapLimits,
) -> Option<AttachPrepared> {
    let error = match prepare_attach(state, client, session, out, caps, profile, limits) {
        Ok(prepared) => return Some(prepared),
        Err(error) => error,
    };
    let (code, message) = match error {
        AttachError::UnknownSession(name) => (
            ErrorCode::SessionNotFound,
            format!("session {name:?} not found"),
        ),
        AttachError::AlreadyAttached(_) => (
            ErrorCode::AlreadyAttached,
            "client is already attached".to_owned(),
        ),
        AttachError::ResourceLimit => (
            ErrorCode::CodecUnavailable,
            "session exceeds bounded aggregate attach limits".to_owned(),
        ),
        AttachError::TerminalProfileUnavailable => (
            ErrorCode::CodecUnavailable,
            apps::terminal::commands::NO_TERMINAL_PROFILE.to_owned(),
        ),
    };
    send_error(out, code, &message).await;
    None
}
