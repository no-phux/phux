//! Resolved-action side effects: `ActionEffects`, the chord consumer,
//! the effect applier, and the `ReattachTarget` vocabulary.

use std::collections::HashMap;

use phux_protocol::ResourceId;
use phux_protocol::wire::frame::{FrameKind, Scope};

use crate::attach::actions;
use crate::attach::connection::Connection;
use crate::attach::focus::FocusHistory;
use crate::attach::outcome::AttachError;
use crate::attach::pane_state::{PaneSlot, reanchor_predict_to_pane};
use crate::layout::ArrangementDoc;
use crate::predict::PredictionState;
use phux_client::layout_ops::{DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, layout_key};

use super::ctx::DispatchCtx;

mod chord;
mod send;
mod types;

pub(super) use chord::{ChordOutcome, consume_chord};
use send::{
    record_reattach_request, send_command_frames, send_detach, send_directory_request,
    send_kill_frames, send_parked_spawns, send_session_rename, start_job,
};
pub use types::ReattachTarget;
pub(super) use types::{ActionEffects, PaneMoveIntent};
pub(in crate::attach) use types::{PendingSessionRename, encode_layout_or_log};

/// Apply a resolved action's side effects, shared by keybindings and overlay
/// commits. The applier order is observable, and is the one thing this
/// function encodes. Returns `true` if the layout changed.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub(super) async fn apply_action_effects<W: crate::attach::RenderSink>(
    effects: ActionEffects,
    out: &mut W,
    conn: &mut Connection,
    ctx: &mut DispatchCtx<'_>,
    focused_resource: &mut Option<ResourceId>,
    predict: &mut PredictionState,
    panes: &HashMap<ResourceId, PaneSlot>,
) -> Result<bool, AttachError> {
    let mut layout_changed = effects.layout_mutated;
    apply_zoom_toggle(effects.toggle_zoom, ctx.zoomed, focused_resource.as_ref());
    apply_sidebar_toggle(effects.toggle_sidebar, ctx.sidebar_enabled);
    apply_focus_effect(
        effects.set_focus,
        effects.clear_predict,
        &mut ctx.focus_history,
        focused_resource,
        predict,
        panes,
    );
    if effects.set_metadata {
        broadcast_layout(conn, ctx).await?;
    }
    if effects.bell {
        let _ = actions::write_bell(out);
    }
    if let Some(text) = effects.clipboard {
        let _ = out.write_all(&crate::attach::copy::osc52_set_clipboard(&text));
        let _ = out.flush();
    }
    send_detach(effects.detach, conn, ctx.detach_pending).await?;
    send_parked_spawns(
        effects.spawn_terminal,
        effects.spawn_window,
        conn,
        ctx.pending_splits,
        ctx.pending_windows,
    )
    .await?;
    if let Some((request_id, title, frame)) = effects.spawn_floating {
        // Parked before the send: the reply can race back before it returns.
        ctx.pending_floating.insert(request_id, title);
        conn.send(&frame).await?;
    }
    send_directory_request(effects.list_directory, conn, ctx.pending_directory).await?;
    if let Some((pending, frame)) = effects.query_path {
        *ctx.pending_path = Some(pending);
        conn.send(&frame).await?;
    }
    if let Some((target, text)) = effects.insert_path {
        // This is an input event, never a shell command or an Enter key.
        let paste = phux_protocol::input::paste::PasteEvent {
            data: text.into_bytes(),
            trust: phux_protocol::input::paste::PasteTrust::Untrusted,
        };
        conn.send(&phux_protocol::input::InputEvent::Paste(paste).into_frame(target))
            .await?;
    }
    send_kill_frames(
        effects.kill_frames,
        effects.expected_closes,
        effects.kill_requests,
        conn,
        ctx.expected_closes,
        ctx.pending_kills,
    )
    .await?;
    send_command_frames(effects.command_frames, conn).await?;
    layout_changed |= Box::pin(apply_pane_move(
        effects.move_pane,
        ctx,
        focused_resource,
        predict,
        panes,
    ))
    .await;
    super::session_management::apply(effects.session_management, ctx).await;
    start_job(effects.job, ctx);
    // Hand a `reload-config` up to the driver, which owns the
    // config-derived state (resolver, theme, keybindings snapshot, status
    // bar) this batch is still borrowing.
    if effects.reload_config {
        *ctx.reload_request = true;
    }
    if effects.switch_host.is_some() {
        *ctx.host_switch_request = effects.switch_host;
    }
    if effects.toggle_project.is_some() {
        *ctx.project_toggle_request = effects.toggle_project;
    }
    if effects.agents_filter.is_some() {
        *ctx.agents_filter_request = effects.agents_filter;
    }
    record_reattach_request(
        effects.reattach,
        ctx.switch_request,
        ctx.session_name,
        ctx.peers.focused_session,
    );
    send_session_rename(effects.rename_session, conn, ctx).await?;
    Ok(layout_changed)
}

