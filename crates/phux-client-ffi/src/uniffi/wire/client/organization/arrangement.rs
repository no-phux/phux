//! The arrangement surface of [`RemoteClient`]: a session's shared split
//! layout, read as flat node lists and edited with typed mutations.
//!
//! The layout is the session's arrangement record (docs/organization.md).
//! Focus, selection and zoom are client-local and never cross here. Each
//! group's tree crosses as nodes in preorder, root first, because a binding
//! record cannot be recursive; a split names its children by index. Leaves
//! name panes by the same `terminal_id` [`PaneDescriptor`] carries.
//!
//! An edit names the revision its caller rendered. The client refuses
//! `stale_arrangement` when the replica has moved on, so a gesture made
//! against an old layout never lands on a new one; the caller re-reads with
//! [`RemoteClient::arrangement`]. Otherwise the edit is applied to the held
//! document and written with a compare-and-swap `ARRANGEMENT_REPLACE`; the
//! outcome arrives as [`WireEvent::OrganizationOperation`] under the
//! returned operation id. Reason words: `not_connected`,
//! `invalid_session_key`, `arrangement_not_held` (call `fetch_arrangement`),
//! `stale_arrangement`, `invalid_terminal_id`, `invalid_group_id`,
//! `invalid_node`, `invalid_ratio`, `invalid_group_index`, `foreign_target`,
//! `duplicate_pane`, `same_pane`, `last_pane`, `unkeyed_pane`, and the
//! submission words of the organization surface.

use phux_client_core::layout::{
    ArrangementDoc, LayoutError, LayoutMutation, LayoutNode, MutationError, SplitDir,
    apply_mutation,
};
use phux_client_runtime::control::ArrangementSubmitError;
use phux_protocol::ids::SessionKey;

use super::*;
use super::{operation_text, submit_error as journal_error};

/// The axis a split divides along.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireSplitDir {
    /// Children side by side, a vertical divider between them.
    SideBySide,
    /// Children stacked, a horizontal divider between them.
    Stacked,
}

impl From<SplitDir> for WireSplitDir {
    fn from(value: SplitDir) -> Self {
        match value {
            SplitDir::Horizontal => Self::SideBySide,
            SplitDir::Vertical => Self::Stacked,
        }
    }
}

impl From<WireSplitDir> for SplitDir {
    fn from(value: WireSplitDir) -> Self {
        match value {
            WireSplitDir::SideBySide => Self::Horizontal,
            WireSplitDir::Stacked => Self::Vertical,
        }
    }
}

/// One node of a group's split tree.
#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum ArrangementNodeDescriptor {
    /// One pane.
    Leaf {
        /// The pane's `terminal_id`.
        terminal_id: String,
    },
    /// Two children sharing this node's rectangle.
    Split {
        dir: WireSplitDir,
        /// Share of the rectangle given to `first`, in `(0, 1)`.
        ratio: f32,
        /// Index of the left or top child in the group's `nodes`.
        first: u32,
        /// Index of the right or bottom child.
        second: u32,
    },
}

/// One group (a tab) of an arrangement.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct ArrangementGroupDescriptor {
    /// The group's identity, 32 hex digits; the same as `PaneDescriptor`'s
    /// `group_id`.
    pub id: String,
    pub name: String,
    /// The tree in preorder, root at index 0; empty when the group shows
    /// nothing that executes here.
    pub nodes: Vec<ArrangementNodeDescriptor>,
}

/// A session's arrangement at one revision.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct ArrangementDescriptor {
    pub session_key: String,
    /// The revision an edit names as `expected_revision`.
    pub revision: u64,
    /// The groups, in display order.
    pub groups: Vec<ArrangementGroupDescriptor>,
    /// Arranged entries that do not execute here, kept on write-back but not
    /// shown.
    pub unplaced: u32,
}

/// One edit of a session's arrangement. Panes are named by `terminal_id`.
#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum ArrangementEdit {
    /// Set the ratio of the split at `node` in a group's preorder `nodes`.
    SetRatio {
        group_id: String,
        node: u32,
        ratio: f32,
    },
    /// Exchange two panes' places.
    Swap { first: String, second: String },
    /// Take `source` out of its place and split `target` with it; `ratio`
    /// is `target`'s share.
    Move {
        source: String,
        target: String,
        dir: WireSplitDir,
        ratio: f32,
    },
    /// Split `target` with `new_pane`, a member not yet arranged; `ratio`
    /// is `target`'s share.
    Split {
        target: String,
        new_pane: String,
        dir: WireSplitDir,
        ratio: f32,
    },
    /// Stop showing `target`, collapsing its split; the pane keeps running.
    Close { target: String },
    /// Move the group at index `from` to index `to`.
    MoveGroup { from: u32, to: u32 },
}

