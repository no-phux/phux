//! Snapshot-graph types delivered with `ATTACHED` per `docs/spec/L1.md` §7.
//!
//! SPEC §13 references `SessionInfo`, `WindowInfo`, `ResourceInfo`, and
//! `SessionSnapshot` but does not define their fields. This module fills that
//! gap with wire-portable shapes that mirror `phux_core::{Session, Window,
//! Pane, LayoutNode, SplitDir}` semantics WITHOUT crossing the
//! core/protocol independence boundary (`phux-protocol` cannot depend on
//! `phux-core`).
//!
//! The snapshot is the minimum a reconnecting client needs to render
//! UI chrome, status bars, and pane layout — terminal contents flow separately
//! through protocol-0.7 bootstrap streams (`ATTACHED` → per-pane
//! `BOOTSTRAP_BEGIN`/`CHUNK`/`READY` → `ATTACH_READY`).

use crate::ids::{ClientId, ResourceId, ResourceKind, SatelliteHost, SessionId, WindowId};

use super::decode::Decoder;
use super::encode::Encoder;
use super::error::DecodeError;
use super::frame::{decode_terminal_id, encode_terminal_id};

// -----------------------------------------------------------------------------
// Tagged-union tags. `pub(crate)` so the codec and tests can spell them
// without re-deriving the byte assignments.
// -----------------------------------------------------------------------------

/// Tag byte for [`LayoutNode::Leaf`] on the wire.
pub(crate) const LAYOUT_TAG_LEAF: u8 = 0;
/// Tag byte for [`LayoutNode::Split`] on the wire.
pub(crate) const LAYOUT_TAG_SPLIT: u8 = 1;

/// Tag byte for [`SplitDir::Horizontal`] on the wire.
pub(crate) const SPLIT_DIR_HORIZONTAL: u8 = 0;
/// Tag byte for [`SplitDir::Vertical`] on the wire.
pub(crate) const SPLIT_DIR_VERTICAL: u8 = 1;

// -----------------------------------------------------------------------------
// SplitDir / LayoutNode
// -----------------------------------------------------------------------------

/// Axis along which a [`LayoutNode::Split`] divides its rectangle.
///
/// Wire-side mirror of `phux_core::window::SplitDir`. Duplication is
/// deliberate — see module docs for the core/protocol independence rationale.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SplitDir {
    /// Split side-by-side (a vertical bar between left and right).
    Horizontal = SPLIT_DIR_HORIZONTAL,
    /// Split stacked (a horizontal bar between top and bottom).
    Vertical = SPLIT_DIR_VERTICAL,
}

/// Wire-side mirror of `phux_core::window::LayoutNode`.
///
/// `Leaf` carries a single [`ResourceId`]; `Split` divides its rectangle between
/// two children along [`SplitDir`] at `ratio` (the left/top child gets
/// `ratio` of the parent dimension along the split axis).
///
/// The server-side bridge (parallel to the `IdBridge` pattern) converts
/// between this type and `phux_core::window::LayoutNode`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum LayoutNode {
    /// A single pane — recursion base.
    Leaf(ResourceId),
    /// An interior node that splits its rectangle in two.
    Split {
        /// The axis the split is taken along.
        dir: SplitDir,
        /// Fraction of the parent dim given to `left`, in the **closed**
        /// interval `0.0..=1.0`.
        ///
        /// Decoders reject NaN, infinite, or out-of-range values as
        /// [`DecodeError::MalformedLayoutRatio`], but admit the endpoints on
        /// purpose — wider than `phux_core`'s constructor-side open interval,
        /// because the reference TUI banks `resize-pane` ratios it has not
        /// applied yet (`phux_client_core::multi_pane::layout`, ADR-0048) and
        /// a transport that rejected `0.0`/`1.0` would drop legitimate client
        /// state. That divergence is deliberate and mapped in
        /// `crates/phux/tests/conformance/layout_conformance.rs`.
        ratio: f32,
        /// Left (for [`SplitDir::Horizontal`]) or top (for [`SplitDir::Vertical`]) child.
        left: Box<Self>,
        /// Right (for [`SplitDir::Horizontal`]) or bottom (for [`SplitDir::Vertical`]) child.
        right: Box<Self>,
    },
}

// -----------------------------------------------------------------------------
// SessionInfo / WindowInfo / ResourceInfo / SessionSnapshot
// -----------------------------------------------------------------------------

