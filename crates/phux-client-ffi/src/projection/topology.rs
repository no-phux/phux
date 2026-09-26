//! The session graph, flattened for a foreign caller.
//!
//! The runtime's [`Topology`] keeps sessions, windows and panes as separate
//! lists joined by id. A binding's consumer paints a list, so the pane entry
//! is denormalized with its window and session context and every terminal
//! identity is already in its [`crate::projection::id`] string form.

use phux_client_runtime::control::Topology;

use super::id;

/// One session visible in the server's snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// The session id.
    pub id: u32,
    /// The session name.
    pub name: String,
    /// Windows in the session.
    pub window_count: u16,
    /// Clients attached to the session.
    pub attached_client_count: u16,
}

/// One pane, denormalized with its window and session so no joins are needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    /// The terminal's string identity.
    pub terminal_id: String,
    /// The session it belongs to.
    pub session_id: u32,
    /// That session's name.
    pub session_name: String,
    /// The window it belongs to.
    pub window_id: u32,
    /// That window's index.
    pub window_index: u16,
    /// That window's name.
    pub window_name: String,
    /// The terminal's title, if known.
    pub title: Option<String>,
    /// The terminal's working directory, if known.
    pub cwd: Option<String>,
    /// Whether this is the attaching client's initial focus.
    pub is_focused: bool,
}

/// The flattened session graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionGraph {
    /// Every session the server listed.
    pub sessions: Vec<Session>,
    /// Every pane the server listed, in its order.
    pub panes: Vec<Pane>,
    /// The attaching client's initial focused pane, in string form.
    pub focused_pane: String,
}

/// Flatten one runtime topology snapshot.
#[must_use]
pub fn session_graph(topology: Topology) -> SessionGraph {
    SessionGraph {
        sessions: topology
            .sessions
            .into_iter()
            .map(|session| Session {
                id: session.id,
                name: session.name,
                window_count: session.window_count,
                attached_client_count: session.attached_client_count,
            })
            .collect(),
        panes: topology
            .panes
            .into_iter()
            .map(|pane| Pane {
                terminal_id: id::encode(&pane.terminal_id),
                session_id: pane.session_id,
                session_name: pane.session_name,
                window_id: pane.window_id,
                window_index: pane.window_index,
                window_name: pane.window_name,
                title: pane.title,
                cwd: pane.cwd,
                is_focused: pane.is_focused,
            })
            .collect(),
        focused_pane: id::encode(&topology.focused_pane),
    }
}

#[cfg(test)]
mod tests {
    use super::session_graph;
    use phux_client_runtime::control::{PaneDescriptor, SessionDescriptor, Topology};
    use phux_protocol::ResourceId;

    fn topology() -> Topology {
        Topology {
            sessions: vec![SessionDescriptor {
                id: 1,
                name: "work".to_owned(),
                window_count: 2,
                attached_client_count: 1,
                keep_empty: true,
                created_at_unix_secs: 42,
            }],
            windows: Vec::new(),
            panes: vec![PaneDescriptor {
                terminal_id: ResourceId::satellite("box", 4),
                session_id: 1,
                session_name: "work".to_owned(),
                window_id: 3,
                window_index: 0,
                window_name: "edit".to_owned(),
                cols: 80,
                rows: 24,
                title: Some("vim".to_owned()),
                cwd: None,
                is_focused: true,
            }],
            agent_sessions: Vec::new(),
            focused_session: 1,
            focused_pane: ResourceId::local(7),
        }
    }

    #[test]
    fn panes_carry_their_window_and_session_context() {
        let graph = session_graph(topology());
        let pane = &graph.panes[0];
        assert_eq!(pane.terminal_id, "satellite:box:4");
        assert_eq!(pane.session_name, "work");
        assert_eq!(pane.window_index, 0);
        assert_eq!(pane.window_name, "edit");
        assert_eq!(pane.title.as_deref(), Some("vim"));
        assert!(pane.is_focused);
    }

    #[test]
    fn the_focused_pane_is_a_string_identity() {
        assert_eq!(session_graph(topology()).focused_pane, "local:7");
    }

    #[test]
    fn sessions_keep_their_counts() {
        let graph = session_graph(topology());
        assert_eq!(graph.sessions[0].window_count, 2);
        assert_eq!(graph.sessions[0].attached_client_count, 1);
    }
}
