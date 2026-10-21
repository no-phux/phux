//! `RESOURCE_SPAWNED`: splits, floating panes, and their refusals.

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{SpawnError, SpawnResult};

use super::replies::unreferenced;
use super::windows::handle_window_spawned;
use super::{FrameCtx, FrameOutcome};
use crate::attach::actions;
use crate::attach::actions::{ParkedAdopt, PendingSplit, SpawnedPane, SplitHost, apply_spawned_ok};
use crate::attach::outcome::AttachError;
use crate::attach::paint::content_rect;
use crate::attach::pane_state::{PaneSlot, reanchor_predict_to_pane};
use crate::layout;
use crate::layout::ArrangementDoc;
use crate::render::chrome::status_bar::Notice;

/// Split-pane reply: apply the parked split on Ok, bell on Err.
pub(super) fn handle_terminal_spawned<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    request_id: u32,
    result: SpawnResult,
) -> Result<FrameOutcome, AttachError> {
    if matches!(result, SpawnResult::Err(SpawnError::Unknown(_))) {
        return Ok(uncertain_spawn(ctx, request_id));
    }
    if let Some(title) = ctx.pending_floating.remove(&request_id) {
        return handle_floating_spawned(ctx, title, result);
    }
    // A local split or window spawns into the attached session.
    if (ctx.pending_windows.contains_key(&request_id)
        || ctx.pending_splits.contains_key(&request_id))
        && let Some(id) = result.spawned_id()
        && id.is_local()
    {
        ctx.members.insert(id.clone());
    }
    // A parked new-window takes priority (ids are unique across both maps).
    if let Some(pending) = ctx.pending_windows.remove(&request_id) {
        return handle_window_spawned(
            ctx.out,
            ctx.workspace,
            ctx.focused_resource,
            ctx.panes,
            &pending,
            result,
        );
    }
    let Some(pending) = ctx.pending_splits.remove(&request_id) else {
        tracing::debug!(
            request_id,
            "stray ResourceSpawned with no matching pending split or window; ignoring",
        );
        return Ok(FrameOutcome::default());
    };
    let instance = result.instance();
    match result {
        // A satellite spawn streams to no one until attached, and the attach
        // can be refused: park the split on that attach, token included.
        SpawnResult::Ok(new_id) | SpawnResult::OkBound { id: new_id, .. } if !new_id.is_local() => {
            Ok(FrameOutcome {
                adopt_spawned: vec![ParkedAdopt::Split(PendingSplit {
                    adopt: Some(SpawnedPane {
                        id: new_id,
                        instance,
                    }),
                    ..pending
                })],
                ..FrameOutcome::default()
            })
        }
        SpawnResult::Ok(new_id) | SpawnResult::OkBound { id: new_id, .. } => {
            let mut outcome = apply_split_spawned(ctx, new_id, &pending)?;
            outcome.notices.extend(split_fallback_notice(&pending.host));
            Ok(outcome)
        }
        SpawnResult::Err(err) => {
            log_split_spawn_error(request_id, &err);
            let _ = actions::write_bell(ctx.out);
            Ok(FrameOutcome {
                notices: split_refusal_notice(&pending.host, &spawn_error_reason(&err))
                    .into_iter()
                    .collect(),
                ..FrameOutcome::default()
            })
        }
        // SpawnResult is also #[non_exhaustive].
        _ => {
            tracing::warn!(request_id, "ResourceSpawned: unknown SpawnResult variant");
            Ok(FrameOutcome::default())
        }
    }
}

/// What a spawn settles with when the server answered without this build
/// being able to tell whether it created a pane.
pub(super) const UNCERTAIN_SPAWN: &str =
    "the new pane's outcome is unknown (a newer server's answer); it may exist";

/// A parked split, window, or floating spawn whose answer does not say
/// whether a pane was created: it settles with a notice saying so, never as
/// "nothing was created". A pane the server did create surfaces through
/// its spawn event like any other.
fn uncertain_spawn<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    request_id: u32,
) -> FrameOutcome {
    let parked = ctx.pending_floating.remove(&request_id).is_some()
        | ctx.pending_windows.remove(&request_id).is_some()
        | ctx.pending_splits.remove(&request_id).is_some();
    if !parked {
        return FrameOutcome::default();
    }
    tracing::warn!(request_id, "spawn answered with an unknown outcome");
    let _ = actions::write_bell(ctx.out);
    FrameOutcome {
        notices: vec![Notice::warn(UNCERTAIN_SPAWN.to_owned())],
        ..FrameOutcome::default()
    }
}

