//! `handle_server_frame`: the per-frame dispatcher, plus the layout
//! reconciliation helpers its metadata arms use.

use std::collections::{HashMap, HashSet};

use phux_client_core::session::EffectBuffer as KernelEffectBuffer;
use phux_protocol::ResourceKind;
use phux_protocol::ids::{ClientId, ResourceId, SessionId};
use phux_protocol::wire::frame::{
    AgentEvent, CONFIG_RELOAD_KEY, DetachReason, ErrorCode, FrameKind, ResourceLifecycle,
    SESSION_KEEP_EMPTY_KEY, SESSION_NAME_KEY, Scope, SpawnError, SpawnResult,
    decode_session_keep_empty, decode_session_rename,
};

use crate::attach::actions::{
    self, Adopt, ParkedAdopt, PendingSplit, PendingWindow, SpawnedPane, SplitHost,
    apply_spawned_ok, apply_terminal_closed,
};
use crate::attach::outcome::{AttachEnd, AttachError, describe_exit};
use crate::attach::paint::{SidebarReservation, StatusBarPaint, content_rect, paint_focused_pane};
use crate::attach::pane_state::{
    AttachKernel, PaneSlot, published_replica, published_terminal, reanchor_predict_to_pane,
};
use crate::attach::render::ReplicaWalk;
use crate::layout::{self, LayoutState, Rect, Workspace};
use crate::predict::{Overlay, PredictionState, reconcile_terminal_output_per_cell_at};
use crate::render::chrome::status_bar::{Notice, StatusBarPainter};
use phux_client::agent_meta::{RESOURCE_AGENT_KEY, RESOURCE_ASKED_KEY};
use phux_client::conditional_kill::BoundResource;
use phux_client::layout_ops::{
    DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, LayoutKeyOwner, layout_key_session,
};

use super::engine_route::{KernelRoute, is_terminal, route_engine_frame};
use super::index::{AgentMetaIndex, note_agent_change};
use super::outcome::{FrameOutcome, frame_kind_label, input_authority_notice, pane_label};

/// The driver state one inbound frame is dispatched against, threaded from
/// [`handle_server_frame`]'s flat parameter list (the driver boundary).
struct FrameCtx<'a, W: crate::attach::RenderSink> {
    /// Read-only here: the kernel mutated in [`route_engine_frame`] already.
    engine_kernel: &'a AttachKernel,
    out: &'a mut W,
    panes: &'a mut HashMap<ResourceId, PaneSlot>,
    workspace: &'a mut Workspace,
    focused_resource: &'a mut Option<ResourceId>,
    // Render/reflow geometry goes through `Workspace::render_window(zoomed)`;
    // focus reconcile uses the real `active_window`. A split un-zooms.
    zoomed: &'a mut Option<ResourceId>,
    session_name: &'a mut String,
    /// ADR-0105: whether the attached session is keep-empty.
    keep_empty_session: &'a mut bool,
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
    overlay: &'a Overlay,
    pending_layout_request: Option<u32>,
    pending_splits: &'a mut HashMap<u32, PendingSplit>,
    pending_windows: &'a mut HashMap<u32, PendingWindow>,
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
    fn decode_own_layout(&self, bytes: &[u8]) -> Result<Workspace, layout::LayoutDecodeError> {
        Workspace::decode_cbor_checked(bytes, &|leaf| self.may_be_layout_leaf(leaf))
    }
}

/// The outcome for a frame on an `AgentSession` stream: nothing to paint, but
/// the chrome projects the stream's state, so it refreshes when the log grew.
fn agent_stream_outcome(terminal_id: &ResourceId, route: KernelRoute) -> FrameOutcome {
    FrameOutcome {
        chrome_dirty: route.agent_touched.contains(terminal_id),
        pty_writes: route.pty_writes,
        notices: route.notices,
        ..FrameOutcome::default()
    }
}

/// Process one server-to-client frame; the [`FrameOutcome`] describes the
/// follow-up the async driver owes.
#[allow(
    clippy::too_many_arguments,
    reason = "the driver's whole per-frame state, threaded verbatim from `main_loop` / `headless`; the arms take a `FrameCtx` built from these, but the entry point's shape is the driver boundary"
)]
pub(in crate::attach) fn handle_server_frame<W: crate::attach::RenderSink>(
    engine_kernel: &mut crate::attach::pane_state::AttachKernel,
    kernel_effects: &mut KernelEffectBuffer,
    out: &mut W,
    frame: FrameKind,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    workspace: &mut Workspace,
    focused_resource: &mut Option<ResourceId>,
    zoomed: &mut Option<ResourceId>,
    session_name: &mut String,
    keep_empty_session: &mut bool,
    focused_session: Option<SessionId>,
    status_bar: Option<&mut StatusBarPainter>,
    sidebar: Option<SidebarReservation>,
    viewport_dims: (u16, u16),
    predict: &mut PredictionState,
    overlay: &Overlay,
    pending_layout_request: Option<u32>,
    pending_splits: &mut HashMap<u32, PendingSplit>,
    pending_windows: &mut HashMap<u32, PendingWindow>,
    expected_closes: &mut HashSet<ResourceId>,
    pending_resource_ops: &mut HashMap<u32, ResourceId>,
    agent_meta: &mut AgentMetaIndex,
    overlay_active: bool,
    defer_paint: bool,
) -> Result<FrameOutcome, AttachError> {
    let is_output = matches!(frame, FrameKind::ResourceOutput { .. });
    let apply_span = is_output.then(|| tracing::debug_span!("vt_apply"));
    let apply_guard = apply_span.as_ref().map(tracing::Span::enter);
    let apply_timer = is_output.then(|| phux_client::perf::VT_APPLY.timer());
    let kernel_route = route_engine_frame(&frame, engine_kernel, kernel_effects);
    drop(apply_timer);
    drop(apply_guard);
    if let Some(verdict) = kernel_route_verdict(&kernel_route, &frame) {
        return verdict;
    }
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
        keep_empty_session,
        focused_session,
        status_bar,
        sidebar,
        viewport_dims,
        predict,
        overlay,
        pending_layout_request,
        pending_splits,
        pending_windows,
        expected_closes,
        pending_resource_ops,
        agent_meta,
        overlay_active,
        defer_paint,
        frame_span: &frame_span,
    };
    dispatch_frame(&mut ctx, frame, kernel_route)
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
        } => Ok(record_bootstrap_chunk(
            ctx,
            &terminal_id,
            payload.len(),
            route,
        )),
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
            pty_writes: route.pty_writes,
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
        // its awaiter (`Connection::await_answer` replays interleaved frames
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
        other => Err(unexpected_frame(&other)),
    }
}

