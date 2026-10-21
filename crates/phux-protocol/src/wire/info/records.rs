//! The session, window, and resource rows of a [`SessionSnapshot`](super::SessionSnapshot), with their facets.

#[cfg(doc)]
use super::SessionSnapshot;
use super::{
    ClientId, CloseReason, ResourceId, ResourceKind, ResourceLifecycle, SessionId, SessionKey,
    SessionLifetime, WindowId,
};

/// One session, sufficient for UI chrome and `phux ls`; its windows are in
/// [`SessionSnapshot::windows`]. Construct via [`Self::new`] and `with_*`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SessionInfo {
    /// Stable session identifier.
    pub id: SessionId,
    /// Human-readable name; `AttachTarget::ByName` matches against this.
    pub name: String,
    /// The session's remembered focus, restored on attach (distinct from the
    /// client's [`SessionSnapshot::focused_window`]).
    pub active_window: Option<WindowId>,
    /// Creation time, seconds since the Unix epoch.
    pub created_at_unix_secs: i64,
    /// Number of windows, denormalized at snapshot time.
    pub window_count: u16,
    /// Number of attached clients, denormalized at snapshot time.
    pub attached_client_count: u16,
    /// Whether the session survives its last window (ADR-0105); rides the
    /// trailing session facets, so older peers read `false`. A server that
    /// reports [`Self::lifetime`] keeps it equal to
    /// `lifetime == Persistent`.
    pub keep_empty: bool,
    /// The session's lifetime (`docs/spec/L1.md` §5.7), from the extension
    /// block's `SESSION_STATE`; `None` from a server that does not report it.
    pub lifetime: Option<SessionLifetime>,
    /// The session's persistent key, from the extension block's
    /// `SESSION_STATE`; `None` from a server that does not report it.
    pub key: Option<SessionKey>,
    /// The session's descriptive attributes (`docs/spec/L2.md` §2.2), in
    /// order, from the extension block's `SESSION_STATE`; empty from a
    /// server that does not report them.
    pub attrs: Vec<(String, String)>,
    /// The session's most recent input target (R14), a Terminal listed in
    /// this snapshot and owned by this session, from the extension block's
    /// `SESSION_STATE`; `None` when the session has none or the server does
    /// not report it.
    pub recent_input: Option<ResourceId>,
}

impl SessionInfo {
    /// A `SessionInfo` with every other field at `None` / `0` / `false`.
    #[must_use]
    pub fn new(id: SessionId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            active_window: None,
            created_at_unix_secs: 0,
            window_count: 0,
            attached_client_count: 0,
            keep_empty: false,
            lifetime: None,
            key: None,
            attrs: Vec::new(),
            recent_input: None,
        }
    }

    /// Builder setter for [`Self::keep_empty`].
    #[must_use]
    pub const fn with_keep_empty(mut self, keep_empty: bool) -> Self {
        self.keep_empty = keep_empty;
        self
    }

    /// Builder setter for [`Self::lifetime`]. Also sets
    /// [`Self::keep_empty`] to `lifetime == Persistent`, so the legacy facet
    /// and the lifetime cannot disagree; `None` leaves `keep_empty` alone.
    #[must_use]
    pub const fn with_lifetime(mut self, lifetime: Option<SessionLifetime>) -> Self {
        self.lifetime = lifetime;
        if let Some(lifetime) = lifetime {
            self.keep_empty = matches!(lifetime, SessionLifetime::Persistent);
        }
        self
    }

    /// Builder setter for [`Self::key`].
    #[must_use]
    pub const fn with_key(mut self, key: Option<SessionKey>) -> Self {
        self.key = key;
        self
    }

    /// Builder setter for [`Self::attrs`].
    #[must_use]
    pub fn with_attrs(mut self, attrs: Vec<(String, String)>) -> Self {
        self.attrs = attrs;
        self
    }

    /// Builder setter for [`Self::recent_input`].
    #[must_use]
    pub fn with_recent_input(mut self, recent_input: Option<ResourceId>) -> Self {
        self.recent_input = recent_input;
        self
    }

    /// Whether this session's extension-block `SESSION_STATE` has anything
    /// to say.
    pub(super) const fn has_session_state(&self) -> bool {
        self.lifetime.is_some()
            || self.key.is_some()
            || !self.attrs.is_empty()
            || self.recent_input.is_some()
    }

    /// Whether the session currently holds no windows: a keep-empty session
    /// whose last window closed, or one created with no seed terminal.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.window_count == 0
    }

    /// Builder setter for [`Self::active_window`].
    #[must_use]
    pub const fn with_active_window(mut self, active_window: Option<WindowId>) -> Self {
        self.active_window = active_window;
        self
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

    /// Builder setter for [`Self::attached_client_count`].
    #[must_use]
    pub const fn with_attached_client_count(mut self, attached_client_count: u16) -> Self {
        self.attached_client_count = attached_client_count;
        self
    }
}