/// ADR-0147: a floating overlay's spawn answered. Seed its slot at the box
/// interior, marked floating, so paint and input find it; it joins no window.
/// A refusal bells with the reason; a second overlay racing an open one, or
/// a non-local id, is killed instead.
fn handle_floating_spawned<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    title: String,
    result: SpawnResult,
) -> Result<FrameOutcome, AttachError> {
    let new_id = match result {
        SpawnResult::Ok(id) | SpawnResult::OkBound { id, .. } => id,
        SpawnResult::Err(err) => {
            let _ = actions::write_bell(ctx.out);
            return Ok(FrameOutcome {
                notices: vec![Notice::warn(format!(
                    "could not open {title}: {}",
                    spawn_error_reason(&err)
                ))],
                ..FrameOutcome::default()
            });
        }
        _ => return Ok(FrameOutcome::default()),
    };
    let taken = crate::attach::floating::floating_pane(ctx.panes).is_some_and(|id| *id != new_id);
    if taken || !new_id.is_local() {
        tracing::warn!(terminal = ?new_id, "floating overlay has nowhere to open; killing it");
        ctx.expected_closes.insert(new_id.clone());
        return Ok(FrameOutcome {
            kill_orphans: vec![new_id],
            ..FrameOutcome::default()
        });
    }
    let bar = ctx.status_bar.as_ref().map(|p| p.position());
    let inner =
        crate::attach::floating::floating_box(content_rect(ctx.viewport_dims, bar, ctx.sidebar))
            .inner;
    // Output may have raced the reply and seeded a plain slot already.
    let slot = match ctx.panes.entry(new_id) {
        std::collections::hash_map::Entry::Occupied(slot) => slot.into_mut(),
        std::collections::hash_map::Entry::Vacant(slot) => {
            slot.insert(PaneSlot::new_with_size(inner.w.max(1), inner.h.max(1))?)
        }
    };
    slot.floating = Some(title);
    Ok(FrameOutcome {
        layout_replaced: true,
        size_floating: true,
        ..FrameOutcome::default()
    })
}

/// ADR-0147: the floating overlay closed or its process exited: drop its
/// slot (ending the modal) and repaint the panes it covered. `None` when
/// `terminal_id` is not the floating pane.
pub(super) fn close_floating<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal_id: &ResourceId,
) -> Option<FrameOutcome> {
    ctx.panes
        .get(terminal_id)
        .and_then(|slot| slot.floating.as_ref())?;
    ctx.panes.remove(terminal_id);
    Some(FrameOutcome {
        layout_replaced: true,
        ..FrameOutcome::default()
    })
}

/// Log why a split's spawn failed, by error kind.
fn log_split_spawn_error(request_id: u32, err: &SpawnError) {
    match err {
        // The default Group always exists; a server-side invariant changed.
        SpawnError::GroupNotFound => tracing::warn!(
            request_id,
            "ResourceSpawned: server reports GroupNotFound for DEFAULT group",
        ),
        SpawnError::SpawnFailed(reason) => tracing::warn!(
            request_id,
            reason = %reason,
            "ResourceSpawned: server-side spawn failed",
        ),
        // SpawnError is #[non_exhaustive] — catch future variants so newer
        // servers don't take the client down.
        other => tracing::warn!(
            request_id,
            error = ?other,
            "ResourceSpawned: spawn refused",
        ),
    }
}

/// A spawn error as the reason a notice gives: the server's own words when
/// it sent some, else the error's name.
fn spawn_error_reason(err: &SpawnError) -> String {
    match err {
        SpawnError::SpawnFailed(reason) | SpawnError::SatelliteUnreachable(reason) => {
            reason.clone()
        }
        other => format!("{other:?}"),
    }
}

/// The notice for a split that could not open on its satellite, naming the
/// host. A split on the attached server's host keeps the bare bell.
fn split_refusal_notice(host: &SplitHost, reason: &str) -> Option<Notice> {
    let SplitHost::Satellite(satellite) = host else {
        return None;
    };
    Some(Notice::warn(format!(
        "could not open on {satellite}: {reason}"
    )))
}

