//! `RESOURCE_CLOSED`: fold a dead pane out of the arrangement and the session.

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    DetachReason, ErrorCode, FrameKind, SessionLifetime, decode_session_keep_empty,
    decode_session_rename,
};

use super::spawned::{close_floating, window_holding_pane};
use super::{FrameCtx, FrameOutcome, pane_label};
use crate::attach::actions::apply_terminal_closed;
use crate::attach::outcome::{AttachEnd, describe_exit};
use crate::attach::session_mirror::unarranged_members;
use crate::render::chrome::status_bar::Notice;

/// A Terminal closed: fold its leaf out and drop its slot.
pub(super) fn handle_terminal_closed<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    frame: FrameKind,
) -> FrameOutcome {
    let FrameKind::ResourceClosed {
        terminal_id,
        exit_status,
        reason,
        signal,
    } = frame
    else {
        return FrameOutcome::default();
    };
    let terminal_id = &terminal_id;
    tracing::info!(
        terminal = ?terminal_id,
        exit_status = ?exit_status,
        ?signal,
        ?reason,
        "ResourceClosed",
    );
    // Drain the expectation unconditionally, so a later spontaneous death of a
    // reused id still notifies.
    let expected = ctx.expected_closes.remove(terminal_id);
    if let Some(outcome) = close_floating(ctx, terminal_id) {
        ctx.members.remove(terminal_id);
        return outcome;
    }
    // An AgentSession has no slot or leaf; its close removes only its row.
    if ctx.is_agent_session(terminal_id) {
        return FrameOutcome {
            chrome_dirty: true,
            ..FrameOutcome::default()
        };
    }
    fold_dead_resource(
        ctx,
        terminal_id,
        exit_status,
        pane_exit_notices(terminal_id, exit_status, expected),
    )
}

/// Fold a Terminal that no longer exists out of this client's projection
/// (slot, leaf, emptied window, focus). Shared by `RESOURCE_CLOSED` and
/// [`fold_missing_resource`].
fn fold_dead_resource<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal_id: &ResourceId,
    exit_status: Option<i32>,
    notices: Vec<Notice>,
) -> FrameOutcome {
    // Drop the slot even for unknown leaves.
    ctx.panes.remove(terminal_id);
    let was_member = ctx.members.remove(terminal_id);
    // Find the window holding this leaf (panes can live in any
    // window, not just the active one) and fold it out there.
    let Some(idx) = window_holding_pane(ctx.workspace, terminal_id) else {
        // An unarranged member ended while nothing is arranged: the
        // "not arranged" count moved, and the session may have ended with it.
        if was_member && ctx.workspace.groups.is_empty() {
            return arrangement_empty(ctx, exit_status, notices, false);
        }
        return FrameOutcome::default();
    };
    let new_state = match apply_terminal_closed(&ctx.workspace.groups[idx].state, terminal_id) {
        Ok(new_state) => new_state,
        Err(err) => {
            // Raced away between lookup and fold; the slot is gone already.
            tracing::debug!(
                error = %err,
                terminal = ?terminal_id,
                "apply_terminal_closed: layout fold failed",
            );
            return FrameOutcome::default();
        }
    };
    ctx.workspace.groups[idx].state = new_state;
    // The fold may have emptied the window; drop any such
    // windows and keep `active` valid.
    ctx.workspace.prune_empty_groups();
    if ctx.workspace.groups.is_empty() {
        return arrangement_empty(ctx, exit_status, notices, true);
    }
    // Re-anchor focus onto the (possibly new) active window.
    *ctx.focused_resource = ctx.workspace.active_group().and_then(|ls| ls.focus.clone());
    FrameOutcome {
        layout_replaced: true,
        emit_set_metadata: true,
        // The survivor's Rect grew; tell the
        // server so its PTY winsize grows too.
        reflow_panes: true,
        notices,
        ..FrameOutcome::default()
    }
}

/// A command was refused with `TERMINAL_NOT_FOUND`: fold the stale leaf out
/// (a pane orphaned by a server restart can be closed no other way).
fn fold_missing_resource<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal_id: &ResourceId,
) -> FrameOutcome {
    tracing::info!(
        terminal = ?terminal_id,
        "server refused a command naming this resource as not found; folding the stale leaf out",
    );
    let expected = ctx.expected_closes.remove(terminal_id);
    let notices = if expected {
        Vec::new()
    } else {
        vec![Notice::warn(format!(
            "{}: gone (the server no longer has this pane)",
            pane_label(terminal_id),
        ))]
    };
    fold_dead_resource(ctx, terminal_id, None, notices)
}

