use serde::{Deserialize, Serialize};

use super::{
    LayoutDecodeError, LayoutNode, SplitDir, TerminalId, unknown_layout_variant, unknown_split_dir,
};

/// Minimal envelope shape used to peek the `version` byte before
/// committing to a full decode (the version selects the v1 vs v2 shape).
#[derive(Debug, Deserialize)]
pub(super) struct VersionProbe {
    /// The envelope schema version (`1` legacy single-window, `2` workspace).
    pub version: u8,
}

/// The legacy v1 single-window envelope.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct CborEnvelope {
    /// Envelope schema version (`1`).
    pub version: u8,
    /// The window's binary split tree.
    pub root: CborLayoutNode,
    /// The focused leaf at encode time.
    pub focus: CborTerminalId,
}

/// The v2 multi-window envelope (docs/spec/L3.md §3.2).
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct CborWorkspaceEnvelope {
    /// Envelope schema version (`2`).
    pub version: u8,
    /// The workspace's windows, in order.
    pub windows: Vec<CborWindow>,
    /// Index into `windows` of the active window.
    pub focused_window_index: u32,
}

/// One window inside [`CborWorkspaceEnvelope`].
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct CborWindow {
    /// The window's display name.
    pub name: String,
    /// The window's binary split tree.
    pub root: CborLayoutNode,
    /// The focused leaf within this window.
    pub focused_terminal: CborTerminalId,
}

/// CBOR shadow of [`LayoutNode`] — the wire crate exposes no `serde`
/// impls, so this mirrors the shape and converts via `From`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum CborLayoutNode {
    /// A single pane (tree leaf).
    Leaf {
        /// The leaf's terminal id.
        pane: CborTerminalId,
    },
    /// An interior split of two child subtrees.
    Split {
        /// Split orientation.
        dir: CborSplitDir,
        /// Fraction of the parent given to `left`, in `(0.0, 1.0)`.
        ratio: f32,
        /// The first (left/top) child subtree.
        left: Box<Self>,
        /// The second (right/bottom) child subtree.
        right: Box<Self>,
    },
}

/// CBOR shadow of [`SplitDir`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CborSplitDir {
    /// A left/right split (vertical divider between panes).
    Horizontal,
    /// A top/bottom split (horizontal divider between panes).
    Vertical,
}

/// CBOR shadow of [`TerminalId`].
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum CborTerminalId {
    /// A terminal local to this server.
    Local {
        /// The local numeric id.
        id: u32,
    },
    /// A terminal hosted on a federated satellite.
    Satellite {
        /// The satellite host identifier.
        host: String,
        /// The terminal's id on that host.
        id: u32,
    },
}

impl From<SplitDir> for CborSplitDir {
    fn from(value: SplitDir) -> Self {
        match value {
            SplitDir::Horizontal => Self::Horizontal,
            SplitDir::Vertical => Self::Vertical,
            // `SplitDir` is `#[non_exhaustive]` (wire-crate concession);
            // a future variant would be wire-breaking and cannot have
            // been decoded into the in-memory tree this function sees.
            _ => unknown_split_dir(),
        }
    }
}

impl From<CborSplitDir> for SplitDir {
    fn from(value: CborSplitDir) -> Self {
        match value {
            CborSplitDir::Horizontal => Self::Horizontal,
            CborSplitDir::Vertical => Self::Vertical,
        }
    }
}

impl From<&TerminalId> for CborTerminalId {
    fn from(value: &TerminalId) -> Self {
        match value {
            TerminalId::Local { id } => Self::Local { id: *id },
            TerminalId::Satellite { host, id } => Self::Satellite {
                host: host.as_str().to_owned(),
                id: *id,
            },
        }
    }
}

impl From<CborTerminalId> for TerminalId {
    fn from(value: CborTerminalId) -> Self {
        match value {
            CborTerminalId::Local { id } => Self::local(id),
            CborTerminalId::Satellite { host, id } => Self::satellite(host.as_str(), id),
        }
    }
}

impl From<&LayoutNode> for CborLayoutNode {
    fn from(value: &LayoutNode) -> Self {
        match value {
            LayoutNode::Leaf(pane) => Self::Leaf { pane: pane.into() },
            LayoutNode::Split {
                dir,
                ratio,
                left,
                right,
            } => Self::Split {
                dir: (*dir).into(),
                ratio: *ratio,
                left: Box::new(Self::from(left.as_ref())),
                right: Box::new(Self::from(right.as_ref())),
            },
            _ => unknown_layout_variant(),
        }
    }
}

impl CborLayoutNode {
    /// Convert this CBOR shadow back into a wire [`LayoutNode`], validating the split ratio.
    pub(super) fn into_layout_node(self) -> Result<LayoutNode, LayoutDecodeError> {
        Ok(match self {
            Self::Leaf { pane } => LayoutNode::Leaf(pane.into()),
            Self::Split {
                dir,
                ratio,
                left,
                right,
            } => {
                if !ratio.is_finite() || !(0.0..=1.0).contains(&ratio) {
                    return Err(LayoutDecodeError::MalformedRatio(ratio));
                }
                LayoutNode::Split {
                    dir: dir.into(),
                    ratio,
                    left: Box::new(left.into_layout_node()?),
                    right: Box::new(right.into_layout_node()?),
                }
            }
        })
    }
}

// -----------------------------------------------------------------------------
// Tests — unit + property
// -----------------------------------------------------------------------------