/// The one rejection this dispatcher makes: a frame no server may send in the
/// attached phase.
fn unexpected_frame(frame: &FrameKind) -> AttachError {
    AttachError::Protocol(format!(
        "frame is not valid from a server in the attached phase: {frame:?}",
    ))
}

/// `ATTACHED` carries the session graph; cells arrive via each bootstrap.
/// The outcome asks the driver to fetch and subscribe the layout key.
fn handle_attached<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    initial_client_id: ClientId,
) -> Result<FrameOutcome, AttachError> {
    // ADR-0105: the focused session's keep-empty mark decides what the last
    // pane's close does, and an empty session has no pane to focus at all.
    *ctx.keep_empty_session = snapshot
        .sessions
        .iter()
        .find(|s| s.id == snapshot.focused_session)
        .is_some_and(|s| s.keep_empty);
    if focused_session_is_empty(snapshot) {
        return Ok(attach_empty_session(ctx, snapshot, initial_client_id));
    }
    // Capture the initial focused pane so subsequent INPUT_* frames
    // know where to route.
    let bootstrap = snapshot.focused_resource.clone();
    tracing::debug!(
        terminal_id = ?bootstrap,
        "ATTACHED: seeding focused_resource from snapshot"
    );
    *ctx.focused_resource = Some(bootstrap.clone());
    // Single-pane seed; the persisted layout replaces it when present.
    *ctx.workspace = Workspace::single(bootstrap.clone());
    // Seed mirrors at server-advertised sizes before output can race the
    // bootstrap (VT interpretation is geometry-sensitive). AgentSessions
    // have no grid and get no slot.
    for pane in snapshot.resources.iter().filter(|pane| is_terminal(pane)) {
        if let std::collections::hash_map::Entry::Vacant(v) = ctx.panes.entry(pane.id.clone()) {
            let slot = v.insert(PaneSlot::new_with_size(pane.cols, pane.rows)?);
            // Seed the pane's cwd from the snapshot (the
            // spawn cwd); `cwd_changed` events refine it live.
            slot.cwd.clone_from(&pane.cwd);
            // ADR-0124: a pane retained after its process exited arrives
            // read-only, with the exit the snapshot reports.
            slot.lifecycle = pane.lifecycle;
            slot.exited = pane
                .exit
                .as_ref()
                .map(crate::attach::pane_state::ExitMark::from_facet);
        }
    }
    // Hand the per-pane cwds up to the driver so the
    // sidebar can derive each window's VCS branch client-side.
    let pane_cwds: Vec<(ResourceId, String)> = snapshot
        .resources
        .iter()
        .filter(|pane| is_terminal(pane))
        .filter_map(|p| p.cwd.clone().map(|cwd| (p.id.clone(), cwd)))
        .collect();
    // The focused pane gets a slot even if the graph omitted it.
    if let std::collections::hash_map::Entry::Vacant(v) = ctx.panes.entry(bootstrap) {
        let content = content_rect(
            ctx.viewport_dims,
            ctx.status_bar.as_ref().map(|p| p.position()),
            ctx.sidebar,
        );
        v.insert(PaneSlot::new_with_size(content.w, content.h)?);
    }
    *ctx.session_name = focused_session_name(snapshot);
    // The session graph for the session picker.
    let session_cache = (snapshot.sessions.clone(), snapshot.focused_session);
    Ok(FrameOutcome {
        subscribe_layout: true,
        sessions: Some(session_cache),
        inventory: Some((snapshot.windows.clone(), snapshot.resources.clone())),
        // ADR-0033: cache our own ClientId so the supervisory badge can
        // distinguish "you hold the wheel" from another client.
        own_client_id: Some(initial_client_id),
        pane_cwds,
        ..FrameOutcome::default()
    })
}

/// ADR-0105: the focused session holds no windows (zero windows AND a `0`
/// sentinel focus, so an unset `window_count` does not read as empty).
fn focused_session_is_empty(snapshot: &phux_protocol::wire::info::SessionSnapshot) -> bool {
    let reports_no_windows = snapshot
        .sessions
        .iter()
        .find(|s| s.id == snapshot.focused_session)
        .is_some_and(phux_protocol::wire::info::SessionInfo::is_empty);
    let focus_is_listed = snapshot
        .resources
        .iter()
        .any(|r| r.id == snapshot.focused_resource);
    reports_no_windows && !focus_is_listed
}

/// ADR-0105: ATTACHED to a session with no windows: start empty; the graph,
/// client id, and layout subscription are recorded as usual.
fn attach_empty_session<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    initial_client_id: ClientId,
) -> FrameOutcome {
    tracing::debug!("ATTACHED: session has no windows; starting in the empty state");
    *ctx.focused_resource = None;
    *ctx.workspace = Workspace::default();
    *ctx.session_name = focused_session_name(snapshot);
    FrameOutcome {
        subscribe_layout: true,
        sessions: Some((snapshot.sessions.clone(), snapshot.focused_session)),
        inventory: Some((snapshot.windows.clone(), snapshot.resources.clone())),
        own_client_id: Some(initial_client_id),
        layout_replaced: true,
        ..FrameOutcome::default()
    }
}