/// Description of a single session, sufficient for UI chrome and `phux ls`.
///
/// Excludes the windows themselves — those are flattened into
/// [`SessionSnapshot::windows`] and joined via `WindowInfo::session_id`.
///
/// Marked `#[non_exhaustive]` so additive field growth (process info, last-
/// attach timestamp, ...) is non-breaking. Construct via [`Self::new`] plus
/// the `with_*` setters; field-literal syntax is reserved for the crate's
/// own decoder and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SessionInfo {
    /// Stable session identifier.
    pub id: SessionId,
    /// Human-readable name; `AttachTarget::ByName` matches against this.
    pub name: String,
    /// Session's remembered focused window. Distinct from
    /// [`SessionSnapshot::focused_window`] — that one tracks the attaching
    /// client's current focus; this one tracks the session's "last known"
    /// focus, restored when a client attaches with no fresher signal.
    pub active_window: Option<WindowId>,
    /// Wall-clock creation time as seconds since the Unix epoch.
    ///
    /// `i64` (not `u64`) is the cross-language standard for Unix time and
    /// costs nothing in bytes; signedness leaves room for sub-1970 cases
    /// future implementations might dream up (none today).
    pub created_at_unix_secs: i64,
    /// Number of windows in this session.
    ///
    /// Denormalized at snapshot time so `phux ls` and status widgets can
    /// render without walking the windows list. Not stored long-term in
    /// core; computed on snapshot construction.
    pub window_count: u16,
    /// Number of clients currently attached to this session.
    ///
    /// Drives multi-attach UX (status-bar indicators, etc.). Like
    /// `window_count`, denormalized at snapshot time.
    pub attached_client_count: u16,
    /// Whether the session survives its last window (ADR-0105).
    ///
    /// A keep-empty session is not reaped when its last window closes; only
    /// an explicit kill removes it. Rides the snapshot's trailing session
    /// facet list (see [`SessionSnapshot`]), so an older peer decodes it as
    /// `false`. Advertised by `ServerFeature::KeepEmptySessions`.
    pub keep_empty: bool,
}

impl SessionInfo {
    /// Construct a `SessionInfo` from its load-bearing fields.
    ///
    /// `active_window`, `created_at_unix_secs`, `window_count`, and
    /// `attached_client_count` default to "unknown" sentinels (`None` / `0`);
    /// `keep_empty` defaults to `false`. Fill them via the `with_*` setters
    /// when the server has the data.
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
        }
    }

    /// Builder setter for [`Self::keep_empty`].
    #[must_use]
    pub const fn with_keep_empty(mut self, keep_empty: bool) -> Self {
        self.keep_empty = keep_empty;
        self
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

/// Description of a single window, sufficient for tab/pane chrome.
///
/// Excludes the resources themselves — those are flattened into
/// [`SessionSnapshot::resources`] and joined via `ResourceInfo::window_id`.
///
/// `#[non_exhaustive]`; construct via [`Self::new`] plus `with_*` setters.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct WindowInfo {
    /// Stable window identifier.
    pub id: WindowId,
    /// Foreign key into [`SessionSnapshot::sessions`].
    pub session_id: SessionId,
    /// Position within the session's windows list.
    ///
    /// Not stored in `phux_core::Window` today; computed at snapshot time
    /// as the position of this window's id in `session.windows`. Tmux-style
    /// numeric indices (`Ctrl-b 2`) bind against this.
    pub index: u16,
    /// Human-readable window name.
    pub name: String,
    /// Window's remembered focused pane.
    pub active_resource: Option<ResourceId>,
    /// Pane layout as a binary split tree.
    ///
    /// `None` iff this window has no resources — `SessionSnapshot::resources`
    /// filtered by `window_id` will be empty.
    pub layout: Option<LayoutNode>,
}

