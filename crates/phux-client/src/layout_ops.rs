//! Shared headless operations over the persisted TUI layout.
//!
//! CLI commands and MCP tools use this module to read a session's
//! `phux.tui.layout/v1/<session>` value, decode either the legacy v1 envelope
//! or the current v2 [`Workspace`] envelope, apply a mutation using the
//! existing `phux-client-core` layout types, and write a v2 envelope back with
//! `SET_METADATA`. No layout vocabulary is added to the wire protocol.
//!
//! This module deliberately exposes no headless focus mutation. Per ADR-0049,
//! focus is client-local and attention is the navigation signal; layout
//! metadata writers have no authority to yank an attached client's viewport.
//! The compatibility focus fields remain solely because v2 encoding requires
//! them, and attached clients ignore them during reconciliation.
//!
//! The coordination model is explicitly **last-write-wins**. A mutation is a
//! `GET_METADATA` followed by a whole-value `SET_METADATA`; concurrent writers
//! can overwrite one another. The trailing `GET_METADATA` is both a flush
//! barrier for the fire-and-forget SET and the value returned to the caller.
//! Callers should use a dedicated connection because request waits consume and
//! ignore unrelated frames.

use phux_protocol::ids::{GroupId, SessionId, TerminalId};
use phux_protocol::wire::frame::{FrameKind, Scope};
use thiserror::Error;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::layout::{
    LayoutDecodeError, LayoutEncodeError, LayoutError, LayoutNode, SplitDir, Workspace, kill_pane,
    leaves, split_at,
};

/// Prefix of the conventional per-session TUI layout metadata key.
pub const LAYOUT_KEY: &str = "phux.tui.layout/v1";

/// The static Group used by v0.x servers for layout metadata.
pub const DEFAULT_LAYOUT_GROUP_ID: GroupId = GroupId::new(1);

/// Return the metadata key for `session`.
#[must_use]
pub fn layout_key(session: SessionId) -> String {
    format!("{LAYOUT_KEY}/{}", session.get())
}

/// One pure mutation of a decoded [`Workspace`].
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutMutation {
    /// Insert `new_pane` beside `target`.
    Split {
        /// Existing pane whose leaf is replaced by a split.
        target: TerminalId,
        /// Already-created pane to insert.
        new_pane: TerminalId,
        /// Split axis.
        dir: SplitDir,
        /// Fraction assigned to the existing target, in `(0, 1)`.
        ratio: f32,
    },
    /// Insert a pane without changing serialized active-window or focus fields.
    /// Headless spawn placement uses this so it cannot publish shared focus.
    SplitPreservingFocus {
        /// Existing pane whose leaf is replaced by a split.
        target: TerminalId,
        /// Already-created pane to insert.
        new_pane: TerminalId,
        /// Split axis.
        dir: SplitDir,
        /// Fraction assigned to the existing target, in `(0, 1)`.
        ratio: f32,
    },
    /// Remove `source` from its old parent (collapsing it), then insert it
    /// beside `target`.
    Move {
        /// Existing pane to relocate.
        source: TerminalId,
        /// Existing destination pane.
        target: TerminalId,
        /// Destination split axis.
        dir: SplitDir,
        /// Fraction assigned to `target`, in `(0, 1)`.
        ratio: f32,
    },
    /// Exchange two leaf positions without changing split geometry.
    Swap {
        /// First existing pane.
        first: TerminalId,
        /// Second existing pane.
        second: TerminalId,
    },
    /// Remove `target`, collapsing its parent split. A one-pane window is
    /// removed; the final pane in the workspace cannot be removed because a
    /// v2 envelope cannot encode an empty workspace.
    Close {
        /// Existing pane to remove.
        target: TerminalId,
    },
}

