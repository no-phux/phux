//! The snapshot graph itself and a hub's host-session inventory.

use super::{
    ResourceId, ResourceInfo, SatelliteHost, SessionId, SessionInfo, WindowId, WindowInfo,
    boxed_hosts,
};

/// One satellite session as a hub lists it in [`SessionSnapshot::hosts`].
///
/// [`Self::id`] is satellite-local and never joined against the hub's
/// sessions; [`Self::active_resource`] is re-tagged `SATELLITE` so it routes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HostSessionInfo {
    /// The satellite-local session id.
    pub id: SessionId,
    /// The session's name on the satellite.
    pub name: String,
    /// Wall-clock creation time as seconds since the Unix epoch.
    pub created_at_unix_secs: i64,
    /// Number of windows in the session.
    pub window_count: u16,
    /// Number of Terminal-kind resources across the session's windows.
    pub pane_count: u16,
    /// Number of clients attached to the session on the satellite.
    pub attached_client_count: u16,
    /// The session's remembered focused pane, re-tagged `SATELLITE`.
    pub active_resource: Option<ResourceId>,
}

impl HostSessionInfo {
    /// A `HostSessionInfo` with zero counts and no active resource.
    #[must_use]
    pub fn new(id: SessionId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            created_at_unix_secs: 0,
            window_count: 0,
            pane_count: 0,
            attached_client_count: 0,
            active_resource: None,
        }
    }

    /// Builder setter for [`Self::created_at_unix_secs`].
    #[must_use]
    pub const fn with_created_at_unix_secs(mut self, created_at_unix_secs: i64) -> Self {
        self.created_at_unix_secs = created_at_unix_secs;
        self
    }

    /// Builder setter for [`Self::window_count`].
    #[must_use]
    pub const fn with_window_count(mut self, window_count: u16) -> Self {
        self.window_count = window_count;
        self
    }

    /// Builder setter for [`Self::pane_count`].
    #[must_use]
    pub const fn with_pane_count(mut self, pane_count: u16) -> Self {
        self.pane_count = pane_count;
        self
    }

    /// Builder setter for [`Self::attached_client_count`].
    #[must_use]
    pub const fn with_attached_client_count(mut self, attached_client_count: u16) -> Self {
        self.attached_client_count = attached_client_count;
        self
    }

    /// Builder setter for [`Self::active_resource`].
    #[must_use]
    pub fn with_active_resource(mut self, active_resource: Option<ResourceId>) -> Self {
        self.active_resource = active_resource;
        self
    }
}

/// One satellite's row in [`SessionSnapshot::hosts`]: its sessions, or why
/// the hub could not list them (so it shows as degraded, not missing).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HostInventory {
    /// The hub-local satellite name that `SATELLITE` ids carry.
    pub host: SatelliteHost,
    /// Diagnostic prose when unreachable; branch on presence only.
    pub unreachable: Option<String>,
    /// The satellite's sessions in reported order; empty when unreachable.
    pub sessions: Vec<HostSessionInfo>,
}

impl HostInventory {
    /// A satellite that answered, with its sessions.
    #[must_use]
    pub const fn reachable(host: SatelliteHost, sessions: Vec<HostSessionInfo>) -> Self {
        Self {
            host,
            unreachable: None,
            sessions,
        }
    }

    /// A satellite the hub could not list, with the hub's diagnostic.
    #[must_use]
    pub fn unreachable(host: SatelliteHost, diagnostic: impl Into<String>) -> Self {
        Self {
            host,
            unreachable: Some(diagnostic.into()),
            sessions: Vec::new(),
        }
    }

    /// Whether the hub listed this satellite.
    #[must_use]
    pub const fn is_reachable(&self) -> bool {
        self.unreachable.is_none()
    }
}

/// Flat, id-joined graph of sessions, windows, and resources delivered with
/// `ATTACHED` and `GET_STATE`.
///
/// The `focused_*` triple is the attaching client's focus, distinct from each
/// container's remembered `active_*` focus.
///
/// # Wire shape
///
/// Positional (`docs/spec/L1.md` §9.1): three `u32`-counted lists, the focus
/// triple, then trailing elements in fixed order: resource facets
/// (`kind`/`parent`/`agent` rows joined by id), [`Self::hosts`], session
/// facets (`id: u32 || flags: u8`, bit 0 = `keep_empty`), the optional
/// listeners JSON, and the field-tagged extension block. Each element is
/// written only when it or a later one is non-empty (earlier ones then get a
/// zero count), and read only while bytes remain, so older peers stay
/// byte-compatible. Rows naming no entry and unknown flag bits are ignored.
///
/// Every list row (a [`SessionInfo`], [`WindowInfo`], [`ResourceInfo`],
/// facet row, [`HostInventory`], or [`HostSessionInfo`]) is a record:
/// `u32` big-endian length, then the row. A newer peer appends fields at a
/// row's end and an older decoder skips them, so rows grow without a
/// version bump. Ids inside a row are frozen leaves.
///
/// # Example
///
/// ```
/// use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
/// use phux_protocol::{ResourceId, SessionId, WindowId};
///
/// let snapshot = SessionSnapshot::new(
///     SessionId::new(1),
///     WindowId::new(10),
///     ResourceId::new(100),
/// )
/// .with_sessions(vec![SessionInfo::new(SessionId::new(1), "work")
///     .with_window_count(1)
///     .with_attached_client_count(1)])
/// .with_windows(vec![
///     WindowInfo::new(WindowId::new(10), SessionId::new(1), "code")
///         .with_active_resource(Some(ResourceId::new(100))),
/// ])
/// .with_resources(vec![ResourceInfo::new(ResourceId::new(100), WindowId::new(10), 80, 24)]);
/// assert_eq!(snapshot.sessions.len(), 1);
/// ```
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SessionSnapshot {
    /// Every session the attaching client can see.
    pub sessions: Vec<SessionInfo>,
    /// Every window across every visible session.
    pub windows: Vec<WindowInfo>,
    /// Every pane across every visible window.
    pub resources: Vec<ResourceInfo>,
    /// The attaching client's initial focused session.
    pub focused_session: SessionId,
    /// The attaching client's initial focused window.
    pub focused_window: WindowId,
    /// The attaching client's initial focused pane.
    pub focused_resource: ResourceId,
    /// Hosts, listeners, and journal head, boxed together (absent when all
    /// are empty) to keep `CommandResult` small.
    pub(super) trail: Option<Box<SessionSnapshotTrail>>,
}

