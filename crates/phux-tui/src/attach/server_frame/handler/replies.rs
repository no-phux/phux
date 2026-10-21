//! `ERROR` and `COMMAND_RESULT` replies: satellites, adoption, and stray panes.

use std::collections::HashMap;

use phux_client::conditional_kill::BoundResource;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{ErrorCode, SpawnError, SpawnResult};

use super::closed::resolve_resource_op;
use super::metadata::metadata_error_outcome;
use super::spawned::{apply_split_spawned, handle_terminal_spawned, window_holding_pane};
use super::{FrameCtx, FrameOutcome};
use crate::attach::actions;
use crate::attach::actions::{Adopt, ParkedAdopt, PendingSplit, PendingWindow};
use crate::attach::outcome::AttachError;
use crate::attach::pane_state::PaneSlot;
use crate::layout::ArrangementDoc;
use crate::render::chrome::status_bar::Notice;

/// ERROR never terminates the attach (SPEC §9: `DETACHED` plus transport
/// close does); the same code is fatal and non-fatal on the same server. An
/// uncorrelated error names its code in a notice.
fn handle_error_frame(request_id: Option<u32>, code: ErrorCode, message: &str) -> FrameOutcome {
    // A correlated error answers a path query or directory listing it
    // names (a newer server's `UNSUPPORTED`, or a reply this build cannot
    // read) as a refusal; each consumer drops an id it is not waiting on,
    // so a raced correlated error stays inert.
    if let Some(request_id) = request_id {
        return correlated_refusal(request_id, message);
    }
    // Uncorrelated `SATELLITE_UNREACHABLE` is a degraded-federation
    // transition with its own wording.
    if code == ErrorCode::SatelliteUnreachable {
        tracing::warn!(message = %message, "federation degraded (satellite unreachable)");
        return FrameOutcome {
            notices: vec![Notice::warn(message.to_owned())],
            ..FrameOutcome::default()
        };
    }
    tracing::warn!(
        ?code,
        scope = ?code.scope(),
        message = %message,
        "server error frame in the attached phase"
    );
    FrameOutcome {
        notices: vec![Notice::warn(format!("server error: {message}"))],
        ..FrameOutcome::default()
    }
}

/// A correlated `ERROR` as the refused answer to whichever path query or
/// directory listing carries `request_id`.
fn correlated_refusal(request_id: u32, message: &str) -> FrameOutcome {
    use phux_protocol::wire::frame::{
        DirectoryErrorCode, DirectoryListingError, PathErrorCode, PathQueryError,
    };
    FrameOutcome {
        path_results: Some((
            request_id,
            Err(PathQueryError {
                root: String::new(),
                code: PathErrorCode::Other,
                message: message.to_owned(),
            }),
        )),
        directory_listing: Some((
            request_id,
            Err(DirectoryListingError {
                path: String::new(),
                code: DirectoryErrorCode::Other,
                message: message.to_owned(),
            }),
        )),
        ..FrameOutcome::default()
    }
}

/// The `ERROR` arm: a correlated refusal of a parked satellite attach
/// decides it; anything else is the ordinary error notice.
pub(super) fn error_frame_outcome<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    request_id: Option<u32>,
    code: ErrorCode,
    message: String,
) -> Result<FrameOutcome, AttachError> {
    let mut marked = note_satellite_down(ctx, code, &message);
    if let Some(parked) = request_id.and_then(|id| take_pending_adopt(ctx, id)) {
        return handle_adopt_reply(ctx, parked, Some(AdoptRefusal { code, message }))
            .map(|outcome| with_satellite_chrome(outcome, marked));
    }
    // A spawn a newer server refused in place (`UNSUPPORTED`) settles like
    // a refused spawn. A code this build does not know, `UNREADABLE_REPLY`
    // included, does not say whether a pane was created: unknown outcome.
    if let Some(id) = request_id.filter(|id| is_pending_spawn(ctx, *id)) {
        let settled = if matches!(code, ErrorCode::Unknown(_)) {
            SpawnResult::Err(SpawnError::Unknown(
                phux_protocol::wire::UnreadableResult::from_wire(Vec::new()),
            ))
        } else {
            SpawnResult::Err(SpawnError::SpawnFailed(message))
        };
        return handle_terminal_spawned(ctx, id, settled)
            .map(|outcome| with_satellite_chrome(outcome, marked));
    }
    if let Some(outcome) = request_id.and_then(|id| metadata_error_outcome(ctx, id)) {
        return Ok(with_satellite_chrome(outcome, marked));
    }
    if let Some(id) = request_id {
        // A refusal naming a replaying pane puts its down flag back.
        marked |= remark_pending_satellite(ctx, id, Some(code));
        if let Some(outcome) = resolve_resource_op(ctx, id, Some(code)) {
            return Ok(with_satellite_chrome(outcome, marked));
        }
    }
    Ok(with_satellite_chrome(
        handle_error_frame(request_id, code, &message),
        marked,
    ))
}

