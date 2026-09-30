//! The existing-pane move shared by headless and interactive clients.
//!
//! Same-session: one confirmed L3 layout mutation. Cross-session: ADR-0056's
//! L1 re-parent, then destination-first layout publication and source
//! cleanup. The [`ResourceId`] is never replaced.

use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, MoveError, MoveResult, Scope, StateScope,
};
use phux_protocol::wire::info::SessionSnapshot;
use thiserror::Error;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::layout::{LayoutNode, SplitDir, Workspace, leaves};
use crate::layout_ops::{
    DEFAULT_LAYOUT_GROUP_ID, LayoutMutation, LayoutOps, LayoutOpsError, layout_key,
    validate_projection_key,
};

/// A confirmed pane move and the destination topology that won publication.
#[derive(Debug, Clone)]
pub struct PaneMoveOutcome {
    /// Fresh server-reported name of the destination session.
    pub destination_session_name: String,
    /// Confirmed destination workspace, suitable for immediate TUI adoption.
    pub destination_workspace: Workspace,
    /// Whether L1 ownership crossed a session boundary.
    pub cross_session: bool,
}

/// Failures from the canonical move transaction.
#[derive(Debug, Error)]
pub enum PaneMoveError {
    /// Source and destination must be distinct.
    #[error("source and destination must be different panes")]
    SamePane,
    /// ADR-0056 currently supports only panes local to the serving server.
    #[error("source and destination must be local panes; satellite panes are not supported")]
    SatellitePane,
    /// The fresh server snapshot no longer contains one selected pane.
    #[error("the {role} pane is no longer present in the server snapshot")]
    UnknownPane {
        /// Which selected pane disappeared.
        role: &'static str,
    },
    /// The destination changed ownership while the move was in flight.
    #[error("the destination pane changed windows while the move was in flight; {rollback}")]
    DestinationChanged {
        /// Whether ownership could be restored.
        rollback: &'static str,
    },
    /// The peer cannot perform ADR-0056 moves.
    #[error("this server predates cross-session pane moves")]
    ServerTooOld,
    /// The server refused the ownership move.
    #[error("server refused the pane move: {0}")]
    MoveRefused(String),
    /// Ownership moved, but the resulting server state could not be read.
    #[error(
        "the server moved the pane but its resulting ownership could not be read ({error}); {rollback}"
    )]
    PostMoveState {
        /// Snapshot failure.
        error: AttachError,
        /// Whether ownership could be restored.
        rollback: &'static str,
    },
    /// Destination geometry did not publish and ownership rollback was attempted.
    #[error("destination layout write failed ({error}); {rollback}")]
    DestinationLayout {
        /// Layout publication failure.
        error: String,
        /// Whether ownership could be restored.
        rollback: &'static str,
    },
    /// Ownership and destination placement committed, but source cleanup failed.
    #[error("the pane was moved and placed, but the source layout could not be cleaned up ({0})")]
    SourceLayout(String),
    /// Same-session metadata operation failed.
    #[error(transparent)]
    Layout(#[from] LayoutOpsError),
    /// Control connection or snapshot protocol failed before ownership changed.
    #[error(transparent)]
    Transport(#[from] AttachError),
    /// A cross-session move must name both `--projection` keys or neither
    /// (ADR-0129: each embeds its own session id).
    #[error(
        "cross-session move-pane must name both the source and destination projection keys, or neither"
    )]
    ProjectionArityMismatch,
    /// A single-session operation named more than one `--projection` key.
    #[error("this operation touches one session layout; pass at most one --projection key")]
    ProjectionArityTooMany,
}

#[derive(Debug)]
struct CrossMovePlan {
    source: ResourceId,
    target: ResourceId,
    source_window: WindowId,
    destination_window: WindowId,
    source_session: SessionId,
    destination_session: SessionId,
    destination_session_name: String,
    dir: SplitDir,
    ratio: f32,
    rollback_owner: Option<ResourceId>,
    /// Named projection keys (ADR-0129); `None` is the default key.
    destination_key: Option<String>,
    source_key: Option<String>,
}

