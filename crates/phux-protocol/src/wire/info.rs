//! Snapshot-graph types delivered with `ATTACHED` (`docs/spec/L1.md` §7).
//!
//! Wire mirrors of `phux_core`'s session/window/layout shapes (this crate
//! cannot depend on `phux-core`): enough to render chrome and layout.
//! Terminal contents flow through the bootstrap streams.

use bytes::BytesMut;

use crate::ids::{ClientId, ResourceId, ResourceKind, SatelliteHost, SessionId, WindowId};

use super::decode::Decoder;
use super::encode::Encoder;
use super::error::DecodeError;
use super::field;
use super::frame::{
    CloseReason, ResourceLifecycle, decode_optional_i32, decode_terminal_id, encode_optional_i32,
    encode_terminal_id,
};

/// Tag byte for [`LayoutNode::Leaf`] on the wire.
pub(crate) const LAYOUT_TAG_LEAF: u8 = 0;
/// Tag byte for [`LayoutNode::Split`] on the wire.
pub(crate) const LAYOUT_TAG_SPLIT: u8 = 1;

/// Tag byte for [`SplitDir::Horizontal`] on the wire.
pub(crate) const SPLIT_DIR_HORIZONTAL: u8 = 0;
/// Tag byte for [`SplitDir::Vertical`] on the wire.
pub(crate) const SPLIT_DIR_VERTICAL: u8 = 1;

/// Axis along which a [`LayoutNode::Split`] divides its rectangle.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SplitDir {
    /// Split side-by-side (a vertical bar between left and right).
    Horizontal = SPLIT_DIR_HORIZONTAL,
    /// Split stacked (a horizontal bar between top and bottom).
    Vertical = SPLIT_DIR_VERTICAL,
}

/// Binary split tree of a window's panes; `Split` gives its left/top child
/// `ratio` of the parent along [`SplitDir`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum LayoutNode {
    /// A single pane — recursion base.
    Leaf(ResourceId),
    /// An interior node that splits its rectangle in two.
    Split {
        /// The axis the split is taken along.
        dir: SplitDir,
        /// Fraction given to `left`, in the closed interval `0.0..=1.0`.
        ///
        /// NaN, infinite, and out-of-range ratios are
        /// [`DecodeError::MalformedLayoutRatio`]. The endpoints are admitted
        /// because clients bank unapplied resize ratios (ADR-0048).
        ratio: f32,
        /// Left (for [`SplitDir::Horizontal`]) or top (for [`SplitDir::Vertical`]) child.
        left: Box<Self>,
        /// Right (for [`SplitDir::Horizontal`]) or bottom (for [`SplitDir::Vertical`]) child.
        right: Box<Self>,
    },
}

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
    /// trailing session facets, so older peers read `false`.
    pub keep_empty: bool,
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
    /// Pane layout; `None` iff the window has no resources.
    pub layout: Option<LayoutNode>,
}

impl WindowInfo {
    /// A `WindowInfo` at index `0` with no active resource or layout.
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
            lifecycle: ResourceLifecycle::Running,
            exit: None,
            input_holder: None,
            viewers: Vec::new(),
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
    const fn has_resource_facets(&self) -> bool {
        !self.kind.is_terminal() || self.parent.is_some() || self.agent.is_some()
    }

    /// Whether the extension block needs a `RESOURCE_STATE` entry for it.
    const fn has_resource_state(&self) -> bool {
        !matches!(self.lifecycle, ResourceLifecycle::Running)
            || self.exit.is_some()
            || self.input_holder.is_some()
            || !self.viewers.is_empty()
    }
}

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
    trail: Option<Box<SessionSnapshotTrail>>,
}

/// Trailing additive payload for [`SessionSnapshot`].
#[derive(Debug, Clone, PartialEq, Default)]
struct SessionSnapshotTrail {
    hosts: Box<[HostInventory]>,
    listeners: Option<crate::wire::listeners::RemoteListenersReport>,
    /// The journal head at the cut (extension block field 2, L1 §7.3).
    journal_head: Option<u64>,
}