/// Whether `request_id` is an outstanding split, window, or floating spawn.
/// An attach-phase entry (adopting or opening an existing pane) is not one.
fn is_pending_spawn<W: crate::attach::RenderSink>(ctx: &FrameCtx<'_, W>, request_id: u32) -> bool {
    ctx.pending_splits
        .get(&request_id)
        .is_some_and(|split| split.adopt.is_none() && split.open_existing.is_none())
        || ctx
            .pending_windows
            .get(&request_id)
            .is_some_and(|window| window.adopt.is_none())
        || ctx.pending_floating.contains_key(&request_id)
}

/// Grey every pane on the host a `SatelliteUnreachable` names. The layout
/// leaf stays; chrome is the only thing that moves.
fn note_satellite_down<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    code: ErrorCode,
    message: &str,
) -> bool {
    code == ErrorCode::SatelliteUnreachable
        && crate::attach::pane_state::note_satellite_unreachable(ctx.panes, message)
}

const fn with_satellite_chrome(mut outcome: FrameOutcome, marked: bool) -> FrameOutcome {
    outcome.chrome_dirty |= marked;
    outcome
}

/// Re-mark the host of a correlated resource reply when unreachable (before
/// [`resolve_resource_op`] removes the pending id).
fn remark_pending_satellite<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    request_id: u32,
    code: Option<ErrorCode>,
) -> bool {
    if !matches!(code, Some(ErrorCode::SatelliteUnreachable)) {
        return false;
    }
    let Some(host) = ctx
        .pending_resource_ops
        .get(&request_id)
        .and_then(ResourceId::host)
        .map(|host| host.as_str().to_owned())
    else {
        return false;
    };
    crate::attach::pane_state::mark_satellite_down(ctx.panes, &host)
}

/// The `COMMAND_RESULT` arm: a parked satellite attach's reply decides its
/// window or split; any other reply is inert.
pub(super) fn command_result_outcome<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    request_id: u32,
    result: phux_protocol::wire::frame::CommandResult,
) -> Result<FrameOutcome, AttachError> {
    let marked = match &result {
        phux_protocol::wire::frame::CommandResult::Error { code, message } => {
            note_satellite_down(ctx, *code, message)
        }
        _ => false,
    };
    if let Some(parked) = take_pending_adopt(ctx, request_id) {
        let refusal = match result {
            phux_protocol::wire::frame::CommandResult::Error { code, message } => {
                Some(AdoptRefusal { code, message })
            }
            _ => None,
        };
        return handle_adopt_reply(ctx, parked, refusal)
            .map(|outcome| with_satellite_chrome(outcome, marked));
    }
    let code = match &result {
        phux_protocol::wire::frame::CommandResult::Error { code, .. } => Some(*code),
        _ => None,
    };
    let marked = marked || remark_pending_satellite(ctx, request_id, code);
    if let Some(outcome) = resolve_resource_op(ctx, request_id, code) {
        return Ok(with_satellite_chrome(outcome, marked));
    }
    tracing::debug!(
        request_id,
        "dropping CommandResult with no matching pending request"
    );
    Ok(with_satellite_chrome(FrameOutcome::default(), marked))
}

/// Take the window or split parked on the satellite attach behind
/// `request_id`; spawn-parked ones stay for their `RESOURCE_SPAWNED`.
fn take_pending_adopt<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    request_id: u32,
) -> Option<ParkedAdopt> {
    if ctx
        .pending_windows
        .get(&request_id)
        .is_some_and(|pending| pending.adopt.is_some())
    {
        return ctx
            .pending_windows
            .remove(&request_id)
            .map(ParkedAdopt::Window);
    }
    if ctx
        .pending_splits
        .get(&request_id)
        .is_some_and(|pending| pending.adopt.is_some() || pending.open_existing.is_some())
    {
        return ctx
            .pending_splits
            .remove(&request_id)
            .map(ParkedAdopt::Split);
    }
    None
}

/// Apply the reply to a parked satellite pane's `ATTACH_RESOURCE`: success
/// opens the window or applies the split; a refusal opens nothing
/// ([`adopt_refused`]).
fn handle_adopt_reply<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    parked: ParkedAdopt,
    refusal: Option<AdoptRefusal>,
) -> Result<FrameOutcome, AttachError> {
    let Some(pane) = parked.pane().cloned() else {
        return Ok(FrameOutcome::default());
    };
    if let Some(refusal) = refusal {
        return Ok(adopt_refused(ctx, &parked, &pane, &refusal));
    }
    match parked {
        ParkedAdopt::Window(window) => open_adopted_window(ctx, &window, pane),
        ParkedAdopt::Split(split) => apply_split_spawned(ctx, pane, &split),
    }
}

/// Why a parked satellite attach was refused: the wire code, which decides
/// whether a kill could reach the pane, and the server's words.
struct AdoptRefusal {
    code: ErrorCode,
    message: String,
}