/// Move `source` beside `target` on a dedicated control connection,
/// preserving the source Terminal identity.
///
/// `projection` names the layout envelope(s) instead of the default
/// (ADR-0129): 0 or 1 keys for a same-session move, 0 or 2 cross-session.
///
/// # Errors
///
/// Refuses stale, same-pane, satellite, and invalid-layout selections before
/// changing ownership. Cross-session publication failures report whether the
/// best-effort ownership rollback succeeded.
pub async fn move_pane(
    conn: &mut Connection,
    source: ResourceId,
    target: ResourceId,
    dir: SplitDir,
    ratio: f32,
    projection: &[String],
) -> Result<PaneMoveOutcome, PaneMoveError> {
    if source == target {
        return Err(PaneMoveError::SamePane);
    }
    if !matches!(source, ResourceId::Local { .. }) || !matches!(target, ResourceId::Local { .. }) {
        return Err(PaneMoveError::SatellitePane);
    }

    let snapshot = read_snapshot(conn, 1).await?;
    let source_window =
        window_for(&snapshot, &source).ok_or(PaneMoveError::UnknownPane { role: "source" })?;
    let destination_window = window_for(&snapshot, &target).ok_or(PaneMoveError::UnknownPane {
        role: "destination",
    })?;
    let source_session = session_for_window(&snapshot, source_window)
        .ok_or(PaneMoveError::UnknownPane { role: "source" })?;
    let destination_session =
        session_for_window(&snapshot, destination_window).ok_or(PaneMoveError::UnknownPane {
            role: "destination",
        })?;
    let destination_session_name = session_name(&snapshot, destination_session)
        .ok_or(PaneMoveError::UnknownPane {
            role: "destination",
        })?
        .to_owned();

    if source_session == destination_session {
        let key = resolve_single_session_projection(projection, source_session)?;
        let mut layout = layout_ops(conn, source_session, key, 2)?;
        let workspace = layout
            .mutate(LayoutMutation::Move {
                source,
                target,
                dir,
                ratio,
            })
            .await?;
        drop(layout);
        return Ok(PaneMoveOutcome {
            destination_session_name,
            destination_workspace: workspace,
            cross_session: false,
        });
    }

    if !conn.negotiated_bootstrap().is_some_and(|bootstrap| {
        bootstrap
            .server_features
            .contains(ServerFeature::MoveResource)
    }) {
        return Err(PaneMoveError::ServerTooOld);
    }
    let (source_key, destination_key) =
        resolve_cross_session_projection(projection, source_session, destination_session)?;
    let plan = CrossMovePlan {
        rollback_owner: sibling_in_window(&snapshot, &source),
        source,
        target,
        source_window,
        destination_window,
        source_session,
        destination_session,
        destination_session_name,
        dir,
        ratio,
        destination_key,
        source_key,
    };
    execute_cross_move(conn, &plan).await
}

/// `--projection` over one session envelope: `[]` or one validated key.
fn resolve_single_session_projection(
    projection: &[String],
    session: SessionId,
) -> Result<Option<String>, PaneMoveError> {
    match projection {
        [] => Ok(None),
        [key] => {
            validate_projection_key(key, session)?;
            Ok(Some(key.clone()))
        }
        _ => Err(PaneMoveError::ProjectionArityTooMany),
    }
}

/// `--projection` for a cross-session move: `[]`, or two keys matched to
/// `(source, destination)` by their embedded session ids in either order.
fn resolve_cross_session_projection(
    projection: &[String],
    source_session: SessionId,
    destination_session: SessionId,
) -> Result<(Option<String>, Option<String>), PaneMoveError> {
    let [first, second] = projection else {
        return if projection.is_empty() {
            Ok((None, None))
        } else {
            Err(PaneMoveError::ProjectionArityMismatch)
        };
    };
    let first_session = crate::layout::projection_key_session(first);
    let second_session = crate::layout::projection_key_session(second);
    if first_session == Some(source_session) && second_session == Some(destination_session) {
        return Ok((Some(first.clone()), Some(second.clone())));
    }
    if first_session == Some(destination_session) && second_session == Some(source_session) {
        return Ok((Some(second.clone()), Some(first.clone())));
    }
    Err(PaneMoveError::ProjectionArityMismatch)
}

