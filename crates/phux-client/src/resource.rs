//! Resource kinds as a client sees them in a `GET_STATE` snapshot (ADR-0102).
//!
//! The one place the client decides which entries are panes (Terminal-kind)
//! and which are `AgentSession` children bound to one, and the one join from
//! a resource to the session it belongs to ([`session_of`]).

use phux_protocol::ids::{ResourceId, ResourceKind, SessionId};
use phux_protocol::wire::frame::{CloseReason, ControlAction, ResourceLifecycle};
use phux_protocol::wire::info::{ExitFacet, ResourceInfo, SessionInfo, SessionSnapshot};

use crate::agent_session::AgentSessionError;
use crate::attach::AttachError;
use crate::attach::connection::Connection;

// The `phux resource` verbs: resumable exit wait, inspection, methods.
pub mod cursor;
pub mod methods;
pub mod show;
pub mod wait;

/// Why one resource could not be read.
#[derive(Debug, thiserror::Error)]
pub enum LookupError {
    /// The connection or a request failed.
    #[error(transparent)]
    Attach(#[from] AttachError),
    /// The resource is not in the server's inventory; with `unreachable`
    /// notices, absence is not proof it is gone.
    #[error("no such resource: {}", crate::selector::format_terminal_id(.resource))]
    NotFound {
        /// The resource that was looked up.
        resource: ResourceId,
        /// The hub's per-satellite degradation notices, if any.
        unreachable: Vec<String>,
    },
}

/// Read the server's inventory on `conn` and return `resource`'s entry, the
/// snapshot it came from, and any degradation notices.
pub(crate) async fn lookup_on(
    conn: &mut Connection,
    resource: &ResourceId,
) -> Result<(ResourceInfo, SessionSnapshot, Vec<String>), LookupError> {
    let (snapshot, degradation) = crate::state::get_state_on(conn).await?.into_parts();
    let notices = degradation.notices().to_vec();
    match find(&snapshot, resource).cloned() {
        Some(info) => Ok((info, snapshot, notices)),
        None => Err(LookupError::NotFound {
            resource: resource.clone(),
            unreachable: notices,
        }),
    }
}

/// The exit facet as the `--json` documents spell it.
#[must_use]
pub fn exit_facet_json(exit: &ExitFacet) -> serde_json::Value {
    serde_json::json!({
        "status": exit.exit_status,
        "signal": exit.signal,
        "reason": close_reason_name(exit.reason),
        "exited_at_ms": exit.exited_at_ms,
        "retained_until_ms": exit.retained_until_ms,
    })
}

/// The `snake_case` name of a lifecycle, as the `--json` documents spell it.
#[must_use]
pub const fn lifecycle_name(lifecycle: ResourceLifecycle) -> &'static str {
    match lifecycle {
        ResourceLifecycle::Running => "running",
        ResourceLifecycle::Frozen => "frozen",
        ResourceLifecycle::Exited => "exited",
    }
}

/// The `snake_case` name of a close reason (the `RESOURCE_CLOSED.reason`
/// vocabulary), or `None` when the server stated none.
#[must_use]
pub const fn close_reason_name(reason: CloseReason) -> Option<&'static str> {
    match reason {
        CloseReason::Exited => Some("exited"),
        CloseReason::Killed => Some("killed"),
        CloseReason::ParentClosed => Some("parent_closed"),
        CloseReason::ServerShutdown => Some("server_shutdown"),
        // `Unknown`, and any reason a newer protocol adds: stated as none.
        _ => None,
    }
}

/// The `snake_case` name of a supervisory action (`terminal_control`).
#[must_use]
pub const fn control_action_name(action: ControlAction) -> &'static str {
    match action {
        ControlAction::Acquired => "acquired",
        ControlAction::Seized => "seized",
        ControlAction::Released => "released",
        ControlAction::Interrupted => "interrupted",
        ControlAction::Frozen => "frozen",
        ControlAction::Resumed => "resumed",
        ControlAction::Terminated => "terminated",
        ControlAction::Killed => "killed",
        ControlAction::Exited => "exited",
        ControlAction::Expired => "expired",
        ControlAction::RoleChanged => "role_changed",
    }
}

/// The `snake_case` name of the Terminal kind, as `phux ls --json` spells it.
pub const TERMINAL: &str = "terminal";
/// The `snake_case` name of the `AgentSession` kind.
pub const AGENT_SESSION: &str = "agent_session";
/// The name rendered for a kind this binary does not know.
pub const UNKNOWN: &str = "unknown";

/// The `snake_case` name of `kind`.
#[must_use]
pub const fn kind_name(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Terminal => TERMINAL,
        ResourceKind::AgentSession => AGENT_SESSION,
        _ => UNKNOWN,
    }
}

/// Whether `info` is a Terminal-kind resource — a pane.
#[must_use]
pub fn is_terminal(info: &ResourceInfo) -> bool {
    info.kind == ResourceKind::Terminal
}