impl SessionSnapshotTrail {
    fn is_empty(&self) -> bool {
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

    fn set_hosts(&mut self, hosts: Vec<HostInventory>) {
        self.edit_trail(|trail| trail.hosts = boxed_hosts(hosts).unwrap_or_default());
    }

    fn set_listeners(&mut self, listeners: Option<crate::wire::listeners::RemoteListenersReport>) {
        self.edit_trail(|trail| trail.listeners = listeners);
    }

    fn set_journal_head(&mut self, journal_head: Option<u64>) {
        self.edit_trail(|trail| trail.journal_head = journal_head);
    }

    /// Edit the trail, dropping it when left empty so equality is canonical.
    fn edit_trail(&mut self, edit: impl FnOnce(&mut SessionSnapshotTrail)) {
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

// Positional encoding helpers.

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
        other => Err(DecodeError::unknown_enum("SplitDir", other)),
    }
}

/// Encode a layout subtree (tag, then leaf id or split fields and children).
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

/// Maximum layout-tree depth the decoder follows before
/// [`DecodeError::LayoutTooDeep`], so hostile nesting cannot overflow the
/// stack; real layouts nest tens deep at most.
pub const MAX_LAYOUT_DEPTH: usize = 64;

/// Decode a layout subtree, validating ratios and bounding depth at
/// [`MAX_LAYOUT_DEPTH`].
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
        other => Err(DecodeError::unknown_enum("LayoutNode", other)),
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
    enc.write_option(info.layout.as_ref(), |e, n| encode_layout_node(n, e));
}

pub(super) fn decode_window_info(dec: &mut Decoder<'_>) -> Result<WindowInfo, DecodeError> {
    let id = WindowId::new(dec.read_u32_be()?);
    let session_id = SessionId::new(dec.read_u32_be()?);
    let index = dec.read_u16_be()?;
    let name = dec.read_str()?.to_owned();
    let active_resource = decode_option_terminal_id(dec)?;
    let layout = dec.read_option("Option<LayoutNode> tag", decode_layout_node)?;
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
        // Not positional: these ride the snapshot extension block.
        lifecycle: ResourceLifecycle::Running,
        exit: None,
        input_holder: None,
        viewers: Vec::new(),
    })
}

/// Session-facet flag bit: the session is keep-empty (ADR-0105).
const SESSION_FACET_KEEP_EMPTY: u8 = 0x01;

/// Write the trailing resource-facet list, or nothing when it is empty and no
/// later element (`more_follow`) needs it as a positional anchor.
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
        enc.write_option(pane.agent.as_ref(), |e, agent| {
            e.write_str(&agent.provider);
            encode_option_str(agent.native_id.as_deref(), e);
            e.write_str(&agent.state);
        });
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
        let agent = dec.read_option("Option<AgentFacet> tag", |d| {
            Ok(AgentFacet {
                provider: d.read_str()?.to_owned(),
                native_id: decode_option_str(d)?.map(str::to_owned),
                state: d.read_str()?.to_owned(),
            })
        })?;
        if let Some(pane) = resources.iter_mut().find(|p| p.id == id) {
            pane.kind = kind;
            pane.parent = parent;
            pane.agent = agent;
        }
    }
    Ok(())
}

pub(super) fn encode_session_snapshot(snap: &SessionSnapshot, enc: &mut Encoder<'_>) {
    encode_list(&snap.sessions, enc, encode_session_info);
    encode_list(&snap.windows, enc, encode_window_info);
    encode_list(&snap.resources, enc, encode_terminal_info);
    enc.write_u32_be(snap.focused_session.get());
    enc.write_u32_be(snap.focused_window.get());
    encode_terminal_id(&snap.focused_resource, enc);
    let extension = encode_snapshot_extension(&snap.resources, snap.journal_head());
    let extension_follows = !extension.is_empty();
    let session_rows = snap.sessions.iter().filter(|s| s.keep_empty).count();
    let listeners_follow = snap.listeners().is_some() || extension_follows;
    let session_facets_follow = session_rows > 0 || listeners_follow;
    let hosts_follow = !snap.hosts().is_empty() || session_facets_follow;
    encode_resource_facets(&snap.resources, hosts_follow, enc);
    encode_host_inventory(snap.hosts(), session_facets_follow, enc);
    encode_session_facets(&snap.sessions, session_rows, listeners_follow, enc);
    encode_listeners(snap.listeners(), extension_follows, enc);
    if extension_follows {
        enc.write_bytes(&extension);
    }
}

