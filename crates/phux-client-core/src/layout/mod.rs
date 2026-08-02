//! Client-side mirror of the binary split-tree pane layout.
//!
//! Per [ADR-0019] decision 3 the reference TUI keeps its own copy of the
//! layout tree. The shape is the wire-side [`LayoutNode`] (re-exported,
//! not redefined); the operations (`split_at`, `kill_pane`,
//! `focus_direction`) are re-implemented here as free functions over the
//! wire type so the client crate's edge to `phux-core` stays as thin as
//! today. Pane-rect tiling lives one module over in
//! [`crate::multi_pane::pane_rects`] — the same local-divider walk paint
//! uses, so reflow and paint can never disagree.
//!
//! Layout persistence (per [ADR-0019] decision 1) wraps the whole
//! [`Workspace`] (the set of windows plus the active index) in a
//! versioned CBOR envelope and stores it server-side under the L3
//! metadata key `phux.tui.layout/v1`. The current envelope is v2 —
//! `{version, windows: [{name, root: LayoutNode, focused_terminal}],
//! focused_window_index}` (docs/spec/L3.md §3.2), encoded by
//! [`Workspace::encode_cbor`] / [`Workspace::decode_cbor`]. Focus fields remain
//! for compatibility but are non-authoritative; ADR-0049 requires recipients
//! to preserve their client-local focus when adopting topology. The legacy
//! v1 single-window envelope (`{version, root, focus}`) is no longer
//! written but is still decoded for back-compat
//! ([`LayoutState::decode_cbor`]) and wrapped as a one-window workspace.
//!
//! The wire crate exposes neither `serde::Serialize` for its types nor
//! a public encoder API; for the CBOR envelope we therefore round-trip
//! through small local shim types (`CborLayoutNode`, `CborSplitDir`,
//! `CborTerminalId`) that mirror the wire shape and convert via `From`.
//!
//! [ADR-0019]: ../../ADR/0019-tui-multi-pane-rendering.md

use std::borrow::Cow;
use std::io::Cursor;

use phux_protocol::TerminalId;
use thiserror::Error;

pub use phux_protocol::wire::info::{LayoutNode, SplitDir};

/// Current version of the layout CBOR envelope.
///
/// Stored as the `version` field of the envelope. Bumped when the
/// envelope shape changes incompatibly; readers MUST refuse unknown
/// versions (see [`LayoutDecodeError::UnsupportedVersion`]).
///
/// v2 is the multi-window [`Workspace`] envelope (`{version, windows,
/// focused_window_index}`, per docs/spec/L3.md §3.2). v1 was the
/// single-window envelope (`{version, root, focus}`); [`Workspace::decode_cbor`]
/// still reads v1 blobs for back-compat, wrapping them as a one-window
/// workspace.
pub(crate) const LAYOUT_ENVELOPE_VERSION: u8 = 2;

/// The legacy single-window envelope version that [`LayoutState::decode_cbor`]
/// expects. [`Workspace::decode_cbor`] accepts it for back-compat with layout
/// blobs written by pre-window clients.
const LAYOUT_ENVELOPE_VERSION_V1: u8 = 1;

// -----------------------------------------------------------------------------
// Direction / Rect — TUI-local geometry types
// -----------------------------------------------------------------------------

/// Cardinal direction for [`focus_direction`].
///
/// Mirrors `phux_core::window::Direction`. Lives here, not in the wire
/// crate, because focus movement is a TUI-private concern (no
/// `FOCUS_CHANGED` frame ever rides the substrate — see ADR-0017 +
/// ADR-0019 decision 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Move focus upward.
    Up,
    /// Move focus downward.
    Down,
    /// Move focus left.
    Left,
    /// Move focus right.
    Right,
}

/// An axis-aligned rectangle in cell coordinates.
///
/// Origin is the outer viewport's
/// top-left. Border-divider accounting (per ADR-0019 decision 4) happens
/// *inside* [`crate::multi_pane::pane_rects`]: it carves one cell per
/// split node out of the bounds for the divider, so leaf rects and
/// divider cells together tile the viewport exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Top-left x coordinate (column).
    pub x: u16,
    /// Top-left y coordinate (row).
    pub y: u16,
    /// Width in cells.
    pub w: u16,
    /// Height in cells.
    pub h: u16,
}

// -----------------------------------------------------------------------------
// NodePath — addressing an interior split node
// -----------------------------------------------------------------------------

/// One step down the binary split tree: into the `left` or `right` child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeStep {
    /// Descend into the `left` child of a [`LayoutNode::Split`].
    Left,
    /// Descend into the `right` child.
    Right,
}

/// A path from the layout root to a specific [`LayoutNode`].
///
/// Empty path ⇒ the root. The divider rasterizer
/// ([`crate::multi_pane`]) tags each interior split's divider line with
/// the path to that split, so a mouse press on a divider cell resolves to
/// the [`LayoutNode::Split`] whose `ratio` a drag adjusts
/// ([`set_ratio_at`]). Decoupled from the wire-side `LayoutNode` so it
/// stays a pure TUI-local geometry concern.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodePath(pub(crate) Vec<NodeStep>);

impl NodePath {
    /// The root path (empty).
    #[must_use]
    pub const fn root() -> Self {
        Self(Vec::new())
    }

    /// Push a step onto the path (descend one level).
    pub(crate) fn push(&mut self, step: NodeStep) {
        self.0.push(step);
    }

    /// Pop the last step (ascend one level).
    pub(crate) fn pop(&mut self) -> Option<NodeStep> {
        self.0.pop()
    }
}

// -----------------------------------------------------------------------------
// Errors
// -----------------------------------------------------------------------------

/// Errors returned by the free-function layout operations.
///
/// Mirrors `phux_core::window::LayoutError`. Duplicated because the
/// client may not depend on `phux-core` for the algorithm types
/// (ADR-0019 decision 3); this is the same enum with the same
/// variants and the same string forms.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum LayoutError {
    /// The target [`TerminalId`] is not present in the tree.
    #[error("pane not in layout: {0:?}")]
    PaneNotInLayout(TerminalId),
    /// The requested split ratio is outside `(0.0, 1.0)`, or is NaN.
    #[error("invalid split ratio: {0}")]
    InvalidRatio(f32),
    /// The tree has only one leaf — [`kill_pane`] would empty it.
    ///
    /// The function returns `Ok(None)` in this case (the empty tree).
    /// This variant exists so the proptest port can keep parity with
    /// the core surface but is not emitted by the implementation.
    #[error("cannot kill the last pane in the layout")]
    LastPane,
}

/// Errors returned by [`LayoutState::decode_cbor`].
#[derive(Debug, Error)]
pub enum LayoutDecodeError {
    /// The envelope's `version` byte is one this build doesn't recognise.
    #[error("unsupported layout envelope version: {0}")]
    UnsupportedVersion(u8),
    /// The envelope decodes as CBOR but `Split.ratio` is NaN, infinite,
    /// or outside `(0.0, 1.0)`.
    #[error("malformed layout ratio: {0}")]
    MalformedRatio(f32),
    /// The envelope's CBOR shape failed to decode.
    #[error("cbor decode failure: {0}")]
    Cbor(String),
}