/// A satellite split a hub without host-aware spawns opened on itself: say
/// the pane is on this host.
fn split_fallback_notice(host: &SplitHost) -> Option<Notice> {
    let SplitHost::AttachedInsteadOf(satellite) = host else {
        return None;
    };
    Some(Notice::warn(format!(
        "this hub may not be able to spawn on {satellite}; the split opened on this host"
    )))
}

/// Apply a spawned split in the window holding its source pane (the user may
/// have switched windows meanwhile), seed the slot, and focus it if that
/// window is on screen. A split whose source is gone is [`split_dropped`].
pub(super) fn apply_split_spawned<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    new_id: ResourceId,
    pending: &PendingSplit,
) -> Result<FrameOutcome, AttachError> {
    let keep_existing = pending.open_existing.is_some();
    let Some(index) = window_holding_pane(ctx.workspace, &pending.focused_at_request) else {
        return Ok(abandon_split(
            ctx,
            &new_id,
            keep_existing,
            "the pane it was split from has closed",
        ));
    };
    let window = &mut ctx.workspace.groups[index].state;
    let new_state = match apply_spawned_ok(window, new_id.clone(), pending) {
        Ok(new_state) => new_state,
        Err(err) => {
            tracing::warn!(error = %err, terminal = ?new_id, "apply_spawned_ok failed");
            return Ok(abandon_split(
                ctx,
                &new_id,
                keep_existing,
                "the layout could not take it",
            ));
        }
    };
    *window = new_state;
    // Seed a warm slot; never overwrite an existing one.
    if let std::collections::hash_map::Entry::Vacant(v) = ctx.panes.entry(new_id.clone()) {
        v.insert(PaneSlot::new()?);
    }
    if index == ctx.workspace.active {
        focus_landed_split(ctx, new_id, pending.zoom_on_spawn);
    }
    Ok(FrameOutcome {
        layout_replaced: true,
        emit_set_metadata: true,
        // Tell the server the real split dims.
        reflow_panes: true,
        ..FrameOutcome::default()
    })
}

/// The split just landed in the window on screen: zoom and focus follow it.
fn focus_landed_split<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    new_id: ResourceId,
    zoom_on_spawn: bool,
) {
    // A split un-zooms (tmux parity) unless the intent asked to zoom the
    // new pane (`placement = "zoomed"`).
    *ctx.zoomed = zoom_on_spawn.then_some(new_id);
    // Move focus to the freshly spawned pane — tmux-compatible
    // (apply_split already sets focus inside the returned state).
    ctx.focused_resource
        .clone_from(&ctx.workspace.active_group().and_then(|ls| ls.focus.clone()));
    // Re-anchor predictive echo to the new pane.
    if let Some(fid) = ctx.focused_resource.as_ref() {
        reanchor_predict_to_pane(ctx.predict, ctx.panes, fid);
    }
}

/// A split that cannot land. A pane this client spawned is killed; an
/// existing pane opened with `resource = "host/@N"` is left alone.
fn abandon_split<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    new_id: &ResourceId,
    keep_existing: bool,
    why: &str,
) -> FrameOutcome {
    if keep_existing {
        tracing::warn!(terminal = ?new_id, why, "split dropped; existing pane kept");
        let _ = actions::write_bell(ctx.out);
        return FrameOutcome {
            notices: vec![Notice::warn(format!("split dropped: {why}"))],
            ..FrameOutcome::default()
        };
    }
    split_dropped(ctx, new_id, why)
}

/// A spawned split with nowhere to go: bell and say why; kill the now
/// unreferenced pane (its host just answered, so it is reachable).
fn split_dropped<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    new_id: &ResourceId,
    why: &str,
) -> FrameOutcome {
    tracing::warn!(terminal = ?new_id, why, "split dropped");
    let _ = actions::write_bell(ctx.out);
    FrameOutcome {
        notices: vec![Notice::warn(format!("split dropped: {why}"))],
        kill_orphans: unreferenced(ctx, new_id).into_iter().collect(),
        ..FrameOutcome::default()
    }
}

/// The index of the window whose layout has `pane` as a leaf.
pub(super) fn window_holding_pane(workspace: &ArrangementDoc, pane: &ResourceId) -> Option<usize> {
    workspace.groups.iter().position(|window| {
        window
            .state
            .tree
            .as_ref()
            .is_some_and(|tree| layout::leaves(tree).contains(pane))
    })
}
