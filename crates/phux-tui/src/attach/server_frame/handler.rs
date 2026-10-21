//! `handle_server_frame`: the per-frame dispatcher, plus the layout
//! reconciliation helpers its metadata arms use.

use std::collections::{HashMap, HashSet};

use phux_protocol::ResourceKind;
use phux_protocol::ids::{ResourceId, SessionId, SessionKey};
use phux_protocol::wire::frame::{FrameKind, SessionLifetime};

use crate::attach::actions::{self, PendingSplit, PendingWindow};
use crate::attach::outcome::AttachError;
use crate::attach::paint::SidebarReservation;
use crate::attach::pane_state::{AttachKernel, PaneSlot};
use crate::layout::{self, ArrangementDoc};
use crate::predict::PredictionState;
use crate::render::chrome::status_bar::StatusBarPainter;

use super::engine_route::{KernelRoute, is_terminal, route_engine_frame};
use super::index::{AgentMetaIndex, note_agent_change};
use super::outcome::{FrameOutcome, frame_kind_label, input_authority_notice, pane_label};
use crate::attach::session_mirror::SessionMirror;

mod attached;
mod closed;
mod events;
mod metadata;
mod output;
mod replies;
mod spawned;
mod windows;

use attached::{
    handle_attached, handle_bootstrap_ready, record_bootstrap_chunk, seed_bootstrap_geometry,
};
use closed::handle_terminal_closed;
use events::handle_agent_event;
use metadata::{
    directory_listing_outcome, handle_detached, handle_metadata_changed, handle_metadata_value,
};
use output::handle_terminal_output;
pub(in crate::attach) use output::{OutputFrame, paint_output_frame};
pub(in crate::attach) use replies::pane_is_referenced;
use replies::{command_result_outcome, error_frame_outcome};
use spawned::handle_terminal_spawned;
#[cfg(test)]
pub(super) use windows::{handle_window_spawned, reconcile_loaded_workspace};

/// The per-frame inputs [`handle_server_frame`] reads beside the mirror: the
/// chrome the paint tiles against, and whether this frame may paint at all.
/// `Default` is a chrome-less, painting frame with no session known yet.
#[derive(Default)]
pub(in crate::attach) struct FrameEnv<'a> {
    /// This client's session, so the layout arm can tell ours from a peer's.
    pub(in crate::attach) focused_session: Option<SessionId>,
    /// `None` when the attach has no configured bar.
    pub(in crate::attach) status_bar: Option<&'a mut StatusBarPainter>,
    /// This frame's sidebar reservation, so every layout site tiles into the
    /// same inset content rect the driver paints against.
    pub(in crate::attach) sidebar: Option<SidebarReservation>,
    /// `(cols, rows)` of the outer terminal.
    pub(in crate::attach) viewport_dims: (u16, u16),
    /// The request id of the initial layout GET, while it is outstanding.
    pub(in crate::attach) pending_layout_request: Option<u32>,
    /// An overlay is on top: mirrors keep ingesting, stdout paints are
    /// suppressed; the driver repaints on dismiss.
    pub(in crate::attach) overlay_active: bool,
    /// An earlier frame of a coalesced burst: apply, but leave the paint to
    /// the pane's last frame in the burst.
    pub(in crate::attach) defer_paint: bool,
}