/// Errors returned by [`Workspace::encode_cbor`].
#[derive(Debug, Error)]
pub enum LayoutEncodeError {
    /// Encoding a layout with no tree (`tree.is_none()`) or no focus
    /// (`focus.is_none()`) is meaningless; the envelope schema
    /// requires both.
    #[error("cannot encode empty layout state")]
    Empty,
    /// The ciborium encoder failed (typically an OOM on the
    /// in-memory `Vec<u8>` buffer — vanishingly rare in practice).
    #[error("cbor encode failure: {0}")]
    Cbor(String),
}

// -----------------------------------------------------------------------------
// LayoutState — in-memory mirror
// -----------------------------------------------------------------------------

/// In-memory mirror of the TUI's binary split tree plus the attaching
/// client's focused leaf.
///
/// The reference TUI holds one of these per attached window. On attach
/// the client requests `phux.tui.layout/v1` from the server's L3
/// metadata; if present, it decodes via [`Self::decode_cbor`] and
/// re-renders multi-pane. If absent, it falls back to single-pane
/// (the [`Default`] shape — an empty tree, no focus). On `split_at` /
/// `kill_pane` the TUI mutates the in-memory tree and pushes the new
/// shape back to the server via [`Workspace::encode_cbor`] +
/// `SET_METADATA`.
///
/// Focus is per-client (ADR-0019 decision 6). Compatibility envelopes still
/// serialize it, but recipients ignore the sender's value during topology
/// reconciliation (ADR-0049); it lives here as renderer-local state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayoutState {
    /// The binary split tree. `None` until the first pane is seeded.
    pub tree: Option<LayoutNode>,
    /// The client-local focused leaf. `None` until the first pane is
    /// seeded; reset to `None` if the tree becomes empty.
    pub focus: Option<TerminalId>,
}

impl LayoutState {
    /// Construct an empty state — no tree, no focus.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            tree: None,
            focus: None,
        }
    }

    /// Construct a state with a single leaf and matching focus.
    #[must_use]
    pub fn single(pane: TerminalId) -> Self {
        let focus = pane.clone();
        Self {
            tree: Some(LayoutNode::Leaf(pane)),
            focus: Some(focus),
        }
    }

    /// Decode a v1 CBOR envelope into a [`LayoutState`].
    ///
    /// # Errors
    /// * [`LayoutDecodeError::UnsupportedVersion`] if the envelope's
    ///   version byte isn't one this build knows about.
    /// * [`LayoutDecodeError::MalformedRatio`] if any `Split.ratio` is
    ///   NaN, infinite, or outside `(0.0, 1.0)`.
    /// * [`LayoutDecodeError::Cbor`] for malformed CBOR.
    pub fn decode_cbor(bytes: &[u8]) -> Result<Self, LayoutDecodeError> {
        let envelope: CborEnvelope = ciborium::de::from_reader(Cursor::new(bytes))
            .map_err(|e| LayoutDecodeError::Cbor(e.to_string()))?;
        if envelope.version != LAYOUT_ENVELOPE_VERSION_V1 {
            return Err(LayoutDecodeError::UnsupportedVersion(envelope.version));
        }
        let tree = envelope.root.into_layout_node()?;
        let focus: TerminalId = envelope.focus.into();
        Ok(Self {
            tree: Some(tree),
            focus: Some(focus),
        })
    }
}

// -----------------------------------------------------------------------------
// Workspace — the multi-window container above LayoutState
// -----------------------------------------------------------------------------

/// One TUI window: a name plus its own pane layout ([`LayoutState`]).
///
/// "Window" is a reference-TUI convention, not a wire concept
/// (ADR-0017); the whole [`Workspace`] is persisted as the L3 metadata
/// blob `phux.tui.layout/v1` (docs/spec/L3.md §3.2). The window's pane
/// tree and focused leaf are exactly the single-window [`LayoutState`]
/// the renderer already knows how to paint.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowState {
    /// Display name, shown in the window/tab bar.
    pub name: String,
    /// This window's pane layout and per-client focus.
    pub state: LayoutState,
}

/// The set of windows the TUI presents for one Group, plus which
/// one is active.
///
/// The renderer and every pure layout helper operate on a single
/// [`LayoutState`]; the driver hands them [`Self::active_window`] so the
/// window dimension is invisible below this type. The active-window
/// index is per-client state (like focus, ADR-0019 decision 6): it is
/// serialized as `focused_window_index` but a bare window switch does
/// not broadcast it.
///
/// Invariant: when `windows` is non-empty, `active < windows.len()`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Workspace {
    /// The windows, in display order. May be empty before the first
    /// pane is seeded (the single-pane fallback renders nothing).
    pub windows: Vec<WindowState>,
    /// Index of the active window into [`Self::windows`].
    pub active: usize,
}

impl Workspace {
    /// An empty workspace — no windows, matching [`LayoutState::default`]'s
    /// "no panes yet" sentinel.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A workspace with a single window named `"1"` holding one pane.
    #[must_use]
    pub fn single(pane: TerminalId) -> Self {
        Self {
            windows: vec![WindowState {
                name: "1".to_owned(),
                state: LayoutState::single(pane),
            }],
            active: 0,
        }
    }

    /// The active window, or `None` when the workspace is empty.
    #[must_use]
    pub fn active_window(&self) -> Option<&LayoutState> {
        self.windows.get(self.active).map(|w| &w.state)
    }

    /// Mutable access to the active window's layout, or `None` when the
    /// workspace is empty.
    pub fn active_window_mut(&mut self) -> Option<&mut LayoutState> {
        self.windows.get_mut(self.active).map(|w| &mut w.state)
    }