impl WindowInfo {
    /// Construct a `WindowInfo` from its load-bearing fields.
    ///
    /// `index` defaults to `0`; `active_resource` and `layout` default to
    /// `None`. Use the `with_*` setters to fill them when meaningful.
    #[must_use]
    pub fn new(id: WindowId, session_id: SessionId, name: impl Into<String>) -> Self {
        Self {
            id,
            session_id,
            index: 0,
            name: name.into(),
            active_resource: None,
            layout: None,
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

    /// Builder setter for [`Self::layout`].
    #[must_use]
    pub fn with_layout(mut self, layout: Option<LayoutNode>) -> Self {
        self.layout = layout;
        self
    }
}

/// The agent-session facet of a [`ResourceKind::AgentSession`] resource, as
/// carried in the snapshot.
///
/// `#[non_exhaustive]`; construct via [`Self::new`] plus `with_*` setters.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AgentFacet {
    /// Agent provider name, e.g. `claude`.
    pub provider: String,
    /// Opaque provider-native session id, when the producer supplied one.
    pub native_id: Option<String>,
    /// Server-derived lifecycle state as an open lower-case string
    /// (`working`, `blocked`, `done`, `idle`, `unknown`, ...). A consumer
    /// treats an unrecognised value as `unknown`.
    pub state: String,
}

impl AgentFacet {
    /// Construct an `AgentFacet` from its provider and derived state.
    /// `native_id` defaults to `None`.
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

/// Description of a single served resource, sufficient for layout chrome.
///
/// Every resource the server serves has an entry here, whatever its
/// [`ResourceKind`]; the name is the Terminal-era one and stays until the
/// wire rename. For a Terminal the entry carries its grid and window. For a
/// non-Terminal kind the Terminal facet is absent, which the positional
/// prefix encodes as `window_id = WindowId(0)` and `cols = rows = 0`: no
/// window owns such a resource and it has no grid, and a `WindowId` of zero
/// is never allocated. Consumers key layout on [`Self::kind`], not on those
/// sentinels.
///
/// Excludes grid contents, cursor state, scrollback, and process info.
/// Grid contents and retained history flow through separate bootstrap/history
/// streams. Process info (PID, command, exit status) is not yet modeled in
/// `phux_core::TerminalDescriptor`; adding wire fields the server can only send
/// `None` for is premature. Revisit when core grows process tracking.
///
/// [`Self::kind`], [`Self::parent`], and [`Self::agent`] are additive: the
/// positional per-entry prefix is unchanged, and the snapshot carries the
/// non-default values in one trailing list decoded with the `at_body_end`
/// convention (see [`SessionSnapshot`]), so a Terminal-only snapshot is
/// byte-identical to one encoded before the fields existed and a snapshot
/// from a peer that predates them decodes with every entry at the defaults.
///
/// `#[non_exhaustive]`; construct via [`Self::new`] plus `with_*` setters.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ResourceInfo {
    /// Stable resource identifier.
    pub id: ResourceId,
    /// Foreign key into [`SessionSnapshot::windows`]; `WindowId(0)` for a
    /// non-Terminal kind, which no window owns.
    pub window_id: WindowId,
    /// Current grid width in cells (from `core::TerminalDescriptor::dims.0`);
    /// `0` for a non-Terminal kind.
    pub cols: u16,
    /// Current grid height in cells (from `core::TerminalDescriptor::dims.1`);
    /// `0` for a non-Terminal kind.
    pub rows: u16,
    /// User-set title, distinct from any title the shell may set.
    pub title: Option<String>,
    /// Working directory as a UTF-8 string.
    ///
    /// `phux_core::TerminalDescriptor::cwd` is `PathBuf`; conversion uses
    /// `to_string_lossy().into_owned()`. Lossy on non-UTF-8 cwds (rare on
    /// modern systems) and acceptable for a display field.
    pub cwd: Option<String>,
    /// What backs the resource. Defaults to [`ResourceKind::Terminal`].
    pub kind: ResourceKind,
    /// The resource this one is bound to, when it is a child. Set at spawn
    /// and immutable; closing the parent closes the child.
    pub parent: Option<ResourceId>,
    /// The agent-session facet, present iff `kind` is
    /// [`ResourceKind::AgentSession`].
    pub agent: Option<AgentFacet>,
}

impl ResourceInfo {
    /// Construct a `ResourceInfo` from its load-bearing fields.
    ///
    /// `title` and `cwd` default to `None`; `kind` to `Terminal`; `parent`
    /// and `agent` to `None`. Set them via the `with_*` helpers when the
    /// server has the data.
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
        }
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

    /// Whether this entry carries anything beyond the Terminal-era
    /// positional prefix, i.e. whether the snapshot's trailing resource
    /// facet list needs a row for it.
    const fn has_resource_facets(&self) -> bool {
        !self.kind.is_terminal() || self.parent.is_some() || self.agent.is_some()
    }
}

/// One session on a federation satellite, as a hub lists it in
/// [`SessionSnapshot::hosts`].
///
/// The hub never renumbers a satellite session into its own id space:
/// [`Self::id`] is the satellite-local [`SessionId`], meaningful only on
/// that satellite and never joined against [`SessionSnapshot::sessions`].
/// [`Self::active_resource`] is the one routable handle, re-tagged
/// `SATELLITE { host, id }` so every relayed verb reaches it through the hub.
///
/// `#[non_exhaustive]`; construct via [`Self::new`] plus `with_*` setters.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HostSessionInfo {
    /// The satellite-local session id. Opaque to the hub and to consumers.
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
    /// The session's remembered focused pane, re-tagged `SATELLITE`, when
    /// the satellite reported one.
    pub active_resource: Option<ResourceId>,
}

