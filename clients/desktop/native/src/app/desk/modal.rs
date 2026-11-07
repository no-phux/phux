//! Modal identity and the immutable session target a confirmation holds.
use crate::connection::ConnectionIdentity;
use phux_protocol::{ResourceId, ids::SessionKey};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRenameTarget {
    pub key: Option<SessionKey>,
    pub id: u32,
    pub identity: ConnectionIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Modal {
    None,
    Commands,
    Capture,
    Goto,
    Notifications,
    AgentFleet,
    NativeLaunch,
    /// Start an explicit, supervised producer on this Mac.
    Mirror {
        source: &'static str,
    },
    Path,
    /// Pick a theme; the highlighted one previews until Enter keeps it.
    Themes,
    MoveResources,
    Rename {
        tab_id: String,
    },
    Session {
        create: bool,
        current: String,
        target: Option<SessionRenameTarget>,
    },
    /// Link a session into a named project. Projects meet by name across
    /// machines, so each machine's server keeps the link for its own session.
    Project {
        /// The machine whose server holds the session.
        host: Option<String>,
        session: SessionKey,
        name: String,
        current: Option<String>,
    },
    /// Kill a whole session: every member ends and the session is deleted.
    KillSession {
        name: String,
        session: u32,
        target: Option<SessionRenameTarget>,
    },
    WindowSession,
    /// Switch this window to a session on any machine, or create one by name.
    Sessions,
    /// Every open native window, its session, host and tabs.
    Windows,
    Terminate {
        terminal: ResourceId,
        title: String,
    },
    /// Register a machine over ssh with `phux host add`.
    AddHost,
    /// Forget a registered machine with `phux host rm`.
    RemoveHost {
        name: String,
    },
    /// A paste held because the placement's program would read its line
    /// breaks as Enter.
    Paste {
        placement_id: String,
        /// The terminal the paste was held for; a retargeted placement
        /// refuses it.
        terminal_id: ResourceId,
        title: String,
        paste: crate::input::HeldPaste,
    },
}
