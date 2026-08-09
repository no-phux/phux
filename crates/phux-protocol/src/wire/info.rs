//! Snapshot-graph types delivered with `ATTACHED` per `docs/spec/L1.md` §7.
//!
//! SPEC §13 references `SessionInfo`, `WindowInfo`, `TerminalInfo`, and
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

use crate::ids::{ClientId, SessionId, TerminalId, WindowId};

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
/// `Leaf` carries a single [`TerminalId`]; `Split` divides its rectangle between
/// two children along [`SplitDir`] at `ratio` (the left/top child gets
/// `ratio` of the parent dimension along the split axis).
///
/// The server-side bridge (parallel to the `IdBridge` pattern) converts
/// between this type and `phux_core::window::LayoutNode`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum LayoutNode {
    /// A single pane — recursion base.
    Leaf(TerminalId),
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
        /// `crates/phux/tests/layout_conformance.rs`.
        ratio: f32,
        /// Left (for [`SplitDir::Horizontal`]) or top (for [`SplitDir::Vertical`]) child.
        left: Box<Self>,
        /// Right (for [`SplitDir::Horizontal`]) or bottom (for [`SplitDir::Vertical`]) child.
        right: Box<Self>,
    },
}

// -----------------------------------------------------------------------------
// SessionInfo / WindowInfo / TerminalInfo / SessionSnapshot
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
}

impl SessionInfo {
    /// Construct a `SessionInfo` from its load-bearing fields.
    ///
    /// `active_window`, `created_at_unix_secs`, `window_count`, and
    /// `attached_client_count` default to "unknown" sentinels (`None` / `0`);
    /// fill them via the `with_*` setters when the server has the data.
    #[must_use]
    pub fn new(id: SessionId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            active_window: None,
            created_at_unix_secs: 0,
            window_count: 0,
            attached_client_count: 0,
        }
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
/// Excludes the panes themselves — those are flattened into
/// [`SessionSnapshot::panes`] and joined via `TerminalInfo::window_id`.
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
    pub active_pane: Option<TerminalId>,
    /// Pane layout as a binary split tree.
    ///
    /// `None` iff this window has no panes — `SessionSnapshot::panes`
    /// filtered by `window_id` will be empty.
    pub layout: Option<LayoutNode>,
}

impl WindowInfo {
    /// Construct a `WindowInfo` from its load-bearing fields.
    ///
    /// `index` defaults to `0`; `active_pane` and `layout` default to
    /// `None`. Use the `with_*` setters to fill them when meaningful.
    #[must_use]
    pub fn new(id: WindowId, session_id: SessionId, name: impl Into<String>) -> Self {
        Self {
            id,
            session_id,
            index: 0,
            name: name.into(),
            active_pane: None,
            layout: None,
        }
    }

    /// Builder setter for [`Self::index`].
    #[must_use]
    pub const fn with_index(mut self, index: u16) -> Self {
        self.index = index;
        self
    }

    /// Builder setter for [`Self::active_pane`].
    #[must_use]
    pub fn with_active_pane(mut self, active_pane: Option<TerminalId>) -> Self {
        self.active_pane = active_pane;
        self
    }

    /// Builder setter for [`Self::layout`].
    #[must_use]
    pub fn with_layout(mut self, layout: Option<LayoutNode>) -> Self {
        self.layout = layout;
        self
    }
}

/// Description of a single terminal, sufficient for layout chrome.
///
/// Excludes grid contents, cursor state, scrollback, and process info.
/// Grid contents and retained history flow through separate bootstrap/history
/// streams. Process info (PID, command, exit status) is not yet modeled in
/// `phux_core::TerminalDescriptor`; adding wire fields the server can only send
/// `None` for is premature. Revisit when core grows process tracking.
///
/// `#[non_exhaustive]`; construct via [`Self::new`] plus `with_*` setters.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TerminalInfo {
    /// Stable terminal identifier.
    pub id: TerminalId,
    /// Foreign key into [`SessionSnapshot::windows`].
    pub window_id: WindowId,
    /// Current grid width in cells (from `core::TerminalDescriptor::dims.0`).
    pub cols: u16,
    /// Current grid height in cells (from `core::TerminalDescriptor::dims.1`).
    pub rows: u16,
    /// User-set title, distinct from any title the shell may set.
    pub title: Option<String>,
    /// Working directory as a UTF-8 string.
    ///
    /// `phux_core::TerminalDescriptor::cwd` is `PathBuf`; conversion uses
    /// `to_string_lossy().into_owned()`. Lossy on non-UTF-8 cwds (rare on
    /// modern systems) and acceptable for a display field.
    pub cwd: Option<String>,
}

