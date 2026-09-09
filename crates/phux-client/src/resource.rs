//! Resource kinds as a client sees them in a `GET_STATE` snapshot (ADR-0102).
//!
//! A snapshot's `panes` list every resource the server serves, not only
//! Terminals: an `AgentSession` rides there too, with its `kind`, its
//! `parent`, and its agent facet. These helpers are the one place the
//! client decides which entries are panes (Terminal-kind, the things a
//! layout slot holds) and which are children bound to one, so the selector,
//! `phux ls`, `phux agent list`, and the session verbs cannot disagree.

use phux_protocol::ids::{ResourceKind, TerminalId};
use phux_protocol::wire::info::{SessionSnapshot, TerminalInfo};

use crate::agent_session::AgentSessionError;

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
pub fn is_terminal(info: &TerminalInfo) -> bool {
    info.kind == ResourceKind::Terminal
}

/// Whether `info` is an `AgentSession` resource.
#[must_use]
pub fn is_agent_session(info: &TerminalInfo) -> bool {
    info.kind == ResourceKind::AgentSession
}

/// The resource `id` names, of any kind.
#[must_use]
pub fn find<'a>(snapshot: &'a SessionSnapshot, id: &TerminalId) -> Option<&'a TerminalInfo> {
    snapshot.panes.iter().find(|info| info.id == *id)
}

/// Every Terminal-kind resource, in snapshot order.
pub fn terminals(snapshot: &SessionSnapshot) -> impl Iterator<Item = &TerminalInfo> {
    snapshot.panes.iter().filter(|info| is_terminal(info))
}

/// Every live `AgentSession` child of `parent`, in snapshot order.
pub fn children_of<'a>(
    snapshot: &'a SessionSnapshot,
    parent: &'a TerminalId,
) -> impl Iterator<Item = &'a TerminalInfo> + 'a {
    snapshot
        .panes
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
    target: &TerminalId,
) -> Result<TerminalId, AgentSessionError> {
    match find(snapshot, target) {
        Some(info) if is_agent_session(info) => Ok(target.clone()),
        Some(info) if !is_terminal(info) => Err(AgentSessionError::WrongKind {
            resource: target.clone(),
            expected: AGENT_SESSION,
            actual: kind_name(info.kind).to_owned(),
        }),
        _ => {
            let children: Vec<TerminalId> = children_of(snapshot, target)
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
    target: &TerminalId,
) -> Result<TerminalId, AgentSessionError> {
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
        SessionSnapshot::new(SessionId::new(1), window, TerminalId::local(7)).with_panes(vec![
            TerminalInfo::new(TerminalId::local(7), window, 80, 24),
            TerminalInfo::new(TerminalId::local(8), window, 80, 24),
            TerminalInfo::new(TerminalId::local(9), window, 0, 0)
                .with_kind(ResourceKind::AgentSession)
                .with_parent(Some(TerminalId::local(7))),
            TerminalInfo::new(TerminalId::local(10), window, 0, 0)
                .with_kind(ResourceKind::AgentSession)
                .with_parent(Some(TerminalId::local(8))),
            TerminalInfo::new(TerminalId::local(11), window, 0, 0)
                .with_kind(ResourceKind::AgentSession)
                .with_parent(Some(TerminalId::local(8))),
        ])
    }

    #[test]
    fn kinds_split_panes_from_children() {
        let snap = snapshot();
        let panes: Vec<_> = terminals(&snap).map(|info| info.id.clone()).collect();
        assert_eq!(panes, [TerminalId::local(7), TerminalId::local(8)]);
        let children: Vec<_> = children_of(&snap, &TerminalId::local(8))
            .map(|info| info.id.clone())
            .collect();
        assert_eq!(children, [TerminalId::local(10), TerminalId::local(11)]);
        assert_eq!(kind_name(ResourceKind::Terminal), "terminal");
        assert_eq!(kind_name(ResourceKind::AgentSession), "agent_session");
    }

    #[test]
    fn session_of_resolves_a_pane_to_its_unique_child_or_refuses() {
        let snap = snapshot();
        assert_eq!(
            session_of(&snap, &TerminalId::local(7)).unwrap(),
            TerminalId::local(9)
        );
        assert_eq!(
            session_of(&snap, &TerminalId::local(9)).unwrap(),
            TerminalId::local(9),
            "a session id names itself"
        );
        assert!(matches!(
            session_of(&snap, &TerminalId::local(8)),
            Err(AgentSessionError::AmbiguousSession { candidates, .. })
                if candidates == [TerminalId::local(10), TerminalId::local(11)]
        ));
        let lone = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), TerminalId::local(1))
            .with_panes(vec![TerminalInfo::new(
                TerminalId::local(1),
                WindowId::new(1),
                80,
                24,
            )]);
        assert!(matches!(
            session_of(&lone, &TerminalId::local(1)),
            Err(AgentSessionError::NoSession { .. })
        ));
    }

    #[test]
    fn terminal_of_resolves_a_session_to_its_parent() {
        let snap = snapshot();
        assert_eq!(
            terminal_of(&snap, &TerminalId::local(9)).unwrap(),
            TerminalId::local(7)
        );
        assert_eq!(
            terminal_of(&snap, &TerminalId::local(7)).unwrap(),
            TerminalId::local(7)
        );
    }
}