/// One window, sufficient for tab/pane chrome; its resources are in
/// [`SessionSnapshot::resources`]. Construct via [`Self::new`] and `with_*`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct WindowInfo {
    /// Stable window identifier.
    pub id: WindowId,
    /// Foreign key into [`SessionSnapshot::sessions`].
    pub session_id: SessionId,
    /// Position within the session's windows list.
    pub index: u16,
    /// Human-readable window name.
    pub name: String,
    /// Window's remembered focused pane.
    pub active_resource: Option<ResourceId>,
}

impl WindowInfo {
    /// A `WindowInfo` at index `0` with no active resource.
    #[must_use]
    pub fn new(id: WindowId, session_id: SessionId, name: impl Into<String>) -> Self {
        Self {
            id,
            session_id,
            index: 0,
            name: name.into(),
            active_resource: None,
        }
    }

    /// Builder setter for [`Self::index`].
    #[must_use]
    pub const fn with_index(mut self, index: u16) -> Self {
        self.index = index;
        self
    }

    /// Builder setter for [`Self::active_resource`].
    #[must_use]
    pub fn with_active_resource(mut self, active_resource: Option<ResourceId>) -> Self {
        self.active_resource = active_resource;
        self
    }
}

/// The snapshot facet of a [`ResourceKind::AgentSession`] resource.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AgentFacet {
    /// Agent provider name, e.g. `claude`.
    pub provider: String,
    /// Opaque provider-native session id, when the producer supplied one.
    pub native_id: Option<String>,
    /// Derived state, an open lower-case string; unknown values read as
    /// `unknown`.
    pub state: String,
}

impl AgentFacet {
    /// An `AgentFacet` with no `native_id`.
    #[must_use]
    pub fn new(provider: impl Into<String>, state: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            native_id: None,
            state: state.into(),
        }
    }

    /// Builder setter for [`Self::native_id`].
    #[must_use]
    pub fn with_native_id(mut self, native_id: Option<String>) -> Self {
        self.native_id = native_id;
        self
    }
}

/// How a retained resource's process ended (ADR-0124); present only while
/// the exited resource is retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExitFacet {
    /// `_exit(n)` status, when known.
    pub exit_status: Option<i32>,
    /// Terminating signal, when known.
    pub signal: Option<i32>,
    /// Why the process ended (`RESOURCE_CLOSED.reason` vocabulary).
    pub reason: CloseReason,
    /// When the process exited, Unix milliseconds.
    pub exited_at_ms: u64,
    /// When the server will close the resource, Unix milliseconds.
    pub retained_until_ms: u64,
}

impl ExitFacet {
    /// An `Exited` facet with no status or signal known.
    #[must_use]
    pub const fn new(exited_at_ms: u64, retained_until_ms: u64) -> Self {
        Self {
            exit_status: None,
            signal: None,
            reason: CloseReason::Exited,
            exited_at_ms,
            retained_until_ms,
        }
    }

    /// Builder setter for [`Self::exit_status`].
    #[must_use]
    pub const fn with_exit_status(mut self, exit_status: Option<i32>) -> Self {
        self.exit_status = exit_status;
        self
    }

    /// Builder setter for [`Self::signal`].
    #[must_use]
    pub const fn with_signal(mut self, signal: Option<i32>) -> Self {
        self.signal = signal;
        self
    }

    /// Builder setter for [`Self::reason`].
    #[must_use]
    pub const fn with_reason(mut self, reason: CloseReason) -> Self {
        self.reason = reason;
        self
    }
}