impl HostSessionInfo {
    /// Construct a `HostSessionInfo` from its id and name; counts default to
    /// `0`, `created_at_unix_secs` to `0`, `active_resource` to `None`.
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

/// One federation satellite's row in [`SessionSnapshot::hosts`]: its
/// sessions, or why the hub could not list them.
///
/// A satellite that could not be reached stays in the inventory with
/// [`Self::unreachable`] set and no sessions, so a consumer can show it as
/// degraded instead of letting it disappear.
///
/// `#[non_exhaustive]`; construct via [`Self::reachable`] or
/// [`Self::unreachable`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HostInventory {
    /// The hub-local satellite name, the same token `SATELLITE { host, .. }`
    /// ids carry.
    pub host: SatelliteHost,
    /// `Some(diagnostic)` when the hub could not list this satellite. The
    /// text is the hub's prose; branch on presence, not on content.
    pub unreachable: Option<String>,
    /// The satellite's sessions, in the order it reported them. Empty when
    /// [`Self::unreachable`] is set.
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

/// Flat graph of sessions/windows/resources delivered with `ATTACHED`.
///
/// All three lists are joined by id. The triple of `focused_*` fields
/// records the **attaching client's** current focus — distinct from the
/// per-container `SessionInfo::active_window` / `WindowInfo::active_resource`,
/// which record the container's remembered focus from when no client was
/// attached (tmux behavior: detach → attach later restores last focus).
///
/// # Wire shape and the trailing resource facets
///
/// The snapshot is positional: three `u32`-counted lists, then the focus
/// triple. After `focused_resource` an encoder appends one more `u32`-counted
/// list, the *resource facets*, with one row per `resources` entry whose
/// [`ResourceInfo::kind`], [`ResourceInfo::parent`], or
/// [`ResourceInfo::agent`] is non-default:
///
/// ```text
/// facet_row = id: ResourceId
///          || kind: u8
///          || parent: optional<ResourceId>
///          || agent: optional<provider: str || native_id: optional<str> || state: str>
/// ```
///
/// The list is written only when it would be non-empty, so a Terminal-only
/// snapshot is byte-identical to one encoded before it existed. A decoder
/// reads it only when bytes remain in the enclosing field (`at_body_end`),
/// so a snapshot from an older peer decodes with every entry at the defaults,
/// and an older decoder stops at `focused_resource` and never sees the list.
/// Rows are joined onto `resources` by id on decode; a row naming no entry is
/// ignored. This is the trailing-additive convention of
/// `docs/spec/appendix-encoding.md` §2 applied at the one place in the
/// snapshot where a trailing value is unambiguous: a per-entry suffix would
/// not be, because the next entry's id tag follows it.
///
/// # The trailing host-session inventory
///
/// After the facet list an encoder appends a second `u32`-counted list,
/// [`Self::hosts`], one row per federation satellite
/// (`ServerFeature::HostSessions`):
///
/// ```text
/// host_row     = host: str || unreachable: optional<str>
///             || sessions: u32-counted list of host_session
/// host_session = id: u32 || name: str || created_at_unix_secs: i64
///             || window_count: u16 || pane_count: u16
///             || attached_client_count: u16 || active_resource: optional<ResourceId>
/// ```
///
/// # The trailing session facets
///
/// After the host inventory an encoder appends a third `u32`-counted list,
/// the *session facets* (ADR-0105, `ServerFeature::KeepEmptySessions`), one
/// row per `sessions` entry whose [`SessionInfo::keep_empty`] is set:
///
/// ```text
/// session_row = id: SessionId (u32) || flags: u8   // bit 0 = keep_empty
/// ```
///
/// # Order of the trailing lists
///
/// The order is fixed: resource facets, then hosts, then session facets.
/// Each list is written only when it or a later list is non-empty, and every
/// earlier list is then written explicitly, with a zero count when it has no
/// rows, so a later list never aliases an earlier one. A decoder reads each
/// list only while bytes remain. A snapshot with no later list is therefore
/// byte-identical to one encoded before that list existed, and an older
/// decoder that stops after the facets or after the hosts is still correct.
/// Unknown session-facet flag bits and rows naming no session are ignored.
///
/// `#[non_exhaustive]`; construct via [`Self::new`] plus `with_*` setters.
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
    /// The host-session inventory: one row per federation satellite, filled
    /// by a hub's `GET_STATE` and empty everywhere else. Satellite sessions
    /// never enter [`Self::sessions`]; their ids are satellite-local.
    ///
    /// Read it through [`Self::hosts`]; `None` is the empty inventory, and
    /// [`Self::with_hosts`] and the decoder never store `Some` of an empty
    /// slice, so two equal inventories compare equal.
    ///
    /// An optional boxed slice rather than a `Vec`: it is empty in every
    /// snapshot but a hub's `GET_STATE` reply, the empty case does not
    /// allocate and stays `const`-constructible, and the eight bytes it
    /// saves keep `CommandResult` (which carries this snapshot inline) under
    /// the large-`Err` size the server's `Result<_, CommandResult>` helpers
    /// are held to.
    pub hosts: Option<Box<[HostInventory]>>,
}