/// The driver state one inbound frame is dispatched against: the
/// [`SessionMirror`] split into its fields (the kernel read-only, since
/// [`route_engine_frame`] already mutated it) plus the [`FrameEnv`].
struct FrameCtx<'a, W: crate::attach::RenderSink> {
    /// Read-only here: the kernel mutated in [`route_engine_frame`] already.
    engine_kernel: &'a AttachKernel,
    out: &'a mut W,
    panes: &'a mut HashMap<ResourceId, PaneSlot>,
    workspace: &'a mut ArrangementDoc,
    focused_resource: &'a mut Option<ResourceId>,
    // Render/reflow geometry goes through `ArrangementDoc::render_group(zoomed)`;
    // focus reconcile uses the real `active_window`. A split un-zooms.
    zoomed: &'a mut Option<ResourceId>,
    session_name: &'a mut String,
    /// The attached session's lifetime.
    lifetime: &'a mut SessionLifetime,
    /// The attached session's record key, when reported.
    session_key: &'a mut Option<SessionKey>,
    /// Terminal-kind members of the attached session.
    members: &'a mut HashSet<ResourceId>,
    // This client's session, so the layout arm can tell ours from a peer's.
    focused_session: Option<SessionId>,
    /// `Option` so an attach with no configured widgets pays nothing for the
    /// chrome path.
    status_bar: Option<&'a mut StatusBarPainter>,
    // Threaded like `status_bar` so every layout site tiles into the same
    // inset content rect the driver paints against.
    sidebar: Option<SidebarReservation>,
    /// `(cols, rows)` of the outer terminal — used by the painter to pick the
    /// bottom row.
    viewport_dims: (u16, u16),
    predict: &'a mut PredictionState,
    pending_layout_request: Option<u32>,
    pending_splits: &'a mut HashMap<u32, PendingSplit>,
    pending_windows: &'a mut HashMap<u32, PendingWindow>,
    // ADR-0147: floating overlay spawns awaiting their reply, with titles.
    pending_floating: &'a mut HashMap<u32, String>,
    // Closes this client requested; their pane-exit notice is suppressed.
    expected_closes: &'a mut HashSet<ResourceId>,
    // `request_id` -> Terminal for commands whose `TERMINAL_NOT_FOUND` is the
    // only evidence the resource is gone (no `RESOURCE_CLOSED` ever comes for
    // one the server never had), so a stale leaf can be folded out.
    pending_resource_ops: &'a mut HashMap<u32, ResourceId>,
    // ADR-0040 `phux.agent/v1` index, read when composing window labels.
    agent_meta: &'a mut AgentMetaIndex,
    // An overlay is on top: mirrors keep ingesting, stdout paints are
    // suppressed; the driver repaints on dismiss.
    overlay_active: bool,
    // An earlier frame of a coalesced burst: apply, but leave the paint to the
    // pane's last frame in the burst.
    defer_paint: bool,
    /// The per-inbound-frame dispatch span. The heavy content arms record
    /// their identifiers and payload sizes onto it.
    frame_span: &'a tracing::Span,
}

impl<W: crate::attach::RenderSink> FrameCtx<'_, W> {
    /// Whether the kernel knows `terminal_id` as an `AgentSession` resource:
    /// a record stream with no grid, no pane slot, and no layout leaf.
    fn is_agent_session(&self, terminal_id: &ResourceId) -> bool {
        matches!(
            self.engine_kernel.resource_kind(terminal_id),
            Some(ResourceKind::AgentSession)
        )
    }

    /// Whether a layout leaf may name `terminal_id`: anything the kernel does
    /// not know to be a non-terminal kind. Peer ids it cannot classify pass.
    fn may_be_layout_leaf(&self, terminal_id: &ResourceId) -> bool {
        !matches!(
            self.engine_kernel.resource_kind(terminal_id),
            Some(kind) if kind != ResourceKind::Terminal
        )
    }

    /// Decode one of OUR layout envelopes, refusing any leaf that names a
    /// known non-terminal resource.
    fn decode_own_layout(&self, bytes: &[u8]) -> Result<ArrangementDoc, layout::LayoutDecodeError> {
        ArrangementDoc::decode_cbor_checked(bytes, &|leaf| self.may_be_layout_leaf(leaf))
    }
}

/// The outcome for a frame on an `AgentSession` stream: nothing to paint, but
/// the chrome projects the stream's state, so it refreshes when the log grew.
fn agent_stream_outcome(terminal_id: &ResourceId, route: KernelRoute) -> FrameOutcome {
    FrameOutcome {
        chrome_dirty: route.agent_touched.contains(terminal_id),
        notices: route.notices,
        ..FrameOutcome::default()
    }
}