/// Build the snapshot extension block: `RESOURCE_STATE` per non-default
/// resource, then the journal head; empty means nothing is written.
fn encode_snapshot_extension(resources: &[ResourceInfo], journal_head: Option<u64>) -> BytesMut {
    let mut block = BytesMut::new();
    let mut enc = Encoder::new(&mut block);
    for info in resources.iter().filter(|r| r.has_resource_state()) {
        enc.write_field_with(field::snapshot_extension::RESOURCE_STATE, |e| {
            encode_resource_state(info, e);
        });
    }
    if let Some(head) = journal_head {
        enc.write_field_with(field::snapshot_extension::JOURNAL_HEAD, |e| {
            e.write_u64_be(head);
        });
    }
    block
}

/// One `RESOURCE_STATE` value: the positional id, then only the fields that
/// differ from the defaults.
fn encode_resource_state(info: &ResourceInfo, enc: &mut Encoder<'_>) {
    encode_terminal_id(&info.id, enc);
    if !matches!(info.lifecycle, ResourceLifecycle::Running) {
        enc.write_field(field::resource_state::LIFECYCLE, &[info.lifecycle.to_u8()]);
    }
    if let Some(exit) = &info.exit {
        enc.write_field_with(field::resource_state::EXIT, |e| encode_exit_facet(exit, e));
    }
    if let Some(holder) = info.input_holder {
        enc.write_field_with(field::resource_state::INPUT_HOLDER, |e| {
            encode_client_id(holder, e);
        });
    }
    for viewer in &info.viewers {
        enc.write_field_with(field::resource_state::VIEWER, |e| {
            encode_client_id(*viewer, e);
        });
    }
}

/// `ExitFacet`, positional: `exit_status: optional<i32> || signal:
/// optional<i32> || reason: u8 || exited_at_ms: u64 || retained_until_ms: u64`.
fn encode_exit_facet(exit: &ExitFacet, enc: &mut Encoder<'_>) {
    encode_optional_i32(exit.exit_status, enc);
    encode_optional_i32(exit.signal, enc);
    enc.write_u8(exit.reason.as_wire());
    enc.write_u64_be(exit.exited_at_ms);
    enc.write_u64_be(exit.retained_until_ms);
}

fn decode_exit_facet(dec: &mut Decoder<'_>) -> Result<ExitFacet, DecodeError> {
    let exit_status = decode_optional_i32(dec)?;
    let signal = decode_optional_i32(dec)?;
    let reason = CloseReason::from_wire(dec.read_u8()?);
    let exited_at_ms = dec.read_u64_be()?;
    let retained_until_ms = dec.read_u64_be()?;
    Ok(ExitFacet::new(exited_at_ms, retained_until_ms)
        .with_exit_status(exit_status)
        .with_signal(signal)
        .with_reason(reason))
}

/// Read the extension block if bytes remain, joining each `RESOURCE_STATE`
/// by id, and return its journal head.
fn decode_snapshot_extension(
    dec: &mut Decoder<'_>,
    resources: &mut [ResourceInfo],
) -> Result<Option<u64>, DecodeError> {
    if dec.at_body_end() {
        return Ok(None);
    }
    let mut journal_head = None;
    let mut block = Decoder::new(dec.read_bytes()?);
    while let Some((id, value)) = block.read_field()? {
        match id {
            field::snapshot_extension::RESOURCE_STATE => apply_resource_state(value, resources)?,
            field::snapshot_extension::JOURNAL_HEAD => {
                journal_head = Some(Decoder::new(value).read_u64_be()?);
            }
            _ => {}
        }
    }
    Ok(journal_head)
}

