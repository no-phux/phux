//! `ATTACH` target resolution and inherited working directories.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;
use phux_terminal_api::actor::TerminalResource as _;

/// A new pane's working directory from `defaults.cwd-inheritance` when the
/// spawn left `cwd` unset; `None` starts in the home directory. The spawn's
/// `owner_terminal` stands in for the spawning client's focused pane.
pub(crate) async fn resolve_inherited_cwd(
    state: &SharedState,
    client_id: ClientId,
    owner_terminal: Option<&phux_protocol::ids::ResourceId>,
) -> Option<String> {
    let mode = state.with(|s| s.config().cwd_inheritance);
    match mode {
        phux_config::CwdInheritance::Home => {
            return std::env::var("HOME").ok().filter(|h| !h.is_empty());
        }
        phux_config::CwdInheritance::Fixed => return state.with(|s| s.config().fixed_cwd.clone()),
        phux_config::CwdInheritance::InheritFocused
        | phux_config::CwdInheritance::SessionRoot
        | phux_config::CwdInheritance::LastCwdPerWindow => {}
    }
    let (session, pane) = state.with(|s| {
        if let Some(owner) = owner_terminal {
            let session = s.session_of_terminal_owner(owner)?;
            return Some((session, s.idspace.resource_from_wire(owner)));
        }
        let session = s.attached().get(&client_id)?.session;
        Some((session, s.sessions().default_pane_of(session)))
    })?;
    let handle = pane.and_then(|pane| state.with(|s| s.resources().handle(pane).cloned()));
    match mode {
        phux_config::CwdInheritance::SessionRoot => session_root_cwd(state, session).await,
        phux_config::CwdInheritance::LastCwdPerWindow => {
            last_window_cwd(state, session, handle).await
        }
        _ => query_pane_cwd(handle?).await,
    }
}

/// The session's creation directory: the seed pane's CWD, frozen on first
/// observation so a later `cd` does not move it.
async fn session_root_cwd(
    state: &SharedState,
    session: phux_core::ids::SessionId,
) -> Option<String> {
    let handle = state.with(|s| {
        if let Some(root) = s.sessions().root(session) {
            return Some(FrozenOrQuery::Frozen(path_to_string(root)?));
        }
        let seed = s.sessions().seed_pane_of(session)?;
        let handle = s.resources().handle(seed).cloned()?;
        Some(FrozenOrQuery::Query(handle))
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

/// The session's CWD: live from the pane `handle`, else the last value
/// recorded for the session.
async fn last_window_cwd(
    state: &SharedState,
    session: phux_core::ids::SessionId,
    handle: Option<ResourceHandle>,
) -> Option<String> {
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