/// Process one server-to-client frame into `mirror`; the [`FrameOutcome`]
/// describes the follow-up the async driver owes.
pub(in crate::attach) fn handle_server_frame<W: crate::attach::RenderSink>(
    mirror: &mut SessionMirror,
    env: FrameEnv<'_>,
    out: &mut W,
    frame: FrameKind,
) -> Result<FrameOutcome, AttachError> {
    let SessionMirror {
        engine_kernel,
        kernel_effects,
        panes,
        workspace,
        focused_resource,
        zoomed,
        session_name,
        lifetime,
        session_key,
        members,
        predict,
        pending_splits,
        pending_windows,
        pending_floating,
        expected_closes,
        pending_resource_ops,
        agent_meta,
    } = mirror;
    let FrameEnv {
        focused_session,
        status_bar,
        sidebar,
        viewport_dims,
        pending_layout_request,
        overlay_active,
        defer_paint,
    } = env;
    let is_output = matches!(frame, FrameKind::ResourceOutput { .. });
    let apply_span = is_output.then(|| tracing::debug_span!("vt_apply"));
    let apply_guard = apply_span.as_ref().map(tracing::Span::enter);
    let apply_timer = is_output.then(|| phux_client::perf::VT_APPLY.timer());
    let mut kernel_route = route_engine_frame(&frame, engine_kernel, kernel_effects);
    drop(apply_timer);
    drop(apply_guard);
    if let Some(verdict) = kernel_route_verdict(&kernel_route, &frame) {
        return verdict;
    }
    let history_chrome_dirty = apply_history_health(panes, &kernel_route);
    // Per-frame dispatch span (debug). Its CLOSE duration is apply+paint cost;
    // fields are recorded in the arms below.
    let frame_span = tracing::debug_span!(
        "handle_server_frame",
        kind = frame_kind_label(&frame),
        terminal_id = tracing::field::Empty,
        seq = tracing::field::Empty,
        bytes = tracing::field::Empty,
    )
    .entered();
    let mut ctx = FrameCtx {
        engine_kernel,
        out,
        panes,
        workspace,
        focused_resource,
        zoomed,
        session_name,
        lifetime,
        session_key,
        members,
        focused_session,
        status_bar,
        sidebar,
        viewport_dims,
        predict,
        pending_layout_request,
        pending_splits,
        pending_windows,
        pending_floating,
        expected_closes,
        pending_resource_ops,
        agent_meta,
        overlay_active,
        defer_paint,
        frame_span: &frame_span,
    };
    let clipboard_writes = std::mem::take(&mut kernel_route.clipboard_writes);
    let mut outcome = dispatch_frame(&mut ctx, frame, kernel_route)?;
    outcome.clipboard_writes = clipboard_writes;
    outcome.chrome_dirty |= history_chrome_dirty;
    Ok(outcome)
}

/// Fold the frame's per-pane history health into the pane slots. Returns
/// whether any mark flipped, so the supervisory badge repaints.
fn apply_history_health(panes: &mut HashMap<ResourceId, PaneSlot>, route: &KernelRoute) -> bool {
    let mut changed = false;
    for (terminal_id, &degraded) in &route.history_degraded {
        let Some(slot) = panes.get_mut(terminal_id) else {
            continue;
        };
        changed |= slot.history_degraded != degraded;
        slot.history_degraded = degraded;
    }
    changed
}

/// The kernel's own routing verdicts: a rejected frame is a protocol error; a
/// resync request or a retired-generation frame ends dispatch. `None` ⇒
/// accepted.
fn kernel_route_verdict(
    route: &KernelRoute,
    frame: &FrameKind,
) -> Option<Result<FrameOutcome, AttachError>> {
    if let Some(error) = route.failed.as_ref() {
        return Some(Err(AttachError::Protocol(format!(
            "session kernel rejected {}: {error}",
            frame_kind_label(frame),
        ))));
    }
    if route.resync_required {
        return Some(Ok(FrameOutcome {
            resync_required: true,
            ..FrameOutcome::default()
        }));
    }
    if route.ignored {
        return Some(Ok(FrameOutcome::default()));
    }
    None
}