impl TerminalInfo {
    /// Construct a `TerminalInfo` from its load-bearing fields.
    ///
    /// `title` and `cwd` default to `None`; set them via the `with_*`
    /// helpers when the server has the data.
    #[must_use]
    pub const fn new(id: TerminalId, window_id: WindowId, cols: u16, rows: u16) -> Self {
        Self {
            id,
            window_id,
            cols,
            rows,
            title: None,
            cwd: None,
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
}

/// Flat graph of sessions/windows/panes delivered with `ATTACHED`.
///
/// All three lists are joined by id. The triple of `focused_*` fields
/// records the **attaching client's** current focus — distinct from the
/// per-container `SessionInfo::active_window` / `WindowInfo::active_pane`,
/// which record the container's remembered focus from when no client was
/// attached (tmux behavior: detach → attach later restores last focus).
///
/// `#[non_exhaustive]`; construct via [`Self::new`] plus `with_*` setters.
///
/// # Example
///
/// ```
/// use phux_protocol::wire::info::{TerminalInfo, SessionInfo, SessionSnapshot, WindowInfo};
/// use phux_protocol::{TerminalId, SessionId, WindowId};
///
/// let snapshot = SessionSnapshot::new(
///     SessionId::new(1),
///     WindowId::new(10),
///     TerminalId::new(100),
/// )
/// .with_sessions(vec![SessionInfo::new(SessionId::new(1), "work")
///     .with_window_count(1)
///     .with_attached_client_count(1)])
/// .with_windows(vec![
///     WindowInfo::new(WindowId::new(10), SessionId::new(1), "code")
///         .with_active_pane(Some(TerminalId::new(100))),
/// ])
/// .with_panes(vec![TerminalInfo::new(TerminalId::new(100), WindowId::new(10), 80, 24)]);
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
    pub panes: Vec<TerminalInfo>,
    /// The attaching client's initial focused session.
    pub focused_session: SessionId,
    /// The attaching client's initial focused window.
    pub focused_window: WindowId,
    /// The attaching client's initial focused pane.
    pub focused_pane: TerminalId,
}

impl SessionSnapshot {
    /// Construct a `SessionSnapshot` from the attaching client's initial
    /// focus triple. Lists default to empty; populate them via the `with_*`
    /// setters.
    #[must_use]
    pub const fn new(
        focused_session: SessionId,
        focused_window: WindowId,
        focused_pane: TerminalId,
    ) -> Self {
        Self {
            sessions: Vec::new(),
            windows: Vec::new(),
            panes: Vec::new(),
            focused_session,
            focused_window,
            focused_pane,
        }
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

    /// Builder setter for [`Self::panes`].
    #[must_use]
    pub fn with_panes(mut self, panes: Vec<TerminalInfo>) -> Self {
        self.panes = panes;
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
/// user has split panes (tens at the very most); `64` is comfortably above
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
    })
}

pub(super) fn encode_window_info(info: &WindowInfo, enc: &mut Encoder<'_>) {
    enc.write_u32_be(info.id.get());
    enc.write_u32_be(info.session_id.get());
    enc.write_u16_be(info.index);
    enc.write_str(&info.name);
    encode_option_terminal_id(info.active_pane.as_ref(), enc);
    encode_option_layout_node(info.layout.as_ref(), enc);
}

pub(super) fn decode_window_info(dec: &mut Decoder<'_>) -> Result<WindowInfo, DecodeError> {
    let id = WindowId::new(dec.read_u32_be()?);
    let session_id = SessionId::new(dec.read_u32_be()?);
    let index = dec.read_u16_be()?;
    let name = dec.read_str()?.to_owned();
    let active_pane = decode_option_terminal_id(dec)?;
    let layout = decode_option_layout_node(dec)?;
    Ok(WindowInfo {
        id,
        session_id,
        index,
        name,
        active_pane,
        layout,
    })
}

pub(super) fn encode_terminal_info(info: &TerminalInfo, enc: &mut Encoder<'_>) {
    encode_terminal_id(&info.id, enc);
    enc.write_u32_be(info.window_id.get());
    enc.write_u16_be(info.cols);
    enc.write_u16_be(info.rows);
    encode_option_str(info.title.as_deref(), enc);
    encode_option_str(info.cwd.as_deref(), enc);
}

pub(super) fn decode_terminal_info(dec: &mut Decoder<'_>) -> Result<TerminalInfo, DecodeError> {
    let id = decode_terminal_id(dec)?;
    let window_id = WindowId::new(dec.read_u32_be()?);
    let cols = dec.read_u16_be()?;
    let rows = dec.read_u16_be()?;
    let title = decode_option_str(dec)?.map(str::to_owned);
    let cwd = decode_option_str(dec)?.map(str::to_owned);
    Ok(TerminalInfo {
        id,
        window_id,
        cols,
        rows,
        title,
        cwd,
    })
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
    encode_list_len(snap.panes.len(), enc);
    for p in &snap.panes {
        encode_terminal_info(p, enc);
    }
    enc.write_u32_be(snap.focused_session.get());
    enc.write_u32_be(snap.focused_window.get());
    encode_terminal_id(&snap.focused_pane, enc);
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
    let panes_len = decode_list_len(dec)?;
    let mut panes = dec.bounded_capacity(panes_len);
    for _ in 0..panes_len {
        panes.push(decode_terminal_info(dec)?);
    }
    let focused_session = SessionId::new(dec.read_u32_be()?);
    let focused_window = WindowId::new(dec.read_u32_be()?);
    let focused_pane = decode_terminal_id(dec)?;
    Ok(SessionSnapshot {
        sessions,
        windows,
        panes,
        focused_session,
        focused_window,
        focused_pane,
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

pub(super) fn encode_option_terminal_id(value: Option<&TerminalId>, enc: &mut Encoder<'_>) {
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
) -> Result<Option<TerminalId>, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        0 => Ok(None),
        1 => Ok(Some(decode_terminal_id(dec)?)),
        other => Err(DecodeError::UnknownEnumValue {
            field: "Option<TerminalId> tag",
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