impl SessionSnapshot {
    /// Construct a `SessionSnapshot` from the attaching client's initial
    /// focus triple. Lists default to empty; populate them via the `with_*`
    /// setters.
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
            hosts: None,
        }
    }

    /// The host-session inventory (see the field docs): one row per
    /// federation satellite, empty unless a hub filled it.
    #[must_use]
    pub fn hosts(&self) -> &[HostInventory] {
        self.hosts.as_deref().unwrap_or(&[])
    }

    /// Builder setter for [`Self::hosts`]. An empty list stores `None`.
    #[must_use]
    pub fn with_hosts(mut self, hosts: Vec<HostInventory>) -> Self {
        self.hosts = boxed_hosts(hosts);
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

// -----------------------------------------------------------------------------
// Encoding helpers. Positional; same conventions as `wire::frame`.
// docs/spec/appendix-encoding.md mandates TLV — tracked in phux-i58.
// -----------------------------------------------------------------------------

pub(super) const fn encode_split_dir(dir: SplitDir) -> u8 {
    match dir {
        SplitDir::Horizontal => SPLIT_DIR_HORIZONTAL,
        SplitDir::Vertical => SPLIT_DIR_VERTICAL,
    }
}

pub(super) fn decode_split_dir(tag: u8) -> Result<SplitDir, DecodeError> {
    match tag {
        SPLIT_DIR_HORIZONTAL => Ok(SplitDir::Horizontal),
        SPLIT_DIR_VERTICAL => Ok(SplitDir::Vertical),
        other => Err(DecodeError::UnknownEnumValue {
            field: "SplitDir",
            value: u32::from(other),
        }),
    }
}

/// Encode a layout subtree. Tag byte selects `Leaf` (0) vs `Split` (1);
/// `Split` recurses into both children.
pub(super) fn encode_layout_node(node: &LayoutNode, enc: &mut Encoder<'_>) {
    match node {
        LayoutNode::Leaf(pane) => {
            enc.write_u8(LAYOUT_TAG_LEAF);
            encode_terminal_id(pane, enc);
        }
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => {
            enc.write_u8(LAYOUT_TAG_SPLIT);
            enc.write_u8(encode_split_dir(*dir));
            enc.write_f32_be(*ratio);
            encode_layout_node(left, enc);
            encode_layout_node(right, enc);
        }
    }
}

/// Maximum nesting depth the layout-tree decoder will follow before
/// rejecting the input with [`DecodeError::LayoutTooDeep`].
///
/// The codec is recursive (`Split` carries two child subtrees), so an
/// unbounded tree of attacker-controlled bytes would overflow the stack and
/// abort the process — a 16 MiB frame admits millions of `Split` levels at
/// roughly six bytes each. A real terminal layout nests only as deep as the
/// user has split resources (tens at the very most); `64` is comfortably above
/// any legitimate value while keeping the worst-case decode recursion shallow
/// enough to never approach the stack limit.
pub const MAX_LAYOUT_DEPTH: usize = 64;

/// Decode a layout subtree. Validates `Split.ratio` to reject NaN, infinite,
/// or out-of-range values that would otherwise round-trip but be useless, and
/// bounds recursion at [`MAX_LAYOUT_DEPTH`] so a pathologically deep tree
/// errors cleanly instead of overflowing the stack.
pub(super) fn decode_layout_node(dec: &mut Decoder<'_>) -> Result<LayoutNode, DecodeError> {
    decode_layout_node_depth(dec, 0)
}

fn decode_layout_node_depth(
    dec: &mut Decoder<'_>,
    depth: usize,
) -> Result<LayoutNode, DecodeError> {
    if depth >= MAX_LAYOUT_DEPTH {
        return Err(DecodeError::LayoutTooDeep);
    }
    let tag = dec.read_u8()?;
    match tag {
        LAYOUT_TAG_LEAF => {
            let pane = decode_terminal_id(dec)?;
            Ok(LayoutNode::Leaf(pane))
        }
        LAYOUT_TAG_SPLIT => {
            let dir = decode_split_dir(dec.read_u8()?)?;
            let ratio = dec.read_f32_be()?;
            if !ratio.is_finite() || !(0.0..=1.0).contains(&ratio) {
                return Err(DecodeError::MalformedLayoutRatio { ratio });
            }
            let left = Box::new(decode_layout_node_depth(dec, depth + 1)?);
            let right = Box::new(decode_layout_node_depth(dec, depth + 1)?);
            Ok(LayoutNode::Split {
                dir,
                ratio,
                left,
                right,
            })
        }
        other => Err(DecodeError::UnknownEnumValue {
            field: "LayoutNode",
            value: u32::from(other),
        }),
    }
}

pub(super) fn encode_option_layout_node(node: Option<&LayoutNode>, enc: &mut Encoder<'_>) {
    match node {
        None => enc.write_u8(0),
        Some(n) => {
            enc.write_u8(1);
            encode_layout_node(n, enc);
        }
    }
}

pub(super) fn decode_option_layout_node(
    dec: &mut Decoder<'_>,
) -> Result<Option<LayoutNode>, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        0 => Ok(None),
        1 => Ok(Some(decode_layout_node(dec)?)),
        other => Err(DecodeError::UnknownEnumValue {
            field: "Option<LayoutNode> tag",
            value: u32::from(other),
        }),
    }
}

pub(super) fn encode_session_info(info: &SessionInfo, enc: &mut Encoder<'_>) {
    enc.write_u32_be(info.id.get());
    enc.write_str(&info.name);
    encode_option_window_id(info.active_window, enc);
    enc.write_i64_be(info.created_at_unix_secs);
    enc.write_u16_be(info.window_count);
    enc.write_u16_be(info.attached_client_count);
}

pub(super) fn decode_session_info(dec: &mut Decoder<'_>) -> Result<SessionInfo, DecodeError> {
    let id = SessionId::new(dec.read_u32_be()?);
    let name = dec.read_str()?.to_owned();
    let active_window = decode_option_window_id(dec)?;
    let created_at_unix_secs = dec.read_i64_be()?;
    let window_count = dec.read_u16_be()?;
    let attached_client_count = dec.read_u16_be()?;
    Ok(SessionInfo {
        id,
        name,
        active_window,
        created_at_unix_secs,
        window_count,
        attached_client_count,
        // Not positional: it rides the snapshot's trailing session facets.
        keep_empty: false,
    })
}