/// Size (or create) the pane's slot at the geometry `BOOTSTRAP_BEGIN`
/// advertises; an `AgentSession` (0x0) seeds nothing.
fn seed_bootstrap_geometry<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal_id: ResourceId,
    cols: u16,
    rows: u16,
) -> Result<FrameOutcome, AttachError> {
    if ctx.is_agent_session(&terminal_id) {
        return Ok(FrameOutcome::default());
    }
    let slot = match ctx.panes.entry(terminal_id) {
        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(PaneSlot::new_with_size(cols, rows)?)
        }
    };
    slot.geometry = (cols.max(1), rows.max(1));
    Ok(FrameOutcome::default())
}

/// Record a bootstrap chunk on the span and forward the kernel's PTY writes.
fn record_bootstrap_chunk<W: crate::attach::RenderSink>(
    ctx: &FrameCtx<'_, W>,
    terminal_id: &ResourceId,
    payload_len: usize,
    route: KernelRoute,
) -> FrameOutcome {
    ctx.frame_span
        .record("terminal_id", tracing::field::debug(terminal_id));
    ctx.frame_span.record("bytes", payload_len);
    FrameOutcome {
        pty_writes: route.pty_writes,
        ..FrameOutcome::default()
    }
}

/// Refresh the pane's chrome caches from the replica the bootstrap just
/// published, and report the repaint the barrier release permits.
fn handle_bootstrap_ready<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal_id: &ResourceId,
    route: KernelRoute,
) -> Result<FrameOutcome, AttachError> {
    ctx.frame_span
        .record("terminal_id", tracing::field::debug(terminal_id));
    let terminal = published_terminal(ctx.engine_kernel, terminal_id).ok_or_else(|| {
        AttachError::Protocol(format!("BOOTSTRAP_READY did not publish {terminal_id:?}"))
    })?;
    let slot = ctx
        .panes
        .get_mut(terminal_id)
        .ok_or_else(|| AttachError::Protocol("READY without pane slot".to_owned()))?;
    let title_changed = slot.title_changed(terminal);
    slot.update_sync_output(terminal, tokio::time::Instant::now());
    let damaged = route.damaged(terminal_id);
    Ok(FrameOutcome {
        layout_replaced: damaged,
        authoritative_damage: damaged.then(|| terminal_id.clone()).into_iter().collect(),
        chrome_dirty: damaged && title_changed,
        history_request: route.history_request,
        pty_writes: route.pty_writes,
        ..FrameOutcome::default()
    })
}

/// Whether this frame's applied bytes may reach stdout: not under a modal,
/// not for an earlier frame of a burst, not inside a sync-output block.
const fn paint_permitted(
    overlay_active: bool,
    defer_paint: bool,
    sync_output_active: bool,
) -> bool {
    !overlay_active && !defer_paint && !sync_output_active
}

/// The rect a first-seen pane will paint into (zoom-honoring), else the
/// content rect; sizes its mirror.
fn initial_pane_dims<W: crate::attach::RenderSink>(
    ctx: &FrameCtx<'_, W>,
    terminal_id: &ResourceId,
    content: Rect,
) -> (u16, u16) {
    ctx.workspace
        .render_window(ctx.zoomed.as_ref())
        .and_then(|ls| {
            crate::attach::paint::tiled_rect(ls.as_ref(), content, ctx.viewport_dims, terminal_id)
                .map(|r| (r.w, r.h))
        })
        .unwrap_or((content.w, content.h))
}

/// Fold applied VT bytes into the pane's chrome caches and paint the result.
/// Metadata (title, sync-output state) refreshes even when the paint is
/// withheld; a withheld frame tiles nothing and composes no chrome.
fn handle_terminal_output<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal_id: &ResourceId,
    seq: u64,
    bytes: &[u8],
    route: KernelRoute,
) -> Result<FrameOutcome, AttachError> {
    let damaged = route.damaged(terminal_id);
    let ack = route.ack;
    let pty_writes = route.pty_writes;
    // Live output can retire scrollback; every exit carries the notice.
    let notices = route.notices;
    // The span's CLOSE duration is the per-frame client paint cost.
    ctx.frame_span
        .record("terminal_id", tracing::field::debug(terminal_id));
    ctx.frame_span.record("seq", seq);
    ctx.frame_span.record("bytes", bytes.len());
    crate::attach::render_prof::note_frames(1);
    let walk = published_replica(ctx.engine_kernel, terminal_id).ok_or_else(|| {
        AttachError::Protocol(format!(
            "RESOURCE_OUTPUT targeted unpublished {terminal_id:?}"
        ))
    })?;
    let terminal = walk.terminal;
    let bar = ctx.status_bar.as_ref().map(|p| p.position());
    let content = content_rect(ctx.viewport_dims, bar, ctx.sidebar);
    // Tile only for a never-seen pane; the steady state is one lookup.
    if !ctx.panes.contains_key(terminal_id) {
        let (cols, rows) = initial_pane_dims(ctx, terminal_id, content);
        ctx.panes
            .insert(terminal_id.clone(), PaneSlot::new_with_size(cols, rows)?);
    }
    let Some(slot) = ctx.panes.get_mut(terminal_id) else {
        return Err(AttachError::Protocol(format!(
            "pane slot missing for {terminal_id:?} after seeding"
        )));
    };
    let title_changed = slot.title_changed(terminal);
    let sync_output_active = slot.update_sync_output(terminal, tokio::time::Instant::now());
    if !damaged {
        return Ok(FrameOutcome {
            ack,
            pty_writes,
            notices,
            ..FrameOutcome::default()
        });
    }
    if !paint_permitted(ctx.overlay_active, ctx.defer_paint, sync_output_active) {
        crate::attach::render_prof::note_skipped(1);
        return Ok(FrameOutcome {
            ack,
            authoritative_damage: vec![terminal_id.clone()],
            chrome_dirty: title_changed,
            pty_writes,
            notices,
            ..FrameOutcome::default()
        });
    }
    let status_bar_painted = paint_output_frame(
        OutputFrame {
            out: ctx.out,
            kernel: ctx.engine_kernel,
            panes: ctx.panes,
            workspace: ctx.workspace,
            zoomed: ctx.zoomed.as_ref(),
            focused_resource: ctx.focused_resource.as_ref(),
            status_bar: ctx.status_bar.as_deref_mut(),
            sidebar: ctx.sidebar,
            viewport_dims: ctx.viewport_dims,
            session_name: ctx.session_name.as_str(),
            predict: ctx.predict,
            overlay: ctx.overlay,
        },
        std::slice::from_ref(terminal_id),
    );
    Ok(FrameOutcome {
        ack,
        authoritative_damage: vec![terminal_id.clone()],
        painted_output: (ctx.focused_resource.as_ref() == Some(terminal_id))
            .then(|| terminal_id.clone()),
        chrome_dirty: title_changed,
        pty_writes,
        notices,
        status_bar_painted,
        ..FrameOutcome::default()
    })
}

