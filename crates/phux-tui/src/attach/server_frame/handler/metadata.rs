//! Detach, directory listings, and metadata values and broadcasts.

use phux_client::agent_meta::{RESOURCE_AGENT_KEY, RESOURCE_ASKED_KEY};
use phux_client::layout_ops::LayoutKeyOwner;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    CONFIG_RELOAD_KEY, DetachReason, SESSION_KEEP_EMPTY_KEY, SESSION_NAME_KEY, Scope,
};

use super::closed::{apply_keep_empty_broadcast, apply_session_rename_broadcast};
use super::spawned::window_holding_pane;
use super::windows::{
    layout_key_scope_session, layout_names_only_absent_panes, reconcile_loaded_workspace,
    unknown_layout_leaves,
};
use super::{FrameCtx, FrameOutcome, note_agent_change};
use crate::attach::outcome::{AttachEnd, AttachError};
use crate::layout::ArrangementDoc;

/// `DETACHED` is the one ending a consumer acts on; carry its reason. The
/// message is diagnostic only.
pub(super) fn handle_detached(reason: Option<DetachReason>, message: &str) -> FrameOutcome {
    tracing::info!(?reason, %message, "DETACHED");
    FrameOutcome {
        exit: true,
        exit_reason: Some(AttachEnd::Detached { reason }),
        ..FrameOutcome::default()
    }
}

/// A `go-to-directory` reply, handed up to the driver to match.
pub(super) fn directory_listing_outcome(
    request_id: u32,
    result: phux_protocol::wire::frame::DirectoryListingResult,
) -> FrameOutcome {
    FrameOutcome {
        directory_listing: Some((request_id, result)),
        ..FrameOutcome::default()
    }
}

/// The correlated layout GET reply: decode and adopt its topology, keeping
/// this client's focus. `value: None` keeps the bootstrap.
pub(super) fn handle_metadata_value<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    request_id: u32,
    value: Option<Vec<u8>>,
) -> Result<FrameOutcome, AttachError> {
    // ADR-0040: a pending per-Terminal `phux.agent/v1` GET reply.
    // `value: None` (key absent) clears any stale record.
    if let Some(terminal) = ctx.agent_meta.asked_pending.remove(&request_id) {
        return Ok(apply_asked_flag(ctx, terminal, value.as_deref()));
    }
    if let Some(terminal) = ctx.agent_meta.pending.remove(&request_id) {
        let changed = ctx.agent_meta.apply(&terminal, value.as_deref());
        if changed {
            note_agent_change(ctx.panes, ctx.focused_resource.as_ref(), &terminal);
        }
        return Ok(FrameOutcome {
            agent_meta_changed: changed,
            agent_meta_terminal: Some(terminal),
            ..FrameOutcome::default()
        });
    }
    if Some(request_id) != ctx.pending_layout_request {
        tracing::debug!(
            request_id,
            "dropping MetadataValue with no matching pending request"
        );
        return Ok(FrameOutcome::default());
    }
    let Some(bytes) = value else {
        return Ok(FrameOutcome {
            layout_get_answered: true,
            ..FrameOutcome::default()
        });
    };
    match ctx.decode_own_layout(&bytes) {
        Ok(new_ws) if layout_names_only_absent_panes(&new_ws, ctx.panes) => {
            tracing::debug!(
                "persisted layout names only panes absent from the snapshot; discarding"
            );
            Ok(FrameOutcome {
                layout_replaced: true,
                layout_get_answered: true,
                ..FrameOutcome::default()
            })
        }
        Ok(new_ws) => {
            let attach_panes = adopt_workspace(ctx, new_ws);
            Ok(FrameOutcome {
                layout_replaced: true,
                layout_get_answered: true,
                // Every restored leaf's rect moved; reflow the PTYs.
                reflow_panes: true,
                attach_panes,
                ..FrameOutcome::default()
            })
        }
        Err(err) => Err(layout_decode_refusal(&err)),
    }
}