fn reason(word: &str) -> WireError {
    WireError::Runtime {
        reason: word.to_owned(),
    }
}

fn index(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Push `node`'s subtree onto `nodes` in preorder; returns its index.
fn flatten(node: &LayoutNode, nodes: &mut Vec<ArrangementNodeDescriptor>) -> u32 {
    let at = nodes.len();
    match node {
        LayoutNode::Leaf(id) => nodes.push(ArrangementNodeDescriptor::Leaf {
            terminal_id: id::encode(id),
        }),
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => {
            nodes.push(ArrangementNodeDescriptor::Split {
                dir: (*dir).into(),
                ratio: *ratio,
                first: 0,
                second: 0,
            });
            let first_index = flatten(left, nodes);
            let second_index = flatten(right, nodes);
            if let ArrangementNodeDescriptor::Split { first, second, .. } = &mut nodes[at] {
                *first = first_index;
                *second = second_index;
            }
        }
    }
    index(at)
}

/// The groups of a handle-space document as descriptors.
fn describe(document: &ArrangementDoc) -> Vec<ArrangementGroupDescriptor> {
    document
        .groups
        .iter()
        .map(|group| {
            let mut nodes = Vec::new();
            if let Some(tree) = &group.state.tree {
                flatten(tree, &mut nodes);
            }
            ArrangementGroupDescriptor {
                id: topology::group_id_text(&group.id),
                name: group.name.clone(),
                nodes,
            }
        })
        .collect()
}

/// Where a preorder walk stands after visiting a subtree.
enum Retune {
    /// The node lies further on; this many nodes remain before it.
    Ahead(u32),
    /// The node was a split and now carries the ratio.
    Done,
    /// The node is a leaf.
    NotASplit,
}

/// Set the ratio of the split `remaining` nodes into `node`'s preorder.
fn retune(node: &mut LayoutNode, remaining: u32, ratio: f32) -> Retune {
    match node {
        LayoutNode::Leaf(_) if remaining == 0 => Retune::NotASplit,
        LayoutNode::Leaf(_) => Retune::Ahead(remaining - 1),
        LayoutNode::Split { ratio: held, .. } if remaining == 0 => {
            *held = ratio;
            Retune::Done
        }
        LayoutNode::Split { left, right, .. } => match retune(left, remaining - 1, ratio) {
            Retune::Ahead(rest) => retune(right, rest, ratio),
            found => found,
        },
    }
}

fn valid_ratio(ratio: f32) -> Result<f32, WireError> {
    if ratio.is_finite() && ratio > 0.0 && ratio < 1.0 {
        Ok(ratio)
    } else {
        Err(reason("invalid_ratio"))
    }
}

fn pane(terminal_id: &str) -> Result<ResourceId, WireError> {
    id::parse(terminal_id).ok_or_else(|| reason("invalid_terminal_id"))
}

fn set_ratio(
    document: &mut ArrangementDoc,
    group_id: &str,
    node: u32,
    ratio: f32,
) -> Result<(), WireError> {
    let ratio = valid_ratio(ratio)?;
    let group = document
        .groups
        .iter_mut()
        .find(|group| topology::group_id_text(&group.id) == group_id)
        .ok_or_else(|| reason("invalid_group_id"))?;
    let tree = group
        .state
        .tree
        .as_mut()
        .ok_or_else(|| reason("invalid_node"))?;
    match retune(tree, node, ratio) {
        Retune::Done => Ok(()),
        Retune::Ahead(_) | Retune::NotASplit => Err(reason("invalid_node")),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Result::map_err consumes the error"
)]
fn mutation_error(error: MutationError) -> WireError {
    reason(match error {
        MutationError::Layout(LayoutError::InvalidRatio(_)) => "invalid_ratio",
        MutationError::Layout(LayoutError::LastPane) | MutationError::LastPane => "last_pane",
        MutationError::Layout(LayoutError::PaneNotInLayout(_))
        | MutationError::ForeignTarget(_) => "foreign_target",
        MutationError::DuplicatePane(_) => "duplicate_pane",
        MutationError::SamePane => "same_pane",
    })
}

fn move_group(document: &mut ArrangementDoc, from: u32, to: u32) -> Result<(), WireError> {
    let from = usize::try_from(from).unwrap_or(usize::MAX);
    let to = usize::try_from(to).unwrap_or(usize::MAX);
    if document.move_group(from, to) {
        Ok(())
    } else {
        Err(reason("invalid_group_index"))
    }
}

/// Apply `edit` to a handle-space document.
fn apply_edit(document: &mut ArrangementDoc, edit: &ArrangementEdit) -> Result<(), WireError> {
    let mutation = match edit {
        ArrangementEdit::SetRatio {
            group_id,
            node,
            ratio,
        } => return set_ratio(document, group_id, *node, *ratio),
        ArrangementEdit::MoveGroup { from, to } => return move_group(document, *from, *to),
        ArrangementEdit::Swap { first, second } => LayoutMutation::Swap {
            first: pane(first)?,
            second: pane(second)?,
        },
        ArrangementEdit::Move {
            source,
            target,
            dir,
            ratio,
        } => LayoutMutation::Move {
            source: pane(source)?,
            target: pane(target)?,
            dir: (*dir).into(),
            ratio: valid_ratio(*ratio)?,
        },
        // Focus is client-local: placing a pane never publishes it.
        ArrangementEdit::Split {
            target,
            new_pane,
            dir,
            ratio,
        } => LayoutMutation::SplitPreservingFocus {
            target: pane(target)?,
            new_pane: pane(new_pane)?,
            dir: (*dir).into(),
            ratio: valid_ratio(*ratio)?,
        },
        ArrangementEdit::Close { target } => LayoutMutation::Close {
            target: pane(target)?,
        },
    };
    apply_mutation(document, &mutation).map_err(mutation_error)
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Result::map_err consumes the error"
)]
fn submit_error(error: ArrangementSubmitError) -> WireError {
    match error {
        ArrangementSubmitError::NotHeld => reason("arrangement_not_held"),
        ArrangementSubmitError::Wire(_) => reason("unkeyed_pane"),
        ArrangementSubmitError::Submit(error) => journal_error(error),
    }
}