async fn execute_cross_move(
    conn: &mut Connection,
    plan: &CrossMovePlan,
) -> Result<PaneMoveOutcome, PaneMoveError> {
    request_move(conn, 10, &plan.source, &plan.target).await?;

    let post_move = match read_snapshot(conn, 11).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let rollback = rollback_suffix(conn, plan).await;
            return Err(PaneMoveError::PostMoveState { error, rollback });
        }
    };
    if window_for(&post_move, &plan.source) != Some(plan.destination_window)
        || window_for(&post_move, &plan.target) != Some(plan.destination_window)
    {
        let rollback = rollback_suffix(conn, plan).await;
        return Err(PaneMoveError::DestinationChanged { rollback });
    }
    let source_session_reaped = !post_move
        .sessions
        .iter()
        .any(|session| session.id == plan.source_session);

    let mut destination_layout = match layout_ops(
        conn,
        plan.destination_session,
        plan.destination_key.clone(),
        12,
    ) {
        Ok(layout) => layout,
        Err(error) => {
            let rollback = rollback_suffix(conn, plan).await;
            return Err(PaneMoveError::DestinationLayout {
                error: error.to_string(),
                rollback,
            });
        }
    };
    let destination_workspace = match destination_layout
        .mutate(LayoutMutation::Split {
            target: plan.target.clone(),
            new_pane: plan.source.clone(),
            dir: plan.dir,
            ratio: plan.ratio,
        })
        .await
    {
        Ok(workspace)
            if workspace_has_placement(
                &workspace,
                &plan.target,
                &plan.source,
                plan.dir,
                plan.ratio,
            ) =>
        {
            workspace
        }
        Ok(_) => {
            let rollback = rollback_suffix(conn, plan).await;
            return Err(PaneMoveError::DestinationLayout {
                error: "a concurrent writer replaced the requested placement, or the write \
                        exceeded limits.metadata-value-bytes and was silently dropped (\
                        SET_METADATA has no reply to report that directly)"
                    .to_owned(),
                rollback,
            });
        }
        Err(error) => {
            let rollback = rollback_suffix(conn, plan).await;
            return Err(PaneMoveError::DestinationLayout {
                error: error.to_string(),
                rollback,
            });
        }
    };
    drop(destination_layout);

    let cleanup = if source_session_reaped {
        delete_layout(conn, plan.source_session, plan.source_key.clone(), 20).await
    } else {
        close_source_leaf(conn, plan).await
    };
    if let Err(error) = cleanup {
        return Err(PaneMoveError::SourceLayout(error.to_string()));
    }

    Ok(PaneMoveOutcome {
        destination_session_name: plan.destination_session_name.clone(),
        destination_workspace,
        cross_session: true,
    })
}

async fn request_move(
    conn: &mut Connection,
    request_id: u32,
    source: &ResourceId,
    owner: &ResourceId,
) -> Result<(), PaneMoveError> {
    let frame = FrameKind::MoveResource {
        request_id,
        terminal: source.clone(),
        owner_terminal: owner.clone(),
    };
    let result = conn
        .request_move(&frame)
        .await?
        .into_result_ignoring_interleaved()
        .map_err(|refusal| PaneMoveError::MoveRefused(refusal.message))?;
    match result {
        MoveResult::Ok(_) => Ok(()),
        MoveResult::Err(MoveError::UnsupportedSatelliteRoute) => Err(PaneMoveError::SatellitePane),
        MoveResult::Err(MoveError::MoveFailed(message)) => Err(PaneMoveError::MoveRefused(message)),
        other => Err(PaneMoveError::MoveRefused(format!(
            "unrecognized move result: {other:?}"
        ))),
    }
}

