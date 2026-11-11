//! A session `ATTACH`'s target: the session it joins, created for a
//! `CreateIfMissing` by the app that seeds sessions.

use phux_protocol::wire::frame::{AttachTarget, ErrorCode};
use tokio_util::sync::CancellationToken;

use super::super::apps::{self, SessionSeed};
use super::super::send_error;
use crate::state::{Outbound, SharedState};

/// Resolve `target` to the session the attach joins: the one the dispatch
/// guard authorized, pinned by id before the first await, or the one a
/// `CreateIfMissing` creates. No name is re-resolved after an await, so a
/// concurrent rename cannot redirect the attach (workload-auth §5).
pub(super) async fn resolve_attach_target(
    state: &SharedState,
    target: AttachTarget,
    pinned: Option<phux_core::ids::SessionId>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) -> Option<phux_core::ids::SessionId> {
    if pinned.is_some() {
        return pinned;
    }
    let refusal = match target {
        AttachTarget::ByName(name) => format!("session {name:?} not found"),
        AttachTarget::ById(id) => format!("session id {} not found", id.get()),
        AttachTarget::Last => "AttachTarget::Last has no live session to resolve".to_owned(),
        AttachTarget::CreateIfMissing { name, command, cwd } => {
            return create_attach_session(
                state,
                name,
                command,
                cwd,
                out_tx,
                root_token,
                default_colors,
            )
            .await;
        }
        _ => "unknown AttachTarget variant".to_owned(),
    };
    send_error(out_tx, ErrorCode::SessionNotFound, &refusal).await;
    None
}

/// `CreateIfMissing` whose session did not exist: create it, then pin the
/// id it was created under.
async fn create_attach_session(
    state: &SharedState,
    name: String,
    command: Option<Vec<String>>,
    cwd: Option<String>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) -> Option<phux_core::ids::SessionId> {
    let seed = SessionSeed {
        state,
        name,
        command,
        cwd,
        out_tx,
        root_token,
        default_colors,
    };
    let Some(seeding) = apps::seed_session(seed) else {
        send_error(
            out_tx,
            ErrorCode::SessionNotFound,
            "CreateIfMissing: this server seeds no session",
        )
        .await;
        return None;
    };
    let name = seeding.await?;
    let created = state.with(|s| s.find_session_by_name(&name));
    if created.is_none() {
        send_error(
            out_tx,
            ErrorCode::SessionNotFound,
            &format!("session {name:?} not found"),
        )
        .await;
    }
    created
}