    /// The layout to **render and reflow** against, honoring a zoom (phux-x2hm).
    ///
    /// When `zoomed` is `Some(id)` and `id` is still a live leaf of the active
    /// window, returns an owned single-leaf layout — the zoomed pane fills the
    /// whole pane viewport, with no dividers and the other panes hidden (a
    /// single-leaf tree already tiles to one full-viewport rect with zero
    /// dividers). Otherwise — no zoom, or the zoom target has since closed or
    /// belongs to a different window — returns the real active window.
    ///
    /// Folding the liveness check in here makes zoom **self-healing**: a
    /// closed or window-switched zoom target silently falls back to the real
    /// layout instead of stranding a dead single pane. `None` only when there
    /// is no active window (pre-attach). Mutation and input-routing paths must
    /// keep using [`Self::active_window`] (the real tree); only render/reflow
    /// reads go through here.
    #[must_use]
    pub fn render_window(&self, zoomed: Option<&TerminalId>) -> Option<Cow<'_, LayoutState>> {
        let active = self.active_window()?;
        if let Some(id) = zoomed
            && active.tree.as_ref().is_some_and(|t| leaves(t).contains(id))
        {
            return Some(Cow::Owned(LayoutState::single(id.clone())));
        }
        Some(Cow::Borrowed(active))
    }

    /// Append a new window named `name` holding a single `seed` pane and
    /// make it active.
    pub fn add_window(&mut self, name: String, seed: TerminalId) {
        self.windows.push(WindowState {
            name,
            state: LayoutState::single(seed),
        });
        self.active = self.windows.len() - 1;
    }

    /// Drop any window whose pane tree has become empty (its last pane
    /// closed). The active window stays pointed at the same window if it
    /// survived, otherwise at the survivor that took its place. Returns
    /// `true` if anything was removed.
    pub fn prune_empty_windows(&mut self) -> bool {
        if self.windows.iter().all(|w| w.state.tree.is_some()) {
            return false;
        }
        let active_survives = self
            .windows
            .get(self.active)
            .is_some_and(|w| w.state.tree.is_some());
        // The active window's new index is the count of survivors that
        // precede it; if it died, that index is where the next survivor
        // lands.
        let survivors_before = self.windows[..self.active.min(self.windows.len())]
            .iter()
            .filter(|w| w.state.tree.is_some())
            .count();
        self.windows.retain(|w| w.state.tree.is_some());
        self.active = if self.windows.is_empty() {
            0
        } else if active_survives {
            survivors_before
        } else {
            survivors_before.min(self.windows.len() - 1)
        };
        true
    }

    /// Switch focus to the next window (wraps).
    pub const fn next(&mut self) {
        if !self.windows.is_empty() {
            self.active = (self.active + 1) % self.windows.len();
        }
    }

    /// Switch focus to the previous window (wraps).
    pub const fn prev(&mut self) {
        if !self.windows.is_empty() {
            self.active = (self.active + self.windows.len() - 1) % self.windows.len();
        }
    }

    /// Select the window at `idx`. Returns `false` (no-op) if out of range.
    pub const fn select(&mut self, idx: usize) -> bool {
        if idx < self.windows.len() {
            self.active = idx;
            true
        } else {
            false
        }
    }

    /// Rename the active window. No-op when the workspace is empty.
    pub fn rename_active(&mut self, name: String) {
        if let Some(w) = self.windows.get_mut(self.active) {
            w.name = name;
        }
    }

    /// The lowest unused positive-integer name (`"1"`, `"2"`, …), used
    /// when a new window is created without an explicit name.
    #[must_use]
    pub fn default_window_name(&self) -> String {
        let used: std::collections::HashSet<u32> = self
            .windows
            .iter()
            .filter_map(|w| w.name.parse::<u32>().ok())
            .collect();
        (1u32..=u32::MAX)
            .find(|n| !used.contains(n))
            .unwrap_or(1)
            .to_string()
    }

    /// Encode the workspace as the v2 CBOR envelope (docs/spec/L3.md §3.2).
    ///
    /// # Errors
    /// * [`LayoutEncodeError::Empty`] if there are no windows, or any
    ///   window has no tree/focus (an un-seeded window can't be encoded).
    /// * [`LayoutEncodeError::Cbor`] if ciborium fails to encode.
    pub fn encode_cbor(&self) -> Result<Vec<u8>, LayoutEncodeError> {
        if self.windows.is_empty() {
            return Err(LayoutEncodeError::Empty);
        }
        let mut windows = Vec::with_capacity(self.windows.len());
        for w in &self.windows {
            let (Some(tree), Some(focus)) = (w.state.tree.as_ref(), w.state.focus.as_ref()) else {
                return Err(LayoutEncodeError::Empty);
            };
            windows.push(CborWindow {
                name: w.name.clone(),
                root: CborLayoutNode::from(tree),
                focused_terminal: CborTerminalId::from(focus),
            });
        }
        let envelope = CborWorkspaceEnvelope {
            version: LAYOUT_ENVELOPE_VERSION,
            windows,
            focused_window_index: u32::try_from(self.active).unwrap_or(0),
        };
        let mut buf = Vec::with_capacity(128);
        ciborium::ser::into_writer(&envelope, &mut buf)
            .map_err(|e| LayoutEncodeError::Cbor(e.to_string()))?;
        Ok(buf)
    }

    /// Decode a layout blob into a [`Workspace`], accepting both the v2
    /// multi-window envelope and the legacy v1 single-window envelope
    /// (wrapped as a one-window workspace named `"1"`).
    ///
    /// # Errors
    /// * [`LayoutDecodeError::UnsupportedVersion`] for any version byte
    ///   other than 1 or 2.
    /// * [`LayoutDecodeError::MalformedRatio`] if any `Split.ratio` is
    ///   NaN, infinite, or outside `(0.0, 1.0)`.
    /// * [`LayoutDecodeError::Cbor`] for malformed CBOR.
    pub fn decode_cbor(bytes: &[u8]) -> Result<Self, LayoutDecodeError> {
        // Probe the version byte first, then re-deserialize the whole
        // buffer into the matching envelope. A bare `{version}` struct
        // deserializes fine from either envelope shape (serde ignores
        // the extra fields).
        let probe: VersionProbe = ciborium::de::from_reader(Cursor::new(bytes))
            .map_err(|e| LayoutDecodeError::Cbor(e.to_string()))?;
        match probe.version {
            LAYOUT_ENVELOPE_VERSION => {
                let envelope: CborWorkspaceEnvelope = ciborium::de::from_reader(Cursor::new(bytes))
                    .map_err(|e| LayoutDecodeError::Cbor(e.to_string()))?;
                let mut windows = Vec::with_capacity(envelope.windows.len());
                for w in envelope.windows {
                    let tree = w.root.into_layout_node()?;
                    let focus: TerminalId = w.focused_terminal.into();
                    windows.push(WindowState {
                        name: w.name,
                        state: LayoutState {
                            tree: Some(tree),
                            focus: Some(focus),
                        },
                    });
                }
                let active = if windows.is_empty() {
                    0
                } else {
                    (envelope.focused_window_index as usize).min(windows.len() - 1)
                };
                Ok(Self { windows, active })
            }
            LAYOUT_ENVELOPE_VERSION_V1 => {
                let state = LayoutState::decode_cbor(bytes)?;
                Ok(Self {
                    windows: vec![WindowState {
                        name: "1".to_owned(),
                        state,
                    }],
                    active: 0,
                })
            }
            other => Err(LayoutDecodeError::UnsupportedVersion(other)),
        }
    }
}

// -----------------------------------------------------------------------------
// Free-function algorithms — ports of phux-core::window
// -----------------------------------------------------------------------------

