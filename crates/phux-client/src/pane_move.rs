//! The existing-pane move shared by headless and interactive clients.
//!
//! Same-session: one arrangement edit ([`crate::layout_ops`]). Cross-session
//! against a server that advertises `ORGANIZATION`: one `RESOURCE_MOVE`
//! (membership only; the server prunes the source arrangement in the same
//! transaction), then a compare-and-swap edit of the destination
//! arrangement, undone by `ORGANIZATION_UNDO` of the move's receipt when the
//! placement cannot be written. With named projection keys: ADR-0056's L1
//! re-parent, then destination-first layout publication and source cleanup.
//! The [`ResourceId`] is never replaced and the process keeps running.

use phux_protocol::ids::{ResourceId, SessionId};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, MoveError, MoveResult, Scope, StateScope,
};
use phux_protocol::wire::info::SessionSnapshot;
use thiserror::Error;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::layout::{ArrangementDoc, LayoutNode, SplitDir, leaves};
use crate::layout_ops::{
    DEFAULT_LAYOUT_GROUP_ID, LayoutMutation, LayoutOps, LayoutOpsError, validate_projection_key,
};

/// A confirmed pane move and the destination topology that won publication.
#[derive(Debug, Clone)]
pub struct PaneMoveOutcome {
    /// Fresh server-reported name of the destination session.
    pub destination_session_name: String,
    /// Confirmed destination workspace, suitable for immediate TUI adoption.
    pub destination_workspace: ArrangementDoc,
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
    #[error("the destination pane changed sessions while the move was in flight; {rollback}")]
    DestinationChanged {
        /// Whether ownership could be restored.
        rollback: &'static str,
    },
    /// The server refused the ownership move.
    #[error("server refused the pane move: {0}")]
    MoveRefused(String),
    /// An organization request failed before membership changed.
    #[error(transparent)]
    Org(#[from] crate::org::OrgError),
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
    source_session: SessionId,
    destination_session: SessionId,
    destination_session_name: String,
    dir: SplitDir,
    ratio: f32,
    rollback_owner: Option<ResourceId>,
    /// Named projection keys (ADR-0129).
    destination_key: String,
    source_key: String,
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
    let source_session = crate::resource::session_of(&snapshot, &source)
        .ok_or(PaneMoveError::UnknownPane { role: "source" })?;
    let destination_session =
        crate::resource::session_of(&snapshot, &target).ok_or(PaneMoveError::UnknownPane {
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

    if projection.is_empty() {
        return move_by_record(
            conn,
            &source,
            &target,
            destination_session,
            destination_session_name,
            dir,
            ratio,
        )
        .await;
    }
    let (source_key, destination_key) =
        resolve_cross_session_projection(projection, source_session, destination_session)?;
    let plan = CrossMovePlan {
        rollback_owner: sibling_in_session(&snapshot, &source),
        source,
        target,
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

/// `--projection` for a cross-session move: two keys matched to
/// `(source, destination)` by their embedded session ids in either order.
fn resolve_cross_session_projection(
    projection: &[String],
    source_session: SessionId,
    destination_session: SessionId,
) -> Result<(String, String), PaneMoveError> {
    let [first, second] = projection else {
        return Err(PaneMoveError::ProjectionArityMismatch);
    };
    let first_session = crate::layout::projection_key_session(first);
    let second_session = crate::layout::projection_key_session(second);
    if first_session == Some(source_session) && second_session == Some(destination_session) {
        return Ok((first.clone(), second.clone()));
    }
    if first_session == Some(destination_session) && second_session == Some(source_session) {
        return Ok((second.clone(), first.clone()));
    }
    Err(PaneMoveError::ProjectionArityMismatch)
}

/// A cross-session move of the default layout: `RESOURCE_MOVE`, then the destination arrangement edit, undone when the
/// edit cannot be written.
async fn move_by_record(
    conn: &mut Connection,
    source: &ResourceId,
    target: &ResourceId,
    destination_session: SessionId,
    destination_session_name: String,
    dir: SplitDir,
    ratio: f32,
) -> Result<PaneMoveOutcome, PaneMoveError> {
    let inventory =
        crate::org::fetch(conn, phux_protocol::wire::frame::OrgFilter::All, 0x6e00).await?;
    let source_key = inventory
        .resource_by_live(source)
        .ok_or(PaneMoveError::UnknownPane { role: "source" })?
        .key;
    let destination_key = inventory
        .session_by_live(destination_session)
        .ok_or(PaneMoveError::UnknownPane {
            role: "destination",
        })?
        .key;
    let receipt = match crate::org::move_resource(conn, source_key, destination_key).await {
        Ok(receipt) => receipt,
        Err(crate::org::OrgError::Refused(refusal)) => {
            return Err(PaneMoveError::MoveRefused(crate::org::refusal_sentence(
                &refusal,
            )));
        }
        Err(error) => return Err(error.into()),
    };
    let placed = LayoutOps::new(conn, destination_session, 0x6e40)
        .mutate_or_seed(
            ArrangementDoc::single(target.clone()),
            LayoutMutation::Split {
                target: target.clone(),
                new_pane: source.clone(),
                dir,
                ratio,
            },
        )
        .await;
    match placed {
        Ok(destination_workspace) => Ok(PaneMoveOutcome {
            destination_session_name,
            destination_workspace,
            cross_session: true,
        }),
        Err(error) => {
            let rollback = if crate::org::undo(conn, &receipt).await.is_ok() {
                "the move was undone"
            } else {
                "the move could not be undone; inspect and place the pane explicitly"
            };
            Err(PaneMoveError::DestinationLayout {
                error: error.to_string(),
                rollback,
            })
        }
    }
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
    if crate::resource::session_of(&post_move, &plan.source) != Some(plan.destination_session)
        || crate::resource::session_of(&post_move, &plan.target) != Some(plan.destination_session)
    {
        let rollback = rollback_suffix(conn, plan).await;
        return Err(PaneMoveError::DestinationChanged { rollback });
    }
    let source_session_reaped = !post_move
        .sessions
        .iter()
        .any(|session| session.id == plan.source_session);

    let mut destination_layout = match LayoutOps::with_key(
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
    let owner_still_at_source = read_snapshot(conn, 30).await.is_ok_and(|snapshot| {
        crate::resource::session_of(&snapshot, owner) == Some(plan.source_session)
    });
    owner_still_at_source
        && request_move(conn, 31, &plan.source, owner).await.is_ok()
        && read_snapshot(conn, 32).await.is_ok_and(|snapshot| {
            crate::resource::session_of(&snapshot, &plan.source) == Some(plan.source_session)
        })
}

async fn rollback_suffix(conn: &mut Connection, plan: &CrossMovePlan) -> &'static str {
    if rollback_move(conn, plan).await {
        "the pane was moved back to its original session"
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
    key: String,
    request_id: u32,
) -> Result<(), LayoutOpsError> {
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
    let mut layout = LayoutOps::with_key(conn, plan.source_session, plan.source_key.clone(), 20)?;
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

fn session_name(snapshot: &SessionSnapshot, session: SessionId) -> Option<&str> {
    snapshot
        .sessions
        .iter()
        .find(|candidate| candidate.id == session)
        .map(|candidate| candidate.name.as_str())
}

/// Another Terminal of `source`'s session: the owner a rollback re-parents
/// the pane beside.
fn sibling_in_session(snapshot: &SessionSnapshot, source: &ResourceId) -> Option<ResourceId> {
    let session = crate::resource::session_of(snapshot, source)?;
    crate::resource::terminals_in(snapshot, session)
        .find(|resource| &resource.id != source)
        .map(|resource| resource.id.clone())
}

fn workspace_contains(workspace: &ArrangementDoc, terminal: &ResourceId) -> bool {
    workspace
        .groups
        .iter()
        .filter_map(|window| window.state.tree.as_ref())
        .any(|tree| leaves(tree).contains(terminal))
}

fn workspace_has_placement(
    workspace: &ArrangementDoc,
    target: &ResourceId,
    moved: &ResourceId,
    dir: SplitDir,
    ratio: f32,
) -> bool {
    workspace
        .groups
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
        LayoutNode::Leaf(_) | LayoutNode::Stack { .. } => false,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests;