pub(super) fn encode_window_info(info: &WindowInfo, enc: &mut Encoder<'_>) {
    enc.write_u32_be(info.id.get());
    enc.write_u32_be(info.session_id.get());
    enc.write_u16_be(info.index);
    enc.write_str(&info.name);
    encode_option_terminal_id(info.active_resource.as_ref(), enc);
    encode_option_layout_node(info.layout.as_ref(), enc);
}

pub(super) fn decode_window_info(dec: &mut Decoder<'_>) -> Result<WindowInfo, DecodeError> {
    let id = WindowId::new(dec.read_u32_be()?);
    let session_id = SessionId::new(dec.read_u32_be()?);
    let index = dec.read_u16_be()?;
    let name = dec.read_str()?.to_owned();
    let active_resource = decode_option_terminal_id(dec)?;
    let layout = decode_option_layout_node(dec)?;
    Ok(WindowInfo {
        id,
        session_id,
        index,
        name,
        active_resource,
        layout,
    })
}

pub(super) fn encode_terminal_info(info: &ResourceInfo, enc: &mut Encoder<'_>) {
    encode_terminal_id(&info.id, enc);
    enc.write_u32_be(info.window_id.get());
    enc.write_u16_be(info.cols);
    enc.write_u16_be(info.rows);
    encode_option_str(info.title.as_deref(), enc);
    encode_option_str(info.cwd.as_deref(), enc);
}

pub(super) fn decode_terminal_info(dec: &mut Decoder<'_>) -> Result<ResourceInfo, DecodeError> {
    let id = decode_terminal_id(dec)?;
    let window_id = WindowId::new(dec.read_u32_be()?);
    let cols = dec.read_u16_be()?;
    let rows = dec.read_u16_be()?;
    let title = decode_option_str(dec)?.map(str::to_owned);
    let cwd = decode_option_str(dec)?.map(str::to_owned);
    Ok(ResourceInfo {
        id,
        window_id,
        cols,
        rows,
        title,
        cwd,
        kind: ResourceKind::Terminal,
        parent: None,
        agent: None,
    })
}

/// Session-facet flag bit: the session is keep-empty (ADR-0105).
const SESSION_FACET_KEEP_EMPTY: u8 = 0x01;

/// Write the trailing resource-facet list (see [`SessionSnapshot`]), or
/// nothing when every entry is a plain Terminal and no later trailing list
/// (`more_follow`) needs the facet count as its positional anchor.
fn encode_resource_facets(resources: &[ResourceInfo], more_follow: bool, enc: &mut Encoder<'_>) {
    let rows = resources.iter().filter(|p| p.has_resource_facets()).count();
    if rows == 0 && !more_follow {
        return;
    }
    encode_list_len(rows, enc);
    for pane in resources.iter().filter(|p| p.has_resource_facets()) {
        encode_terminal_id(&pane.id, enc);
        enc.write_u8(pane.kind.as_wire());
        encode_option_terminal_id(pane.parent.as_ref(), enc);
        match &pane.agent {
            None => enc.write_u8(0),
            Some(agent) => {
                enc.write_u8(1);
                enc.write_str(&agent.provider);
                encode_option_str(agent.native_id.as_deref(), enc);
                enc.write_str(&agent.state);
            }
        }
    }
}

/// Read the trailing resource-facet list if the enclosing field has bytes
/// left, joining each row onto its `resources` entry by id.
fn decode_resource_facets(
    dec: &mut Decoder<'_>,
    resources: &mut [ResourceInfo],
) -> Result<(), DecodeError> {
    if dec.at_body_end() {
        return Ok(());
    }
    let rows = decode_list_len(dec)?;
    for _ in 0..rows {
        let id = decode_terminal_id(dec)?;
        let kind = ResourceKind::from_wire(dec.read_u8()?);
        let parent = decode_option_terminal_id(dec)?;
        let agent = match dec.read_u8()? {
            0 => None,
            1 => {
                let provider = dec.read_str()?.to_owned();
                let native_id = decode_option_str(dec)?.map(str::to_owned);
                let state = dec.read_str()?.to_owned();
                Some(AgentFacet {
                    provider,
                    native_id,
                    state,
                })
            }
            other => {
                return Err(DecodeError::UnknownEnumValue {
                    field: "Option<AgentFacet> tag",
                    value: u32::from(other),
                });
            }
        };
        if let Some(pane) = resources.iter_mut().find(|p| p.id == id) {
            pane.kind = kind;
            pane.parent = parent;
            pane.agent = agent;
        }
    }
    Ok(())
}

