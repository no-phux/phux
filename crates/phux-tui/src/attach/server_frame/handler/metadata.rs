//! Detach, directory listings, and metadata values and broadcasts.

use phux_client::agent_meta::{
    RESOURCE_AGENT_KEY, RESOURCE_ASKED_KEY, RESOURCE_PANE_OCCUPANT_KEY, parse_pane_occupant,
};
use phux_client::layout_ops::LayoutKeyOwner;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    CONFIG_RELOAD_KEY, DetachReason, SESSION_KEEP_EMPTY_KEY, SESSION_NAME_KEY, Scope,
};

use super::closed::{apply_keep_empty_broadcast, apply_session_rename_broadcast};
use super::spawned::window_holding_pane;
use super::windows::layout_key_scope_session;
use super::{FrameCtx, FrameOutcome, note_agent_change};
use crate::attach::outcome::{AttachEnd, AttachError};
use crate::layout::ArrangementDoc;
use crate::render::chrome::status_bar::Notice;

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
    value: Option<&[u8]>,
) -> Result<FrameOutcome, AttachError> {
    // ADR-0040: a pending per-Terminal `phux.agent/v1` GET reply.
    // `value: None` (key absent) clears any stale record.
    if let Some(terminal) = ctx.agent_meta.asked_pending.remove(&request_id) {
        return Ok(apply_asked_flag(ctx, terminal, value));
    }
    if let Some(terminal) = ctx.agent_meta.occupant_pending.remove(&request_id) {
        return Ok(apply_occupant(ctx, &terminal, value));
    }
    if let Some(terminal) = ctx.agent_meta.pending.remove(&request_id) {
        let changed = ctx.agent_meta.apply(&terminal, value);
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
    let layout = value
        .map(ArrangementDoc::decode_cbor)
        .transpose()
        .map_err(|err| layout_decode_refusal(&err))?;
    Ok(ctx.adopt_layout(layout, true))
}

/// A pending metadata GET answered with a correlated `ERROR`: the request
/// settles, nothing is replaced, and the layout write fence stays up.
pub(super) fn metadata_error_outcome<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    request_id: u32,
) -> Option<FrameOutcome> {
    if ctx.agent_meta.asked_pending.remove(&request_id).is_some()
        || ctx
            .agent_meta
            .occupant_pending
            .remove(&request_id)
            .is_some()
        || ctx.agent_meta.pending.remove(&request_id).is_some()
    {
        return Some(FrameOutcome::default());
    }
    (Some(request_id) == ctx.pending_layout_request).then(|| FrameOutcome {
        layout_get_refused: true,
        notices: vec![Notice::warn(
            "the shared layout could not be read; layout edits stay off".to_owned(),
        )],
        ..FrameOutcome::default()
    })
}

/// The pane's foreground from its `phux.pane-occupant/v1` record: it names
/// an auto-numbered window, so a change repaints the chrome.
fn apply_occupant<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal: &ResourceId,
    value: Option<&[u8]>,
) -> FrameOutcome {
    let foreground = value
        .and_then(parse_pane_occupant)
        .map(|record| record.foreground);
    let Some(slot) = ctx.panes.get_mut(terminal) else {
        return FrameOutcome::default();
    };
    let changed = slot.foreground != foreground;
    slot.foreground = foreground;
    FrameOutcome {
        chrome_dirty: changed,
        ..FrameOutcome::default()
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
    if key == phux_protocol::wire::frame::RESOURCE_NOTE_KEY {
        return Ok(note_outcome(scope, value.as_deref()));
    }
    if key == RESOURCE_ASKED_KEY {
        let Scope::Resource(terminal) = scope else {
            return Ok(FrameOutcome::default());
        };
        return Ok(apply_asked_flag(ctx, terminal.clone(), value.as_deref()));
    }
    if key == RESOURCE_PANE_OCCUPANT_KEY {
        let Scope::Resource(terminal) = scope else {
            return Ok(FrameOutcome::default());
        };
        return Ok(apply_occupant(ctx, terminal, value.as_deref()));
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
    if key == phux_client::session_order::SESSION_ORDER_KEY && matches!(scope, Scope::Global) {
        return Ok(FrameOutcome {
            session_order: Some(phux_client::session_order::decode(value.as_deref())),
            ..FrameOutcome::default()
        });
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
    let layout = ArrangementDoc::decode_cbor(&bytes).map_err(|err| layout_decode_refusal(&err))?;
    Ok(ctx.adopt_layout(Some(layout), false))
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

/// A program posted a note for a pane (`phux notify`): toast it, naming the
/// pane under the note's own words.
fn note_outcome(scope: &Scope, value: Option<&[u8]>) -> FrameOutcome {
    let (Scope::Resource(terminal), Some(note)) =
        (scope, value.and_then(phux_client_core::note::parse_note))
    else {
        return FrameOutcome::default();
    };
    let from = format!(
        "from {}",
        phux_client::selector::format_terminal_id(terminal)
    );
    let lines = note.body.into_iter().chain([from]).collect();
    FrameOutcome {
        note: Some(crate::attach::jobs::Toast::lines(note.title, lines)),
        ..FrameOutcome::default()
    }
}