/// Whether `info` is an `AgentSession` resource.
#[must_use]
pub fn is_agent_session(info: &ResourceInfo) -> bool {
    info.kind == ResourceKind::AgentSession
}

/// The resource `id` names, of any kind.
#[must_use]
pub fn find<'a>(snapshot: &'a SessionSnapshot, id: &ResourceId) -> Option<&'a ResourceInfo> {
    snapshot.resources.iter().find(|info| info.id == *id)
}

/// The session `id` belongs to.
///
/// The snapshot's `session` facet, or, from a server that predates it, the
/// session of the legacy window the resource sits in. `None` for a resource
/// the snapshot does not list, or one whose session it cannot name (a
/// satellite resource behind a hub).
#[must_use]
pub fn session_of(snapshot: &SessionSnapshot, id: &ResourceId) -> Option<SessionId> {
    session_of_info(snapshot, find(snapshot, id)?)
}

/// The session's most recent input target as the server reports it, when it
/// is still a Terminal in the session.
///
/// The session-level `SESSION_STATE.recent_input` (`docs/spec/L1.md`
/// §9.1), else the session's legacy window's `active_resource` (ADR-0161
/// §7). The window is read whenever `recent_input` is absent, from any
/// server: a snapshot does not say whether its server reports
/// `recent_input`, and every server falls the window back to the session's
/// first Terminal, so a session with no recent input still resolves to that
/// Terminal here. Callers' own first-leaf fallbacks take over only when the
/// window read goes with the legacy windows (ADR-0161 §9 step 3).
#[must_use]
pub fn recent_terminal(snapshot: &SessionSnapshot, session: &SessionInfo) -> Option<ResourceId> {
    let in_session = |id: &ResourceId| {
        find(snapshot, id).is_some_and(is_terminal) && session_of(snapshot, id) == Some(session.id)
    };
    session
        .recent_input
        .clone()
        .filter(in_session)
        .or_else(|| legacy_window_focus(snapshot, session).filter(in_session))
}

/// The `active_resource` of the session's active legacy window, else of its
/// first one.
fn legacy_window_focus(snapshot: &SessionSnapshot, session: &SessionInfo) -> Option<ResourceId> {
    session
        .active_window
        .and_then(|window| snapshot.windows.iter().find(|w| w.id == window))
        .or_else(|| snapshot.windows.iter().find(|w| w.session_id == session.id))
        .and_then(|window| window.active_resource.clone())
}

/// Whether the snapshot's server reports each resource's session.
///
/// Some local resource carries the `RESOURCE_SESSION` facet (ADR-0161 §7).
/// Such a server's legacy windows are synthesized, one per non-empty
/// session, so a client reads nothing from them.
#[must_use]
pub fn reports_sessions(snapshot: &SessionSnapshot) -> bool {
    snapshot
        .resources
        .iter()
        .any(|info| info.id.is_local() && info.session.is_some())
}

/// [`session_of`] for a resource already in hand. A satellite resource's
/// `session` facet names a session on the satellite, never one of these.
fn session_of_info(snapshot: &SessionSnapshot, info: &ResourceInfo) -> Option<SessionId> {
    if !info.id.is_local() {
        return None;
    }
    info.session.or_else(|| {
        snapshot
            .windows
            .iter()
            .find(|window| window.id == info.window_id)
            .map(|window| window.session_id)
    })
}

/// Every resource of any kind that belongs to `session`, in snapshot order.
pub fn in_session(
    snapshot: &SessionSnapshot,
    session: SessionId,
) -> impl Iterator<Item = &ResourceInfo> {
    snapshot
        .resources
        .iter()
        .filter(move |info| session_of_info(snapshot, info) == Some(session))
}

/// Every Terminal-kind resource that belongs to `session`, in snapshot order.
pub fn terminals_in(
    snapshot: &SessionSnapshot,
    session: SessionId,
) -> impl Iterator<Item = &ResourceInfo> {
    in_session(snapshot, session).filter(|info| is_terminal(info))
}

/// Every Terminal-kind resource, in snapshot order.
pub fn terminals(snapshot: &SessionSnapshot) -> impl Iterator<Item = &ResourceInfo> {
    snapshot.resources.iter().filter(|info| is_terminal(info))
}

/// Every live `AgentSession` child of `parent`, in snapshot order.
pub fn children_of<'a>(
    snapshot: &'a SessionSnapshot,
    parent: &'a ResourceId,
) -> impl Iterator<Item = &'a ResourceInfo> + 'a {
    snapshot
        .resources
        .iter()
        .filter(move |info| is_agent_session(info) && info.parent.as_ref() == Some(parent))
}

