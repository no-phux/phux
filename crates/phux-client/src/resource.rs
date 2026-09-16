//! Resource kinds as a client sees them in a `GET_STATE` snapshot (ADR-0102).
//!
//! A snapshot's `panes` list every resource the server serves, not only
//! Terminals: an `AgentSession` rides there too, with its `kind`, its
//! `parent`, and its agent facet. These helpers are the one place the
//! client decides which entries are panes (Terminal-kind, the things a
//! layout slot holds) and which are children bound to one, so the selector,
//! `phux ls`, `phux agent list`, and the session verbs cannot disagree.

use phux_protocol::ids::{ResourceId, ResourceKind};
use phux_protocol::wire::frame::{CloseReason, ControlAction, ResourceLifecycle};
use phux_protocol::wire::info::{ExitFacet, ResourceInfo, SessionSnapshot};

use crate::agent_session::AgentSessionError;
use crate::attach::AttachError;
use crate::attach::connection::Connection;

// The `phux resource` verbs (PHA-406): a cursor-resumable wait on a
// resource's exit (D2), one resource's inspection record, and the kind
// catalog intersected with what the server negotiated (D4).
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
    /// The resource is not in the server's inventory. `unreachable` is
    /// non-empty when the view was partial (a federation satellite did not
    /// answer), in which case the absence is not proof it is gone.
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
/// [`AgentSessionError::NoSession`] when a Terminal has no live child,
/// [`AgentSessionError::AmbiguousSession`] when it has several, and
/// [`AgentSessionError::WrongKind`] for a resource of any other kind. A
/// target absent from the snapshot is reported as a Terminal with no
/// session — the caller has already established it resolved.
pub fn session_of(
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

/// The Terminal a target resolves to for a Terminal-facet operation: a
/// Terminal names itself; an `AgentSession` names its parent.
///
/// # Errors
///
/// [`AgentSessionError::WrongKind`] for a resource of any other kind, or an
/// `AgentSession` whose parent is not in the snapshot.
pub fn terminal_of(
    snapshot: &SessionSnapshot,
    target: &ResourceId,
) -> Result<ResourceId, AgentSessionError> {
    match find(snapshot, target) {
        Some(info) if is_terminal(info) => Ok(target.clone()),
        Some(info) if is_agent_session(info) => {
            info.parent
                .clone()
                .ok_or_else(|| AgentSessionError::WrongKind {
                    resource: target.clone(),
                    expected: TERMINAL,
                    actual: AGENT_SESSION.to_owned(),
                })
        }
        Some(info) => Err(AgentSessionError::WrongKind {
            resource: target.clone(),
            expected: TERMINAL,
            actual: kind_name(info.kind).to_owned(),
        }),
        None => Ok(target.clone()),
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
    fn session_of_resolves_a_pane_to_its_unique_child_or_refuses() {
        let snap = snapshot();
        assert_eq!(
            session_of(&snap, &ResourceId::local(7)).unwrap(),
            ResourceId::local(9)
        );
        assert_eq!(
            session_of(&snap, &ResourceId::local(9)).unwrap(),
            ResourceId::local(9),
            "a session id names itself"
        );
        assert!(matches!(
            session_of(&snap, &ResourceId::local(8)),
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
            session_of(&lone, &ResourceId::local(1)),
            Err(AgentSessionError::NoSession { .. })
        ));
    }

    #[test]
    fn terminal_of_resolves_a_session_to_its_parent() {
        let snap = snapshot();
        assert_eq!(
            terminal_of(&snap, &ResourceId::local(9)).unwrap(),
            ResourceId::local(7)
        );
        assert_eq!(
            terminal_of(&snap, &ResourceId::local(7)).unwrap(),
            ResourceId::local(7)
        );
    }
}