/// Split the leaf for `target` into two, with `new_pane` as the new
/// sibling along `dir` at `ratio`.
///
/// Mirrors `phux_core::Window::split` semantics: on success the tree
/// grows by one [`LayoutNode::Leaf`] and one [`LayoutNode::Split`];
/// `target` becomes the `left` child and `new_pane` the `right` child
/// of the new interior node.
///
/// `tree` is `None` only for a fresh window. The first leaf is seeded
/// via `tree = Some(LayoutNode::Leaf(pane))` directly — callers don't
/// need a separate `seed_layout` helper because there is no
/// `LayoutNode` invariant to protect (unlike `phux-core::Window`,
/// which also tracks `panes: Vec<TerminalId>`).
///
/// # Errors
/// * [`LayoutError::PaneNotInLayout`] if `target` is not present.
/// * [`LayoutError::InvalidRatio`] if `ratio` is NaN or outside `(0, 1)`.
pub fn split_at(
    tree: &LayoutNode,
    target: &TerminalId,
    new_pane: &TerminalId,
    dir: SplitDir,
    ratio: f32,
) -> Result<LayoutNode, LayoutError> {
    validate_ratio(ratio)?;
    if !contains(tree, target) {
        return Err(LayoutError::PaneNotInLayout(target.clone()));
    }
    Ok(split_inner(tree, target, new_pane, dir, ratio))
}

/// Replace the `ratio` of the [`LayoutNode::Split`] addressed by `path`,
/// returning the rewritten tree.
///
/// `ratio` is **not** validated or clamped here — callers (the drag
/// machine, the divider-resize action) clamp into `(0, 1)` with the same
/// `clamp_ratio` the keyboard resize uses before calling. Returns `None`
/// if `path` does not address a `Split` (it ran off a leaf, or the path
/// is the empty root over a single-leaf tree) — a stale path from a
/// layout that changed between press and motion.
#[must_use]
pub fn set_ratio_at(tree: &LayoutNode, path: &NodePath, ratio: f32) -> Option<LayoutNode> {
    set_ratio_inner(tree, &path.0, ratio)
}

fn set_ratio_inner(node: &LayoutNode, steps: &[NodeStep], ratio: f32) -> Option<LayoutNode> {
    match node {
        LayoutNode::Split {
            dir,
            ratio: r,
            left,
            right,
        } => match steps.split_first() {
            // Path ends here: this is the split to retune.
            None => Some(LayoutNode::Split {
                dir: *dir,
                ratio,
                left: left.clone(),
                right: right.clone(),
            }),
            Some((NodeStep::Left, rest)) => {
                let new_left = set_ratio_inner(left, rest, ratio)?;
                Some(LayoutNode::Split {
                    dir: *dir,
                    ratio: *r,
                    left: Box::new(new_left),
                    right: right.clone(),
                })
            }
            Some((NodeStep::Right, rest)) => {
                let new_right = set_ratio_inner(right, rest, ratio)?;
                Some(LayoutNode::Split {
                    dir: *dir,
                    ratio: *r,
                    left: left.clone(),
                    right: Box::new(new_right),
                })
            }
        },
        // Ran off a leaf (or hit a leaf at the path tip) — the path no
        // longer names a split in this tree.
        LayoutNode::Leaf(_) => None,
        _ => unknown_layout_variant(),
    }
}

/// Centralised handler for the `#[non_exhaustive]` wildcard arms on
/// matches over [`LayoutNode`]. v0.1 only knows `Leaf` and `Split`;
/// any future variant would need a corresponding update here and a
/// wire-protocol bump (see docs/spec/). Reached only via a forward-
/// compatible decode from a newer server.
//
// `clippy::panic` is workspace-denied to keep production panics rare,
// but the alternative (returning `Result` through every internal
// helper to thread a "future variant" error up to the public surface)
// trades a real algorithmic complexity for a `#[non_exhaustive]`
// concession the wire validation already enforces. Localised allow.
#[cold]
#[inline(never)]
#[allow(clippy::panic)]
fn unknown_layout_variant() -> ! {
    panic!("unknown LayoutNode variant — wire-protocol newer than this client")
}

/// Same role as [`unknown_layout_variant`] for [`SplitDir`].
#[cold]
#[inline(never)]
#[allow(clippy::panic)]
fn unknown_split_dir() -> ! {
    panic!("unknown SplitDir variant — wire-protocol newer than this client")
}

fn split_inner(
    node: &LayoutNode,
    target: &TerminalId,
    new_pane: &TerminalId,
    dir: SplitDir,
    ratio: f32,
) -> LayoutNode {
    match node {
        LayoutNode::Leaf(p) if p == target => LayoutNode::Split {
            dir,
            ratio,
            left: Box::new(LayoutNode::Leaf(target.clone())),
            right: Box::new(LayoutNode::Leaf(new_pane.clone())),
        },
        LayoutNode::Leaf(p) => LayoutNode::Leaf(p.clone()),
        LayoutNode::Split {
            dir: sd,
            ratio: r,
            left,
            right,
        } => {
            if contains(left, target) {
                LayoutNode::Split {
                    dir: *sd,
                    ratio: *r,
                    left: Box::new(split_inner(left, target, new_pane, dir, ratio)),
                    right: right.clone(),
                }
            } else {
                LayoutNode::Split {
                    dir: *sd,
                    ratio: *r,
                    left: left.clone(),
                    right: Box::new(split_inner(right, target, new_pane, dir, ratio)),
                }
            }
        }
        // `LayoutNode` is `#[non_exhaustive]`; v0.1 only defines `Leaf`
        // and `Split`. Future variants would be wire-breaking and would
        // need to reach this module before being decoded into a tree.
        _ => unknown_layout_variant(),
    }
}

/// Remove the leaf for `target`, collapsing its parent [`LayoutNode::Split`]
/// so the sibling takes its grandparent's slot.
///
/// Returns `Ok(None)` iff `target` was the last leaf — the caller
/// (typically the TUI driver) is expected to drop the whole window in
/// that case. This packs the `LastPane` signal into the `Option`,
/// which is more ergonomic for callers than the `Result<_, LastPane>`
/// shape the core surface uses.
///
/// # Errors
/// [`LayoutError::PaneNotInLayout`] if `target` is not present.
pub fn kill_pane(
    tree: &LayoutNode,
    target: &TerminalId,
) -> Result<Option<LayoutNode>, LayoutError> {
    match tree {
        LayoutNode::Leaf(p) if p == target => Ok(None),
        LayoutNode::Leaf(_) => Err(LayoutError::PaneNotInLayout(target.clone())),
        LayoutNode::Split { .. } => {
            let (new_root, found) = collapse(tree, target);
            if found {
                Ok(Some(new_root))
            } else {
                Err(LayoutError::PaneNotInLayout(target.clone()))
            }
        }
        _ => unknown_layout_variant(),
    }
}

fn collapse(node: &LayoutNode, target: &TerminalId) -> (LayoutNode, bool) {
    match node {
        LayoutNode::Leaf(p) => (LayoutNode::Leaf(p.clone()), false),
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => {
            if let LayoutNode::Leaf(p) = left.as_ref()
                && p == target
            {
                return ((**right).clone(), true);
            }
            if let LayoutNode::Leaf(p) = right.as_ref()
                && p == target
            {
                return ((**left).clone(), true);
            }
            let (new_left, found_l) = collapse(left, target);
            if found_l {
                return (
                    LayoutNode::Split {
                        dir: *dir,
                        ratio: *ratio,
                        left: Box::new(new_left),
                        right: right.clone(),
                    },
                    true,
                );
            }
            let (new_right, found_r) = collapse(right, target);
            (
                LayoutNode::Split {
                    dir: *dir,
                    ratio: *ratio,
                    left: Box::new(new_left),
                    right: Box::new(new_right),
                },
                found_r,
            )
        }
        _ => unknown_layout_variant(),
    }
}