/// Everything one composited output frame paints from; built by the
/// `RESOURCE_OUTPUT` arm and by the driver's pacer settle.
pub(in crate::attach) struct OutputFrame<'a, W> {
    pub(in crate::attach) out: &'a mut W,
    pub(in crate::attach) kernel: &'a AttachKernel,
    pub(in crate::attach) panes: &'a mut HashMap<ResourceId, PaneSlot>,
    pub(in crate::attach) workspace: &'a Workspace,
    /// The pane zoomed to fill the window, if any.
    pub(in crate::attach) zoomed: Option<&'a ResourceId>,
    pub(in crate::attach) focused_resource: Option<&'a ResourceId>,
    pub(in crate::attach) status_bar: Option<&'a mut StatusBarPainter>,
    pub(in crate::attach) sidebar: Option<SidebarReservation>,
    pub(in crate::attach) viewport_dims: (u16, u16),
    pub(in crate::attach) session_name: &'a str,
    pub(in crate::attach) predict: &'a mut PredictionState,
    pub(in crate::attach) overlay: &'a Overlay,
}

/// Composite and ship ONE frame covering every pane in `targets`: a single
/// DEC 2026 block with each pane interior, the predictive overlay, the bar,
/// and the cursor, then one flush. A paced settle passes several panes so
/// they cannot tear across blocks.
pub(in crate::attach) fn paint_output_frame<W: crate::attach::RenderSink>(
    paint: OutputFrame<'_, W>,
    targets: &[ResourceId],
) -> StatusBarPaint {
    let OutputFrame {
        out,
        kernel,
        panes,
        workspace,
        zoomed,
        focused_resource,
        status_bar,
        sidebar,
        viewport_dims,
        session_name,
        predict,
        overlay,
    } = paint;
    // Tile against the zoom-honoring view; panes off it get no rect.
    let Some(active_ls) = workspace.render_window(zoomed) else {
        return StatusBarPaint::NotPublished;
    };
    let active_ls = active_ls.as_ref();
    let bar = status_bar.as_ref().map(|p| p.position());
    let content = content_rect(viewport_dims, bar, sidebar);
    // Its own span, so traces separate paint cost from `vt_apply`.
    let _paint_trigger = tracing::debug_span!("paint_trigger", rows = viewport_dims.1).entered();
    let mut block = crate::attach::paint::FrameBlock::begin(out);
    for terminal_id in targets {
        let rect = crate::attach::paint::tiled_rect(active_ls, content, viewport_dims, terminal_id);
        let Some(walk) = published_replica(kernel, terminal_id) else {
            continue;
        };
        if focused_resource == Some(terminal_id) {
            paint_focused_interior(
                &mut block,
                rect.unwrap_or(content),
                panes,
                kernel,
                terminal_id,
                walk,
                predict,
                overlay,
            );
        } else if let Some(rect) = rect {
            // Non-focused panes repaint on their own output; no rect, no paint.
            paint_background_interior(&mut block, rect, panes, terminal_id, walk);
        }
    }
    let (painted, shipped) = finish_output_frame(
        block,
        status_bar,
        &FrameTail {
            focused_resource,
            panes,
            active_ls,
            content,
            viewport_dims,
            sidebar,
            session_name,
        },
    );
    if !shipped {
        // The panes recorded cells the terminal may never have
        // received, and their dirty bits are already cleared.
        crate::attach::pane_state::invalidate_all_fronts(panes);
    }
    painted
}

/// The chrome-and-cursor tail of a composited frame.
struct FrameTail<'a> {
    focused_resource: Option<&'a ResourceId>,
    panes: &'a HashMap<ResourceId, PaneSlot>,
    active_ls: &'a LayoutState,
    content: Rect,
    viewport_dims: (u16, u16),
    sidebar: Option<SidebarReservation>,
    session_name: &'a str,
}