/// Execute a picker-committed pane move on a dedicated control connection:
/// the attach connection's correlated helpers would consume interleaved pane
/// output, and a failure stays a toast instead of ending the attach.
#[allow(
    clippy::future_not_send,
    reason = "ADR-0003 binds TUI state and its async dispatcher to the current thread"
)]
async fn apply_pane_move(
    intent: Option<PaneMoveIntent>,
    ctx: &mut DispatchCtx<'_>,
    focused_resource: &mut Option<ResourceId>,
    predict: &mut PredictionState,
    panes: &HashMap<ResourceId, PaneSlot>,
) -> bool {
    let Some(intent) = intent else {
        return false;
    };
    let result = async {
        let dial = ctx.control_dial.ok_or_else(|| {
            AttachError::Protocol("pane move has no dedicated control dial".to_owned())
        })?;
        let mut conn = Connection::connect_dial(dial).await?;
        phux_client::pane_move::move_pane(
            &mut conn,
            intent.source.clone(),
            intent.target,
            intent.dir,
            intent.ratio,
            &[],
        )
        .await
        .map_err(|error| AttachError::Protocol(error.to_string()))
    }
    .await;

    match result {
        Ok(outcome) if outcome.cross_session => {
            let Some((window, pane)) =
                pane_coordinates(&outcome.destination_workspace, &intent.source)
            else {
                push_move_failure(
                    ctx,
                    "the move committed, but the destination layout no longer contains the pane",
                );
                return false;
            };
            *ctx.switch_request = Some(ReattachTarget::Existing {
                name: outcome.destination_session_name,
                id: None,
                window: Some(window),
                pane: Some(pane),
                resource: Some(intent.source),
            });
            true
        }
        Ok(outcome) => {
            *ctx.workspace = outcome.destination_workspace;
            *ctx.zoomed = None;
            apply_focus_effect(
                Some(intent.source),
                true,
                &mut ctx.focus_history,
                focused_resource,
                predict,
                panes,
            );
            true
        }
        Err(error) => {
            tracing::warn!(error = %error, "move-pane failed");
            push_move_failure(ctx, &error.to_string());
            false
        }
    }
}

fn pane_coordinates(workspace: &ArrangementDoc, target: &ResourceId) -> Option<(usize, usize)> {
    workspace
        .groups
        .iter()
        .enumerate()
        .find_map(|(window, item)| {
            let tree = item.state.tree.as_ref()?;
            crate::layout::leaves(tree)
                .iter()
                .position(|pane| pane == target)
                .map(|pane| (window, pane))
        })
}

fn push_move_failure(ctx: &mut DispatchCtx<'_>, message: &str) {
    ctx.overlays
        .push(Box::new(crate::render::overlay::ToastOverlay::new(
            "Pane move failed",
            vec![message.to_owned()],
            ctx.theme,
        )));
}

/// Flip pane-zoom. Un-zoom if zoomed; otherwise zoom the
/// focused pane. `run_action` already gated single-pane windows.
fn apply_zoom_toggle(
    toggle: bool,
    zoomed: &mut Option<ResourceId>,
    focused_resource: Option<&ResourceId>,
) {
    if !toggle {
        return;
    }
    *zoomed = if zoomed.is_some() {
        None
    } else {
        focused_resource.cloned()
    };
}

/// Flip the window-sidebar on/off state. The driver re-folds
/// `sidebar_enabled` into the per-frame reservation after dispatch, so
/// the `layout_mutated` repaint tiles into the new content rect.
const fn apply_sidebar_toggle(toggle: bool, sidebar_enabled: &mut bool) {
    if toggle {
        *sidebar_enabled = !*sidebar_enabled;
    }
}

/// Move the driver's focus (re-anchoring predict to the new pane), or just
/// drop the prediction queue when only that was asked.
fn apply_focus_effect(
    set_focus: Option<ResourceId>,
    clear_predict: bool,
    focus_history: &mut FocusHistory,
    focused_resource: &mut Option<ResourceId>,
    predict: &mut PredictionState,
    panes: &HashMap<ResourceId, PaneSlot>,
) {
    let Some(target) = set_focus else {
        if clear_predict {
            predict.clear();
        }
        return;
    };
    focus_history.transition(focused_resource, Some(target));
    if let Some(fid) = focused_resource.as_ref() {
        reanchor_predict_to_pane(predict, panes, fid);
    }
}

/// Broadcast the workspace on this session's layout key (`SET_METADATA`),
/// once the initial shared read has landed; with an arrangement record,
/// leave the write to the driver after the batch.
#[allow(
    clippy::future_not_send,
    reason = "DispatchCtx holds current-thread state"
)]
pub(super) async fn broadcast_layout(
    conn: &mut Connection,
    ctx: &mut DispatchCtx<'_>,
) -> Result<(), AttachError> {
    if !ctx.layout_read_complete {
        tracing::info!(
            reason = "layout read fence",
            groups = ctx.workspace.groups.len(),
            "arrangement save skipped"
        );
        return Ok(());
    }
    if let Some(owed) = ctx.arrangement_write_owed.as_deref_mut() {
        *owed = true;
        return Ok(());
    }
    let Some(session) = ctx.peers.focused_session else {
        tracing::info!(
            reason = "no focused session",
            groups = ctx.workspace.groups.len(),
            "arrangement save skipped"
        );
        return Ok(());
    };
    tracing::debug!(
        %session,
        groups = ctx.workspace.groups.len(),
        path = "metadata",
        "arrangement save attempt"
    );
    let Some(bytes) = encode_layout_or_log(ctx.workspace) else {
        return Ok(());
    };
    let request_id = ctx.take_request_id();
    conn.send(&FrameKind::SetMetadata {
        request_id,
        scope: Scope::Group(DEFAULT_GROUP_ID),
        key: layout_key(session),
        value: bytes,
    })
    .await
}