/// Join one `RESOURCE_STATE` value onto its entry (ignored when none); an
/// unknown lifecycle byte reads as `Running`.
fn apply_resource_state(value: &[u8], resources: &mut [ResourceInfo]) -> Result<(), DecodeError> {
    let mut dec = Decoder::new(value);
    let id = decode_terminal_id(&mut dec)?;
    let mut lifecycle = ResourceLifecycle::Running;
    let mut exit = None;
    let mut input_holder = None;
    let mut viewers = Vec::new();
    while let Some((field_id, v)) = dec.read_field()? {
        let mut v = Decoder::new(v);
        match field_id {
            field::resource_state::VIEWER => viewers.push(decode_client_id(&mut v)?),
            field::resource_state::LIFECYCLE => {
                lifecycle = ResourceLifecycle::from_u8(v.read_u8()?).unwrap_or_default();
            }
            field::resource_state::EXIT => exit = Some(decode_exit_facet(&mut v)?),
            field::resource_state::INPUT_HOLDER => input_holder = Some(decode_client_id(&mut v)?),
            _ => {}
        }
    }
    if let Some(info) = resources.iter_mut().find(|r| r.id == id) {
        info.lifecycle = lifecycle;
        info.exit = exit;
        info.input_holder = input_holder;
        info.viewers = viewers;
    }
    Ok(())
}

/// Store an inventory canonically: `None` when empty, so a snapshot built
/// with no hosts and one decoded without the trailing list compare equal.
fn boxed_hosts(hosts: Vec<HostInventory>) -> Option<Box<[HostInventory]>> {
    (!hosts.is_empty()).then(|| hosts.into_boxed_slice())
}

/// Write the trailing host inventory, or nothing when it is empty and no
/// later element needs it as an anchor.
fn encode_host_inventory(hosts: &[HostInventory], more_follow: bool, enc: &mut Encoder<'_>) {
    if hosts.is_empty() && !more_follow {
        return;
    }
    encode_list(hosts, enc, |row, enc| {
        enc.write_str(row.host.as_str());
        encode_option_str(row.unreachable.as_deref(), enc);
        encode_list(&row.sessions, enc, encode_host_session);
    });
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
    decode_list(dec, |dec| {
        Ok(HostInventory {
            host: SatelliteHost::new(dec.read_str()?),
            unreachable: decode_option_str(dec)?.map(str::to_owned),
            sessions: decode_list(dec, decode_host_session)?,
        })
    })
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

/// Write the trailing session-facet list, or nothing when it is empty and no
/// later element needs it as an anchor.
fn encode_session_facets(
    sessions: &[SessionInfo],
    rows: usize,
    more_follow: bool,
    enc: &mut Encoder<'_>,
) {
    if rows == 0 && !more_follow {
        return;
    }
    encode_list_len(rows, enc);
    for session in sessions.iter().filter(|s| s.keep_empty) {
        enc.write_u32_be(session.id.get());
        enc.write_u8(SESSION_FACET_KEEP_EMPTY);
    }
}

/// Write the trailing listeners JSON; an unset report writes nothing, or an
/// absent marker when the extension block follows.
fn encode_listeners(
    report: Option<&crate::wire::listeners::RemoteListenersReport>,
    more_follow: bool,
    enc: &mut Encoder<'_>,
) {
    let Some(report) = report else {
        if more_follow {
            encode_option_str(None, enc);
        }
        return;
    };
    let json = report.to_json();
    encode_option_str(Some(json.as_str()), enc);
}

/// Read the trailing remote-listeners JSON if bytes remain.
fn decode_listeners(
    dec: &mut Decoder<'_>,
) -> Result<Option<crate::wire::listeners::RemoteListenersReport>, DecodeError> {
    if dec.at_body_end() {
        return Ok(None);
    }
    Ok(decode_option_str(dec)?.and_then(crate::wire::listeners::RemoteListenersReport::from_json))
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
    let mut sessions = decode_list(dec, decode_session_info)?;
    let windows = decode_list(dec, decode_window_info)?;
    let mut resources = decode_list(dec, decode_terminal_info)?;
    let focused_session = SessionId::new(dec.read_u32_be()?);
    let focused_window = WindowId::new(dec.read_u32_be()?);
    let focused_resource = decode_terminal_id(dec)?;
    decode_resource_facets(dec, &mut resources)?;
    let hosts = decode_host_inventory(dec)?;
    decode_session_facets(dec, &mut sessions)?;
    let listeners = decode_listeners(dec)?;
    let journal_head = decode_snapshot_extension(dec, &mut resources)?;
    let mut snapshot = SessionSnapshot {
        sessions,
        windows,
        resources,
        focused_session,
        focused_window,
        focused_resource,
        trail: None,
    };
    if !hosts.is_empty() {
        snapshot.set_hosts(hosts);
    }
    if let Some(listeners) = listeners {
        snapshot.set_listeners(Some(listeners));
    }
    if journal_head.is_some() {
        snapshot.set_journal_head(journal_head);
    }
    Ok(snapshot)
}

// Presence-byte option and `u32` list-length helpers.

pub(super) fn encode_option_window_id(value: Option<WindowId>, enc: &mut Encoder<'_>) {
    enc.write_option(value, |e, id| e.write_u32_be(id.get()));
}

pub(super) fn decode_option_window_id(
    dec: &mut Decoder<'_>,
) -> Result<Option<WindowId>, DecodeError> {
    dec.read_option("Option<WindowId> tag", |d| {
        Ok(WindowId::new(d.read_u32_be()?))
    })
}

pub(super) fn encode_option_terminal_id(value: Option<&ResourceId>, enc: &mut Encoder<'_>) {
    enc.write_option(value, |e, id| encode_terminal_id(id, e));
}

pub(super) fn decode_option_terminal_id(
    dec: &mut Decoder<'_>,
) -> Result<Option<ResourceId>, DecodeError> {
    dec.read_option("Option<ResourceId> tag", decode_terminal_id)
}

pub(in crate::wire) fn encode_option_str(value: Option<&str>, enc: &mut Encoder<'_>) {
    enc.write_option(value, Encoder::write_str);
}

pub(in crate::wire) fn decode_option_str<'a>(
    dec: &mut Decoder<'a>,
) -> Result<Option<&'a str>, DecodeError> {
    dec.read_option("Option<str> tag", Decoder::read_str)
}