pub(super) fn encode_session_snapshot(snap: &SessionSnapshot, enc: &mut Encoder<'_>) {
    encode_list_len(snap.sessions.len(), enc);
    for s in &snap.sessions {
        encode_session_info(s, enc);
    }
    encode_list_len(snap.windows.len(), enc);
    for w in &snap.windows {
        encode_window_info(w, enc);
    }
    encode_list_len(snap.resources.len(), enc);
    for p in &snap.resources {
        encode_terminal_info(p, enc);
    }
    enc.write_u32_be(snap.focused_session.get());
    enc.write_u32_be(snap.focused_window.get());
    encode_terminal_id(&snap.focused_resource, enc);
    let session_rows = snap.sessions.iter().filter(|s| s.keep_empty).count();
    let session_facets_follow = session_rows > 0;
    let hosts_follow = !snap.hosts().is_empty() || session_facets_follow;
    encode_resource_facets(&snap.resources, hosts_follow, enc);
    encode_host_inventory(snap.hosts(), session_facets_follow, enc);
    encode_session_facets(&snap.sessions, session_rows, enc);
}

/// Store an inventory canonically: `None` when empty, so a snapshot built
/// with no hosts and one decoded without the trailing list compare equal.
fn boxed_hosts(hosts: Vec<HostInventory>) -> Option<Box<[HostInventory]>> {
    (!hosts.is_empty()).then(|| hosts.into_boxed_slice())
}

/// Write the trailing host-session inventory (see [`SessionSnapshot`]), or
/// nothing when it is empty and no later trailing list (`more_follow`)
/// needs its count as a positional anchor.
fn encode_host_inventory(hosts: &[HostInventory], more_follow: bool, enc: &mut Encoder<'_>) {
    if hosts.is_empty() && !more_follow {
        return;
    }
    encode_list_len(hosts.len(), enc);
    for row in hosts {
        enc.write_str(row.host.as_str());
        encode_option_str(row.unreachable.as_deref(), enc);
        encode_list_len(row.sessions.len(), enc);
        for session in &row.sessions {
            encode_host_session(session, enc);
        }
    }
}

fn encode_host_session(session: &HostSessionInfo, enc: &mut Encoder<'_>) {
    enc.write_u32_be(session.id.get());
    enc.write_str(&session.name);
    enc.write_i64_be(session.created_at_unix_secs);
    enc.write_u16_be(session.window_count);
    enc.write_u16_be(session.pane_count);
    enc.write_u16_be(session.attached_client_count);
    encode_option_terminal_id(session.active_resource.as_ref(), enc);
}

/// Read the trailing host-session inventory if the enclosing field has bytes
/// left; an absent list is an empty inventory.
fn decode_host_inventory(dec: &mut Decoder<'_>) -> Result<Vec<HostInventory>, DecodeError> {
    if dec.at_body_end() {
        return Ok(Vec::new());
    }
    let rows = decode_list_len(dec)?;
    let mut hosts = dec.bounded_capacity(rows);
    for _ in 0..rows {
        let host = SatelliteHost::new(dec.read_str()?);
        let unreachable = decode_option_str(dec)?.map(str::to_owned);
        let count = decode_list_len(dec)?;
        let mut sessions = dec.bounded_capacity(count);
        for _ in 0..count {
            sessions.push(decode_host_session(dec)?);
        }
        hosts.push(HostInventory {
            host,
            unreachable,
            sessions,
        });
    }
    Ok(hosts)
}

fn decode_host_session(dec: &mut Decoder<'_>) -> Result<HostSessionInfo, DecodeError> {
    let id = SessionId::new(dec.read_u32_be()?);
    let name = dec.read_str()?.to_owned();
    let created_at_unix_secs = dec.read_i64_be()?;
    let window_count = dec.read_u16_be()?;
    let pane_count = dec.read_u16_be()?;
    let attached_client_count = dec.read_u16_be()?;
    let active_resource = decode_option_terminal_id(dec)?;
    Ok(HostSessionInfo {
        id,
        name,
        created_at_unix_secs,
        window_count,
        pane_count,
        attached_client_count,
        active_resource,
    })
}

/// Write the trailing session-facet list (see [`SessionSnapshot`]), or
/// nothing when no session carries a facet.
fn encode_session_facets(sessions: &[SessionInfo], rows: usize, enc: &mut Encoder<'_>) {
    if rows == 0 {
        return;
    }
    encode_list_len(rows, enc);
    for session in sessions.iter().filter(|s| s.keep_empty) {
        enc.write_u32_be(session.id.get());
        enc.write_u8(SESSION_FACET_KEEP_EMPTY);
    }
}

/// Read the trailing session-facet list if bytes remain, joining each row
/// onto its `sessions` entry by id. Unknown flag bits are ignored.
fn decode_session_facets(
    dec: &mut Decoder<'_>,
    sessions: &mut [SessionInfo],
) -> Result<(), DecodeError> {
    if dec.at_body_end() {
        return Ok(());
    }
    let rows = decode_list_len(dec)?;
    for _ in 0..rows {
        let id = SessionId::new(dec.read_u32_be()?);
        let flags = dec.read_u8()?;
        if let Some(session) = sessions.iter_mut().find(|s| s.id == id) {
            session.keep_empty = flags & SESSION_FACET_KEEP_EMPTY != 0;
        }
    }
    Ok(())
}