fn session_key(text: &str) -> Result<SessionKey, WireError> {
    SessionKey::parse(text).ok_or_else(|| reason("invalid_session_key"))
}

#[uniffi::export]
impl RemoteClient {
    /// A session's arrangement as the replica holds it. `None` until
    /// `fetch_arrangement` has read it, or without `ORGANIZATION`. The
    /// replica re-reads a held arrangement when it changes and reports
    /// [`WireEvent::OrganizationChanged`]; read again then.
    ///
    /// # Errors
    /// `invalid_session_key`.
    pub fn arrangement(
        &self,
        session_key_text: String,
    ) -> Result<Option<ArrangementDescriptor>, WireError> {
        let key = session_key(&session_key_text)?;
        let Some(client) = self.runtime_client() else {
            return Ok(None);
        };
        let replica = client.organization();
        Ok(replica
            .arrangement_document(&key)
            .map(|(_, revision, document, binding)| ArrangementDescriptor {
                session_key: key.to_string(),
                revision: revision.get(),
                groups: describe(&document),
                unplaced: index(binding.unplaced_count()),
            }))
    }

    /// Edit a session's arrangement as rendered at `expected_revision`.
    /// Returns the operation id its outcome is reported under.
    ///
    /// # Errors
    /// See the module docs.
    pub fn edit_arrangement(
        &self,
        session_key_text: String,
        expected_revision: u64,
        edit: ArrangementEdit,
    ) -> Result<String, WireError> {
        let key = session_key(&session_key_text)?;
        let client = self
            .runtime_client()
            .ok_or_else(|| reason("not_connected"))?;
        let (_, revision, mut document, _) = client
            .organization()
            .arrangement_document(&key)
            .ok_or_else(|| reason("arrangement_not_held"))?;
        if revision.get() != expected_revision {
            return Err(reason("stale_arrangement"));
        }
        apply_edit(&mut document, &edit)?;
        client
            .replace_arrangement(key, &document)
            .map(|operation| operation_text(&operation))
            .map_err(submit_error)
    }

    /// Subscribe to a pane without voting on its size, so it can be shown
    /// at the host's geometry; 0 when the id is not a terminal here.
    pub fn attach_terminal_preserving_geometry(&self, terminal_id: String) -> u32 {
        self.with_terminal(&terminal_id, Client::attach_terminal_preserving_geometry)
            .unwrap_or(0)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests;