/// Errors from pure mutations and metadata request/reply operations.
#[derive(Debug, Error)]
pub enum LayoutOpsError {
    /// Transport or framing failed.
    #[error(transparent)]
    Transport(#[from] AttachError),
    /// The stored envelope was malformed or unsupported.
    #[error(transparent)]
    Decode(#[from] LayoutDecodeError),
    /// The rewritten v2 envelope could not be encoded.
    #[error(transparent)]
    Encode(#[from] LayoutEncodeError),
    /// An existing tree operation rejected the request.
    #[error(transparent)]
    Layout(#[from] LayoutError),
    /// No layout value exists for the requested session.
    #[error("session has no persisted layout metadata")]
    MissingLayout,
    /// A mutation named a pane outside this workspace.
    #[error("pane is not in this session layout: {0:?}")]
    ForeignTarget(TerminalId),
    /// A split tried to insert an id already present in the workspace.
    #[error("pane is already in this session layout: {0:?}")]
    DuplicatePane(TerminalId),
    /// A two-target operation named the same pane twice.
    #[error("layout operation requires two distinct panes")]
    SamePane,
    /// Closing the final pane would produce an unencodable empty workspace.
    #[error("cannot close the final pane in a persisted layout")]
    LastPane,
    /// The server rejected a correlated request.
    #[error("server refused layout request: {0}")]
    Refused(String),
    /// A future `LayoutNode` variant reached a client that cannot rewrite it.
    #[error("unsupported layout node variant")]
    UnsupportedLayoutNode,
}

/// Stateful request-id allocator and layout metadata client.
///
/// Construct one over a dedicated [`Connection`], then call [`Self::read`] or
/// [`Self::mutate`].
#[derive(Debug)]
pub struct LayoutOps<'a> {
    conn: &'a mut Connection,
    session: SessionId,
    group: GroupId,
    next_request_id: u32,
}

impl<'a> LayoutOps<'a> {
    /// Use the default layout Group and begin allocating at `first_request_id`.
    #[must_use]
    pub const fn new(conn: &'a mut Connection, session: SessionId, first_request_id: u32) -> Self {
        Self::in_group(conn, session, DEFAULT_LAYOUT_GROUP_ID, first_request_id)
    }

    /// Use an explicit Group (primarily useful to non-default server setups).
    #[must_use]
    pub const fn in_group(
        conn: &'a mut Connection,
        session: SessionId,
        group: GroupId,
        first_request_id: u32,
    ) -> Self {
        Self {
            conn,
            session,
            group,
            next_request_id: first_request_id,
        }
    }

    /// Read and decode this session's layout, accepting v1 and v2 envelopes.
    ///
    /// # Errors
    ///
    /// Returns [`LayoutOpsError::MissingLayout`] when the key is absent, plus
    /// transport, refusal, or envelope decode errors.
    pub async fn read(&mut self) -> Result<Workspace, LayoutOpsError> {
        let value = self.get_value().await?;
        let bytes = value.ok_or(LayoutOpsError::MissingLayout)?;
        Workspace::decode_cbor(&bytes).map_err(Into::into)
    }

    /// Read, mutate, encode as v2, SET, then read back the winning value.
    ///
    /// This is deliberately last-write-wins rather than compare-and-set. The
    /// returned workspace is the trailing GET's value, which can differ from
    /// the local rewrite if another writer won the race.
    ///
    /// # Errors
    ///
    /// Returns transport/envelope errors or a mutation-specific rejection.
    pub async fn mutate(&mut self, mutation: LayoutMutation) -> Result<Workspace, LayoutOpsError> {
        let mut workspace = self.read().await?;
        apply_mutation(&mut workspace, &mutation)?;
        self.write_and_confirm(&workspace).await
    }

    /// Mutate the stored workspace, or seed it from `fallback` when this
    /// session has no layout metadata yet.
    ///
    /// # Errors
    ///
    /// Returns transport/envelope errors or a mutation-specific rejection.
    pub async fn mutate_or_seed(
        &mut self,
        fallback: Workspace,
        mutation: LayoutMutation,
    ) -> Result<Workspace, LayoutOpsError> {
        let mut workspace = match self.read().await {
            Ok(workspace) => workspace,
            Err(LayoutOpsError::MissingLayout) => fallback,
            Err(err) => return Err(err),
        };
        apply_mutation(&mut workspace, &mutation)?;
        self.write_and_confirm(&workspace).await
    }

    async fn get_value(&mut self) -> Result<Option<Vec<u8>>, LayoutOpsError> {
        let request_id = self.allocate_request_id();
        self.conn
            .send(&FrameKind::GetMetadata {
                request_id,
                scope: Scope::Group(self.group),
                key: layout_key(self.session),
            })
            .await?;
        self.wait_for_metadata(request_id).await
    }

    async fn write_and_confirm(
        &mut self,
        workspace: &Workspace,
    ) -> Result<Workspace, LayoutOpsError> {
        let bytes = workspace.encode_cbor()?;
        let set_request_id = self.allocate_request_id();
        self.conn
            .send(&FrameKind::SetMetadata {
                request_id: set_request_id,
                scope: Scope::Group(self.group),
                key: layout_key(self.session),
                value: bytes,
            })
            .await?;
        // SET_METADATA has no reply. The ordered trailing GET proves the
        // server consumed it and also reports a concurrent last writer.
        let value = self.get_value().await?;
        let bytes = value.ok_or(LayoutOpsError::MissingLayout)?;
        Workspace::decode_cbor(&bytes).map_err(Into::into)
    }

    async fn wait_for_metadata(
        &mut self,
        expected: u32,
    ) -> Result<Option<Vec<u8>>, LayoutOpsError> {
        loop {
            match self.conn.recv().await? {
                FrameKind::MetadataValue { request_id, value } if request_id == expected => {
                    return Ok(value);
                }
                FrameKind::Error {
                    request_id: Some(request_id),
                    message,
                    ..
                } if request_id == expected => return Err(LayoutOpsError::Refused(message)),
                _ => {}
            }
        }
    }

    const fn allocate_request_id(&mut self) -> u32 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        id
    }
}

/// Apply one mutation without doing I/O.
///
/// This is public so CLI/MCP code can test or compose layout changes without
/// duplicating tree algorithms. It only uses `phux-client-core`'s existing
/// [`Workspace`] and [`LayoutNode`] types.
///
/// # Errors
///
/// Rejects missing/duplicate targets, invalid ratios, same-pane operations,
/// and closing the final pane.
pub fn apply_mutation(
    workspace: &mut Workspace,
    mutation: &LayoutMutation,
) -> Result<(), LayoutOpsError> {
    match mutation {
        LayoutMutation::Split {
            target,
            new_pane,
            dir,
            ratio,
        }
        | LayoutMutation::SplitPreservingFocus {
            target,
            new_pane,
            dir,
            ratio,
        } => {
            if find_window(workspace, new_pane).is_some() {
                return Err(LayoutOpsError::DuplicatePane(new_pane.clone()));
            }
            let index = find_window(workspace, target)
                .ok_or_else(|| LayoutOpsError::ForeignTarget(target.clone()))?;
            let tree = workspace.windows[index]
                .state
                .tree
                .as_ref()
                .ok_or_else(|| LayoutOpsError::ForeignTarget(target.clone()))?;
            workspace.windows[index].state.tree =
                Some(split_at(tree, target, new_pane, *dir, *ratio)?);
            if matches!(mutation, LayoutMutation::Split { .. }) {
                workspace.windows[index].state.focus = Some(new_pane.clone());
                workspace.active = index;
            }
        }
        LayoutMutation::Move {
            source,
            target,
            dir,
            ratio,
        } => apply_move(workspace, source, target, *dir, *ratio)?,
        LayoutMutation::Swap { first, second } => {
            if first == second {
                return Err(LayoutOpsError::SamePane);
            }
            find_window(workspace, first)
                .ok_or_else(|| LayoutOpsError::ForeignTarget(first.clone()))?;
            find_window(workspace, second)
                .ok_or_else(|| LayoutOpsError::ForeignTarget(second.clone()))?;
            for window in &mut workspace.windows {
                if let Some(tree) = window.state.tree.as_ref() {
                    window.state.tree = Some(swap_leaves(tree, first, second)?);
                }
            }
            // Focus follows Terminal identity, not physical leaf position.
        }
        LayoutMutation::Close { target } => {
            if pane_count(workspace) == 1 {
                return Err(LayoutOpsError::LastPane);
            }
            let index = find_window(workspace, target)
                .ok_or_else(|| LayoutOpsError::ForeignTarget(target.clone()))?;
            let tree = workspace.windows[index]
                .state
                .tree
                .as_ref()
                .ok_or_else(|| LayoutOpsError::ForeignTarget(target.clone()))?;
            workspace.windows[index].state.tree = kill_pane(tree, target)?;
            repair_focus(&mut workspace.windows[index].state);
            workspace.active = index;
            workspace.prune_empty_windows();
        }
    }
    Ok(())
}

fn apply_move(
    workspace: &mut Workspace,
    source: &TerminalId,
    target: &TerminalId,
    dir: SplitDir,
    ratio: f32,
) -> Result<(), LayoutOpsError> {
    if source == target {
        return Err(LayoutOpsError::SamePane);
    }
    let source_index = find_window(workspace, source)
        .ok_or_else(|| LayoutOpsError::ForeignTarget(source.clone()))?;
    let target_index = find_window(workspace, target)
        .ok_or_else(|| LayoutOpsError::ForeignTarget(target.clone()))?;
    // Validate the destination and ratio before collapsing the source so the
    // public pure helper is transactional on ordinary validation errors.
    let target_tree = workspace.windows[target_index]
        .state
        .tree
        .as_ref()
        .ok_or_else(|| LayoutOpsError::ForeignTarget(target.clone()))?;
    let _ = split_at(target_tree, target, source, dir, ratio)?;

    if source_index == target_index {
        let tree = workspace.windows[source_index]
            .state
            .tree
            .as_ref()
            .ok_or_else(|| LayoutOpsError::ForeignTarget(source.clone()))?;
        let collapsed = kill_pane(tree, source)?.ok_or(LayoutOpsError::LastPane)?;
        let moved = split_at(&collapsed, target, source, dir, ratio)?;
        workspace.windows[source_index].state.tree = Some(moved);
        workspace.windows[source_index].state.focus = Some(source.clone());
        workspace.active = source_index;
    } else {
        let source_tree = workspace.windows[source_index]
            .state
            .tree
            .as_ref()
            .ok_or_else(|| LayoutOpsError::ForeignTarget(source.clone()))?;
        workspace.windows[source_index].state.tree = kill_pane(source_tree, source)?;
        repair_focus(&mut workspace.windows[source_index].state);

        let target_tree = workspace.windows[target_index]
            .state
            .tree
            .as_ref()
            .ok_or_else(|| LayoutOpsError::ForeignTarget(target.clone()))?;
        workspace.windows[target_index].state.tree =
            Some(split_at(target_tree, target, source, dir, ratio)?);
        workspace.windows[target_index].state.focus = Some(source.clone());
        workspace.active = target_index;
        workspace.prune_empty_windows();
    }
    Ok(())
}

fn find_window(workspace: &Workspace, target: &TerminalId) -> Option<usize> {
    workspace.windows.iter().position(|window| {
        window
            .state
            .tree
            .as_ref()
            .is_some_and(|tree| leaves(tree).contains(target))
    })
}

fn pane_count(workspace: &Workspace) -> usize {
    workspace
        .windows
        .iter()
        .filter_map(|window| window.state.tree.as_ref())
        .map(|tree| leaves(tree).len())
        .sum()
}

fn repair_focus(state: &mut crate::layout::LayoutState) {
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

fn swap_leaves(
    node: &LayoutNode,
    first: &TerminalId,
    second: &TerminalId,
) -> Result<LayoutNode, LayoutOpsError> {
    match node {
        LayoutNode::Leaf(id) if id == first => Ok(LayoutNode::Leaf(second.clone())),
        LayoutNode::Leaf(id) if id == second => Ok(LayoutNode::Leaf(first.clone())),
        LayoutNode::Leaf(id) => Ok(LayoutNode::Leaf(id.clone())),
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => Ok(LayoutNode::Split {
            dir: *dir,
            ratio: *ratio,
            left: Box::new(swap_leaves(left, first, second)?),
            right: Box::new(swap_leaves(right, first, second)?),
        }),
        _ => Err(LayoutOpsError::UnsupportedLayoutNode),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{LayoutState, WindowState};
    use crate::testkit::{ScriptSpec, ScriptedServer};
    use phux_protocol::wire::frame::ErrorCode;

    fn tid(id: u32) -> TerminalId {
        TerminalId::local(id)
    }

    fn split(left: u32, right: u32, dir: SplitDir, ratio: f32) -> LayoutNode {
        LayoutNode::Split {
            dir,
            ratio,
            left: Box::new(LayoutNode::Leaf(tid(left))),
            right: Box::new(LayoutNode::Leaf(tid(right))),
        }
    }

    fn two_window_workspace() -> Workspace {
        Workspace {
            windows: vec![
                WindowState {
                    name: "editor".to_owned(),
                    state: LayoutState {
                        tree: Some(split(1, 2, SplitDir::Horizontal, 0.6)),
                        focus: Some(tid(1)),
                    },
                },
                WindowState {
                    name: "tests".to_owned(),
                    state: LayoutState::single(tid(3)),
                },
            ],
            active: 0,
        }
    }

    // Fixed bytes emitted by the pre-window v1 encoder. Keeping this literal
    // prevents a current encoder change from silently changing the back-compat
    // fixture along with the decoder under test.
    const LEGACY_V1_FIXTURE: &[u8] = &[
        163, 103, 118, 101, 114, 115, 105, 111, 110, 1, 100, 114, 111, 111, 116, 165, 100, 107,
        105, 110, 100, 101, 115, 112, 108, 105, 116, 99, 100, 105, 114, 104, 118, 101, 114, 116,
        105, 99, 97, 108, 101, 114, 97, 116, 105, 111, 249, 52, 0, 100, 108, 101, 102, 116, 162,
        100, 107, 105, 110, 100, 100, 108, 101, 97, 102, 100, 112, 97, 110, 101, 162, 100, 107,
        105, 110, 100, 101, 108, 111, 99, 97, 108, 98, 105, 100, 1, 101, 114, 105, 103, 104, 116,
        162, 100, 107, 105, 110, 100, 100, 108, 101, 97, 102, 100, 112, 97, 110, 101, 162, 100,
        107, 105, 110, 100, 101, 108, 111, 99, 97, 108, 98, 105, 100, 2, 101, 102, 111, 99, 117,
        115, 162, 100, 107, 105, 110, 100, 101, 108, 111, 99, 97, 108, 98, 105, 100, 2,
    ];

    #[test]
    fn fixed_v1_fixture_decodes_and_reencodes_as_v2() {
        let mut workspace = Workspace::decode_cbor(LEGACY_V1_FIXTURE).unwrap();
        assert_eq!(workspace.windows.len(), 1);
        assert_eq!(
            leaves(workspace.active_window().unwrap().tree.as_ref().unwrap()),
            vec![tid(1), tid(2)]
        );
        apply_mutation(
            &mut workspace,
            &LayoutMutation::Split {
                target: tid(1),
                new_pane: tid(4),
                dir: SplitDir::Horizontal,
                ratio: 0.5,
            },
        )
        .unwrap();
        let v2 = workspace.encode_cbor().unwrap();
        assert_eq!(Workspace::decode_cbor(&v2).unwrap(), workspace);
        assert_ne!(LEGACY_V1_FIXTURE, v2, "every write migrates to v2");
    }

    #[test]
    fn v2_fixture_supports_split_and_swap() {
        let fixture = two_window_workspace().encode_cbor().unwrap();
        let mut workspace = Workspace::decode_cbor(&fixture).unwrap();

        apply_mutation(
            &mut workspace,
            &LayoutMutation::Split {
                target: tid(3),
                new_pane: tid(4),
                dir: SplitDir::Vertical,
                ratio: 0.3,
            },
        )
        .unwrap();
        assert_eq!(workspace.windows[1].state.focus, Some(tid(4)));
        assert_eq!(
            leaves(workspace.windows[1].state.tree.as_ref().unwrap()),
            vec![tid(3), tid(4)]
        );

        apply_mutation(
            &mut workspace,
            &LayoutMutation::Swap {
                first: tid(1),
                second: tid(4),
            },
        )
        .unwrap();
        assert_eq!(
            leaves(workspace.windows[0].state.tree.as_ref().unwrap()),
            vec![tid(4), tid(2)]
        );
        assert_eq!(
            leaves(workspace.windows[1].state.tree.as_ref().unwrap()),
            vec![tid(3), tid(1)]
        );
    }

    #[test]
    fn headless_split_preserves_serialized_focus_and_active_window() {
        let mut workspace = two_window_workspace();
        apply_mutation(
            &mut workspace,
            &LayoutMutation::SplitPreservingFocus {
                target: tid(3),
                new_pane: tid(4),
                dir: SplitDir::Vertical,
                ratio: 0.3,
            },
        )
        .unwrap();
        assert_eq!(workspace.active, 0);
        assert_eq!(workspace.windows[0].state.focus, Some(tid(1)));
        assert_eq!(workspace.windows[1].state.focus, Some(tid(3)));
        assert_eq!(
            leaves(workspace.windows[1].state.tree.as_ref().unwrap()),
            vec![tid(3), tid(4)]
        );
    }

    #[test]
    fn close_collapses_nested_parent_and_repairs_focus() {
        let nested = LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(split(1, 2, SplitDir::Vertical, 0.4)),
            right: Box::new(LayoutNode::Leaf(tid(3))),
        };
        let mut workspace = Workspace {
            windows: vec![WindowState {
                name: "1".to_owned(),
                state: LayoutState {
                    tree: Some(nested),
                    focus: Some(tid(2)),
                },
            }],
            active: 0,
        };
        apply_mutation(&mut workspace, &LayoutMutation::Close { target: tid(2) }).unwrap();
        assert_eq!(
            leaves(workspace.active_window().unwrap().tree.as_ref().unwrap()),
            vec![tid(1), tid(3)]
        );
        assert_eq!(workspace.active_window().unwrap().focus, Some(tid(1)));
    }

    #[test]
    fn move_collapses_source_and_can_remove_an_empty_window() {
        let mut workspace = two_window_workspace();
        apply_mutation(
            &mut workspace,
            &LayoutMutation::Move {
                source: tid(3),
                target: tid(2),
                dir: SplitDir::Vertical,
                ratio: 0.7,
            },
        )
        .unwrap();
        assert_eq!(workspace.windows.len(), 1);
        assert_eq!(
            leaves(workspace.windows[0].state.tree.as_ref().unwrap()),
            vec![tid(1), tid(2), tid(3)]
        );
        assert_eq!(workspace.windows[0].state.focus, Some(tid(3)));
        let LayoutNode::Split { right, .. } = workspace.windows[0].state.tree.as_ref().unwrap()
        else {
            panic!("expected outer split");
        };
        assert_eq!(leaves(right), vec![tid(2), tid(3)]);
    }

    #[test]
    fn malformed_and_foreign_targets_are_rejected_without_mutation() {
        assert!(matches!(
            Workspace::decode_cbor(b"not cbor"),
            Err(LayoutDecodeError::Cbor(_))
        ));
        let mut workspace = two_window_workspace();
        let original = workspace.clone();
        let err =
            apply_mutation(&mut workspace, &LayoutMutation::Close { target: tid(99) }).unwrap_err();
        assert!(matches!(err, LayoutOpsError::ForeignTarget(id) if id == tid(99)));
        assert_eq!(workspace, original);

        let err = apply_mutation(
            &mut workspace,
            &LayoutMutation::Split {
                target: tid(1),
                new_pane: tid(2),
                dir: SplitDir::Horizontal,
                ratio: 0.5,
            },
        )
        .unwrap_err();
        assert!(matches!(err, LayoutOpsError::DuplicatePane(id) if id == tid(2)));
        assert!(matches!(
            apply_mutation(
                &mut Workspace::single(tid(1)),
                &LayoutMutation::Close { target: tid(1) }
            ),
            Err(LayoutOpsError::LastPane)
        ));

        let original = workspace.clone();
        assert!(matches!(
            apply_mutation(
                &mut workspace,
                &LayoutMutation::Move {
                    source: tid(1),
                    target: tid(3),
                    dir: SplitDir::Horizontal,
                    ratio: f32::NAN,
                }
            ),
            Err(LayoutOpsError::Layout(LayoutError::InvalidRatio(ratio))) if ratio.is_nan()
        ));
        assert_eq!(workspace, original, "a rejected move is transactional");
    }

    #[tokio::test]
    async fn mutate_correlates_replies_and_confirms_set() {
        let (client_stream, server_stream) = tokio::net::UnixStream::pair().unwrap();
        let mut client = Connection::from_stream(client_stream);
        let initial = two_window_workspace();

        // The harness stores what this session SETs and hands it back on the
        // confirming GET, so the read-modify-write round trip runs against
        // the same read-your-own-write behaviour `handle_set_metadata` /
        // `handle_get_metadata` give it — not a canned echo. The
        // METADATA_VALUE for request 999 belongs to a different pipelined
        // request and is pushed AHEAD of this one's reply, which is the only
        // ordering in which mis-correlation is a hazard.
        let spec = ScriptSpec::new()
            .foreign_metadata_value(999)
            .stored_metadata(
                Scope::Group(DEFAULT_LAYOUT_GROUP_ID),
                &layout_key(SessionId::new(7)),
                initial.encode_cbor().unwrap(),
            );
        let server_task = tokio::spawn(ScriptedServer::on_stream(server_stream, spec).run());

        let mut ops = LayoutOps::new(&mut client, SessionId::new(7), 10);
        let confirmed = ops
            .mutate(LayoutMutation::Swap {
                first: tid(1),
                second: tid(2),
            })
            .await
            .unwrap();
        assert_eq!(
            leaves(confirmed.windows[0].state.tree.as_ref().unwrap()),
            vec![tid(2), tid(1)]
        );
        // `ops` only borrows the connection; dropping the connection is what
        // ends the harness's serve loop.
        drop(client);

        let seen = server_task.await.unwrap();
        assert!(
            matches!(
                seen.first(),
                Some(FrameKind::GetMetadata { request_id: 10, scope, key })
                    if *scope == Scope::Group(DEFAULT_LAYOUT_GROUP_ID)
                        && *key == layout_key(SessionId::new(7))
            ),
            "the read leg is a group-scoped GET on the session's layout key; got {:?}",
            seen.first()
        );
        let Some(FrameKind::SetMetadata {
            request_id: 11,
            value,
            ..
        }) = seen.get(1)
        else {
            panic!("expected SET on request 11, got {:?}", seen.get(1));
        };
        let written = Workspace::decode_cbor(value).unwrap();
        assert_eq!(
            leaves(written.windows[0].state.tree.as_ref().unwrap()),
            vec![tid(2), tid(1)]
        );
        assert!(
            matches!(
                seen.get(2),
                Some(FrameKind::GetMetadata { request_id: 12, .. })
            ),
            "the confirming GET closes the CAS; got {:?}",
            seen.get(2)
        );
    }

    #[tokio::test]
    async fn correlated_error_is_reported() {
        let (client_stream, server_stream) = tokio::net::UnixStream::pair().unwrap();
        let mut client = Connection::from_stream(client_stream);
        let spec = ScriptSpec::new().refuse_metadata(ErrorCode::InvalidCommand, "foreign group");
        let server_task = tokio::spawn(ScriptedServer::on_stream(server_stream, spec).run());
        let mut ops = LayoutOps::in_group(&mut client, SessionId::new(1), GroupId::new(77), 5);
        assert!(
            matches!(ops.read().await, Err(LayoutOpsError::Refused(message)) if message == "foreign group")
        );
        drop(client);
        server_task.await.unwrap();
    }
}