/// Return the neighbour of `current` in direction `dir`, if any.
///
/// Mirrors `phux_core::Window::focus_direction` — see that function's
/// docs for the algorithm. Returns `None` if `current` is not in the
/// tree or if no neighbour exists in that direction (border of the
/// outer viewport).
#[must_use]
pub fn focus_direction(
    tree: &LayoutNode,
    current: &TerminalId,
    dir: Direction,
) -> Option<TerminalId> {
    let mut path: Vec<(SplitDir, ChildSide)> = Vec::new();
    if !record_path(tree, current, &mut path) {
        return None;
    }
    for i in (0..path.len()).rev() {
        let (split_dir, came_from) = path[i];
        if matches_to_sibling(split_dir, dir, came_from) {
            let sibling = sibling_at_depth(tree, &path, i)?;
            return Some(descend_to_leaf(sibling, dir, &path[i + 1..]));
        }
    }
    None
}

// -----------------------------------------------------------------------------
// Internal helpers — same shapes as phux-core::window
// -----------------------------------------------------------------------------

fn contains(node: &LayoutNode, target: &TerminalId) -> bool {
    match node {
        LayoutNode::Leaf(p) => p == target,
        LayoutNode::Split { left, right, .. } => contains(left, target) || contains(right, target),
        _ => unknown_layout_variant(),
    }
}

/// Collect every leaf of `node` in left-to-right depth-first order.
///
/// Useful for the "default focus on attach" rule from ADR-0019
/// decision 6 (focus defaults to the first leaf in left-to-right
/// traversal order) and for invariant proptests.
#[must_use]
pub fn leaves(node: &LayoutNode) -> Vec<TerminalId> {
    let mut out = Vec::new();
    collect_leaves(node, &mut out);
    out
}

fn collect_leaves(node: &LayoutNode, out: &mut Vec<TerminalId>) {
    match node {
        LayoutNode::Leaf(p) => out.push(p.clone()),
        LayoutNode::Split { left, right, .. } => {
            collect_leaves(left, out);
            collect_leaves(right, out);
        }
        _ => unknown_layout_variant(),
    }
}

fn validate_ratio(ratio: f32) -> Result<(), LayoutError> {
    if ratio.is_nan() || ratio <= 0.0 || ratio >= 1.0 {
        Err(LayoutError::InvalidRatio(ratio))
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChildSide {
    Left,
    Right,
}

fn record_path(
    node: &LayoutNode,
    target: &TerminalId,
    out: &mut Vec<(SplitDir, ChildSide)>,
) -> bool {
    match node {
        LayoutNode::Leaf(p) => p == target,
        LayoutNode::Split {
            dir: sd,
            left,
            right,
            ..
        } => {
            out.push((*sd, ChildSide::Left));
            if record_path(left, target, out) {
                return true;
            }
            out.pop();
            out.push((*sd, ChildSide::Right));
            if record_path(right, target, out) {
                return true;
            }
            out.pop();
            false
        }
        _ => unknown_layout_variant(),
    }
}

const fn matches_to_sibling(split: SplitDir, dir: Direction, came_from: ChildSide) -> bool {
    matches!(
        (split, dir, came_from),
        (SplitDir::Horizontal, Direction::Right, ChildSide::Left)
            | (SplitDir::Horizontal, Direction::Left, ChildSide::Right)
            | (SplitDir::Vertical, Direction::Down, ChildSide::Left)
            | (SplitDir::Vertical, Direction::Up, ChildSide::Right)
    )
}

fn sibling_at_depth<'a>(
    root: &'a LayoutNode,
    path: &[(SplitDir, ChildSide)],
    depth: usize,
) -> Option<&'a LayoutNode> {
    let mut cur = root;
    for (_, side) in &path[..depth] {
        let LayoutNode::Split { left, right, .. } = cur else {
            return None;
        };
        cur = match side {
            ChildSide::Left => left,
            ChildSide::Right => right,
        };
    }
    let LayoutNode::Split { left, right, .. } = cur else {
        return None;
    };
    let (_, came_from) = path[depth];
    Some(match came_from {
        ChildSide::Left => right,
        ChildSide::Right => left,
    })
}

fn descend_to_leaf(
    node: &LayoutNode,
    dir: Direction,
    suffix: &[(SplitDir, ChildSide)],
) -> TerminalId {
    let perp = perpendicular_axis(dir);
    let hints: Vec<ChildSide> = suffix
        .iter()
        .filter_map(|(sd, side)| if *sd == perp { Some(*side) } else { None })
        .collect();
    let mut hint_idx = 0;
    let mut cur = node;
    loop {
        match cur {
            LayoutNode::Leaf(p) => return p.clone(),
            LayoutNode::Split {
                dir: sd,
                left,
                right,
                ..
            } => {
                if axis_parallel(*sd, dir) {
                    cur = match dir {
                        Direction::Right | Direction::Down => left,
                        Direction::Left | Direction::Up => right,
                    };
                } else {
                    let side = hints.get(hint_idx).copied().unwrap_or(ChildSide::Left);
                    hint_idx += 1;
                    cur = match side {
                        ChildSide::Left => left,
                        ChildSide::Right => right,
                    };
                }
            }
            _ => unknown_layout_variant(),
        }
    }
}

const fn axis_parallel(split: SplitDir, dir: Direction) -> bool {
    matches!(
        (split, dir),
        (SplitDir::Horizontal, Direction::Left | Direction::Right)
            | (SplitDir::Vertical, Direction::Up | Direction::Down)
    )
}

const fn perpendicular_axis(dir: Direction) -> SplitDir {
    match dir {
        Direction::Left | Direction::Right => SplitDir::Vertical,
        Direction::Up | Direction::Down => SplitDir::Horizontal,
    }
}

// -----------------------------------------------------------------------------
// CBOR envelope — local shim types
// -----------------------------------------------------------------------------
//
// The wire-side `LayoutNode`, `SplitDir`, and `TerminalId` don't derive
// `serde::Serialize`/`Deserialize` and we can't modify the wire crate
// from this ticket (sibling-agent rule). The CBOR envelope therefore
// round-trips through small local types that mirror the wire shapes 1:1.
// Conversions are pure (no allocation beyond the recursive tree clone)
// and unit-tested below.

/// CBOR shadow types + conversions for layout persistence (L3 metadata).
mod serialize;