/// The `AgentSession` a target resolves to: an `AgentSession` id names
/// itself; a Terminal names its unique live child.
///
/// # Errors
///
/// [`AgentSessionError::NoSession`] / [`AgentSessionError::AmbiguousSession`]
/// for a Terminal with no / several live children (an absent target counts
/// as a Terminal), [`AgentSessionError::WrongKind`] for any other kind.
pub fn agent_session_of(
    snapshot: &SessionSnapshot,
    target: &ResourceId,
) -> Result<ResourceId, AgentSessionError> {
    match find(snapshot, target) {
        Some(info) if is_agent_session(info) => Ok(target.clone()),
        Some(info) if !is_terminal(info) => Err(AgentSessionError::WrongKind {
            resource: target.clone(),
            expected: AGENT_SESSION,
            actual: kind_name(info.kind).to_owned(),
        }),
        _ => {
            let children: Vec<ResourceId> = children_of(snapshot, target)
                .map(|info| info.id.clone())
                .collect();
            match children.as_slice() {
                [] => Err(AgentSessionError::NoSession {
                    terminal: target.clone(),
                }),
                [only] => Ok(only.clone()),
                _ => Err(AgentSessionError::AmbiguousSession {
                    terminal: target.clone(),
                    candidates: children,
                }),
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;
    use phux_protocol::ids::{SessionId, WindowId};

    fn snapshot() -> SessionSnapshot {
        let window = WindowId::new(1);
        SessionSnapshot::new(SessionId::new(1), window, ResourceId::local(7)).with_resources(vec![
            ResourceInfo::new(ResourceId::local(7), window, 80, 24),
            ResourceInfo::new(ResourceId::local(8), window, 80, 24),
            ResourceInfo::new(ResourceId::local(9), window, 0, 0)
                .with_kind(ResourceKind::AgentSession)
                .with_parent(Some(ResourceId::local(7))),
            ResourceInfo::new(ResourceId::local(10), window, 0, 0)
                .with_kind(ResourceKind::AgentSession)
                .with_parent(Some(ResourceId::local(8))),
            ResourceInfo::new(ResourceId::local(11), window, 0, 0)
                .with_kind(ResourceKind::AgentSession)
                .with_parent(Some(ResourceId::local(8))),
        ])
    }

    #[test]
    fn kinds_split_panes_from_children() {
        let snap = snapshot();
        let panes: Vec<_> = terminals(&snap).map(|info| info.id.clone()).collect();
        assert_eq!(panes, [ResourceId::local(7), ResourceId::local(8)]);
        let children: Vec<_> = children_of(&snap, &ResourceId::local(8))
            .map(|info| info.id.clone())
            .collect();
        assert_eq!(children, [ResourceId::local(10), ResourceId::local(11)]);
        assert_eq!(kind_name(ResourceKind::Terminal), "terminal");
        assert_eq!(kind_name(ResourceKind::AgentSession), "agent_session");
    }

    #[test]
    fn agent_session_of_resolves_a_pane_to_its_unique_child_or_refuses() {
        let snap = snapshot();
        assert_eq!(
            agent_session_of(&snap, &ResourceId::local(7)).unwrap(),
            ResourceId::local(9)
        );
        assert_eq!(
            agent_session_of(&snap, &ResourceId::local(9)).unwrap(),
            ResourceId::local(9),
            "a session id names itself"
        );
        assert!(matches!(
            agent_session_of(&snap, &ResourceId::local(8)),
            Err(AgentSessionError::AmbiguousSession { candidates, .. })
                if candidates == [ResourceId::local(10), ResourceId::local(11)]
        ));
        let lone = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
            .with_resources(vec![ResourceInfo::new(
                ResourceId::local(1),
                WindowId::new(1),
                80,
                24,
            )]);
        assert!(matches!(
            agent_session_of(&lone, &ResourceId::local(1)),
            Err(AgentSessionError::NoSession { .. })
        ));
    }

    /// The `session` facet wins; a server that predates it is joined through
    /// its legacy window.
    #[test]
    fn session_of_prefers_the_session_facet_and_falls_back_to_the_window() {
        use phux_protocol::wire::info::WindowInfo;
        let window = WindowId::new(4);
        let snap = SessionSnapshot::new(SessionId::new(1), window, ResourceId::local(1))
            .with_windows(vec![WindowInfo::new(window, SessionId::new(2), "window-0")])
            .with_resources(vec![
                ResourceInfo::new(ResourceId::local(1), window, 80, 24)
                    .with_session(Some(SessionId::new(3))),
                ResourceInfo::new(ResourceId::local(2), window, 80, 24),
                ResourceInfo::new(ResourceId::local(5), WindowId::new(99), 80, 24),
            ]);
        assert_eq!(
            session_of(&snap, &ResourceId::local(1)),
            Some(SessionId::new(3))
        );
        assert_eq!(
            session_of(&snap, &ResourceId::local(2)),
            Some(SessionId::new(2))
        );
        assert_eq!(session_of(&snap, &ResourceId::local(5)), None);
        assert_eq!(session_of(&snap, &ResourceId::local(6)), None);
        let in_two: Vec<_> = terminals_in(&snap, SessionId::new(2))
            .map(|info| info.id.clone())
            .collect();
        assert_eq!(in_two, [ResourceId::local(2)]);
    }
}
