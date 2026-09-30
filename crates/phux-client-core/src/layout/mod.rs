//! Client-side mirror of the binary split-tree pane layout (ADR-0019).
//!
//! The tree is [`LayoutNode`], a consumer convention rather than a wire type
//! (ADR-0030); the operations are free functions over it. Tiling lives in [`crate::multi_pane::pane_rects`],
//! the same walk paint uses. The whole [`Workspace`] persists as the v3
//! CBOR envelope under L3 key `phux.tui.layout/v1` (docs/spec/L3.md §3.2).
//! Focus fields are non-authoritative (ADR-0049); earlier envelope
//! versions and missing window identities are refused, never migrated.
//! The envelope uses local serde shim types, so its schema stays pinned
//! independently of the in-memory tree.

use std::borrow::Cow;
use std::io::Cursor;

use phux_protocol::ResourceId;
use thiserror::Error;

/// Axis along which a [`LayoutNode::Split`] divides its rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    /// Split side-by-side (a vertical bar between left and right).
    Horizontal,
    /// Split stacked (a horizontal bar between top and bottom).
    Vertical,
}

/// Binary split tree of a window's panes (ADR-0012); `Split` gives its
/// left/top child `ratio` of the parent along [`SplitDir`]. It lives in L3
/// metadata (docs/spec/L3.md §3.2), never on the wire.
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutNode {
    /// A single pane — recursion base.
    Leaf(ResourceId),
    /// An interior node that splits its rectangle in two.
    Split {
        /// The axis the split is taken along.
        dir: SplitDir,
        /// Fraction given to `left`, in the closed interval `0.0..=1.0`; the
        /// endpoints are admitted because clients bank unapplied resize
        /// ratios (ADR-0048).
        ratio: f32,
        /// Left (for [`SplitDir::Horizontal`]) or top (for [`SplitDir::Vertical`]) child.
        left: Box<Self>,
        /// Right (for [`SplitDir::Horizontal`]) or bottom (for [`SplitDir::Vertical`]) child.
        right: Box<Self>,
    },
}

/// Current layout envelope version; readers refuse any other.
pub(crate) const LAYOUT_ENVELOPE_VERSION: u8 = 3;

/// Cardinal direction for [`focus_direction`].
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

/// An axis-aligned rectangle in cell coordinates, origin at the outer
/// viewport's top-left.
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

/// One step down the binary split tree: into the `left` or `right` child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeStep {
    /// Descend into the `left` child of a [`LayoutNode::Split`].
    Left,
    /// Descend into the `right` child.
    Right,
}

/// A path from the layout root to a [`LayoutNode`] (empty = root). Divider
/// cells carry the path of their split so a drag can find its `ratio`.
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

/// Errors returned by the free-function layout operations.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum LayoutError {
    /// The target [`ResourceId`] is not present in the tree.
    #[error("pane not in layout: {0:?}")]
    PaneNotInLayout(ResourceId),
    /// The requested split ratio is outside `(0.0, 1.0)`, or is NaN.
    #[error("invalid split ratio: {0}")]
    InvalidRatio(f32),
    /// Closing this pane would leave the workspace with no panes.
    #[error("cannot close the final pane in a persisted layout")]
    LastPane,
}

/// Errors returned by [`Workspace::decode_cbor`].
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
    /// A leaf names a resource that is not a Terminal. Only Terminal-kind
    /// resources occupy layout slots; an `AgentSession` has no grid to tile.
    #[error("layout leaf {0:?} is not a terminal resource")]
    NonTerminalLeaf(ResourceId),
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

/// One window's split tree plus this client's focused leaf. Focus is
/// per-client (ADR-0019); peers ignore a sender's focus (ADR-0049).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LayoutState {
    /// The binary split tree. `None` until the first pane is seeded.
    pub tree: Option<LayoutNode>,
    /// The client-local focused leaf. `None` until the first pane is
    /// seeded; reset to `None` if the tree becomes empty.
    pub focus: Option<ResourceId>,
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
    pub fn single(pane: ResourceId) -> Self {
        let focus = pane.clone();
        Self {
            tree: Some(LayoutNode::Leaf(pane)),
            focus: Some(focus),
        }
    }
}

/// One TUI window: a stable identity, a name, and its pane layout.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowState {
    /// Durable layout identity, distinct from the server registry's window ID.
    pub id: [u8; 16],
    /// Display name, shown in the window/tab bar.
    pub name: String,
    /// This window's pane layout and per-client focus.
    pub state: LayoutState,
}