/// Close a composited frame: bar, cursor, epilogue, one flush. The second
/// value is whether it shipped.
fn finish_output_frame<W: crate::attach::RenderSink>(
    block: crate::attach::paint::FrameBlock<'_, W>,
    status_bar: Option<&mut StatusBarPainter>,
    tail: &FrameTail<'_>,
) -> (StatusBarPaint, bool) {
    let focused_cursor = tail
        .focused_resource
        .and_then(|fid| tail.panes.get(fid))
        .and_then(|slot| slot.renderer.last_cursor());
    // With no cursor, park at the focused pane's origin, never the bar's tail.
    let fallback_origin = tail
        .focused_resource
        .and_then(|fid| {
            crate::attach::paint::tiled_rect(tail.active_ls, tail.content, tail.viewport_dims, fid)
        })
        .map_or(Some((0, 0)), |r| Some((r.x, r.y)));
    crate::attach::paint::close_frame_reporting(
        block,
        status_bar,
        tail.viewport_dims,
        tail.sidebar,
        tail.session_name,
        focused_cursor,
        fallback_origin,
        // A pane-output frame changes no bar input, so the widget pipeline
        // runs only if a setter already marked the strip dirty.
        crate::render::chrome::status_bar::ComposePolicy::WhenDirty,
    )
}

/// Render the focused pane's interior and reconcile predictions against the
/// cells that just landed.
#[allow(
    clippy::too_many_arguments,
    reason = "the focused-pane paint context: sink, geometry, mirrors, kernel, predictor, overlay; same arg-list refactor follow-up as paint_full_frame"
)]
fn paint_focused_interior<W: crate::attach::RenderSink>(
    out: &mut W,
    rect: Rect,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    kernel: &AttachKernel,
    fid: &ResourceId,
    walk: ReplicaWalk<'_, 'static, 'static>,
    predict: &mut PredictionState,
    overlay: &Overlay,
) {
    let _ = paint_focused_pane(out, rect, panes, kernel, fid, false);
    // Reconcile and overlay run in pane-local coordinates.
    let (focused_cursor_local, pane_origin) = panes.get(fid).map_or((None, (0, 0)), |s| {
        (s.renderer.last_cursor_local(), s.renderer.last_origin())
    });
    // ADR-0090: sync the screen mode first; a screen switch drops predictions
    // anchored to the other screen.
    predict.set_alt_screen(crate::attach::input_dispatch::terminal_in_alt_screen(
        walk.terminal,
    ));
    // Per-cell reconcile of pending predictions (see [`crate::predict`]).
    let now_ms = crate::attach::input_dispatch::predict_now_ms();
    if let Some((row, col)) = focused_cursor_local {
        let _stats = reconcile_terminal_output_per_cell_at(predict, row, col, now_ms, |r, c| {
            panes.get_mut(fid).and_then(|s| {
                // Full grapheme clusters, so multi-codepoint predictions match.
                s.renderer
                    .read_grapheme_string_at(walk, r, c)
                    .ok()
                    .flatten()
            })
        });
    } else {
        // Cursor hidden: cannot anchor, drain wholesale.
        predict.clear();
    }
    // Paint surviving predictions, gated by the ADR-0090 display policy.
    if predict.should_display(now_ms) {
        let _ = overlay.render(predict, pane_origin, out);
        // The guesses now sit over the pane's cells; the front
        // buffer must not keep claiming what was there before them.
        if let Some(slot) = panes.get_mut(fid) {
            crate::attach::pane_state::invalidate_predicted_rows(slot, predict);
        }
    }
}

/// Repaint a non-focused pane on its own output (dirty rows only); the
/// frame tail restores the focused cursor.
fn paint_background_interior<W: crate::attach::RenderSink>(
    out: &mut W,
    rect: Rect,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    terminal_id: &ResourceId,
    walk: ReplicaWalk<'_, 'static, 'static>,
) {
    let Some(slot) = panes.get_mut(terminal_id) else {
        return;
    };
    // Letterbox like `paint_full_frame`, or dirty rows land offset from
    // full-frame rows (doubled text).
    let mirror = crate::attach::paint::mirror_dims(walk.terminal, rect);
    let _ = slot.renderer.render_at_letterboxed(
        walk,
        out,
        (rect.x, rect.y),
        (rect.w, rect.h),
        mirror,
        false,
    );
}

/// `DETACHED` is the one ending a consumer acts on; carry its reason. The
/// message is diagnostic only.
fn handle_detached(reason: Option<DetachReason>, message: &str) -> FrameOutcome {
    tracing::info!(?reason, %message, "DETACHED");
    FrameOutcome {
        exit: true,
        exit_reason: Some(AttachEnd::Detached { reason }),
        ..FrameOutcome::default()
    }
}

