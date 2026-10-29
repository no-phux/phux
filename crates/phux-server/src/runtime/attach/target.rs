//! `ATTACH` target resolution and inherited working directories.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;

/// What `handle_attach` needs after leaving the state lock: the snapshot, the
/// initial client id, the panes to bootstrap, and snapshot participants with
/// no actor (published as `RESOURCE_CLOSED` before `ATTACH_READY`).
pub(crate) type AttachPrepared = (
    phux_protocol::wire::info::SessionSnapshot,
    phux_protocol::ids::ClientId,
    Vec<AttachSnapshotPane>,
    Vec<phux_protocol::ids::ResourceId>,
);

/// Resolve `target` to the session the attach joins: the one the dispatch
/// guard authorized, pinned by id before the first await, or the one a
/// `CreateIfMissing` creates. No name is re-resolved after an await, so a
/// concurrent rename cannot redirect the attach (workload-auth §5).
pub(crate) async fn resolve_attach_target(
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
    let name = resolve_create_if_missing(
        state,
        name,
        command,
        cwd,
        out_tx,
        root_token,
        default_colors,
    )
    .await?;
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

/// Handle [`AttachTarget::CreateIfMissing`] (SPEC §13): return `name` when
/// the session exists, otherwise seed it as an ephemeral session (PTY-backed
/// when the server runs with PTYs) and return `name` for the normal attach
/// path.
///
/// With PTYs, a server-wide seed command wins over the wire `command`, and
/// the wire `cwd` applies only when it names an existing directory (a stale
/// client path falls back rather than failing the attach). Spawn failure is
/// reported as `SessionNotFound`: the session is not available to attach.
pub(crate) async fn resolve_create_if_missing(
    state: &SharedState,
    name: String,
    command: Option<Vec<String>>,
    cwd: Option<String>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) -> Option<String> {
    if state.with(|s| s.session_by_name(&name).is_some()) {
        debug!(session = %name, "CreateIfMissing: session already exists, attaching");
        return Some(name);
    }

    let (with_pty, override_cmd, scrollback, term) = state.with(|s| {
        (
            s.attach_create_seeds_pty(),
            s.attach_create_seed_command(),
            s.scrollback_limits(),
            s.term().to_owned(),
        )
    });

    let seed_result = if with_pty {
        let mut seed_cmd = override_cmd.unwrap_or_else(|| spawn_argv_builder(state, command));
        crate::terminal_actor::apply_spawn_cwd(&mut seed_cmd, cwd.as_deref(), &name);
        crate::terminal_actor::apply_term(&mut seed_cmd, &term);
        seed_session_with_pty_and_colors(
            state,
            &name,
            seed_cmd,
            scrollback,
            root_token,
            default_colors,
            phux_core::SessionLifetime::Ephemeral,
        )
    } else {
        // No child to exec a wire `command` on.
        crate::runtime::commands::seed_session_with_actor_as(
            state,
            &name,
            scrollback,
            root_token,
            phux_core::SessionLifetime::Ephemeral,
        )
    };

    if let Err(err) = seed_result {
        warn!(
            session = %name,
            error = %err,
            "CreateIfMissing: failed to spawn pane actor for newly-created session",
        );
        send_error(
            out_tx,
            ErrorCode::SessionNotFound,
            &format!("CreateIfMissing: failed to create session {name:?}: {err}"),
        )
        .await;
        return None;
    }

    debug!(
        session = %name,
        pty = with_pty,
        "CreateIfMissing: created session and seeded pane"
    );
    Some(name)
}

/// A new pane's working directory from `defaults.cwd-inheritance` when the
/// spawn left `cwd` unset; `None` inherits the server's CWD.
pub(crate) async fn resolve_inherited_cwd(
    state: &SharedState,
    client_id: ClientId,
) -> Option<String> {
    match state.with(crate::state::ServerState::cwd_inheritance) {
        phux_config::CwdInheritance::InheritFocused => focused_pane_cwd(state, client_id).await,
        phux_config::CwdInheritance::Home => std::env::var("HOME").ok().filter(|h| !h.is_empty()),
        phux_config::CwdInheritance::SessionRoot => session_root_cwd(state, client_id).await,
        phux_config::CwdInheritance::LastCwdPerWindow => last_window_cwd(state, client_id).await,
    }
}

/// The live PTY CWD of the spawning client's focused pane.
async fn focused_pane_cwd(state: &SharedState, client_id: ClientId) -> Option<String> {
    let handle = state.with(|s| {
        let session = s.attached().get(&client_id)?.session;
        let focused = s.active_pane_of_session(session)?;
        s.resource_handle(focused).cloned()
    })?;
    query_pane_cwd(handle).await
}

/// The session's creation directory: the seed pane's CWD, frozen on first
/// observation so a later `cd` does not move it.
async fn session_root_cwd(state: &SharedState, client_id: ClientId) -> Option<String> {
    let (session, handle) = state.with(|s| {
        let session = s.attached().get(&client_id)?.session;
        if let Some(root) = s.session_root(session) {
            return Some((session, FrozenOrQuery::Frozen(path_to_string(root)?)));
        }
        let seed = s.seed_pane_of_session(session)?;
        let handle = s.resource_handle(seed).cloned()?;
        Some((session, FrozenOrQuery::Query(handle)))
    })?;
    match handle {
        FrozenOrQuery::Frozen(root) => Some(root),
        FrozenOrQuery::Query(handle) => {
            let resolved = query_pane_cwd(handle).await?;
            // A racing spawn may have frozen a root first; keep that one.
            let frozen = state.with_mut(|s| {
                path_to_string(s.record_session_root(session, std::path::PathBuf::from(&resolved)))
            });
            frozen.or(Some(resolved))
        }
    }
}

/// The spawning client's session CWD: live from its default pane, else the
/// last value recorded for the session.
async fn last_window_cwd(state: &SharedState, client_id: ClientId) -> Option<String> {
    let (session, handle) = state.with(|s| {
        let session = s.attached().get(&client_id)?.session;
        let handle = s
            .active_pane_of_session(session)
            .and_then(|p| s.resource_handle(p).cloned());
        Some((session, handle))
    })?;
    let resolved = match handle {
        Some(handle) => query_pane_cwd(handle).await,
        None => None,
    };
    if let Some(cwd) = resolved {
        state.with_mut(|s| {
            s.record_session_last_cwd(session, std::path::PathBuf::from(&cwd));
        });
        return Some(cwd);
    }
    state.with(|s| s.session_last_cwd(session).and_then(|p| path_to_string(p)))
}

/// A frozen session root, or the handle to query for one off-lock.
enum FrozenOrQuery {
    Frozen(String),
    Query(ResourceHandle),
}

/// `path` as UTF-8; a non-UTF-8 directory yields no override.
fn path_to_string(path: &std::path::Path) -> Option<String> {
    path.to_str().map(ToOwned::to_owned)
}

/// Ask `handle`'s Terminal engine for its PTY child's live CWD. `None` for a
/// non-Terminal, a gone actor, or an unsupported query.
pub(crate) async fn query_pane_cwd(handle: ResourceHandle) -> Option<String> {
    let terminal = handle.terminal().ok()?;
    let (reply, rx) = tokio::sync::oneshot::channel();
    terminal.pwd.send(PwdRequest { reply }).await.ok()?;
    rx.await.ok().flatten()
}

/// Refresh every pane's registry `cwd` from its PTY child before the
/// `ATTACHED` snapshot, since the spawn-time stamp goes stale on `cd`.
/// Best-effort and concurrent; replies that miss the deadline keep their
/// stamped value, so a wedged actor cannot stall `ATTACHED`.
pub(crate) async fn refresh_registry_cwds(state: &SharedState) {
    const CWD_REFRESH_DEADLINE: std::time::Duration = std::time::Duration::from_millis(250);

    let handles: Vec<(ResourceId, ResourceHandle)> =
        state.with(crate::state::ServerState::all_resource_handles);
    if handles.is_empty() {
        return;
    }
    let mut queries: FuturesUnordered<_> = handles
        .into_iter()
        .map(|(id, handle)| async move { (id, query_pane_cwd(handle).await) })
        .collect();
    let mut resolved: Vec<(ResourceId, std::path::PathBuf)> = Vec::new();
    let drain = async {
        while let Some((id, cwd)) = queries.next().await {
            if let Some(cwd) = cwd {
                resolved.push((id, std::path::PathBuf::from(cwd)));
            }
        }
    };
    if tokio::time::timeout(CWD_REFRESH_DEADLINE, drain)
        .await
        .is_err()
    {
        debug!("attach cwd refresh hit deadline; using stamped values for stragglers");
    }
    if resolved.is_empty() {
        return;
    }
    state.with_mut(|s| {
        for (id, cwd) in resolved {
            if let Some(desc) = s.registry_mut().terminal_mut(id) {
                desc.cwd = cwd;
            }
        }
    });
}