pub(super) fn decode_session_snapshot(
    dec: &mut Decoder<'_>,
) -> Result<SessionSnapshot, DecodeError> {
    // Clamp each list's reservation to the bytes remaining in the frame
    // body. Every element occupies multiple bytes on the wire, so remaining
    // bytes is a safe upper bound on element count; an over-declared length
    // errors on EOF in the read loop rather than pre-allocating gigabytes
    // (a decode-path DoS otherwise).
    let sessions_len = decode_list_len(dec)?;
    let mut sessions = dec.bounded_capacity(sessions_len);
    for _ in 0..sessions_len {
        sessions.push(decode_session_info(dec)?);
    }
    let windows_len = decode_list_len(dec)?;
    let mut windows = dec.bounded_capacity(windows_len);
    for _ in 0..windows_len {
        windows.push(decode_window_info(dec)?);
    }
    let resources_len = decode_list_len(dec)?;
    let mut resources = dec.bounded_capacity(resources_len);
    for _ in 0..resources_len {
        resources.push(decode_terminal_info(dec)?);
    }
    let focused_session = SessionId::new(dec.read_u32_be()?);
    let focused_window = WindowId::new(dec.read_u32_be()?);
    let focused_resource = decode_terminal_id(dec)?;
    decode_resource_facets(dec, &mut resources)?;
    let hosts = boxed_hosts(decode_host_inventory(dec)?);
    decode_session_facets(dec, &mut sessions)?;
    Ok(SessionSnapshot {
        sessions,
        windows,
        resources,
        focused_session,
        focused_window,
        focused_resource,
        hosts,
    })
}

// -----------------------------------------------------------------------------
// Small option-of-id and list-length helpers. Mirror the conventions used in
// `wire::frame` (presence byte + body, u32 length-prefixed lists).
// -----------------------------------------------------------------------------

pub(super) fn encode_option_window_id(value: Option<WindowId>, enc: &mut Encoder<'_>) {
    match value {
        None => enc.write_u8(0),
        Some(id) => {
            enc.write_u8(1);
            enc.write_u32_be(id.get());
        }
    }
}

pub(super) fn decode_option_window_id(
    dec: &mut Decoder<'_>,
) -> Result<Option<WindowId>, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        0 => Ok(None),
        1 => Ok(Some(WindowId::new(dec.read_u32_be()?))),
        other => Err(DecodeError::UnknownEnumValue {
            field: "Option<WindowId> tag",
            value: u32::from(other),
        }),
    }
}

pub(super) fn encode_option_terminal_id(value: Option<&ResourceId>, enc: &mut Encoder<'_>) {
    match value {
        None => enc.write_u8(0),
        Some(id) => {
            enc.write_u8(1);
            encode_terminal_id(id, enc);
        }
    }
}

pub(super) fn decode_option_terminal_id(
    dec: &mut Decoder<'_>,
) -> Result<Option<ResourceId>, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        0 => Ok(None),
        1 => Ok(Some(decode_terminal_id(dec)?)),
        other => Err(DecodeError::UnknownEnumValue {
            field: "Option<ResourceId> tag",
            value: u32::from(other),
        }),
    }
}

pub(super) fn encode_option_str(value: Option<&str>, enc: &mut Encoder<'_>) {
    match value {
        None => enc.write_u8(0),
        Some(s) => {
            enc.write_u8(1);
            enc.write_str(s);
        }
    }
}

pub(super) fn decode_option_str<'a>(dec: &mut Decoder<'a>) -> Result<Option<&'a str>, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        0 => Ok(None),
        1 => Ok(Some(dec.read_str()?)),
        other => Err(DecodeError::UnknownEnumValue {
            field: "Option<str> tag",
            value: u32::from(other),
        }),
    }
}

pub(super) fn encode_list_len(len: usize, enc: &mut Encoder<'_>) {
    debug_assert!(
        u32::try_from(len).is_ok(),
        "list length exceeds u32 (positional encoding cap)",
    );
    let len_u32 = u32::try_from(len).unwrap_or(u32::MAX);
    enc.write_u32_be(len_u32);
}

pub(super) fn decode_list_len(dec: &mut Decoder<'_>) -> Result<usize, DecodeError> {
    let len = dec.read_u32_be()?;
    usize::try_from(len).map_err(|_| DecodeError::LengthOverflow)
}

// -----------------------------------------------------------------------------
// ClientId option encoding — used in ATTACHED for `initial_client_id` once
// the server starts allocating, but the field itself is required, not optional,
// per SPEC §13. Kept here as a single source of truth for ClientId on the wire.
// -----------------------------------------------------------------------------

pub(super) fn encode_client_id(id: ClientId, enc: &mut Encoder<'_>) {
    enc.write_u32_be(id.get());
}

pub(super) fn decode_client_id(dec: &mut Decoder<'_>) -> Result<ClientId, DecodeError> {
    Ok(ClientId::new(dec.read_u32_be()?))
}