use serialize::{
    CborEnvelope, CborLayoutNode, CborTerminalId, CborWindow, CborWorkspaceEnvelope, VersionProbe,
};

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use std::collections::HashSet;

    use proptest::prelude::*;

    use super::serialize::CborSplitDir;
    use super::*;
    use serde::Serialize;

    fn t(id: u32) -> TerminalId {
        TerminalId::local(id)
    }

    fn leaf(id: u32) -> LayoutNode {
        LayoutNode::Leaf(t(id))
    }

    /// Test-only v1 writer. Production stopped emitting the v1
    /// single-window envelope (every write path is v2,
    /// [`Workspace::encode_cbor`]), but the v1 decode path is retained
    /// for back-compat, so tests synthesize v1 blobs here.
    fn encode_cbor_v1(state: &LayoutState) -> Vec<u8> {
        let envelope = CborEnvelope {
            version: LAYOUT_ENVELOPE_VERSION_V1,
            root: CborLayoutNode::from(state.tree.as_ref().expect("tree")),
            focus: CborTerminalId::from(state.focus.as_ref().expect("focus")),
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&envelope, &mut buf).expect("cbor encode");
        buf
    }

    fn ws_split(a: u32, b: u32, focus: u32) -> Workspace {
        let tree = split_at(&leaf(a), &t(a), &t(b), SplitDir::Horizontal, 0.5).unwrap();
        Workspace {
            windows: vec![WindowState {
                name: "1".to_owned(),
                state: LayoutState {
                    tree: Some(tree),
                    focus: Some(t(focus)),
                },
            }],
            active: 0,
        }
    }

    #[test]
    fn render_window_zooms_to_a_live_leaf() {
        let ws = ws_split(1, 2, 1);
        let rendered = ws.render_window(Some(&t(2))).expect("active window");
        // A single-leaf synthetic layout of the zoomed pane.
        assert_eq!(rendered.tree, Some(LayoutNode::Leaf(t(2))));
        assert_eq!(rendered.focus, Some(t(2)));
        // The real workspace is untouched (still a split).
        assert!(matches!(
            ws.active_window().unwrap().tree,
            Some(LayoutNode::Split { .. })
        ));
    }

    #[test]
    fn render_window_self_heals_when_zoom_target_is_not_a_leaf() {
        // A zoom id absent from the active window (closed pane, or another
        // window's) falls back to the real layout — not a dead single pane.
        let ws = ws_split(1, 2, 1);
        let rendered = ws.render_window(Some(&t(99))).expect("active window");
        assert!(matches!(rendered.tree, Some(LayoutNode::Split { .. })));
    }

    #[test]
    fn render_window_without_zoom_is_the_active_window() {
        let ws = ws_split(1, 2, 1);
        let rendered = ws.render_window(None).expect("active window");
        assert!(matches!(rendered.tree, Some(LayoutNode::Split { .. })));
    }

    // -------------------------------------------------------------------------
    // split_at
    // -------------------------------------------------------------------------

    #[test]
    fn split_at_replaces_leaf_with_split() {
        let tree = leaf(1);
        let out = split_at(&tree, &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } = out
        else {
            panic!("expected Split");
        };
        assert_eq!(dir, SplitDir::Horizontal);
        assert_eq!(ratio, 0.5);
        assert!(matches!(*left, LayoutNode::Leaf(ref p) if *p == t(1)));
        assert!(matches!(*right, LayoutNode::Leaf(ref p) if *p == t(2)));
    }

    #[test]
    fn split_at_rejects_missing_target() {
        let tree = leaf(1);
        let err = split_at(&tree, &t(99), &t(2), SplitDir::Horizontal, 0.5).unwrap_err();
        assert!(matches!(err, LayoutError::PaneNotInLayout(_)));
    }

    #[test]
    fn split_at_rejects_bad_ratio() {
        let tree = leaf(1);
        for bad in [0.0_f32, 1.0, -0.1, 1.1, f32::NAN] {
            let err = split_at(&tree, &t(1), &t(2), SplitDir::Horizontal, bad).unwrap_err();
            assert!(matches!(err, LayoutError::InvalidRatio(_)));
        }
    }

    #[test]
    fn split_at_deep() {
        // Build (1|2) then split 2 vertically with 3.
        let t1 = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let t2 = split_at(&t1, &t(2), &t(3), SplitDir::Vertical, 0.3).unwrap();
        let leaves_v = leaves(&t2);
        assert_eq!(leaves_v, vec![t(1), t(2), t(3)]);
    }

    // -------------------------------------------------------------------------
    // kill_pane
    // -------------------------------------------------------------------------

    #[test]
    fn kill_pane_last_leaf_returns_none() {
        let out = kill_pane(&leaf(1), &t(1)).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn kill_pane_missing_returns_err() {
        let err = kill_pane(&leaf(1), &t(99)).unwrap_err();
        assert!(matches!(err, LayoutError::PaneNotInLayout(_)));
    }

    #[test]
    fn kill_pane_collapses_split() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let out = kill_pane(&tree, &t(2)).unwrap().expect("non-empty");
        assert!(matches!(out, LayoutNode::Leaf(ref p) if *p == t(1)));
    }

    #[test]
    fn kill_pane_collapses_deep() {
        // ((1|2)|3) — kill 1 should leave (2|3) at the root.
        let t1 = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let t2 = split_at(&t1, &t(2), &t(3), SplitDir::Vertical, 0.5).unwrap();
        let out = kill_pane(&t2, &t(1)).unwrap().expect("non-empty");
        // After killing 1 from ((1|(2/3))), the left subtree collapses to
        // (2/3). Tree shape: Split[h, (2/3), ?]... wait — let's just
        // check leaves.
        let mut got: Vec<_> = leaves(&out);
        got.sort_by_key(|id| id.local_id().unwrap_or_default());
        assert_eq!(got, vec![t(2), t(3)]);
    }

    // -------------------------------------------------------------------------
    // focus_direction
    // -------------------------------------------------------------------------

    #[test]
    fn focus_direction_right_across_split() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        assert_eq!(focus_direction(&tree, &t(1), Direction::Right), Some(t(2)));
        assert_eq!(focus_direction(&tree, &t(2), Direction::Left), Some(t(1)));
        assert_eq!(focus_direction(&tree, &t(1), Direction::Up), None);
    }

    #[test]
    fn focus_direction_returns_none_for_missing() {
        let tree = leaf(1);
        assert_eq!(focus_direction(&tree, &t(99), Direction::Right), None);
    }

    // -------------------------------------------------------------------------
    // CBOR round-trip
    // -------------------------------------------------------------------------

    #[test]
    fn cbor_round_trip_single_pane() {
        let state = LayoutState::single(t(7));
        let bytes = encode_cbor_v1(&state);
        let decoded = LayoutState::decode_cbor(&bytes).unwrap();
        assert_eq!(decoded, state);
    }

    #[test]
    fn cbor_round_trip_multi_split() {
        // ((1 | 2) / 3)
        let t1 = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.4).unwrap();
        let t2 = split_at(&t1, &t(2), &t(3), SplitDir::Vertical, 0.6).unwrap();
        let state = LayoutState {
            tree: Some(t2),
            focus: Some(t(2)),
        };
        let bytes = encode_cbor_v1(&state);
        let decoded = LayoutState::decode_cbor(&bytes).unwrap();
        assert_eq!(decoded, state);
    }

    #[test]
    fn cbor_round_trip_satellite_focus() {
        let focus = TerminalId::satellite("peer.example", 42);
        let state = LayoutState {
            tree: Some(LayoutNode::Leaf(focus.clone())),
            focus: Some(focus),
        };
        let bytes = encode_cbor_v1(&state);
        let decoded = LayoutState::decode_cbor(&bytes).unwrap();
        assert_eq!(decoded, state);
    }

    #[test]
    fn cbor_rejects_unsupported_version() {
        // Hand-build an envelope with version=99.
        #[derive(Serialize)]
        struct Forged {
            version: u8,
            root: CborLayoutNode,
            focus: CborTerminalId,
        }
        let forged = Forged {
            version: 99,
            root: CborLayoutNode::Leaf {
                pane: CborTerminalId::Local { id: 1 },
            },
            focus: CborTerminalId::Local { id: 1 },
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&forged, &mut buf).unwrap();
        let err = LayoutState::decode_cbor(&buf).unwrap_err();
        assert!(matches!(err, LayoutDecodeError::UnsupportedVersion(99)));
    }

    #[test]
    fn cbor_rejects_malformed_ratio() {
        #[derive(Serialize)]
        struct Forged {
            version: u8,
            root: CborLayoutNode,
            focus: CborTerminalId,
        }
        let forged = Forged {
            version: 1,
            root: CborLayoutNode::Split {
                dir: CborSplitDir::Horizontal,
                ratio: 2.0, // out of range
                left: Box::new(CborLayoutNode::Leaf {
                    pane: CborTerminalId::Local { id: 1 },
                }),
                right: Box::new(CborLayoutNode::Leaf {
                    pane: CborTerminalId::Local { id: 2 },
                }),
            },
            focus: CborTerminalId::Local { id: 1 },
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&forged, &mut buf).unwrap();
        let err = LayoutState::decode_cbor(&buf).unwrap_err();
        assert!(matches!(err, LayoutDecodeError::MalformedRatio(_)));
    }

    // -------------------------------------------------------------------------
    // Workspace — ops
    // -------------------------------------------------------------------------

    fn ws3() -> Workspace {
        let mut ws = Workspace::single(t(1));
        ws.add_window("2".to_owned(), t(2));
        ws.add_window("3".to_owned(), t(3));
        ws
    }

    #[test]
    fn add_window_appends_and_activates() {
        let mut ws = Workspace::single(t(1));
        ws.add_window(ws.default_window_name(), t(2));
        assert_eq!(ws.windows.len(), 2);
        assert_eq!(ws.active, 1);
        assert_eq!(ws.windows[1].name, "2");
        assert_eq!(ws.active_window().unwrap().focus, Some(t(2)));
    }

    #[test]
    fn next_prev_wrap() {
        let mut ws = ws3();
        assert_eq!(ws.active, 2);
        ws.next();
        assert_eq!(ws.active, 0);
        ws.prev();
        assert_eq!(ws.active, 2);
        ws.prev();
        assert_eq!(ws.active, 1);
    }

    #[test]
    fn select_out_of_bounds_is_noop() {
        let mut ws = ws3();
        assert!(ws.select(0));
        assert_eq!(ws.active, 0);
        assert!(!ws.select(9));
        assert_eq!(ws.active, 0);
    }

    #[test]
    fn rename_active_updates_name() {
        let mut ws = ws3();
        ws.rename_active("build".to_owned());
        assert_eq!(ws.windows[2].name, "build");
    }

    #[test]
    fn default_window_name_skips_used_integers() {
        let mut ws = Workspace::single(t(1)); // "1"
        ws.add_window("build".to_owned(), t(2)); // non-integer, ignored
        assert_eq!(ws.default_window_name(), "2");
        ws.add_window("2".to_owned(), t(3));
        assert_eq!(ws.default_window_name(), "3");
    }

    #[test]
    fn prune_empty_windows_removes_treeless_window_and_keeps_active() {
        let mut ws = ws3(); // active = 2
        // Empty the middle window's tree (its last pane closed).
        ws.windows[1].state.tree = None;
        ws.windows[1].state.focus = None;
        assert!(ws.prune_empty_windows());
        assert_eq!(ws.windows.len(), 2);
        // Active window ("3") survived; it shifted from index 2 to 1.
        assert_eq!(ws.windows[ws.active].name, "3");
    }

    #[test]
    fn prune_empty_windows_when_active_dies_lands_on_survivor() {
        let mut ws = ws3();
        ws.select(1); // active = middle ("2")
        ws.windows[1].state.tree = None;
        assert!(ws.prune_empty_windows());
        assert_eq!(ws.windows.len(), 2);
        // "2" died; the survivor that took its slot is "3".
        assert_eq!(ws.windows[ws.active].name, "3");
    }

    // -------------------------------------------------------------------------
    // Workspace — CBOR v2 + v1 back-compat
    // -------------------------------------------------------------------------

    #[test]
    fn cbor_v2_round_trip_multi_window() {
        let mut ws = Workspace::single(t(1));
        let split = split_at(&leaf(2), &t(2), &t(3), SplitDir::Vertical, 0.4).unwrap();
        ws.windows.push(WindowState {
            name: "editor".to_owned(),
            state: LayoutState {
                tree: Some(split),
                focus: Some(t(3)),
            },
        });
        ws.active = 1;
        let bytes = ws.encode_cbor().unwrap();
        let decoded = Workspace::decode_cbor(&bytes).unwrap();
        assert_eq!(decoded, ws);
    }

    #[test]
    fn cbor_v1_blob_decodes_as_single_window() {
        // A v1 single-window blob written by a pre-window client.
        let v1 = LayoutState {
            tree: Some(split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap()),
            focus: Some(t(2)),
        };
        let bytes = encode_cbor_v1(&v1);
        let ws = Workspace::decode_cbor(&bytes).unwrap();
        assert_eq!(ws.windows.len(), 1);
        assert_eq!(ws.active, 0);
        assert_eq!(ws.windows[0].name, "1");
        assert_eq!(ws.windows[0].state, v1);
    }

    #[test]
    fn cbor_v2_focused_index_out_of_range_clamps() {
        #[derive(Serialize)]
        struct ForgedWin {
            name: String,
            root: CborLayoutNode,
            focused_terminal: CborTerminalId,
        }
        #[derive(Serialize)]
        struct Forged {
            version: u8,
            windows: Vec<ForgedWin>,
            focused_window_index: u32,
        }
        let forged = Forged {
            version: 2,
            windows: vec![ForgedWin {
                name: "1".to_owned(),
                root: CborLayoutNode::Leaf {
                    pane: CborTerminalId::Local { id: 1 },
                },
                focused_terminal: CborTerminalId::Local { id: 1 },
            }],
            focused_window_index: 99,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&forged, &mut buf).unwrap();
        let ws = Workspace::decode_cbor(&buf).unwrap();
        assert_eq!(ws.active, 0); // clamped to last (only) window
    }

    #[test]
    fn cbor_workspace_rejects_unsupported_version() {
        #[derive(Serialize)]
        struct Forged {
            version: u8,
            windows: Vec<u8>,
            focused_window_index: u32,
        }
        let forged = Forged {
            version: 3,
            windows: vec![],
            focused_window_index: 0,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&forged, &mut buf).unwrap();
        let err = Workspace::decode_cbor(&buf).unwrap_err();
        assert!(matches!(err, LayoutDecodeError::UnsupportedVersion(3)));
    }

    #[test]
    fn cbor_workspace_rejects_empty() {
        let err = Workspace::default().encode_cbor().unwrap_err();
        assert!(matches!(err, LayoutEncodeError::Empty));
    }

    // -------------------------------------------------------------------------
    // Proptest invariants — ported from phux-core::window proptests.
    // -------------------------------------------------------------------------

    #[derive(Debug, Clone, Copy)]
    enum Op {
        AddPane,
        KillPaneAt(usize),
    }

    fn arb_op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => Just(Op::AddPane),
            1 => (0_usize..16).prop_map(Op::KillPaneAt),
        ]
    }

    /// Apply `ops` against a fresh single-leaf tree, returning the final
    /// tree (or `None` if killed empty) plus the ordered list of leaves
    /// that should currently live in the tree.
    #[allow(clippy::needless_pass_by_value)]
    fn apply_ops(ops: Vec<Op>) -> (Option<LayoutNode>, Vec<TerminalId>) {
        let mut next_id: u32 = 1;
        let first = TerminalId::local(next_id);
        next_id += 1;
        let mut tree: Option<LayoutNode> = Some(LayoutNode::Leaf(first.clone()));
        let mut alive: Vec<TerminalId> = vec![first];

        for op in ops {
            match op {
                Op::AddPane => {
                    let new_pane = TerminalId::local(next_id);
                    next_id += 1;
                    let Some(target) = alive.last().cloned() else {
                        // Tree was empty — reseed.
                        tree = Some(LayoutNode::Leaf(new_pane.clone()));
                        alive.push(new_pane);
                        continue;
                    };
                    let Some(cur) = tree else {
                        tree = Some(LayoutNode::Leaf(new_pane.clone()));
                        alive.push(new_pane);
                        continue;
                    };
                    let dir = if next_id.is_multiple_of(2) {
                        SplitDir::Horizontal
                    } else {
                        SplitDir::Vertical
                    };
                    match split_at(&cur, &target, &new_pane, dir, 0.5) {
                        Ok(t) => {
                            tree = Some(t);
                            alive.push(new_pane);
                        }
                        Err(_) => {
                            // Restore tree and skip.
                            tree = Some(cur);
                        }
                    }
                }
                Op::KillPaneAt(idx) => {
                    if alive.is_empty() {
                        continue;
                    }
                    let target = alive[idx % alive.len()].clone();
                    let Some(cur) = tree else { continue };
                    match kill_pane(&cur, &target) {
                        Ok(new_tree) => {
                            tree = new_tree;
                            alive.retain(|p| *p != target);
                        }
                        Err(_) => {
                            tree = Some(cur);
                        }
                    }
                }
            }
        }
        (tree, alive)
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

        /// Invariant 1: every TerminalId appears as exactly one leaf.
        #[test]
        fn proptest_leaves_match_alive(ops in prop::collection::vec(arb_op(), 1..20)) {
            let (tree, alive) = apply_ops(ops);
            let tree_leaves = tree.as_ref().map_or_else(Vec::new, leaves);
            let alive_set: HashSet<_> = alive.into_iter().collect();
            let leaf_set: HashSet<_> = tree_leaves.iter().cloned().collect();
            // Set equality.
            prop_assert_eq!(&alive_set, &leaf_set);
            // Exactly one leaf per id (no duplicates).
            prop_assert_eq!(tree_leaves.len(), leaf_set.len());
        }

        /// Invariant 3: `focus_direction` is partial, deterministic, and
        /// only returns ids that are leaves of the tree.
        #[test]
        fn proptest_focus_direction_partial_deterministic(
            ops in prop::collection::vec(arb_op(), 1..20),
            dir_pick in 0_u8..4,
        ) {
            let (tree, alive) = apply_ops(ops);
            let Some(tree) = tree else { return Ok(()) };
            if alive.is_empty() { return Ok(()) }
            let leaf_set: HashSet<_> = leaves(&tree).into_iter().collect();
            let dir = match dir_pick {
                0 => Direction::Up,
                1 => Direction::Down,
                2 => Direction::Left,
                _ => Direction::Right,
            };
            for src in &alive {
                let a = focus_direction(&tree, src, dir);
                let b = focus_direction(&tree, src, dir);
                // Deterministic.
                prop_assert_eq!(&a, &b);
                if let Some(neighbour) = a {
                    // Neighbour is a leaf of the tree.
                    prop_assert!(leaf_set.contains(&neighbour));
                    // Different from source.
                    prop_assert_ne!(&neighbour, src);
                }
            }
        }

        /// Invariant 4: CBOR round-trips for any state derived from
        /// random ops (with focus on the last surviving leaf).
        #[test]
        fn proptest_cbor_round_trip(ops in prop::collection::vec(arb_op(), 1..15)) {
            let (tree, alive) = apply_ops(ops);
            let Some(tree) = tree else { return Ok(()) };
            let Some(focus) = alive.last().cloned() else { return Ok(()) };
            let state = LayoutState { tree: Some(tree), focus: Some(focus) };
            let bytes = encode_cbor_v1(&state);
            let decoded = LayoutState::decode_cbor(&bytes).expect("decode");
            prop_assert_eq!(decoded, state);
        }

        /// Invariant 5: a multi-window [`Workspace`] CBOR-round-trips for
        /// any windows derived from random ops.
        #[test]
        fn proptest_workspace_cbor_round_trip(
            per_window in prop::collection::vec(
                prop::collection::vec(arb_op(), 1..10), 1..5),
        ) {
            let mut windows = Vec::new();
            for (i, ops) in per_window.into_iter().enumerate() {
                let (tree, alive) = apply_ops(ops);
                let (Some(tree), Some(focus)) = (tree, alive.last().cloned()) else {
                    continue;
                };
                windows.push(WindowState {
                    name: (i + 1).to_string(),
                    state: LayoutState { tree: Some(tree), focus: Some(focus) },
                });
            }
            prop_assume!(!windows.is_empty());
            let active = windows.len() / 2;
            let ws = Workspace { windows, active };
            let bytes = ws.encode_cbor().expect("encode");
            let decoded = Workspace::decode_cbor(&bytes).expect("decode");
            prop_assert_eq!(decoded, ws);
        }
    }
}
