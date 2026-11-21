//! `ATTACH` target resolution and inherited working directories.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;
use phux_terminal_api::actor::TerminalResource as _;

/// Handle [`AttachTarget::CreateIfMissing`]: seed `name` as an
/// ephemeral session when it does not exist. A server-wide seed command wins
/// over the wire `command`; a stale wire `cwd` falls back. Spawn failure is
/// `SessionNotFound`.
pub(crate) async fn resolve_create_if_missing(
    seed: crate::runtime::apps::SessionSeed<'_>,
) -> Option<String> {
    let crate::runtime::apps::SessionSeed {
        state,
        name,
        command,
        cwd,
        out_tx,
        root_token,
        default_colors,
    } = seed;
    if state.with(|s| s.sessions().by_name(&name).is_some()) {
        debug!(session = %name, "CreateIfMissing: session already exists, attaching");
        return Some(name);
    }

    let (with_pty, scrollback, term) = state.with(|s| {
        (
            s.config().attach_create_seeds_pty,
            s.config().scrollback,
            s.config().term.as_str().to_owned(),
        )
    });

    // `cwd-inheritance = "fixed"` places a session the request left unplaced.
    let cwd = cwd.or_else(|| state.with(|s| s.config().fixed_cwd.clone()));
    let seed_result = if with_pty {
        let mut seed_cmd = spawn_argv_builder(state, command);
        phux_terminal_api::launch::apply_spawn_cwd(&mut seed_cmd, cwd.as_deref(), &name);
        phux_terminal_api::launch::apply_term(&mut seed_cmd, &term);
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
        crate::runtime::apps::terminal::commands::seed::seed_session_with_actor_as(
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
    match state.with(|s| s.config().cwd_inheritance) {
        phux_config::CwdInheritance::InheritFocused => focused_pane_cwd(state, client_id).await,
        phux_config::CwdInheritance::Home => std::env::var("HOME").ok().filter(|h| !h.is_empty()),
        phux_config::CwdInheritance::SessionRoot => session_root_cwd(state, client_id).await,
        phux_config::CwdInheritance::LastCwdPerWindow => last_window_cwd(state, client_id).await,
        phux_config::CwdInheritance::Fixed => state.with(|s| s.config().fixed_cwd.clone()),
    }
}

/// The live PTY CWD of the spawning client's focused pane.
async fn focused_pane_cwd(state: &SharedState, client_id: ClientId) -> Option<String> {
    let handle = state.with(|s| {
        let session = s.attached().get(&client_id)?.session;
        let focused = s.sessions().default_pane_of(session)?;
        s.resources().handle(focused).cloned()
    })?;
    query_pane_cwd(handle).await
}

/// The session's creation directory: the seed pane's CWD, frozen on first
/// observation so a later `cd` does not move it.
async fn session_root_cwd(state: &SharedState, client_id: ClientId) -> Option<String> {
    let (session, handle) = state.with(|s| {
        let session = s.attached().get(&client_id)?.session;
        if let Some(root) = s.sessions().root(session) {
            return Some((session, FrozenOrQuery::Frozen(path_to_string(root)?)));
        }
        let seed = s.sessions().seed_pane_of(session)?;
        let handle = s.resources().handle(seed).cloned()?;
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
            .sessions()
            .default_pane_of(session)
            .and_then(|p| s.resources().handle(p).cloned());
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
    state.with(|s| {
        s.sessions()
            .last_cwd(session)
            .and_then(|p| path_to_string(p))
    })
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

    let handles: Vec<(ResourceId, ResourceHandle)> = state.with(|s| s.resources().all_handles());
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