async fn rollback_move(conn: &mut Connection, plan: &CrossMovePlan) -> bool {
    let Some(owner) = &plan.rollback_owner else {
        return false;
    };
    let owner_still_at_source = read_snapshot(conn, 30)
        .await
        .is_ok_and(|snapshot| window_for(&snapshot, owner) == Some(plan.source_window));
    owner_still_at_source
        && request_move(conn, 31, &plan.source, owner).await.is_ok()
        && read_snapshot(conn, 32)
            .await
            .is_ok_and(|snapshot| window_for(&snapshot, &plan.source) == Some(plan.source_window))
}

async fn rollback_suffix(conn: &mut Connection, plan: &CrossMovePlan) -> &'static str {
    if rollback_move(conn, plan).await {
        "the pane was moved back to its original window"
    } else {
        "the pane's current ownership could not be restored; inspect and place it explicitly"
    }
}

async fn read_snapshot(
    conn: &mut Connection,
    request_id: u32,
) -> Result<SessionSnapshot, AttachError> {
    let result = conn
        .request(
            request_id,
            Command::GetState {
                scope: StateScope::Server,
            },
        )
        .await?
        .into_result_ignoring_interleaved();
    match result {
        CommandResult::OkWith(CommandValue::State(snapshot)) => Ok(snapshot),
        other => Err(AttachError::Protocol(format!(
            "GET_STATE returned {other:?}"
        ))),
    }
}

async fn delete_layout(
    conn: &mut Connection,
    session: SessionId,
    key: Option<String>,
    request_id: u32,
) -> Result<(), LayoutOpsError> {
    let key = key.unwrap_or_else(|| layout_key(session));
    conn.send(&FrameKind::DeleteMetadata {
        request_id,
        scope: Scope::Group(DEFAULT_LAYOUT_GROUP_ID),
        key: key.clone(),
    })
    .await?;
    let read = LayoutOps::with_key(conn, session, key, request_id.wrapping_add(1))?
        .read()
        .await;
    match read {
        Err(LayoutOpsError::MissingLayout) => Ok(()),
        Ok(_) => Err(LayoutOpsError::Refused(
            "source layout still exists after deletion".to_owned(),
        )),
        Err(error) => Err(error),
    }
}

/// The layout handle for `session`: the named projection `key`, else the
/// default key.
pub(crate) fn layout_ops(
    conn: &mut Connection,
    session: SessionId,
    key: Option<String>,
    first_request_id: u32,
) -> Result<LayoutOps<'_>, LayoutOpsError> {
    match key {
        Some(key) => LayoutOps::with_key(conn, session, key, first_request_id),
        None => Ok(LayoutOps::new(conn, session, first_request_id)),
    }
}