/// One served resource of any [`ResourceKind`], sufficient for layout chrome.
///
/// A non-Terminal kind has no window or grid, encoded as `WindowId(0)` and
/// `0 x 0`; key layout on [`Self::kind`], not those sentinels. `kind`,
/// `parent`, and `agent` ride the snapshot's trailing facet list.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ResourceInfo {
    /// Stable resource identifier.
    pub id: ResourceId,
    /// Foreign key into [`SessionSnapshot::windows`]; `WindowId(0)` for a
    /// non-Terminal kind, which no window owns.
    pub window_id: WindowId,
    /// Grid width in cells; `0` for a non-Terminal kind.
    pub cols: u16,
    /// Grid height in cells; `0` for a non-Terminal kind.
    pub rows: u16,
    /// User-set title, distinct from any shell-set title.
    pub title: Option<String>,
    /// Working directory (lossy UTF-8, display only).
    pub cwd: Option<String>,
    /// What backs the resource; defaults to [`ResourceKind::Terminal`].
    pub kind: ResourceKind,
    /// The parent this child is bound to; closing the parent closes it.
    pub parent: Option<ResourceId>,
    /// Agent facet, present iff `kind` is [`ResourceKind::AgentSession`].
    pub agent: Option<AgentFacet>,
    /// `Running`, or `Exited` while retained (ADR-0124); rides the extension
    /// block.
    pub lifecycle: ResourceLifecycle,
    /// How a retained resource's process ended; `None` while it runs.
    pub exit: Option<ExitFacet>,
    /// Input-lease holder; `None` while open to every client (ADR-0033).
    pub input_holder: Option<ClientId>,
    /// `VIEWER` subscribers, ascending (ADR-0127).
    pub viewers: Vec<ClientId>,
    /// The session the resource belongs to, from the extension block's
    /// `RESOURCE_SESSION`; `None` from a server that does not report it, in
    /// which case the join runs through [`Self::window_id`].
    pub session: Option<SessionId>,
}

impl ResourceInfo {
    /// A running Terminal entry with every optional field unset.
    #[must_use]
    pub const fn new(id: ResourceId, window_id: WindowId, cols: u16, rows: u16) -> Self {
        Self {
            id,
            window_id,
            cols,
            rows,
            title: None,
            cwd: None,
            kind: ResourceKind::Terminal,
            parent: None,
            agent: None,
            lifecycle: ResourceLifecycle::Running,
            exit: None,
            input_holder: None,
            viewers: Vec::new(),
            session: None,
        }
    }

    /// Builder setter for [`Self::session`].
    #[must_use]
    pub const fn with_session(mut self, session: Option<SessionId>) -> Self {
        self.session = session;
        self
    }

    /// Construct the entry for a non-Terminal resource: no window
    /// (`WindowId(0)`), no grid (`0 x 0`), the given `kind`.
    #[must_use]
    pub const fn resource(id: ResourceId, kind: ResourceKind) -> Self {
        Self {
            id,
            window_id: WindowId::new(0),
            cols: 0,
            rows: 0,
            title: None,
            cwd: None,
            kind,
            parent: None,
            agent: None,
            lifecycle: ResourceLifecycle::Running,
            exit: None,
            input_holder: None,
            viewers: Vec::new(),
            session: None,
        }
    }

    /// Builder setter for [`Self::title`].
    #[must_use]
    pub fn with_title(mut self, title: Option<String>) -> Self {
        self.title = title;
        self
    }

    /// Builder setter for [`Self::cwd`].
    #[must_use]
    pub fn with_cwd(mut self, cwd: Option<String>) -> Self {
        self.cwd = cwd;
        self
    }

    /// Builder setter for [`Self::kind`].
    #[must_use]
    pub const fn with_kind(mut self, kind: ResourceKind) -> Self {
        self.kind = kind;
        self
    }

    /// Builder setter for [`Self::parent`].
    #[must_use]
    pub fn with_parent(mut self, parent: Option<ResourceId>) -> Self {
        self.parent = parent;
        self
    }

    /// Builder setter for [`Self::agent`].
    #[must_use]
    pub fn with_agent(mut self, agent: Option<AgentFacet>) -> Self {
        self.agent = agent;
        self
    }

    /// Builder setter for [`Self::lifecycle`].
    #[must_use]
    pub const fn with_lifecycle(mut self, lifecycle: ResourceLifecycle) -> Self {
        self.lifecycle = lifecycle;
        self
    }

    /// Builder setter for [`Self::exit`].
    #[must_use]
    pub const fn with_exit(mut self, exit: Option<ExitFacet>) -> Self {
        self.exit = exit;
        self
    }

    /// Builder setter for [`Self::input_holder`].
    #[must_use]
    pub const fn with_input_holder(mut self, input_holder: Option<ClientId>) -> Self {
        self.input_holder = input_holder;
        self
    }

    /// Builder setter for [`Self::viewers`].
    #[must_use]
    pub fn with_viewers(mut self, viewers: Vec<ClientId>) -> Self {
        self.viewers = viewers;
        self
    }

    /// Whether the trailing resource facet list needs a row for this entry.
    pub(super) const fn has_resource_facets(&self) -> bool {
        !self.kind.is_terminal() || self.parent.is_some() || self.agent.is_some()
    }

    /// Whether the extension block needs a `RESOURCE_STATE` entry for it.
    pub(super) const fn has_resource_state(&self) -> bool {
        !matches!(self.lifecycle, ResourceLifecycle::Running)
            || self.exit.is_some()
            || self.input_holder.is_some()
            || !self.viewers.is_empty()
    }
}