/// A `go-to-directory` reply, handed up to the driver to match.
fn directory_listing_outcome(
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
fn handle_metadata_value<W: crate::attach::RenderSink>(
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
fn handle_metadata_changed<W: crate::attach::RenderSink>(
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
            .map_or_else(Workspace::default, Workspace::single);
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
    incoming: Workspace,
) -> Vec<ResourceId> {
    let reconciled =
        reconcile_loaded_workspace(incoming, ctx.workspace, ctx.focused_resource.as_ref());
    *ctx.workspace = reconciled;
    let attach_panes = unknown_layout_leaves(ctx.workspace, ctx.panes);
    *ctx.focused_resource = ctx
        .workspace
        .active_window()
        .and_then(|ls| ls.focus.clone());
    attach_panes
}

/// Split-pane reply: apply the parked split on Ok, bell on Err.
fn handle_terminal_spawned<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    request_id: u32,
    result: SpawnResult,
) -> Result<FrameOutcome, AttachError> {
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
        "could not split onto satellite {satellite}: {reason}"
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
fn apply_split_spawned<W: crate::attach::RenderSink>(
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
    let window = &mut ctx.workspace.windows[index].state;
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
    ctx.focused_resource.clone_from(
        &ctx.workspace
            .active_window()
            .and_then(|ls| ls.focus.clone()),
    );
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
fn window_holding_pane(workspace: &Workspace, pane: &ResourceId) -> Option<usize> {
    workspace.windows.iter().position(|window| {
        window
            .state
            .tree
            .as_ref()
            .is_some_and(|tree| layout::leaves(tree).contains(pane))
    })
}

/// A Terminal closed: fold its leaf out and drop its slot.
fn handle_terminal_closed<W: crate::attach::RenderSink>(
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
    // Find the window holding this leaf (panes can live in any
    // window, not just the active one) and fold it out there.
    let Some(idx) = window_holding_pane(ctx.workspace, terminal_id) else {
        return FrameOutcome::default();
    };
    let new_state = match apply_terminal_closed(&ctx.workspace.windows[idx].state, terminal_id) {
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
    ctx.workspace.windows[idx].state = new_state;
    // The fold may have emptied the window; drop any such
    // windows and keep `active` valid.
    ctx.workspace.prune_empty_windows();
    // Consumer-owned detach policy: nothing left to render means detach,
    // unless the session is keep-empty (ADR-0105).
    if ctx.workspace.windows.is_empty() && *ctx.keep_empty_session {
        return last_pane_closed_keep_empty(ctx, notices);
    }
    if ctx.workspace.windows.is_empty() {
        tracing::info!("ResourceClosed folded the last pane; detaching");
        return FrameOutcome {
            exit: true,
            // Carry the status so the CLI can explain the exit.
            exit_reason: Some(AttachEnd::LastPaneClosed { exit_status }),
            ..FrameOutcome::default()
        };
    }
    // Re-anchor focus onto the (possibly new) active window.
    *ctx.focused_resource = ctx
        .workspace
        .active_window()
        .and_then(|ls| ls.focus.clone());
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
fn resolve_resource_op<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    request_id: u32,
    code: Option<ErrorCode>,
) -> Option<FrameOutcome> {
    let terminal_id = ctx.pending_resource_ops.remove(&request_id)?;
    (code == Some(ErrorCode::TerminalNotFound)).then(|| fold_missing_resource(ctx, &terminal_id))
}

/// ADR-0105: a keep-empty session's last pane closed: stay attached, paint
/// the empty state, and tombstone the dead layout.
fn last_pane_closed_keep_empty<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    notices: Vec<Notice>,
) -> FrameOutcome {
    tracing::info!("ResourceClosed folded the last pane of a keep-empty session; staying attached");
    *ctx.focused_resource = None;
    *ctx.zoomed = None;
    FrameOutcome {
        layout_replaced: true,
        clear_layout: true,
        notices,
        ..FrameOutcome::default()
    }
}

/// ADR-0105: a `phux.session.keep_empty/v1` broadcast. Only a mark on this
/// client's own session changes what its last pane's close does.
fn apply_keep_empty_broadcast<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    value: Option<&[u8]>,
) -> FrameOutcome {
    if let Some((name, keep)) = value.and_then(decode_session_keep_empty)
        && name == ctx.session_name.as_str()
    {
        *ctx.keep_empty_session = keep;
    }
    FrameOutcome::default()
}

/// A `phux.session.name/v1` broadcast: rename our status name when it names
/// us; the driver folds the pair into the peer graph.
fn apply_session_rename_broadcast<W: crate::attach::RenderSink>(
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

/// Dispatch one agent event; most do not affect this client and must not
/// tear down the attach.
fn handle_agent_event<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    frame: FrameKind,
    route: &KernelRoute,
) -> FrameOutcome {
    match frame {
        // A live-spawned `AgentSession` under one of our panes: attach its
        // record stream.
        FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::ResourceSpawned { .. },
            ..
        } if route.declared_agent.as_ref() == Some(&terminal) => FrameOutcome {
            attach_panes: vec![terminal],
            chrome_dirty: true,
            ..FrameOutcome::default()
        },
        FrameKind::Event {
            terminal: Some(terminal),
            event:
                AgentEvent::TerminalControl {
                    lifecycle,
                    input_holder,
                    exit_status,
                    ..
                },
            ..
        } => fold_terminal_control(ctx, &terminal, lifecycle, input_holder, exit_status),
        FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::Asked { .. },
            ..
        } => fold_agent_ask(ctx, terminal),
        FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::CwdChanged { cwd },
            ..
        } => fold_cwd_changed(ctx, &terminal, cwd),
        FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::CommandFinished { exit_code },
            ..
        } => fold_command_finished(ctx, &terminal, exit_code),
        // Another session's pane set changed (server-wide event subscription).
        FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::ResourceSpawned { .. } | AgentEvent::ResourceClosed { .. },
            ..
        } if !ctx.panes.contains_key(&terminal) && !ctx.is_agent_session(&terminal) => {
            FrameOutcome {
                foreign_pane_set_dirty: true,
                ..FrameOutcome::default()
            }
        }
        _ => FrameOutcome::default(),
    }
}

/// ADR-0033: fold a `TerminalControl` broadcast into the slot for the badge.
/// A holder transition on the focused pane raises a notice; the first event
/// a slot sees is the attach-time state and stays silent.
fn fold_terminal_control<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal: &ResourceId,
    lifecycle: ResourceLifecycle,
    input_holder: Option<ClientId>,
    exit_status: Option<i32>,
) -> FrameOutcome {
    let Some(slot) = ctx.panes.get_mut(terminal) else {
        // Can precede the first snapshot; the next event re-states the lease.
        return FrameOutcome::default();
    };
    let initial_state = !slot.control_seen;
    slot.control_seen = true;
    let holder_changed = slot.input_holder != input_holder;
    slot.lifecycle = lifecycle;
    slot.input_holder = input_holder;
    // ADR-0124: the first `Exited` carries the status.
    if matches!(lifecycle, ResourceLifecycle::Exited) && slot.exited.is_none() {
        slot.exited = Some(crate::attach::pane_state::ExitMark {
            status: exit_status,
            signal: None,
        });
    }
    let announce =
        holder_changed && !initial_state && ctx.focused_resource.as_ref() == Some(terminal);
    let notices = if announce {
        vec![Notice::info(input_authority_notice(input_holder))]
    } else {
        Vec::new()
    };
    FrameOutcome {
        chrome_dirty: true,
        notices,
        ..FrameOutcome::default()
    }
}