/// Collapse the source leaf out of its still-live source envelope.
async fn close_source_leaf(
    conn: &mut Connection,
    plan: &CrossMovePlan,
) -> Result<(), LayoutOpsError> {
    let mut layout = layout_ops(conn, plan.source_session, plan.source_key.clone(), 20)?;
    let workspace = layout
        .mutate(LayoutMutation::Close {
            target: plan.source.clone(),
        })
        .await?;
    drop(layout);
    if workspace_contains(&workspace, &plan.source) {
        Err(LayoutOpsError::Refused(
            "a concurrent writer restored the source leaf, or the close write exceeded \
             limits.metadata-value-bytes and was silently dropped (SET_METADATA has no reply \
             to report that directly)"
                .to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn window_for(snapshot: &SessionSnapshot, terminal: &ResourceId) -> Option<WindowId> {
    snapshot
        .resources
        .iter()
        .find(|resource| &resource.id == terminal)
        .map(|resource| resource.window_id)
}

fn session_for_window(snapshot: &SessionSnapshot, window: WindowId) -> Option<SessionId> {
    snapshot
        .windows
        .iter()
        .find(|candidate| candidate.id == window)
        .map(|candidate| candidate.session_id)
}

fn session_name(snapshot: &SessionSnapshot, session: SessionId) -> Option<&str> {
    snapshot
        .sessions
        .iter()
        .find(|candidate| candidate.id == session)
        .map(|candidate| candidate.name.as_str())
}

fn sibling_in_window(snapshot: &SessionSnapshot, source: &ResourceId) -> Option<ResourceId> {
    let source_window = window_for(snapshot, source)?;
    snapshot
        .resources
        .iter()
        .find(|resource| resource.window_id == source_window && &resource.id != source)
        .map(|resource| resource.id.clone())
}

fn workspace_contains(workspace: &Workspace, terminal: &ResourceId) -> bool {
    workspace
        .windows
        .iter()
        .filter_map(|window| window.state.tree.as_ref())
        .any(|tree| leaves(tree).contains(terminal))
}

fn workspace_has_placement(
    workspace: &Workspace,
    target: &ResourceId,
    moved: &ResourceId,
    dir: SplitDir,
    ratio: f32,
) -> bool {
    workspace
        .windows
        .iter()
        .filter_map(|window| window.state.tree.as_ref())
        .any(|tree| tree_has_placement(tree, target, moved, dir, ratio))
}

fn tree_has_placement(
    node: &LayoutNode,
    target: &ResourceId,
    moved: &ResourceId,
    expected_dir: SplitDir,
    expected_ratio: f32,
) -> bool {
    match node {
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => {
            (*dir == expected_dir
                && ratio.to_bits() == expected_ratio.to_bits()
                && matches!(left.as_ref(), LayoutNode::Leaf(id) if id == target)
                && matches!(right.as_ref(), LayoutNode::Leaf(id) if id == moved))
                || tree_has_placement(left, target, moved, expected_dir, expected_ratio)
                || tree_has_placement(right, target, moved, expected_dir, expected_ratio)
        }
        LayoutNode::Leaf(_) => false,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use phux_protocol::caps::{ServerFeature, ServerFeatureSet};
    use phux_protocol::wire::frame::{ErrorCode, FrameKind};
    use phux_protocol::wire::info::{ResourceInfo, SessionInfo, WindowInfo};

    use super::*;
    use crate::layout::{LayoutState, WindowState};
    use crate::testkit::{ScriptSpec, serve_one};

    fn tid(id: u32) -> ResourceId {
        ResourceId::local(id)
    }

    fn split(left: u32, right: u32) -> LayoutNode {
        LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(LayoutNode::Leaf(tid(left))),
            right: Box::new(LayoutNode::Leaf(tid(right))),
        }
    }

    fn workspace(name: &str, tree: LayoutNode, focus: u32) -> Workspace {
        Workspace {
            windows: vec![WindowState::new(
                name.to_owned(),
                LayoutState {
                    tree: Some(tree),
                    focus: Some(tid(focus)),
                },
            )],
            active: 0,
        }
    }

    fn snapshot(source_in_destination: bool, source_session_present: bool) -> SessionSnapshot {
        let source_session = SessionId::new(1);
        let destination_session = SessionId::new(2);
        let source_window = WindowId::new(10);
        let destination_window = WindowId::new(20);
        let mut sessions = vec![SessionInfo::new(destination_session, "dest")];
        let mut windows = vec![WindowInfo::new(
            destination_window,
            destination_session,
            "target",
        )];
        if source_session_present {
            sessions.insert(0, SessionInfo::new(source_session, "source"));
            windows.insert(0, WindowInfo::new(source_window, source_session, "origin"));
        }
        SessionSnapshot::new(destination_session, destination_window, tid(3))
            .with_sessions(sessions)
            .with_windows(windows)
            .with_resources(vec![
                ResourceInfo::new(
                    tid(1),
                    if source_in_destination {
                        destination_window
                    } else {
                        source_window
                    },
                    80,
                    24,
                ),
                ResourceInfo::new(tid(2), source_window, 80, 24),
                ResourceInfo::new(tid(3), destination_window, 80, 24),
            ])
    }

    async fn serve(
        spec: ScriptSpec,
    ) -> (
        tempfile::TempDir,
        Connection,
        tokio::task::JoinHandle<Vec<FrameKind>>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (socket, server) = serve_one(dir.path(), spec);
        let conn = Connection::connect(&socket).await.unwrap();
        (dir, conn, server)
    }

    /// A move-capable server answering `states` in order, `result` to the
    /// move, with source `origin/{1,2}` and destination `target/3` layouts
    /// stored under `keys` (source, destination).
    fn cross_spec(
        states: impl IntoIterator<Item = SessionSnapshot>,
        result: MoveResult,
        (source_key, destination_key): (&str, &str),
        source: LayoutNode,
    ) -> ScriptSpec {
        let group = Scope::Group(DEFAULT_LAYOUT_GROUP_ID);
        ScriptSpec::new()
            .server_features(ServerFeatureSet::with(&[ServerFeature::MoveResource]))
            .states(states)
            .move_result(result)
            .stored_metadata(
                group.clone(),
                source_key,
                workspace("origin", source, 1).encode_cbor().unwrap(),
            )
            .stored_metadata(
                group,
                destination_key,
                workspace("target", LayoutNode::Leaf(tid(3)), 3)
                    .encode_cbor()
                    .unwrap(),
            )
    }

    fn default_keys() -> (String, String) {
        (layout_key(SessionId::new(1)), layout_key(SessionId::new(2)))
    }

    fn spawned_or_moved(seen: &[FrameKind]) -> Vec<(ResourceId, ResourceId)> {
        seen.iter()
            .filter_map(|frame| match frame {
                FrameKind::MoveResource {
                    terminal,
                    owner_terminal,
                    ..
                } => Some((terminal.clone(), owner_terminal.clone())),
                FrameKind::SpawnResource { .. } => panic!("a move must never spawn"),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn same_session_move_changes_layout_without_recreating_the_terminal() {
        let session = SessionId::new(1);
        let window = WindowId::new(10);
        let snapshot = SessionSnapshot::new(session, window, tid(1))
            .with_sessions(vec![SessionInfo::new(session, "work")])
            .with_windows(vec![WindowInfo::new(window, session, "main")])
            .with_resources(vec![
                ResourceInfo::new(tid(1), window, 80, 24),
                ResourceInfo::new(tid(2), window, 80, 24),
                ResourceInfo::new(tid(3), window, 80, 24),
            ]);
        let initial = workspace("main", split(1, 2), 2);
        let spec = ScriptSpec::new().state(snapshot).stored_metadata(
            Scope::Group(DEFAULT_LAYOUT_GROUP_ID),
            &layout_key(session),
            initial.encode_cbor().unwrap(),
        );
        let (_dir, mut conn, server) = serve(spec).await;

        let outcome = move_pane(&mut conn, tid(1), tid(2), SplitDir::Vertical, 0.6, &[])
            .await
            .unwrap();
        assert!(!outcome.cross_session);
        assert_eq!(
            outcome.destination_workspace.windows[0].state.focus,
            Some(tid(1)),
            "focus follows the moved Terminal identity"
        );
        drop(conn);
        assert!(spawned_or_moved(&server.await.unwrap()).is_empty());
    }

    #[tokio::test]
    async fn cross_session_move_reparents_the_same_id_and_cleans_the_source_layout() {
        let (source_key, destination_key) = default_keys();
        let spec = cross_spec(
            [snapshot(false, true), snapshot(true, true)],
            MoveResult::Ok(tid(1)),
            (&source_key, &destination_key),
            split(1, 2),
        );
        let (_dir, mut conn, server) = serve(spec).await;

        let outcome = move_pane(&mut conn, tid(1), tid(3), SplitDir::Horizontal, 0.5, &[])
            .await
            .unwrap();
        assert!(outcome.cross_session);
        assert_eq!(
            leaves(
                outcome.destination_workspace.windows[0]
                    .state
                    .tree
                    .as_ref()
                    .unwrap()
            ),
            vec![tid(3), tid(1)]
        );
        assert_eq!(
            outcome.destination_workspace.windows[0].state.focus,
            Some(tid(1))
        );
        drop(conn);
        let seen = server.await.unwrap();
        assert_eq!(
            spawned_or_moved(&seen),
            vec![(tid(1), tid(3))],
            "the existing identity is reparented once"
        );
        let source_write = seen
            .iter()
            .find_map(|frame| match frame {
                FrameKind::SetMetadata { key, value, .. }
                    if key == &layout_key(SessionId::new(1)) =>
                {
                    Workspace::decode_cbor(value).ok()
                }
                _ => None,
            })
            .expect("source cleanup write");
        assert_eq!(
            leaves(source_write.windows[0].state.tree.as_ref().unwrap()),
            vec![tid(2)]
        );
    }

    #[tokio::test]
    async fn last_pane_move_reaps_source_layout_instead_of_recreating_a_pane() {
        let (source_key, destination_key) = default_keys();
        let spec = cross_spec(
            [snapshot(false, true), snapshot(true, false)],
            MoveResult::Ok(tid(1)),
            (&source_key, &destination_key),
            LayoutNode::Leaf(tid(1)),
        );
        let (_dir, mut conn, server) = serve(spec).await;
        move_pane(&mut conn, tid(1), tid(3), SplitDir::Horizontal, 0.5, &[])
            .await
            .unwrap();
        drop(conn);
        let seen = server.await.unwrap();
        assert!(seen.iter().any(
            |frame| matches!(frame, FrameKind::DeleteMetadata { key, .. } if key == &source_key)
        ));
        spawned_or_moved(&seen);
    }

    #[tokio::test]
    async fn destination_layout_failure_rolls_ownership_back_and_reports_failure() {
        let spec = ScriptSpec::new()
            .server_features(ServerFeatureSet::with(&[ServerFeature::MoveResource]))
            .states([
                snapshot(false, true),
                snapshot(true, true),
                snapshot(true, true),
                snapshot(false, true),
            ])
            .move_result(MoveResult::Ok(tid(1)))
            .refuse_metadata(ErrorCode::PermissionDenied, "layout blocked");
        let (_dir, mut conn, server) = serve(spec).await;
        let error = move_pane(&mut conn, tid(1), tid(3), SplitDir::Horizontal, 0.5, &[])
            .await
            .unwrap_err();
        assert!(
            matches!(error, PaneMoveError::DestinationLayout { rollback, .. } if rollback.contains("moved back"))
        );
        drop(conn);
        assert_eq!(
            spawned_or_moved(&server.await.unwrap()),
            vec![(tid(1), tid(3)), (tid(1), tid(2))]
        );
    }

    /// Naming only one side's projection is refused before any write.
    #[tokio::test]
    async fn cross_session_move_with_one_projection_key_is_refused() {
        let spec = ScriptSpec::new()
            .server_features(ServerFeatureSet::with(&[ServerFeature::MoveResource]))
            .states([snapshot(false, true)]);
        let (_dir, mut conn, server) = serve(spec).await;
        let error = move_pane(
            &mut conn,
            tid(1),
            tid(3),
            SplitDir::Horizontal,
            0.5,
            &["myapp.layout/v1/1".to_owned()],
        )
        .await
        .unwrap_err();
        assert!(matches!(error, PaneMoveError::ProjectionArityMismatch));
        drop(conn);
        let seen = server.await.unwrap();
        assert!(
            seen.iter()
                .all(|frame| !matches!(frame, FrameKind::SetMetadata { .. })),
            "an arity refusal must happen before any layout write"
        );
    }

    /// Naming both envelopes (in either order) resolves each key to the
    /// session whose id it embeds and writes there.
    #[tokio::test]
    async fn cross_session_move_with_both_projection_keys_writes_the_named_envelopes() {
        let source_key = "src.layout/v1/1".to_owned();
        let destination_key = "dst.layout/v1/2".to_owned();
        let spec = cross_spec(
            [snapshot(false, true), snapshot(true, true)],
            MoveResult::Ok(tid(1)),
            (&source_key, &destination_key),
            split(1, 2),
        );
        let (_dir, mut conn, server) = serve(spec).await;

        // Destination-first: keys match by embedded session id, not position.
        let outcome = move_pane(
            &mut conn,
            tid(1),
            tid(3),
            SplitDir::Horizontal,
            0.5,
            &[destination_key.clone(), source_key.clone()],
        )
        .await
        .unwrap();
        assert!(outcome.cross_session);
        drop(conn);

        let seen = server.await.unwrap();
        assert!(
            seen.iter()
                .any(|frame| matches!(frame, FrameKind::SetMetadata { key, .. } if key == &destination_key)),
            "the destination write must target the named destination projection"
        );
        assert!(
            !seen.iter().any(|frame| matches!(
                frame,
                FrameKind::SetMetadata { key, .. } if key == &layout_key(SessionId::new(2))
            )),
            "the default destination key must never be touched"
        );
    }

    #[test]
    fn single_session_projection_arity_and_validation() {
        let session = SessionId::new(1);
        assert_eq!(
            resolve_single_session_projection(&[], session).unwrap(),
            None
        );
        assert_eq!(
            resolve_single_session_projection(&["myapp.layout/v1/1".to_owned()], session).unwrap(),
            Some("myapp.layout/v1/1".to_owned())
        );
        assert!(matches!(
            resolve_single_session_projection(
                &["a.layout/v1/1".to_owned(), "b.layout/v1/1".to_owned()],
                session
            ),
            Err(PaneMoveError::ProjectionArityTooMany)
        ));
        assert!(matches!(
            resolve_single_session_projection(&["myapp.layout/v1/2".to_owned()], session),
            Err(PaneMoveError::Layout(LayoutOpsError::InvalidProjectionKey(
                _
            )))
        ));
    }

    #[test]
    fn cross_session_projection_resolution_matches_by_embedded_session_id() {
        let source = SessionId::new(1);
        let destination = SessionId::new(2);
        assert_eq!(
            resolve_cross_session_projection(&[], source, destination).unwrap(),
            (None, None)
        );
        assert!(matches!(
            resolve_cross_session_projection(&["a.layout/v1/1".to_owned()], source, destination),
            Err(PaneMoveError::ProjectionArityMismatch)
        ));
        assert!(matches!(
            resolve_cross_session_projection(
                &["a.layout/v1/1".to_owned(), "b.layout/v1/1".to_owned()],
                source,
                destination
            ),
            Err(PaneMoveError::ProjectionArityMismatch)
        ));
        let (resolved_source, resolved_destination) = resolve_cross_session_projection(
            &["b.layout/v1/2".to_owned(), "a.layout/v1/1".to_owned()],
            source,
            destination,
        )
        .unwrap();
        assert_eq!(resolved_source.as_deref(), Some("a.layout/v1/1"));
        assert_eq!(resolved_destination.as_deref(), Some("b.layout/v1/2"));
    }

    #[tokio::test]
    async fn a_scoped_denial_refuses_the_move_instead_of_wedging_it() {
        // An out-of-scope MOVE_RESOURCE is refused with the move's own reply
        // (workload-auth §7); the move must end on it.
        let (source_key, destination_key) = default_keys();
        let spec = cross_spec(
            [snapshot(false, true), snapshot(true, true)],
            MoveResult::Err(MoveError::MoveFailed("permission denied".to_owned())),
            (&source_key, &destination_key),
            split(1, 2),
        );
        let (_dir, mut conn, server) = serve(spec).await;

        let refused = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            move_pane(&mut conn, tid(1), tid(3), SplitDir::Horizontal, 0.5, &[]),
        )
        .await
        .expect("a denied move must return; a timeout here is the wedge itself");
        match refused {
            Err(PaneMoveError::MoveRefused(message)) => assert_eq!(message, "permission denied"),
            Err(other) => panic!("expected the move's own refusal, got {other}"),
            Ok(_) => panic!("a denied move must not succeed"),
        }
        drop(conn);
        server.await.unwrap();
    }
}