/// Drain a correlated reply's resource op; `Some` ⇒ it was
/// `TERMINAL_NOT_FOUND` and the leaf was folded out.
pub(super) fn resolve_resource_op<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    request_id: u32,
    code: Option<ErrorCode>,
) -> Option<FrameOutcome> {
    let terminal_id = ctx.pending_resource_ops.remove(&request_id)?;
    (code == Some(ErrorCode::TerminalNotFound)).then(|| fold_missing_resource(ctx, &terminal_id))
}

/// A resource close left nothing arranged. The attach policy (R15): detach
/// only when the session itself ends, which for an ephemeral session is the
/// moment its last member is gone. Otherwise stay attached and paint the
/// empty state, which counts members still running with no view.
/// `emptied` is true when this close folded the last leaf, so the stored
/// arrangement changes with it.
fn arrangement_empty<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    exit_status: Option<i32>,
    notices: Vec<Notice>,
    emptied: bool,
) -> FrameOutcome {
    let running = unarranged_members(ctx.members, ctx.workspace, ctx.panes).len();
    if running == 0 && *ctx.lifetime == SessionLifetime::Ephemeral {
        tracing::info!("the ephemeral session's last terminal closed; detaching");
        return FrameOutcome {
            exit: true,
            // Carry the status so the CLI can explain the exit.
            exit_reason: Some(AttachEnd::LastPaneClosed { exit_status }),
            ..FrameOutcome::default()
        };
    }
    tracing::info!(running, "nothing is arranged; staying attached");
    *ctx.focused_resource = None;
    *ctx.zoomed = None;
    FrameOutcome {
        layout_replaced: true,
        // A persistent session with nothing left drops its dead layout;
        // one whose members still run keeps an empty arrangement.
        clear_layout: emptied && running == 0,
        emit_set_metadata: emptied && running > 0,
        notices,
        ..FrameOutcome::default()
    }
}

/// The session this client is attached to was deleted (R15): detach, as the
/// server's `SessionKilled` would.
///
/// An ephemeral session is deleted because its last member left, and
/// organization events reach this client as they commit: ahead of that
/// member's `RESOURCE_CLOSED` and the server's trailing `DETACHED {
/// SESSION_KILLED }`. Those frames end the attach instead, so the teardown
/// line names the last pane's exit rather than a kill.
pub(super) fn session_deleted(lifetime: SessionLifetime) -> FrameOutcome {
    if lifetime == SessionLifetime::Ephemeral {
        tracing::info!(
            "the attached ephemeral session was deleted; awaiting its last close or detach"
        );
        return FrameOutcome::default();
    }
    tracing::info!("the attached session was deleted; detaching");
    FrameOutcome {
        exit: true,
        exit_reason: Some(AttachEnd::Detached {
            reason: Some(DetachReason::SessionKilled),
        }),
        ..FrameOutcome::default()
    }
}

/// A legacy server's `phux.session.keep_empty/v1` broadcast: the session's
/// lifetime as that server spells it. Only a mark on this client's own
/// session changes what its last pane's close does.
pub(super) fn apply_keep_empty_broadcast<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    value: Option<&[u8]>,
) -> FrameOutcome {
    if let Some((name, keep)) = value.and_then(decode_session_keep_empty)
        && name == ctx.session_name.as_str()
    {
        *ctx.lifetime = if keep {
            SessionLifetime::Persistent
        } else {
            SessionLifetime::Ephemeral
        };
    }
    FrameOutcome::default()
}

/// A `phux.session.name/v1` broadcast: rename our status name when it names
/// us; the driver folds the pair into the peer graph.
pub(super) fn apply_session_rename_broadcast<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    value: Option<&[u8]>,
) -> FrameOutcome {
    let Some((current, new_name)) = value.and_then(decode_session_rename) else {
        return FrameOutcome::default();
    };
    if current == ctx.session_name.as_str() {
        new_name.clone_into(ctx.session_name);
    }
    FrameOutcome {
        session_rename: Some((current.to_owned(), new_name.to_owned())),
        chrome_dirty: true,
        ..FrameOutcome::default()
    }
}

/// A Warn notice naming a dead survivor pane, except for a clean exit 0 or a
/// close this client requested.
fn pane_exit_notices(
    terminal_id: &ResourceId,
    exit_status: Option<i32>,
    expected: bool,
) -> Vec<Notice> {
    if expected || exit_status == Some(0) {
        return Vec::new();
    }
    vec![Notice::warn(format!(
        "{}: {}",
        pane_label(terminal_id),
        describe_exit(exit_status),
    ))]
}