/// A refused satellite attach: bell, name the host, and kill the pane when
/// this client spawned it and nothing references it ([`orphaned_pane`]).
fn adopt_refused<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    parked: &ParkedAdopt,
    pane: &ResourceId,
    refusal: &AdoptRefusal,
) -> FrameOutcome {
    let host = pane.host().map_or_else(String::new, ToString::to_string);
    let reason = &refusal.message;
    tracing::warn!(%host, %reason, code = ?refusal.code, "satellite attach refused");
    let _ = actions::write_bell(ctx.out);
    let text = match parked {
        ParkedAdopt::Window(window) => {
            format!("could not open {} on {host}: {reason}", window.name)
        }
        ParkedAdopt::Split(_) => format!("could not open on {host}: {reason}"),
    };
    FrameOutcome {
        notices: vec![Notice::warn(text)],
        kill_orphans: orphaned_pane(ctx, parked, refusal.code)
            .into_iter()
            .collect(),
        unreachable_strays: stranded_bound_pane(ctx, parked, refusal.code)
            .into_iter()
            .collect(),
        ..FrameOutcome::default()
    }
}

/// The pane a refused attach leaves running with nothing referencing it,
/// when a kill could reach it; never a satellite session's own pane.
fn orphaned_pane<W: crate::attach::RenderSink>(
    ctx: &FrameCtx<'_, W>,
    parked: &ParkedAdopt,
    code: ErrorCode,
) -> Option<ResourceId> {
    if !kill_can_reach(code) {
        return None;
    }
    unreferenced(ctx, &parked.spawned_pane()?.id)
}

/// The pane an unreachable refusal leaves running, when it was spawned bound
/// to an instance token and is unreferenced: retried later only through the
/// conditional kill (ADR-0109). Unbound panes are never returned.
fn stranded_bound_pane<W: crate::attach::RenderSink>(
    ctx: &FrameCtx<'_, W>,
    parked: &ParkedAdopt,
    code: ErrorCode,
) -> Option<BoundResource> {
    if kill_can_reach(code) {
        return None;
    }
    let bound = parked.spawned_pane()?.bound()?;
    unreferenced(ctx, &bound.id).map(|_| bound)
}

/// Whether a kill could reach the pane behind a refusal with `code`. Not
/// after `SATELLITE_UNREACHABLE`: the hub would hold every keystroke behind
/// the kill for its relay deadline, and an unconditional retry could hit a
/// restarted satellite's reused id.
const fn kill_can_reach(code: ErrorCode) -> bool {
    !matches!(code, ErrorCode::SatelliteUnreachable)
}

/// `pane`, when no window holds it and no other parked open waits on it.
pub(super) fn unreferenced<W: crate::attach::RenderSink>(
    ctx: &FrameCtx<'_, W>,
    pane: &ResourceId,
) -> Option<ResourceId> {
    let referenced =
        pane_is_referenced(ctx.workspace, ctx.pending_windows, ctx.pending_splits, pane);
    (!referenced).then(|| pane.clone())
}

/// Whether a window holds `pane` or a parked window or split adopts it (this
/// client's view only).
pub(in crate::attach) fn pane_is_referenced(
    workspace: &ArrangementDoc,
    pending_windows: &HashMap<u32, PendingWindow>,
    pending_splits: &HashMap<u32, PendingSplit>,
    pane: &ResourceId,
) -> bool {
    let adopting_window = pending_windows
        .values()
        .any(|window| window.adopt.as_ref().map(Adopt::pane) == Some(pane));
    let adopting_split = pending_splits.values().any(|split| {
        split.adopt.as_ref().map(|spawned| &spawned.id) == Some(pane)
            || split.open_existing.as_ref() == Some(pane)
    });
    window_holding_pane(workspace, pane).is_some() || adopting_window || adopting_split
}

/// Open a satellite pane's window once its attach succeeded, with the same
/// follow-up a spawned new window gets.
fn open_adopted_window<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    pending: &PendingWindow,
    pane: ResourceId,
) -> Result<FrameOutcome, AttachError> {
    open_window(
        ctx.workspace,
        ctx.focused_resource,
        ctx.panes,
        &pending.name,
        pane,
    )
}

/// Append an active window named `name` on `pane`, seed its slot, focus it,
/// and request repaint, broadcast, and reflow.
pub(super) fn open_window(
    workspace: &mut ArrangementDoc,
    focused_resource: &mut Option<ResourceId>,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    name: &str,
    pane: ResourceId,
) -> Result<FrameOutcome, AttachError> {
    workspace.add_group(name.to_owned(), pane.clone());
    if let std::collections::hash_map::Entry::Vacant(slot) = panes.entry(pane) {
        slot.insert(PaneSlot::new()?);
    }
    *focused_resource = workspace.active_group().and_then(|ls| ls.focus.clone());
    Ok(FrameOutcome {
        layout_replaced: true,
        emit_set_metadata: true,
        reflow_panes: true,
        ..FrameOutcome::default()
    })
}