/// A sibling's layout broadcast: adopt topology (ADR-0049: never its
/// focus). A tombstone resets to the single-pane bootstrap.
pub(super) fn handle_metadata_changed<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    scope: &Scope,
    key: &str,
    value: Option<Vec<u8>>,
) -> Result<FrameOutcome, AttachError> {
    if key == RESOURCE_AGENT_KEY {
        return Ok(apply_agent_broadcast(ctx, scope, value));
    }
    if key == RESOURCE_ASKED_KEY {
        let Scope::Resource(terminal) = scope else {
            return Ok(FrameOutcome::default());
        };
        return Ok(apply_asked_flag(ctx, terminal.clone(), value.as_deref()));
    }
    // The config-reload doorbell; the value is a nonce, a tombstone is not a
    // request.
    if key == CONFIG_RELOAD_KEY && matches!(scope, Scope::Global) {
        return Ok(FrameOutcome {
            config_reload: value.is_some(),
            ..FrameOutcome::default()
        });
    }
    if key == SESSION_KEEP_EMPTY_KEY && matches!(scope, Scope::Global) {
        return Ok(apply_keep_empty_broadcast(ctx, value.as_deref()));
    }
    if key == SESSION_NAME_KEY && matches!(scope, Scope::Global) {
        return Ok(apply_session_rename_broadcast(ctx, value.as_deref()));
    }
    if key == phux_protocol::wire::frame::SESSION_PROJECT_KEY && matches!(scope, Scope::Global) {
        return Ok(crate::attach::server_frame::outcome::project_tag_outcome(
            value.as_deref(),
        ));
    }
    let Some(LayoutKeyOwner::Session(key_session)) = layout_key_scope_session(scope, key) else {
        return Ok(FrameOutcome::default());
    };
    // Session-key attribution is the authority; a bare key identifies no owner.
    if ctx.focused_session != Some(key_session) {
        return Ok(FrameOutcome {
            foreign_layout: Some((key_session, value)),
            ..FrameOutcome::default()
        });
    }
    let Some(bytes) = value else {
        // Tombstone: layout reset. Fall back to single-pane
        // bootstrap (or empty if there's no focus to anchor on).
        *ctx.workspace = ctx
            .focused_resource
            .clone()
            .map_or_else(ArrangementDoc::default, ArrangementDoc::single);
        return Ok(FrameOutcome {
            layout_replaced: true,
            ..FrameOutcome::default()
        });
    };
    match ctx.decode_own_layout(&bytes) {
        Ok(new_ws) => {
            let attach_panes = adopt_workspace(ctx, new_ws);
            Ok(FrameOutcome {
                layout_replaced: true,
                // A peer's topology change reshapes our tiles too.
                reflow_panes: true,
                attach_panes,
                ..FrameOutcome::default()
            })
        }
        Err(err) => Err(layout_decode_refusal(&err)),
    }
}

fn layout_decode_refusal(error: &crate::layout::LayoutDecodeError) -> AttachError {
    AttachError::Protocol(format!(
        "shared layout refused: {error}; stored metadata was preserved, not reset"
    ))
}

/// ADR-0136: `phux.agent.asked/v1` is `1` while an ask is pending. A
/// workspace pane stores it on `PaneSlot::attention`; anything else is a
/// foreign attention change.
fn apply_asked_flag<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal: ResourceId,
    value: Option<&[u8]>,
) -> FrameOutcome {
    let asked = value == Some(b"1");
    let in_workspace = window_holding_pane(ctx.workspace, &terminal).is_some();
    let chrome_dirty = if in_workspace
        && let Some(slot) = ctx.panes.get_mut(&terminal)
        && slot.attention != asked
    {
        slot.attention = asked;
        true
    } else {
        false
    };
    if in_workspace {
        return FrameOutcome {
            chrome_dirty,
            ..FrameOutcome::default()
        };
    }
    if asked {
        FrameOutcome {
            foreign_attention: Some(terminal),
            ..FrameOutcome::default()
        }
    } else {
        FrameOutcome {
            foreign_attention_clear: Some(terminal),
            ..FrameOutcome::default()
        }
    }
}

fn apply_agent_broadcast<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    scope: &Scope,
    value: Option<Vec<u8>>,
) -> FrameOutcome {
    let Scope::Resource(terminal) = scope else {
        return FrameOutcome::default();
    };
    // A mirror slot can come from the server-wide graph; only workspace
    // membership makes this agent local.
    if window_holding_pane(ctx.workspace, terminal).is_none() {
        return FrameOutcome {
            foreign_agent: Some((terminal.clone(), value)),
            ..FrameOutcome::default()
        };
    }
    let changed = ctx.agent_meta.apply(terminal, value.as_deref());
    if changed {
        note_agent_change(ctx.panes, ctx.focused_resource.as_ref(), terminal);
    }
    FrameOutcome {
        agent_meta_changed: changed,
        agent_meta_terminal: Some(terminal.clone()),
        ..FrameOutcome::default()
    }
}

/// Replace the local workspace with a decoded envelope's topology, re-anchor
/// focus, and report leaves this client has never seen.
fn adopt_workspace<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    incoming: ArrangementDoc,
) -> Vec<ResourceId> {
    let reconciled =
        reconcile_loaded_workspace(incoming, ctx.workspace, ctx.focused_resource.as_ref());
    *ctx.workspace = reconciled;
    let attach_panes = unknown_layout_leaves(ctx.workspace, ctx.panes);
    *ctx.focused_resource = ctx.workspace.active_group().and_then(|ls| ls.focus.clone());
    attach_panes
}
