//! The session graph, projected from an `ATTACHED` or `GET_STATE` snapshot.

use phux_protocol::ids::{ResourceId, ResourceKind};
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

/// One session in the server's graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDescriptor {
    /// The session id.
    pub id: u32,
    /// The session name.
    pub name: String,
    /// Windows in the session.
    pub window_count: u16,
    /// Clients attached to the session.
    pub attached_client_count: u16,
    /// The session survives its last window (ADR-0105).
    pub keep_empty: bool,
    /// Creation time, seconds since the Unix epoch.
    pub created_at_unix_secs: i64,
}

/// One window in the server's graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowDescriptor {
    /// The window id.
    pub id: u32,
    /// The session the window belongs to.
    pub session_id: u32,
    /// The window's index within its session.
    pub index: u16,
    /// The window name.
    pub name: String,
    /// The window's active resource, if any.
    pub active_resource: Option<ResourceId>,
}

/// One Terminal-kind resource, denormalized with its window and session so
/// a consumer needs no joins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneDescriptor {
    /// The terminal.
    pub terminal_id: ResourceId,
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
    /// The terminal's grid width.
    pub cols: u16,
    /// The terminal's grid height.
    pub rows: u16,
    /// The terminal's title, if known.
    pub title: Option<String>,
    /// The terminal's working directory, if known.
    pub cwd: Option<String>,
    /// Whether this is the attaching client's initial focus.
    pub is_focused: bool,
}

/// One `AgentSession` resource: a record stream bound to a Terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionDescriptor {
    /// The resource.
    pub id: ResourceId,
    /// The Terminal it is bound to.
    pub parent: Option<ResourceId>,
    /// The provider slug, if declared.
    pub provider: Option<String>,
    /// The provider's own session id, if declared.
    pub native_id: Option<String>,
    /// The server-derived state word, if declared.
    pub state: Option<String>,
}

/// The session/window/pane graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    /// Every session the server listed.
    pub sessions: Vec<SessionDescriptor>,
    /// Every window the server listed.
    pub windows: Vec<WindowDescriptor>,
    /// Every Terminal-kind resource, in the server's order.
    pub panes: Vec<PaneDescriptor>,
    /// Every `AgentSession` resource.
    pub agent_sessions: Vec<AgentSessionDescriptor>,
    /// The focused session.
    pub focused_session: u32,
    /// The focused terminal.
    pub focused_pane: ResourceId,
}

impl Topology {
    /// Project a wire snapshot.
    #[must_use]
    pub fn from_snapshot(snapshot: &SessionSnapshot) -> Self {
        let sessions = snapshot
            .sessions
            .iter()
            .map(|session| SessionDescriptor {
                id: session.id.get(),
                name: session.name.clone(),
                window_count: session.window_count,
                attached_client_count: session.attached_client_count,
                keep_empty: session.keep_empty,
                created_at_unix_secs: session.created_at_unix_secs,
            })
            .collect();
        let windows = snapshot
            .windows
            .iter()
            .map(|window| WindowDescriptor {
                id: window.id.get(),
                session_id: window.session_id.get(),
                index: window.index,
                name: window.name.clone(),
                active_resource: window.active_resource.clone(),
            })
            .collect();
        let panes = terminal_resources(snapshot)
            .map(|pane| {
                let window = snapshot
                    .windows
                    .iter()
                    .find(|window| window.id == pane.window_id);
                let session_id = window.map(|window| window.session_id);
                let session = snapshot
                    .sessions
                    .iter()
                    .find(|session| Some(session.id) == session_id);
                PaneDescriptor {
                    terminal_id: pane.id.clone(),
                    session_id: session_id.map_or(0, phux_protocol::SessionId::get),
                    session_name: session.map_or_else(String::new, |s| s.name.clone()),
                    window_id: pane.window_id.get(),
                    window_index: window.map_or(0, |w| w.index),
                    window_name: window.map_or_else(String::new, |w| w.name.clone()),
                    cols: pane.cols,
                    rows: pane.rows,
                    title: pane.title.clone(),
                    cwd: pane.cwd.clone(),
                    is_focused: pane.id == snapshot.focused_resource,
                }
            })
            .collect();
        let agent_sessions = snapshot
            .resources
            .iter()
            .filter(|resource| resource.kind == ResourceKind::AgentSession)
            .map(|resource| AgentSessionDescriptor {
                id: resource.id.clone(),
                parent: resource.parent.clone(),
                provider: resource.agent.as_ref().map(|facet| facet.provider.clone()),
                native_id: resource
                    .agent
                    .as_ref()
                    .and_then(|facet| facet.native_id.clone()),
                state: resource.agent.as_ref().map(|facet| facet.state.clone()),
            })
            .collect();
        Self {
            sessions,
            windows,
            panes,
            agent_sessions,
            focused_session: snapshot.focused_session.get(),
            focused_pane: snapshot.focused_resource.clone(),
        }
    }

    /// The pane entry for `terminal_id`.
    #[must_use]
    pub fn pane(&self, terminal_id: &ResourceId) -> Option<&PaneDescriptor> {
        self.panes
            .iter()
            .find(|pane| &pane.terminal_id == terminal_id)
    }

    pub(super) fn pane_mut(&mut self, terminal_id: &ResourceId) -> Option<&mut PaneDescriptor> {
        self.panes
            .iter_mut()
            .find(|pane| &pane.terminal_id == terminal_id)
    }

    /// The session entry named `name`.
    #[must_use]
    pub fn session_named(&self, name: &str) -> Option<&SessionDescriptor> {
        self.sessions.iter().find(|session| session.name == name)
    }

    /// The first live pane of session `session_id`, in the server's order.
    #[must_use]
    pub fn first_pane_of(&self, session_id: u32) -> Option<&ResourceId> {
        self.panes
            .iter()
            .find(|pane| pane.session_id == session_id)
            .map(|pane| &pane.terminal_id)
    }

    /// The Terminal-kind resources that are no longer listed by `snapshot`.
    pub(super) fn vanished(&self, snapshot: &SessionSnapshot) -> Vec<ResourceId> {
        self.panes
            .iter()
            .map(|pane| pane.terminal_id.clone())
            .filter(|id| !terminal_resources(snapshot).any(|pane| &pane.id == id))
            .collect()
    }
}

/// The Terminal-kind entries of a snapshot's inventory. Skipping every other
/// kind is a client obligation (ADR-0102): an agent session has no window,
/// grid, or PTY, so projecting one as a pane would paint a row no input can
/// reach.
pub(super) fn terminal_resources(
    snapshot: &SessionSnapshot,
) -> impl Iterator<Item = &ResourceInfo> {
    snapshot
        .resources
        .iter()
        .filter(|resource| resource.kind == ResourceKind::Terminal)
}