/// Trailing additive payload for [`SessionSnapshot`].
#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct SessionSnapshotTrail {
    pub(super) hosts: Box<[HostInventory]>,
    pub(super) listeners: Option<crate::wire::listeners::RemoteListenersReport>,
    /// The journal head at the cut (extension block field 2, L1 §7.3).
    pub(super) journal_head: Option<u64>,
}

impl SessionSnapshotTrail {
    pub(super) fn is_empty(&self) -> bool {
        self.hosts.is_empty() && self.listeners.is_none() && self.journal_head.is_none()
    }
}

impl SessionSnapshot {
    /// An empty snapshot with the attaching client's focus triple.
    #[must_use]
    pub const fn new(
        focused_session: SessionId,
        focused_window: WindowId,
        focused_resource: ResourceId,
    ) -> Self {
        Self {
            sessions: Vec::new(),
            windows: Vec::new(),
            resources: Vec::new(),
            focused_session,
            focused_window,
            focused_resource,
            trail: None,
        }
    }

    /// The host-session inventory, one row per satellite; empty off a hub.
    #[must_use]
    pub fn hosts(&self) -> &[HostInventory] {
        self.trail
            .as_ref()
            .map_or(&[], |trail| trail.hosts.as_ref())
    }

    /// Builder setter for the hosts inventory.
    #[must_use]
    pub fn with_hosts(mut self, hosts: Vec<HostInventory>) -> Self {
        self.set_hosts(hosts);
        self
    }

    /// Remote listener bind report, when the serving peer filled it.
    #[must_use]
    pub fn listeners(&self) -> Option<&crate::wire::listeners::RemoteListenersReport> {
        self.trail
            .as_ref()
            .and_then(|trail| trail.listeners.as_ref())
    }

    /// Builder setter for the remote-listeners report.
    #[must_use]
    pub fn with_listeners(
        mut self,
        listeners: crate::wire::listeners::RemoteListenersReport,
    ) -> Self {
        self.set_listeners(Some(listeners));
        self
    }

    /// The same snapshot with no listener report: what a server returns to a
    /// caller not entitled to server-global data (workload-auth §6).
    #[must_use]
    pub fn without_listeners(mut self) -> Self {
        self.set_listeners(None);
        self
    }

    pub(super) fn set_hosts(&mut self, hosts: Vec<HostInventory>) {
        self.edit_trail(|trail| trail.hosts = boxed_hosts(hosts).unwrap_or_default());
    }

    pub(super) fn set_listeners(
        &mut self,
        listeners: Option<crate::wire::listeners::RemoteListenersReport>,
    ) {
        self.edit_trail(|trail| trail.listeners = listeners);
    }

    pub(super) fn set_journal_head(&mut self, journal_head: Option<u64>) {
        self.edit_trail(|trail| trail.journal_head = journal_head);
    }

    /// Edit the trail, dropping it when left empty so equality is canonical.
    pub(super) fn edit_trail(&mut self, edit: impl FnOnce(&mut SessionSnapshotTrail)) {
        let mut trail = self.trail.take().map(|trail| *trail).unwrap_or_default();
        edit(&mut trail);
        self.trail = (!trail.is_empty()).then(|| Box::new(trail));
    }

    /// The newest event-journal `seq` at the cut (L1 §7.3), or `None` from a
    /// peer without `EVENT_JOURNAL`.
    #[must_use]
    pub fn journal_head(&self) -> Option<u64> {
        self.trail.as_ref().and_then(|trail| trail.journal_head)
    }

    /// Builder setter for [`Self::journal_head`].
    #[must_use]
    pub fn with_journal_head(mut self, journal_head: Option<u64>) -> Self {
        self.set_journal_head(journal_head);
        self
    }

    /// Builder setter for [`Self::sessions`].
    #[must_use]
    pub fn with_sessions(mut self, sessions: Vec<SessionInfo>) -> Self {
        self.sessions = sessions;
        self
    }

    /// Builder setter for [`Self::windows`].
    #[must_use]
    pub fn with_windows(mut self, windows: Vec<WindowInfo>) -> Self {
        self.windows = windows;
        self
    }

    /// Builder setter for [`Self::resources`].
    #[must_use]
    pub fn with_resources(mut self, resources: Vec<ResourceInfo>) -> Self {
        self.resources = resources;
        self
    }
}