/// ADR-0035: an agent waits on a human. Raise the pane's attention flag
/// (cleared by input to the pane); a repeat requests no repaint.
fn fold_agent_ask<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal: ResourceId,
) -> FrameOutcome {
    let local = window_holding_pane(ctx.workspace, &terminal).is_some();
    let Some(slot) = ctx.panes.get_mut(&terminal) else {
        return FrameOutcome {
            foreign_attention: Some(terminal),
            ..FrameOutcome::default()
        };
    };
    // Asks are coalesced server-side; keep early local asks until the layout
    // arrives and forward peers to the foreign cache.
    let changed = !slot.attention;
    slot.attention = true;
    FrameOutcome {
        chrome_dirty: local && changed,
        foreign_attention: (!local).then_some(terminal),
        ..FrameOutcome::default()
    }
}

/// The shell changed directory; dirty chrome only when it moved.
fn fold_cwd_changed<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal: &ResourceId,
    cwd: String,
) -> FrameOutcome {
    match ctx.panes.get_mut(terminal) {
        Some(slot) if slot.cwd.as_deref() != Some(cwd.as_str()) => {
            slot.cwd = Some(cwd);
            FrameOutcome {
                chrome_dirty: true,
                ..FrameOutcome::default()
            }
        }
        // Unchanged value, or a pane we have no slot for yet — the
        // next cwd_changed (or the ATTACHED seed) covers it.
        _ => FrameOutcome::default(),
    }
}

/// A command finished; record its OSC-133 exit code (`None` included).
fn fold_command_finished<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal: &ResourceId,
    exit_code: Option<i32>,
) -> FrameOutcome {
    match ctx.panes.get_mut(terminal) {
        Some(slot) if slot.last_exit != exit_code => {
            slot.last_exit = exit_code;
            FrameOutcome {
                chrome_dirty: true,
                ..FrameOutcome::default()
            }
        }
        _ => FrameOutcome::default(),
    }
}