/// Route one accepted frame to its arm. Arm order is routing precedence, so
/// new arms go within their semantic group.
fn dispatch_frame<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    frame: FrameKind,
    route: KernelRoute,
) -> Result<FrameOutcome, AttachError> {
    match frame {
        FrameKind::Attached {
            attach_id: _,
            snapshot,
            initial_client_id,
        } => handle_attached(ctx, &snapshot, initial_client_id),
        FrameKind::BootstrapBegin {
            terminal_id,
            cols,
            rows,
            ..
        } => seed_bootstrap_geometry(ctx, terminal_id, cols, rows),
        FrameKind::BootstrapChunk {
            terminal_id,
            payload,
            ..
        } => Ok(record_bootstrap_chunk(ctx, &terminal_id, payload.len())),
        // A record stream has nothing to paint: its READY and its live
        // output only refresh the chrome that projects it.
        FrameKind::BootstrapReady { terminal_id, .. }
        | FrameKind::ResourceOutput { terminal_id, .. }
            if ctx.is_agent_session(&terminal_id) =>
        {
            Ok(agent_stream_outcome(&terminal_id, route))
        }
        FrameKind::BootstrapReady { terminal_id, .. } => {
            handle_bootstrap_ready(ctx, &terminal_id, route)
        }
        FrameKind::HistoryPage { .. }
        | FrameKind::HistoryTombstone { .. }
        | FrameKind::HistoryRejected { .. } => Ok(FrameOutcome {
            history_request: route.history_request,
            notices: route.notices,
            ..FrameOutcome::default()
        }),
        FrameKind::AttachReady { .. } => {
            let authoritative_damage = route.damaged.iter().cloned().collect();
            Ok(FrameOutcome {
                layout_replaced: !route.damaged.is_empty(),
                authoritative_damage,
                ..FrameOutcome::default()
            })
        }
        FrameKind::ResourceOutput {
            terminal_id,
            stream_id: _,
            bootstrap_id: _,
            seq,
            bytes,
        } => handle_terminal_output(ctx, &terminal_id, seq, &bytes, route),
        FrameKind::BootstrapTombstone { .. } => Ok(FrameOutcome::default()),
        FrameKind::Detached { reason, message } => Ok(handle_detached(reason, &message)),
        FrameKind::Bell { .. } => {
            // Forward the bell through the sink so headless captures see it.
            let _ = actions::write_bell(ctx.out);
            Ok(FrameOutcome::default())
        }
        FrameKind::MetadataValue { request_id, value } => {
            handle_metadata_value(ctx, request_id, value)
        }
        FrameKind::MetadataChanged {
            scope, key, value, ..
        } => handle_metadata_changed(ctx, &scope, &key, value),
        FrameKind::DirectoryListing { request_id, result } => {
            Ok(directory_listing_outcome(request_id, result))
        }
        FrameKind::PathResults { request_id, result } => Ok(FrameOutcome {
            path_results: Some((request_id, result)),
            ..FrameOutcome::default()
        }),
        FrameKind::ResourceSpawned { request_id, result } => {
            handle_terminal_spawned(ctx, request_id, result)
        }
        closed @ FrameKind::ResourceClosed { .. } => Ok(handle_terminal_closed(ctx, closed)),
        event @ FrameKind::Event { .. } => Ok(handle_agent_event(ctx, event, &route)),
        FrameKind::Error {
            request_id,
            code,
            message,
        } => error_frame_outcome(ctx, request_id, code, message),
        // A request-correlated reply that reached the dispatcher instead of
        // its awaiter (`Connection::request` returns interleaved frames
        // here). Inert, never terminal: L1 §5 requires clients to tolerate
        // replies interleaved with other traffic.
        FrameKind::CommandResult { request_id, result } => {
            command_result_outcome(ctx, request_id, result)
        }
        FrameKind::ResourceMoved { request_id, .. } => {
            tracing::debug!(
                request_id,
                "dropping ResourceMoved with no matching pending request"
            );
            Ok(FrameOutcome::default())
        }
        other => unexpected_frame_outcome(&other),
    }
}

/// A frame no arm takes. A server-direction frame is dropped: a newer server
/// on the same major may send a reply or notice this client never asked
/// for, and ignoring it is safe. A duplicate `HELLO_OK` or a client-direction
/// frame is a peer fault no version skew explains, and stays fatal.
fn unexpected_frame_outcome(frame: &FrameKind) -> Result<FrameOutcome, AttachError> {
    if !is_ignorable_server_frame(frame) {
        return Err(unexpected_frame(frame));
    }
    tracing::debug!(
        frame = frame_kind_label(frame),
        "ignoring a server frame this client has no use for"
    );
    Ok(FrameOutcome::default())
}

/// Server-to-client frames carry the high type bit (`docs/spec/proto.md`
/// §7); `HELLO_OK` is one, but only the handshake may carry it.
const fn is_ignorable_server_frame(frame: &FrameKind) -> bool {
    frame.type_byte() & 0x80 != 0 && !matches!(frame, FrameKind::HelloOk { .. })
}

/// The one rejection this dispatcher makes: a frame no server may send in the
/// attached phase.
fn unexpected_frame(frame: &FrameKind) -> AttachError {
    AttachError::Protocol(format!(
        "frame is not valid from a server in the attached phase: {frame:?}",
    ))
}

#[cfg(test)]
mod unexpected_frame_tests;