pub(super) fn encode_list_len(len: usize, enc: &mut Encoder<'_>) {
    debug_assert!(
        u32::try_from(len).is_ok(),
        "list length exceeds u32 (positional encoding cap)",
    );
    let len_u32 = u32::try_from(len).unwrap_or(u32::MAX);
    enc.write_u32_be(len_u32);
}

/// Write a `u32`-counted list, each item via `item`.
fn encode_list<T>(items: &[T], enc: &mut Encoder<'_>, item: impl Fn(&T, &mut Encoder<'_>)) {
    encode_list_len(items.len(), enc);
    for value in items {
        item(value, enc);
    }
}

/// Read a `u32`-counted list; an over-declared count cannot pre-allocate
/// past the remaining bytes.
fn decode_list<'a, T>(
    dec: &mut Decoder<'a>,
    item: impl Fn(&mut Decoder<'a>) -> Result<T, DecodeError>,
) -> Result<Vec<T>, DecodeError> {
    let len = decode_list_len(dec)?;
    let mut out = dec.bounded_capacity(len);
    for _ in 0..len {
        out.push(item(dec)?);
    }
    Ok(out)
}

pub(super) fn decode_list_len(dec: &mut Decoder<'_>) -> Result<usize, DecodeError> {
    let len = dec.read_u32_be()?;
    usize::try_from(len).map_err(|_| DecodeError::LengthOverflow)
}

// The one `ClientId` wire codec.

pub(super) fn encode_client_id(id: ClientId, enc: &mut Encoder<'_>) {
    enc.write_u32_be(id.get());
}

pub(super) fn decode_client_id(dec: &mut Decoder<'_>) -> Result<ClientId, DecodeError> {
    Ok(ClientId::new(dec.read_u32_be()?))
}