/// ERROR never terminates the attach (SPEC §9: `DETACHED` plus transport
/// close does); the same code is fatal and non-fatal on the same server. An
/// uncorrelated error names its code in a notice.
fn handle_error_frame(request_id: Option<u32>, code: ErrorCode, message: &str) -> FrameOutcome {
    // A raced correlated error is inert.
    if request_id.is_some() {
        return FrameOutcome::default();
    }
    // Uncorrelated `SATELLITE_UNREACHABLE` is a degraded-federation
    // transition with its own wording.
    if code == ErrorCode::SatelliteUnreachable {
        tracing::warn!(message = %message, "federation degraded (satellite unreachable)");
        return FrameOutcome {
            notices: vec![Notice::warn(format!("federation degraded: {message}"))],
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
        notices: vec![Notice::warn(format!("server error ({code:?}): {message}"))],
        ..FrameOutcome::default()
    }
}

/// The `ERROR` arm: a correlated refusal of a parked satellite attach
/// decides it; anything else is the ordinary error notice.
fn error_frame_outcome<W: crate::attach::RenderSink>(
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
fn command_result_outcome<W: crate::attach::RenderSink>(
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
            format!(
                "could not open {} on satellite {host}: {reason}",
                window.name
            )
        }
        ParkedAdopt::Split(_) => format!("could not split onto satellite {host}: {reason}"),
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
fn unreferenced<W: crate::attach::RenderSink>(
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
    workspace: &Workspace,
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
fn open_window(
    workspace: &mut Workspace,
    focused_resource: &mut Option<ResourceId>,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    name: &str,
    pane: ResourceId,
) -> Result<FrameOutcome, AttachError> {
    workspace.add_window(name.to_owned(), pane.clone());
    if let std::collections::hash_map::Entry::Vacant(slot) = panes.entry(pane) {
        slot.insert(PaneSlot::new()?);
    }
    *focused_resource = workspace.active_window().and_then(|ls| ls.focus.clone());
    Ok(FrameOutcome {
        layout_replaced: true,
        emit_set_metadata: true,
        reflow_panes: true,
        ..FrameOutcome::default()
    })
}

/// Apply a `RESOURCE_SPAWNED` reply for a parked `new-window`: append an
/// active window on the new pane, seed its slot, focus it, and request
/// repaint, broadcast, and reflow.
pub(super) fn handle_window_spawned<W: crate::attach::RenderSink>(
    out: &mut W,
    workspace: &mut Workspace,
    focused_resource: &mut Option<ResourceId>,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    pending: &PendingWindow,
    result: SpawnResult,
) -> Result<FrameOutcome, AttachError> {
    let instance = result.instance();
    match result {
        // A satellite spawn streams to no one until attached: park the window
        // on that attach, token included.
        SpawnResult::Ok(new_id) | SpawnResult::OkBound { id: new_id, .. } if !new_id.is_local() => {
            Ok(FrameOutcome {
                adopt_spawned: vec![ParkedAdopt::Window(PendingWindow {
                    name: pending.name.clone(),
                    adopt: Some(Adopt::Spawned(SpawnedPane {
                        id: new_id,
                        instance,
                    })),
                })],
                ..FrameOutcome::default()
            })
        }
        SpawnResult::Ok(new_id) | SpawnResult::OkBound { id: new_id, .. } => {
            open_window(workspace, focused_resource, panes, &pending.name, new_id)
        }
        SpawnResult::Err(err) => {
            tracing::warn!(error = ?err, "new-window: server-side spawn failed");
            let _ = actions::write_bell(out);
            Ok(FrameOutcome::default())
        }
        // SpawnResult is #[non_exhaustive] — tolerate future variants.
        _ => {
            tracing::warn!("new-window: unknown SpawnResult variant");
            Ok(FrameOutcome::default())
        }
    }
}

/// The focused session's display name from an `ATTACHED` snapshot, or empty.
pub(super) fn focused_session_name(
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
) -> String {
    snapshot
        .sessions
        .iter()
        .find(|s| s.id == snapshot.focused_session)
        .map(|s| s.name.clone())
        .unwrap_or_default()
}

/// Which session an ADR-0019 layout key (`phux.tui.layout/v1[/<session>]`,
/// default Group) names; `None` ⇒ not a layout key we can attribute. The
/// caller must know whose layout arrived before adopting it.
pub(super) fn layout_key_scope_session(scope: &Scope, key: &str) -> Option<LayoutKeyOwner> {
    if !matches!(scope, Scope::Group(id) if *id == DEFAULT_GROUP_ID) {
        return None;
    }
    layout_key_session(key)
}

/// Whether a persisted layout names panes and none is in the `ATTACHED`
/// snapshot (ADR-0105): a stale tree of dead panes that would hide the empty
/// state.
fn layout_names_only_absent_panes(
    incoming: &Workspace,
    panes: &HashMap<ResourceId, PaneSlot>,
) -> bool {
    let mut leaves = incoming
        .windows
        .iter()
        .filter_map(|window| window.state.tree.as_ref())
        .flat_map(crate::layout::leaves)
        .peekable();
    leaves.peek().is_some() && leaves.all(|leaf| !panes.contains_key(&leaf))
}

pub(super) fn unknown_layout_leaves(
    incoming: &Workspace,
    panes: &HashMap<ResourceId, PaneSlot>,
) -> Vec<ResourceId> {
    incoming
        .windows
        .iter()
        .filter_map(|window| window.state.tree.as_ref())
        .flat_map(crate::layout::leaves)
        .filter(|terminal| !panes.contains_key(terminal))
        .collect()
}

/// Reconcile a workspace whose session ownership was established by its key or
/// correlated GET. Terminal overlap and replica admission are not authority.
pub(super) fn reconcile_loaded_workspace(
    mut incoming: Workspace,
    local: &Workspace,
    bootstrap_focus: Option<&ResourceId>,
) -> Workspace {
    for window in &mut incoming.windows {
        let local_focus = local
            .windows
            .iter()
            .find(|old| old.id == window.id)
            .and_then(|local_window| local_window.state.focus.as_ref())
            .or(bootstrap_focus);
        reconcile_loaded_layout(&mut window.state, local_focus);
    }
    incoming.active = reconciled_active_window(&incoming, local, bootstrap_focus);
    incoming
}

fn reconciled_active_window(
    incoming: &Workspace,
    local: &Workspace,
    bootstrap_focus: Option<&ResourceId>,
) -> usize {
    let active_id = local.windows.get(local.active).map(|window| window.id);
    incoming
        .windows
        .iter()
        .position(|window| Some(window.id) == active_id)
        .or_else(|| window_containing_focus(incoming, bootstrap_focus))
        .unwrap_or_else(|| local.active.min(incoming.windows.len().saturating_sub(1)))
}

fn window_containing_focus(workspace: &Workspace, focus: Option<&ResourceId>) -> Option<usize> {
    let focus = focus?;
    workspace.windows.iter().position(|window| {
        window
            .state
            .tree
            .as_ref()
            .is_some_and(|tree| crate::layout::leaves(tree).contains(focus))
    })
}

/// Preserve a valid local focus while adopting `state`'s tree; otherwise the
/// first depth-first leaf (ADR-0019).
pub(super) fn reconcile_loaded_layout(state: &mut LayoutState, local_focus: Option<&ResourceId>) {
    let tree_leaves = state
        .tree
        .as_ref()
        .map(crate::layout::leaves)
        .unwrap_or_default();
    state.focus = local_focus
        .filter(|focus| tree_leaves.contains(focus))
        .cloned()
        .or_else(|| tree_leaves.into_iter().next());
}

#[cfg(test)]
mod session_name_tests {
    use super::focused_session_name;
    use phux_protocol::ids::{ResourceId, SessionId, WindowId};
    use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};

    fn snapshot_with(sessions: Vec<SessionInfo>, focused: SessionId) -> SessionSnapshot {
        SessionSnapshot::new(focused, WindowId::new(0), ResourceId::local(0))
            .with_sessions(sessions)
    }

    #[test]
    fn focused_session_name_resolves_the_matching_session() {
        // The widget reads the name of the focused session,
        // not the first session in the list.
        let snapshot = snapshot_with(
            vec![
                SessionInfo::new(SessionId::new(1), "work"),
                SessionInfo::new(SessionId::new(2), "play"),
            ],
            SessionId::new(2),
        );
        assert_eq!(focused_session_name(&snapshot), "play");
    }

    #[test]
    fn focused_session_name_is_empty_when_focus_is_absent() {
        // Degrade to an empty widget rather than panic if the focused
        // session somehow isn't in the list.
        let snapshot = snapshot_with(
            vec![SessionInfo::new(SessionId::new(1), "work")],
            SessionId::new(99),
        );
        assert_eq!(focused_session_name(&snapshot), "");
    }
}