impl WindowState {
    /// Seed an identity once from the first durable terminal. Keep it when the
    /// tree changes, including when that original terminal is removed.
    #[must_use]
    pub fn new(name: String, state: LayoutState) -> Self {
        let id = identity::seed_id(&state);
        Self { id, name, state }
    }
}

mod identity;
mod projection;

pub use projection::{LAYOUT_METADATA_GROUP, MAX_LAYOUT_METADATA_BYTES, projection_key_session};

/// The windows the TUI presents for one Group, plus the (per-client)
/// active index.
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
    pub fn single(pane: ResourceId) -> Self {
        Self {
            windows: vec![WindowState {
                id: identity::terminal_id(&pane),
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

    /// The layout to render and reflow against: a single-leaf layout when
    /// `zoomed` is a live leaf of the active window, else the active window
    /// (so a stale zoom heals itself). Mutation and input routing use
    /// [`Self::active_window`].
    #[must_use]
    pub fn render_window(&self, zoomed: Option<&ResourceId>) -> Option<Cow<'_, LayoutState>> {
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
    pub fn add_window(&mut self, name: String, seed: ResourceId) {
        self.windows.push(WindowState {
            id: identity::fresh_id(&seed, &self.windows),
            name,
            state: LayoutState::single(seed),
        });
        self.active = self.windows.len() - 1;
    }

    /// Drop windows whose tree became empty, keeping `active` on the same
    /// window or its successor. Returns `true` if anything was removed.
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

    /// Remove `target`, refusing to close the workspace's final pane.
    ///
    /// A one-pane window is pruned after the leaf is removed. Focus stays
    /// on a surviving leaf of the affected window when it is still live,
    /// otherwise it moves to the first remaining leaf. `active` follows
    /// that window, or its post-prune successor.
    ///
    /// # Errors
    /// * [`LayoutError::LastPane`] if `target` is the only remaining pane.
    /// * [`LayoutError::PaneNotInLayout`] if `target` is not in any window.
    pub fn close_pane(&mut self, target: &ResourceId) -> Result<(), LayoutError> {
        if pane_count(self) == 1 {
            return Err(LayoutError::LastPane);
        }
        let index = self
            .windows
            .iter()
            .position(|window| {
                window
                    .state
                    .tree
                    .as_ref()
                    .is_some_and(|tree| leaves(tree).contains(target))
            })
            .ok_or_else(|| LayoutError::PaneNotInLayout(target.clone()))?;
        let tree = self.windows[index]
            .state
            .tree
            .as_ref()
            .ok_or_else(|| LayoutError::PaneNotInLayout(target.clone()))?;
        self.windows[index].state.tree = kill_pane(tree, target)?;
        repair_focus(&mut self.windows[index].state);
        self.active = index;
        self.prune_empty_windows();
        Ok(())
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

    /// Move the window at `from` to position `to`; `active` follows its
    /// window. Returns `false` when out of range or `from == to`.
    pub fn move_window(&mut self, from: usize, to: usize) -> bool {
        let len = self.windows.len();
        if from >= len || to >= len || from == to {
            return false;
        }
        let window = self.windows.remove(from);
        self.windows.insert(to, window);
        self.active = shifted_index(self.active, from, to);
        true
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

    /// Encode the workspace as the v3 CBOR envelope (docs/spec/L3.md §3.2).
    ///
    /// # Errors
    /// * [`LayoutEncodeError::Empty`] if there are no windows, or any
    ///   window has no tree/focus (an un-seeded window can't be encoded).
    /// * [`LayoutEncodeError::Cbor`] if ciborium fails to encode.
    pub fn encode_cbor(&self) -> Result<Vec<u8>, LayoutEncodeError> {
        if self.windows.is_empty() {
            return Err(LayoutEncodeError::Empty);
        }
        self.encode_topology_cbor()
    }

    /// Encode shared topology, including an intentionally empty workspace.
    /// Empty differs from missing metadata: consumers must not synthesize windows.
    ///
    /// # Errors
    /// Returns [`LayoutEncodeError::Empty`] for an unseeded window, or
    /// [`LayoutEncodeError::Cbor`] if serialization fails.
    pub fn encode_topology_cbor(&self) -> Result<Vec<u8>, LayoutEncodeError> {
        let mut windows = Vec::with_capacity(self.windows.len());
        for w in &self.windows {
            if w.id == [0; 16] {
                return Err(LayoutEncodeError::Empty);
            }
            let (Some(tree), Some(focus)) = (w.state.tree.as_ref(), w.state.focus.as_ref()) else {
                return Err(LayoutEncodeError::Empty);
            };
            windows.push(CborWindow {
                id: w.id,
                name: w.name.clone(),
                root: CborLayoutNode::from(tree),
                focused_terminal: CborResourceId::from(focus),
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

    /// Decode a current v3 layout blob into a [`Workspace`]. Missing identities
    /// and earlier schema versions are refused without migration or fallback.
    ///
    /// # Errors
    /// * [`LayoutDecodeError::UnsupportedVersion`] for any version byte
    ///   other than 3.
    /// * [`LayoutDecodeError::MalformedRatio`] if any `Split.ratio` is
    ///   NaN, infinite, or outside `(0.0, 1.0)`.
    /// * [`LayoutDecodeError::Cbor`] for malformed CBOR.
    pub fn decode_cbor(bytes: &[u8]) -> Result<Self, LayoutDecodeError> {
        // Probe the version, then decode the whole matching envelope.
        let probe: VersionProbe = ciborium::de::from_reader(Cursor::new(bytes))
            .map_err(|e| LayoutDecodeError::Cbor(e.to_string()))?;
        if probe.version != LAYOUT_ENVELOPE_VERSION {
            return Err(LayoutDecodeError::UnsupportedVersion(probe.version));
        }
        Self::decode_current(bytes)
    }

    fn decode_current(bytes: &[u8]) -> Result<Self, LayoutDecodeError> {
        let envelope: CborWorkspaceEnvelope = ciborium::de::from_reader(Cursor::new(bytes))
            .map_err(|e| LayoutDecodeError::Cbor(e.to_string()))?;
        let mut windows = Vec::with_capacity(envelope.windows.len());
        for w in envelope.windows {
            let tree = w.root.into_layout_node()?;
            let focus: ResourceId = w.focused_terminal.into();
            windows.push(WindowState {
                id: w.id,
                name: w.name,
                state: LayoutState {
                    tree: Some(tree),
                    focus: Some(focus),
                },
            });
        }
        identity::validate(&windows)?;
        let active = (envelope.focused_window_index as usize).min(windows.len().saturating_sub(1));
        Ok(Self { windows, active })
    }

    /// Decode a layout blob and reject any leaf `is_terminal` refuses.
    ///
    /// The envelope carries bare ids, so the caller supplies what it knows
    /// about resource kinds. Ids the caller cannot classify (a peer session's
    /// panes, say) must return `true`: the check refuses known non-terminal
    /// resources, it does not demand proof of terminal-ness.
    ///
    /// # Errors
    /// Every [`Self::decode_cbor`] error, plus
    /// [`LayoutDecodeError::NonTerminalLeaf`] naming the first refused leaf in
    /// window then depth-first order.
    pub fn decode_cbor_checked(
        bytes: &[u8],
        is_terminal: &dyn Fn(&ResourceId) -> bool,
    ) -> Result<Self, LayoutDecodeError> {
        let workspace = Self::decode_cbor(bytes)?;
        let refused = workspace
            .windows
            .iter()
            .filter_map(|window| window.state.tree.as_ref())
            .flat_map(leaves)
            .find(|leaf| !is_terminal(leaf));
        refused.map_or(Ok(workspace), |leaf| {
            Err(LayoutDecodeError::NonTerminalLeaf(leaf))
        })
    }
}

fn pane_count(workspace: &Workspace) -> usize {
    workspace
        .windows
        .iter()
        .filter_map(|window| window.state.tree.as_ref())
        .map(|tree| leaves(tree).len())
        .sum()
}

fn repair_focus(state: &mut LayoutState) {
    state.focus = state.tree.as_ref().and_then(|tree| {
        let panes = leaves(tree);
        state
            .focus
            .as_ref()
            .filter(|focus| panes.contains(focus))
            .cloned()
            .or_else(|| panes.into_iter().next())
    });
}

/// Where index `i` lands after the element at `from` moves to `to`.
const fn shifted_index(i: usize, from: usize, to: usize) -> usize {
    if i == from {
        to
    } else if from < i && i <= to {
        i - 1
    } else if to <= i && i < from {
        i + 1
    } else {
        i
    }
}

/// Split the leaf for `target`: it becomes the `left` child and `new_pane`
/// the `right` child of a new split along `dir` at `ratio`.
///
/// # Errors
/// * [`LayoutError::PaneNotInLayout`] if `target` is not present.
/// * [`LayoutError::InvalidRatio`] if `ratio` is NaN or outside `(0, 1)`.
pub fn split_at(
    tree: &LayoutNode,
    target: &ResourceId,
    new_pane: &ResourceId,
    dir: SplitDir,
    ratio: f32,
) -> Result<LayoutNode, LayoutError> {
    validate_ratio(ratio)?;
    if !contains(tree, target) {
        return Err(LayoutError::PaneNotInLayout(target.clone()));
    }
    Ok(split_inner(tree, target, new_pane, dir, ratio))
}

/// Replace the `ratio` of the split addressed by `path` (callers clamp).
/// `None` when `path` no longer addresses a split.
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
    }
}

fn split_inner(
    node: &LayoutNode,
    target: &ResourceId,
    new_pane: &ResourceId,
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
    }
}

/// Remove the leaf for `target`, collapsing its parent split. `Ok(None)`
/// when it was the last leaf.
///
/// # Errors
/// [`LayoutError::PaneNotInLayout`] if `target` is not present.
pub fn kill_pane(
    tree: &LayoutNode,
    target: &ResourceId,
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
    }
}

fn collapse(node: &LayoutNode, target: &ResourceId) -> (LayoutNode, bool) {
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
    }
}

/// The neighbour of `current` in direction `dir`, if any.
#[must_use]
pub fn focus_direction(
    tree: &LayoutNode,
    current: &ResourceId,
    dir: Direction,
) -> Option<ResourceId> {
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

fn contains(node: &LayoutNode, target: &ResourceId) -> bool {
    match node {
        LayoutNode::Leaf(p) => p == target,
        LayoutNode::Split { left, right, .. } => contains(left, target) || contains(right, target),
    }
}

/// Every leaf of `node` in left-to-right depth-first order.
#[must_use]
pub fn leaves(node: &LayoutNode) -> Vec<ResourceId> {
    let mut out = Vec::new();
    collect_leaves(node, &mut out);
    out
}

fn collect_leaves(node: &LayoutNode, out: &mut Vec<ResourceId>) {
    match node {
        LayoutNode::Leaf(p) => out.push(p.clone()),
        LayoutNode::Split { left, right, .. } => {
            collect_leaves(left, out);
            collect_leaves(right, out);
        }
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
    target: &ResourceId,
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
) -> ResourceId {
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

/// CBOR shadow types + conversions for layout persistence (L3 metadata).
mod serialize;

use serialize::{CborLayoutNode, CborResourceId, CborWindow, CborWorkspaceEnvelope, VersionProbe};

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use std::collections::HashSet;

    use proptest::prelude::*;

    use super::*;
    use serde::Serialize;

    fn t(id: u32) -> ResourceId {
        ResourceId::local(id)
    }

    fn leaf(id: u32) -> LayoutNode {
        LayoutNode::Leaf(t(id))
    }

    fn encode_state(state: &LayoutState) -> Vec<u8> {
        Workspace {
            windows: vec![WindowState::new("1".into(), state.clone())],
            active: 0,
        }
        .encode_cbor()
        .unwrap()
    }

    fn ws_split(a: u32, b: u32, focus: u32) -> Workspace {
        let tree = split_at(&leaf(a), &t(a), &t(b), SplitDir::Horizontal, 0.5).unwrap();
        Workspace {
            windows: vec![WindowState::new(
                "1".to_owned(),
                LayoutState {
                    tree: Some(tree),
                    focus: Some(t(focus)),
                },
            )],
            active: 0,
        }
    }

    #[test]
    fn checked_decode_refuses_a_known_non_terminal_leaf() {
        let ws = ws_split(1, 2, 1);
        let bytes = ws.encode_cbor().expect("encode");
        let refused = Workspace::decode_cbor_checked(&bytes, &|id| id != &t(2))
            .expect_err("a leaf naming a non-terminal resource must be refused");
        assert!(matches!(
            refused,
            LayoutDecodeError::NonTerminalLeaf(ref id) if id == &t(2)
        ));
        // Unclassifiable ids pass: the predicate only refuses what it knows.
        let accepted = Workspace::decode_cbor_checked(&bytes, &|_| true).expect("decode");
        assert_eq!(accepted, ws);
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
    fn kill_pane_collapses_the_parent_and_reports_last_and_missing_leaves() {
        assert!(kill_pane(&leaf(1), &t(1)).unwrap().is_none(), "last leaf");
        assert!(matches!(
            kill_pane(&leaf(1), &t(99)),
            Err(LayoutError::PaneNotInLayout(_))
        ));
        let t1 = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        let out = kill_pane(&t1, &t(2)).unwrap().expect("non-empty");
        assert!(matches!(out, LayoutNode::Leaf(ref p) if *p == t(1)));
        let t2 = split_at(&t1, &t(2), &t(3), SplitDir::Vertical, 0.5).unwrap();
        let out = kill_pane(&t2, &t(1)).unwrap().expect("non-empty");
        assert_eq!(leaves(&out), vec![t(2), t(3)]);
    }

    #[test]
    fn focus_direction_right_across_split() {
        let tree = split_at(&leaf(1), &t(1), &t(2), SplitDir::Horizontal, 0.5).unwrap();
        assert_eq!(focus_direction(&tree, &t(1), Direction::Right), Some(t(2)));
        assert_eq!(focus_direction(&tree, &t(2), Direction::Left), Some(t(1)));
        assert_eq!(focus_direction(&tree, &t(1), Direction::Up), None);
        assert_eq!(focus_direction(&tree, &t(99), Direction::Right), None);
    }

    #[test]
    fn cbor_round_trip_satellite_focus() {
        let focus = ResourceId::satellite("peer.example", 42);
        let state = LayoutState {
            tree: Some(LayoutNode::Leaf(focus.clone())),
            focus: Some(focus),
        };
        let bytes = encode_state(&state);
        let decoded = Workspace::decode_cbor(&bytes).unwrap();
        assert_eq!(decoded.windows[0].state, state);
    }

    #[test]
    fn cbor_rejects_malformed_ratio() {
        let bytes = ws_split(1, 2, 1).encode_cbor().unwrap();
        let mut forged: CborWorkspaceEnvelope =
            ciborium::de::from_reader(bytes.as_slice()).unwrap();
        let CborLayoutNode::Split { ratio, .. } = &mut forged.windows[0].root else {
            panic!("split")
        };
        *ratio = 2.0;
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&forged, &mut buf).unwrap();
        let err = Workspace::decode_cbor(&buf).unwrap_err();
        assert!(matches!(err, LayoutDecodeError::MalformedRatio(_)));
    }

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

    fn names(ws: &Workspace) -> Vec<&str> {
        ws.windows.iter().map(|w| w.name.as_str()).collect()
    }

    #[test]
    fn move_window_reorders_and_active_follows_its_window() {
        let mut ws = ws3(); // active = "3"
        assert!(ws.move_window(2, 0));
        assert_eq!(names(&ws), ["3", "1", "2"]);
        assert_eq!(ws.windows[ws.active].name, "3");

        assert!(ws.move_window(0, 2));
        assert_eq!(names(&ws), ["1", "2", "3"]);
        assert_eq!(ws.windows[ws.active].name, "3");
    }

    #[test]
    fn move_window_keeps_an_uninvolved_active_window_selected() {
        for (from, to) in [(0, 2), (2, 0), (0, 1), (1, 2), (2, 1), (1, 0)] {
            for active in 0..3 {
                let mut ws = ws3();
                ws.select(active);
                let focused = ws.windows[active].name.clone();
                assert!(ws.move_window(from, to));
                assert_eq!(
                    ws.windows[ws.active].name, focused,
                    "move {from}->{to} with active {active}"
                );
            }
        }
    }

    #[test]
    fn move_window_out_of_range_or_in_place_is_noop() {
        let mut ws = ws3();
        assert!(!ws.move_window(1, 1));
        assert!(!ws.move_window(3, 0));
        assert!(!ws.move_window(0, 3));
        assert_eq!(names(&ws), ["1", "2", "3"]);
        assert_eq!(ws.active, 2);
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

    #[test]
    fn close_pane_refuses_the_last_pane_without_mutation() {
        let mut ws = Workspace::single(t(1));
        let original = ws.clone();
        assert!(matches!(ws.close_pane(&t(1)), Err(LayoutError::LastPane)));
        assert_eq!(ws, original);
    }

    #[test]
    fn close_pane_refuses_a_foreign_target_without_mutation() {
        let mut ws = ws3();
        let original = ws.clone();
        assert!(matches!(
            ws.close_pane(&t(99)),
            Err(LayoutError::PaneNotInLayout(id)) if id == t(99)
        ));
        assert_eq!(ws, original);
    }

    #[test]
    fn close_pane_repairs_focus_onto_the_first_surviving_leaf() {
        let mut ws = ws_split(1, 2, 2);
        ws.close_pane(&t(2)).unwrap();
        assert_eq!(ws.windows.len(), 1);
        assert_eq!(
            leaves(ws.windows[0].state.tree.as_ref().unwrap()),
            vec![t(1)]
        );
        assert_eq!(ws.windows[0].state.focus, Some(t(1)));
        assert_eq!(ws.active, 0);
    }

    #[test]
    fn close_pane_prunes_an_emptied_window_and_keeps_a_survivor() {
        let mut ws = ws3();
        ws.select(0);
        ws.close_pane(&t(2)).unwrap();
        assert_eq!(ws.windows.len(), 2);
        assert_eq!(ws.windows[0].name, "1");
        assert_eq!(ws.windows[1].name, "3");
        assert_eq!(ws.windows[0].state.focus, Some(t(1)));
        assert_eq!(ws.windows[1].state.focus, Some(t(3)));
        assert_eq!(ws.active, 1);
        assert_eq!(
            leaves(ws.windows[1].state.tree.as_ref().unwrap()),
            vec![t(3)]
        );
    }

    #[test]
    fn unsupported_schema_versions_are_refused_without_migration() {
        for version in [1, 2, 99] {
            let value = ciborium::Value::Map(vec![(
                ciborium::Value::Text("version".into()),
                ciborium::Value::Integer(version.into()),
            )]);
            let mut bytes = Vec::new();
            ciborium::ser::into_writer(&value, &mut bytes).unwrap();
            assert!(
                matches!(Workspace::decode_cbor(&bytes), Err(LayoutDecodeError::UnsupportedVersion(found)) if found == version)
            );
        }
    }

    #[test]
    fn cbor_current_focused_index_out_of_range_clamps() {
        #[derive(Serialize)]
        struct ForgedWin {
            id: [u8; 16],
            name: String,
            root: CborLayoutNode,
            focused_terminal: CborResourceId,
        }
        #[derive(Serialize)]
        struct Forged {
            version: u8,
            windows: Vec<ForgedWin>,
            focused_window_index: u32,
        }
        let forged = Forged {
            version: LAYOUT_ENVELOPE_VERSION,
            windows: vec![ForgedWin {
                id: [1; 16],
                name: "1".to_owned(),
                root: CborLayoutNode::Leaf {
                    pane: CborResourceId::Local { id: 1 },
                },
                focused_terminal: CborResourceId::Local { id: 1 },
            }],
            focused_window_index: 99,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&forged, &mut buf).unwrap();
        let ws = Workspace::decode_cbor(&buf).unwrap();
        assert_eq!(ws.active, 0); // clamped to last (only) window
    }

    #[test]
    fn cbor_workspace_rejects_empty() {
        let err = Workspace::default().encode_cbor().unwrap_err();
        assert!(matches!(err, LayoutEncodeError::Empty));
    }

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
    fn apply_ops(ops: Vec<Op>) -> (Option<LayoutNode>, Vec<ResourceId>) {
        apply_ops_from(ops, 1)
    }

    fn apply_ops_from(ops: Vec<Op>, mut next_id: u32) -> (Option<LayoutNode>, Vec<ResourceId>) {
        let first = ResourceId::local(next_id);
        next_id += 1;
        let mut tree: Option<LayoutNode> = Some(LayoutNode::Leaf(first.clone()));
        let mut alive: Vec<ResourceId> = vec![first];

        for op in ops {
            match op {
                Op::AddPane => {
                    let new_pane = ResourceId::local(next_id);
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

        /// Invariant 1: every ResourceId appears as exactly one leaf.
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
            let bytes = encode_state(&state);
            let decoded = Workspace::decode_cbor(&bytes).expect("decode");
            prop_assert_eq!(&decoded.windows[0].state, &state);
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
                // A workspace has one durable namespace, not independently
                // reused terminal IDs in each generated window.
                let seed = u32::try_from(i).unwrap() * 100 + 1;
                let (tree, alive) = apply_ops_from(ops, seed);
                let (Some(tree), Some(focus)) = (tree, alive.last().cloned()) else {
                    continue;
                };
                windows.push(WindowState::new((i + 1).to_string(), LayoutState { tree: Some(tree), focus: Some(focus) }));
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
