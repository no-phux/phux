//! `handle_server_frame`: the per-frame dispatcher, plus the layout
//! reconciliation helpers its metadata arms use.

use std::collections::{HashMap, HashSet};

use phux_client_core::session::EffectBuffer as KernelEffectBuffer;
use phux_protocol::ResourceKind;
use phux_protocol::ids::{ClientId, ResourceId, SessionId};
use phux_protocol::wire::frame::{
    AgentEvent, CONFIG_RELOAD_KEY, CloseReason, DetachReason, ErrorCode, FrameKind,
    ResourceLifecycle, SESSION_KEEP_EMPTY_KEY, Scope, SpawnError, SpawnResult,
    decode_session_keep_empty,
};

use crate::attach::actions::{
    self, PendingSplit, PendingWindow, apply_spawned_ok, apply_terminal_closed,
};
use crate::attach::outcome::{AttachEnd, AttachError, describe_exit};
use crate::attach::paint::{SidebarReservation, StatusBarPaint, content_rect, paint_focused_pane};
use crate::attach::pane_state::{
    AttachKernel, PaneSlot, published_replica, published_terminal, reanchor_predict_to_pane,
};
use crate::attach::render::ReplicaWalk;
use crate::layout::{self, LayoutState, Rect, Workspace};
use crate::predict::{Overlay, PredictionState, reconcile_terminal_output_per_cell};
use crate::render::chrome::status_bar::{Notice, StatusBarPainter};
use phux_client::agent_meta::RESOURCE_AGENT_KEY;
use phux_client::layout_ops::{
    DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, LayoutKeyOwner, layout_key_session,
};

use super::engine_route::{KernelRoute, is_terminal, route_engine_frame};
use super::index::{AgentMetaIndex, note_agent_change};
use super::outcome::{FrameOutcome, frame_kind_label, input_authority_notice, pane_label};

/// The driver state one inbound frame is dispatched against.
///
/// Every field is threaded verbatim from [`handle_server_frame`]'s own
/// parameters — this is the client's per-frame state, gathered once so the
/// per-frame-kind handlers below take a context instead of twenty loose
/// arguments. The entry point keeps the flat parameter list because that is
/// the driver boundary (`driver::main_loop` / `driver::headless` own these
/// pieces separately and hand them over per frame).
struct FrameCtx<'a, W: crate::attach::RenderSink> {
    /// The published-replica side of the session kernel. Read-only here: the
    /// kernel's own mutation already happened in [`route_engine_frame`],
    /// before this context exists.
    engine_kernel: &'a AttachKernel,
    out: &'a mut W,
    panes: &'a mut HashMap<ResourceId, PaneSlot>,
    workspace: &'a mut Workspace,
    focused_resource: &'a mut Option<ResourceId>,
    // phux-x2hm: the driver's pane-zoom state. RENDER/REFLOW geometry reads go
    // through `Workspace::render_window(zoomed)` so a zoomed pane paints to the
    // full window and non-zoomed panes (absent from the synthetic single-leaf
    // layout) correctly do not paint. A `ResourceSpawned`-ok split clears this
    // (`*zoomed = None`) so a new pane un-zooms, matching tmux. Mutation/input
    // reads (focus reconcile) keep using the REAL `active_window`.
    zoomed: &'a mut Option<ResourceId>,
    session_name: &'a mut String,
    /// ADR-0105: whether the attached session is keep-empty. Set from
    /// ATTACHED and from `phux.session.keep_empty/v1` broadcasts; read when
    /// the last pane closes, to show the empty state instead of detaching.
    keep_empty_session: &'a mut bool,
    // phux-k0cw: this client's own session, so the layout arm can tell OUR
    // layout broadcast from a peer's. No layout is attributed locally until
    // ATTACHED resolves the session.
    focused_session: Option<SessionId>,
    /// `Option` so an attach with no configured widgets pays nothing for the
    /// chrome path.
    status_bar: Option<&'a mut StatusBarPainter>,
    // phux-4h5a: the window-sidebar reservation, threaded identically to
    // `status_bar` so every layout site in this dispatcher tiles panes into
    // the SAME inset content rect the driver paints + reflows against. `None`
    // (sidebar disabled, the default) makes `content_rect` the full pane
    // viewport, so the whole dispatcher stays byte-identical to the
    // pre-sidebar path.
    sidebar: Option<SidebarReservation>,
    /// `(cols, rows)` of the outer terminal — used by the painter to pick the
    /// bottom row.
    viewport_dims: (u16, u16),
    predict: &'a mut PredictionState,
    overlay: &'a Overlay,
    pending_layout_request: Option<u32>,
    pending_splits: &'a mut HashMap<u32, PendingSplit>,
    pending_windows: &'a mut HashMap<u32, PendingWindow>,
    // phux-i0e8.2.2: Terminals whose close THIS client asked for
    // (kill-pane / kill-window soft-kill dispatch). The `ResourceClosed`
    // arm drains the marker and suppresses the pane-exit notice for an
    // expected close — the user killed it; telling them it died is noise.
    expected_closes: &'a mut HashSet<ResourceId>,
    // ADR-0040: the driver-held `phux.agent/v1` index. The MetadataValue /
    // MetadataChanged arms decode agent records into it; the driver reads
    // it when composing window labels.
    agent_meta: &'a mut AgentMetaIndex,
    // phux-5ke.4: when `true` an overlay is on top; pane libghostty
    // mirrors keep ingesting `vt_write` (per ADR-0013) but stdout
    // flushes (render_at, bar paint, predict-overlay paint) are
    // suppressed so the modal doesn't get scribbled over. The driver
    // triggers a full repaint on overlay dismiss.
    overlay_active: bool,
    // phux-jhv8: when `true` this frame is an earlier member of a coalesced
    // burst — a later frame in the same drain targets this pane, so its
    // libghostty mirror still ingests `vt_write` (state stays correct) but the
    // stdout paint (render_at, bar, predict-overlay, reconcile) is suppressed.
    // The driver passes `defer_paint = false` for each pane's LAST frame in the
    // burst, so every touched pane settles exactly once instead of repainting
    // on every intermediate redraw. Same vt_write-but-no-paint contract as
    // `overlay_active`, minus the modal semantics.
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

/// Process one server-to-client frame. Returns a [`FrameOutcome`]
/// describing any follow-up the async driver needs to perform.
///
/// `status_bar` is `Option<&mut StatusBarPainter>` so an attach with no
/// configured widgets pays nothing for the chrome path. `viewport_dims`
/// is `(cols, rows)` of the outer terminal — used by the painter to
/// pick the bottom row.
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
    // Per-inbound-frame dispatch span (debug; off under the default
    // `phux=info` filter). Content-frame CLOSE duration is client apply+paint
    // cost, while identifiers and payload sizes are recorded in their arms.
    // below. Declared `Empty` so they exist for later `record`.
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
        agent_meta,
        overlay_active,
        defer_paint,
        frame_span: &frame_span,
    };
    dispatch_frame(&mut ctx, frame, kernel_route)
}

/// The verdicts the session kernel's own routing reaches before any frame arm
/// runs: a rejected frame is a protocol error, a resync request and an
/// ignored (retired-generation) frame each end the dispatch on their own.
///
/// `None` ⇒ the kernel accepted the frame; dispatch it.
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

/// Route one accepted frame to its arm.
///
/// The arm order is the cohesive ordered protocol dispatch: keeping
/// tombstones and request errors in their semantic groups preserves routing
/// precedence, so arms are appended within their group rather than at the end.
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
        FrameKind::AttachReady { .. } => Ok(FrameOutcome {
            layout_replaced: !route.damaged.is_empty(),
            ..FrameOutcome::default()
        }),
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
            // Forward bell to the outer terminal. The user's terminal
            // emulator decides whether to render visually, audibly, or
            // not at all. Routed through the injected sink so a headless
            // capture sees the BEL too (an agent can observe `\x07`).
            let _ = actions::write_bell(ctx.out);
            Ok(FrameOutcome::default())
        }
        FrameKind::MetadataValue { request_id, value } => {
            handle_metadata_value(ctx, request_id, value)
        }
        FrameKind::MetadataChanged { scope, key, value } => {
            handle_metadata_changed(ctx, &scope, &key, value)
        }
        FrameKind::DirectoryListing { request_id, result } => {
            Ok(directory_listing_outcome(request_id, result))
        }
        FrameKind::ResourceSpawned { request_id, result } => {
            handle_terminal_spawned(ctx, request_id, result)
        }
        FrameKind::ResourceClosed {
            terminal_id,
            exit_status,
            reason,
        } => Ok(handle_terminal_closed(
            ctx,
            &terminal_id,
            exit_status,
            reason,
        )),
        event @ FrameKind::Event { .. } => Ok(handle_agent_event(ctx, event, &route)),
        FrameKind::Error {
            request_id,
            code,
            message,
        } => error_frame_outcome(ctx, request_id, code, message),
        // A request-correlated reply that reached the dispatcher instead of
        // its awaiter. Inert, never terminal — the same rule the `ERROR` arm
        // above states for the failure twin of these frames, and the same
        // "no matching pending request" drop the `MetadataValue` arm makes.
        //
        // This is conformance, not policy. `docs/spec/L1.md` §5 is explicit:
        // "A `COMMAND` is asynchronous: the server MAY emit other messages
        // (including events relevant to the command's effect) before
        // `COMMAND_RESULT`. Clients MUST tolerate that ordering." A client
        // that tears itself down on an interleaved `COMMAND_RESULT` does not
        // tolerate it, so the arm below is what the spec already required.
        //
        // How one gets here: `Connection::await_answer` loops until the reply
        // carrying ITS `request_id` arrives and pushes every other frame onto
        // `interleaved`, which is replayed through this dispatcher. So a
        // reply whose awaiter has already been answered — or has gone away —
        // is delivered here as ordinary interleaved traffic. It is
        // direction-valid server output, correlated by construction
        // (`request_id` is a `u32`, not an `Option`), and carries no state
        // this client has not already applied.
        //
        // Before this arm existed, both frames fell to the catch-all below
        // and killed a healthy attach with a protocol error. That is the
        // regression `spatial_e2e` caught: `C-a o` while the layout was being
        // driven concurrently produced `CommandResult { request_id: 4,
        // result: Ok }` on the dispatcher path, and the client tore itself
        // down over a SUCCESS reply it simply had nowhere to put.
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

/// `ATTACHED` per SPEC §13 carries the session/window/pane graph; the
/// per-pane initial cells arrive separately through each bootstrap
/// transcript.
///
/// phux-4li.5: the returned outcome signals the driver to emit `GET_METADATA`
/// and `SUBSCRIBE_METADATA` for the layout key so we (a) reconcile against a
/// persisted layout from a previous session and (b) receive `METADATA_CHANGED`
/// broadcasts from sibling clients (ADR-0019 decision 2).
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
    // phux-4li.4: seed the workspace with a single window holding
    // one leaf so the existing single-pane render path keeps
    // working. The L3 metadata-fetch path replaces this with the
    // server-stored layout (possibly multi-window) when present.
    *ctx.workspace = Workspace::single(bootstrap.clone());
    // Seed client-side mirrors at their server-advertised sizes
    // before any RESOURCE_OUTPUT can race ahead of the per-pane
    // bootstrap transcript. VT interpretation is geometry-sensitive;
    // starting at 80x24 and resizing later corrupts wraps, clips,
    // and absolute cursor movement for wider/taller viewports.
    //
    // Only Terminal-kind resources get a slot: an AgentSession has no grid
    // (its snapshot entry says 0x0), and a mirror sized from it would be a
    // lie the first paint trips over. Its record stream lives in the kernel.
    for pane in snapshot.resources.iter().filter(|pane| is_terminal(pane)) {
        if let std::collections::hash_map::Entry::Vacant(v) = ctx.panes.entry(pane.id.clone()) {
            let slot = v.insert(PaneSlot::new_with_size(pane.cols, pane.rows)?);
            // phux-foz.4: seed the pane's cwd from the snapshot (the
            // spawn cwd); `cwd_changed` events refine it live.
            slot.cwd.clone_from(&pane.cwd);
        }
    }
    // phux-p4vp: hand the per-pane cwds up to the driver so the
    // sidebar can derive each window's VCS branch client-side.
    let pane_cwds: Vec<(ResourceId, String)> = snapshot
        .resources
        .iter()
        .filter(|pane| is_terminal(pane))
        .filter_map(|p| p.cwd.clone().map(|cwd| (p.id.clone(), cwd)))
        .collect();
    // Ensure the focused pane has a slot even if an older server's
    // ATTACHED graph omitted it. Fall back to the current pane
    // viewport (the same dimensions used for rendering) rather
    // than the historical 80x24 placeholder.
    if let std::collections::hash_map::Entry::Vacant(v) = ctx.panes.entry(bootstrap) {
        let content = content_rect(
            ctx.viewport_dims,
            ctx.status_bar.as_ref().map(|p| p.position()),
            ctx.sidebar,
        );
        v.insert(PaneSlot::new_with_size(content.w, content.h)?);
    }
    // phux-17u: stash the session name for the status-bar
    // `WidgetContext`. The snapshot carries `sessions:
    // Vec<SessionInfo>` plus `focused_session`; the name is the
    // `SessionInfo` whose `id` matches the focused session. The
    // server populates this from `Session::name` in
    // `build_session_snapshot`. Falls back to empty if the
    // focused session somehow isn't in the list (shouldn't
    // happen — the focused session is always one of them).
    *ctx.session_name = focused_session_name(snapshot);
    // phux-4li.20: hand the driver the full session graph so the
    // `<leader> a` session picker can list peer sessions. The
    // snapshot is the authoritative session list at attach time;
    // a dedicated request/response frame would be redundant.
    let session_cache = (snapshot.sessions.clone(), snapshot.focused_session);
    Ok(FrameOutcome {
        subscribe_layout: true,
        sessions: Some(session_cache),
        // ADR-0033: cache our own ClientId so the supervisory badge can
        // distinguish "you hold the wheel" from another client.
        own_client_id: Some(initial_client_id),
        pane_cwds,
        ..FrameOutcome::default()
    })
}

/// Whether an `ATTACHED` snapshot's focused session holds no windows
/// (ADR-0105): it reports zero windows AND its focus is the `0` sentinel
/// rather than a listed resource. Requiring both keeps a snapshot that merely
/// left `window_count` unset from reading as empty.
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

/// ADR-0105: ATTACHED to a session with no windows. There is no pane to
/// focus or to seed a mirror for, so the workspace starts empty and the
/// driver paints the empty state; the session graph, client id, and layout
/// subscription are recorded exactly as for a populated attach.
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
        own_client_id: Some(initial_client_id),
        layout_replaced: true,
        ..FrameOutcome::default()
    }
}

/// Point the pane's slot at the geometry `BOOTSTRAP_BEGIN` advertises,
/// creating the slot at that size when this is the pane's first sight.
///
/// An `AgentSession` stream has no grid to size a slot from (its BEGIN says
/// `0x0`), so it seeds nothing.
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

/// Correlate one bootstrap-transcript chunk onto the dispatch span and
/// forward the PTY writes the kernel's apply produced. The chunk's bytes
/// themselves are already inside the kernel's staging replica.
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
        chrome_dirty: damaged && title_changed,
        history_request: route.history_request,
        pty_writes: route.pty_writes,
        ..FrameOutcome::default()
    })
}

/// Whether this frame's applied bytes may reach stdout.
///
/// phux-5ke.4 (a modal overlay is on top), phux-jhv8 (an earlier frame of a
/// coalesced burst) and an open synchronized-output block each keep the
/// libghostty mirror ingesting while suppressing the paint.
const fn paint_permitted(
    overlay_active: bool,
    defer_paint: bool,
    sync_output_active: bool,
) -> bool {
    !overlay_active && !defer_paint && !sync_output_active
}

/// The rect this pane will paint into, used to size a mirror on first sight.
///
/// phux-x2hm: read through the zoom-honoring view, so a zoomed pane is sized
/// against the whole window. Falls back to the content rect when the pane has
/// no tile (single-pane bootstrap, or a pane in a non-active window).
///
/// Reached ONLY the first time a pane is seen. Its caller used to run this on
/// every output frame — before the damage check and before the coalescing
/// gate — to size a mirror that, on all but the first frame, already existed.
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
///
/// The kernel already applied these bytes to the published libghostty
/// terminal, including for an off-screen pane. Pane metadata is refreshed
/// from that authoritative terminal before deciding whether the aggregate
/// attach barrier permits paint damage: a pre-barrier OSC title must update
/// chrome caches even though its visible repaint remains suppressed until
/// `ATTACH_READY`.
///
/// Everything a suppressed frame does NOT do is as load-bearing as what a
/// painted one does. A frame whose paint is withheld — by the coalescing
/// mask, a modal overlay, an open synchronized-output transaction, or the
/// driver's frame pacer — applies its bytes (that already happened in the
/// kernel), refreshes the pane's title and sync-output bookkeeping, and
/// returns. It tiles no layout, allocates no mirror, and composes no chrome.
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
    // phux-ijuj: live output can retire this pane's scrollback
    // (a pruned or codec-failed anchor). Every exit from this arm
    // carries the resulting notice; the branches are exclusive, so
    // only one of them moves it.
    let notices = route.notices;
    // Correlate this apply: which pane, which seq, how many bytes.
    // The span's CLOSE duration is the per-frame client paint cost
    // (vt_write + render_at for the focused pane) — the headline
    // client lag signal a trace reader greps `handle_server_frame`
    // with `kind=terminal_output` for.
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
    // Seed a mirror only for a pane we have never seen. The tiling that sizes
    // it is computed inside this branch, so the steady-state frame pays one
    // hash lookup instead of a full `compute_layout_in`.
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
        chrome_dirty: title_changed,
        pty_writes,
        notices,
        status_bar_painted,
        ..FrameOutcome::default()
    })
}

/// Everything one composited output frame paints from.
///
/// Gathered as a struct rather than a positional list because there are two
/// callers with unrelated shapes: this module's `RESOURCE_OUTPUT` arm, which
/// reborrows disjoint [`FrameCtx`] fields, and the driver's frame pacer,
/// which builds it from [`crate::attach::driver`]-owned state when a withheld
/// paint's deadline expires.
pub(in crate::attach) struct OutputFrame<'a, W> {
    pub(in crate::attach) out: &'a mut W,
    pub(in crate::attach) kernel: &'a AttachKernel,
    pub(in crate::attach) panes: &'a mut HashMap<ResourceId, PaneSlot>,
    pub(in crate::attach) workspace: &'a Workspace,
    /// phux-x2hm: the pane zoomed to fill the window, if any.
    pub(in crate::attach) zoomed: Option<&'a ResourceId>,
    pub(in crate::attach) focused_resource: Option<&'a ResourceId>,
    pub(in crate::attach) status_bar: Option<&'a mut StatusBarPainter>,
    pub(in crate::attach) sidebar: Option<SidebarReservation>,
    pub(in crate::attach) viewport_dims: (u16, u16),
    pub(in crate::attach) session_name: &'a str,
    pub(in crate::attach) predict: &'a mut PredictionState,
    pub(in crate::attach) overlay: &'a Overlay,
}

/// Composite and ship ONE frame covering every pane in `targets`.
///
/// The frame is a single DEC 2026 synchronized-output block containing, in
/// order: each target pane's interior, the predictive-echo overlay, the status
/// bar, and the end-of-frame cursor — then one flush. Before this, the
/// incremental path emitted the pane and the bar as separate visible states
/// with a flush apiece, so a terminal could present a frame with new cells and
/// last frame's chrome, and the off-loop stdout writer was woken twice per
/// frame.
///
/// `targets` is a slice rather than one id because a paced frame settles every
/// pane whose paint was withheld during the window, and they must land in the
/// same block: two panes settling in two blocks is the tearing this function
/// exists to remove, just at a coarser grain.
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
    // The libghostty mirrors are warm even for panes in a non-active window
    // (off-screen invariant). Rendering only applies to the active window's
    // composition; with no active window there is nothing on-screen to
    // repaint. phux-x2hm: tile against the zoom-honoring view, so a zoomed
    // pane paints to the whole window and the others — absent from the
    // synthetic single-leaf layout — get no rect and so do not paint.
    let Some(active_ls) = workspace.render_window(zoomed) else {
        return StatusBarPaint::NotPublished;
    };
    let active_ls = active_ls.as_ref();
    let bar = status_bar.as_ref().map(|p| p.position());
    let content = content_rect(viewport_dims, bar, sidebar);
    // phux-flywheel: the paint trigger. Its OWN child span isolates paint cost
    // from the `vt_apply` above, so a trace shows apply-ms vs paint-ms
    // separately. Debug-level + lazy `rows` field => free at the default filter.
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
            // phux-2x9: a non-focused pane repaints on its own output so it is
            // not visually frozen. A pane with no rect is off-screen (another
            // window) and paints nothing.
            paint_background_interior(&mut block, rect, panes, terminal_id, walk);
        }
    }
    finish_output_frame(
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
    )
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

/// Close a composited frame: status bar, then the one cursor placement, then
/// the block epilogue and its single flush.
fn finish_output_frame<W: crate::attach::RenderSink>(
    block: crate::attach::paint::FrameBlock<'_, W>,
    status_bar: Option<&mut StatusBarPainter>,
    tail: &FrameTail<'_>,
) -> StatusBarPaint {
    let focused_cursor = tail
        .focused_resource
        .and_then(|fid| tail.panes.get(fid))
        .and_then(|slot| slot.renderer.last_cursor());
    // phux-9xn: the focused pane's Rect origin parks (and hides) the cursor
    // when `last_cursor` is None, so a frame never strands it at the bar's
    // tail — bottom-right of the host terminal.
    let fallback_origin = tail
        .focused_resource
        .and_then(|fid| {
            crate::attach::paint::tiled_rect(tail.active_ls, tail.content, tail.viewport_dims, fid)
        })
        .map_or(Some((0, 0)), |r| Some((r.x, r.y)));
    crate::attach::paint::close_frame_with_chrome(
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
    // The reconcile + overlay work entirely in PANE-LOCAL
    // coordinates (predictions are pane-local; the cell reader
    // indexes the pane's own grid). The outer `last_cursor` is
    // kept only for the frame's cursor tail.
    let (focused_cursor_local, pane_origin) = panes.get(fid).map_or((None, (0, 0)), |s| {
        (s.renderer.last_cursor_local(), s.renderer.last_origin())
    });
    // ADR-0090: sync the screen mode before reconciling — the
    // frame just applied may have switched screens (vim
    // starting or exiting), and predictions anchored to the
    // other screen must drop rather than reconcile against
    // this one's cells. A transition also resets the echo
    // evidence inside the predictor.
    predict.set_alt_screen(crate::attach::input_dispatch::terminal_in_alt_screen(
        walk.terminal,
    ));
    // Per-cell match reconcile (phux-9gw.1.1): walk pending
    // predictions against the freshly painted cell grid;
    // confirmed predictions drop, contradictions drop their
    // suffix, predictions still ahead of confirmed state
    // stay alive. See [`crate::predict`] for the truth table.
    if let Some((row, col)) = focused_cursor_local {
        let _stats = reconcile_terminal_output_per_cell(predict, row, col, |r, c| {
            panes.get_mut(fid).and_then(|s| {
                // Read the full grapheme cluster, not just the
                // base scalar, so multi-codepoint Insert
                // predictions (flag emoji, ZWJ sequences, base
                // plus combining marks) reconcile against the
                // whole painted cluster (phux-9gw.1.6).
                s.renderer
                    .read_grapheme_string_at(walk, r, c)
                    .ok()
                    .flatten()
            })
        });
    } else {
        // Cursor hidden — we can't anchor reliably; fall
        // back to the wholesale drain. Rare path (programs
        // that hide the cursor before a redraw).
        predict.clear();
    }
    // Overlay paints any predictions still alive (the tail
    // of a partial confirmation), shifted by the focused pane's
    // outer origin. On a fully-drained queue this is a no-op.
    // ADR-0090: the display policy gates the paint — while the
    // alt-screen echo latch is locked, the state is tentative,
    // or the front guess is past the TTL, the tail reconciles
    // silently instead of painting.
    if predict.should_display(crate::attach::input_dispatch::predict_now_ms()) {
        let _ = overlay.render(predict, pane_origin, out);
    }
}

/// phux-2x9: repaint a NON-focused pane on its own output so it isn't
/// visually frozen — output (and the post-split/resize resync snapshot) must
/// show without the user focusing the pane. `render_at` is dirty-tracked, so
/// steady-state output only repaints changed rows. The frame's shared tail
/// then restores the focused pane's cursor, so the host cursor ends where the
/// user is typing.
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
    // phux-foz.11: letterbox like every other paint path.
    // An undersized mirror (resize handshake in flight)
    // painted incrementally at the rect origin here, while
    // `paint_full_frame` centres the same mirror — dirty
    // rows then land offset from the full-frame rows and
    // the pane shows doubled text until a full repaint.
    // Mirror >= rect degrades to the prior `render_at`.
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

/// The reason is the whole point of the frame (phux-l83x): with `ERROR`
/// non-fatal at the receiver, `DETACHED` plus transport close is the only
/// ending a consumer may act on, so this is the one place the client learns
/// *why*. The message is diagnostic text — logged, never trusted as the
/// contract.
fn handle_detached(reason: Option<DetachReason>, message: &str) -> FrameOutcome {
    tracing::info!(?reason, %message, "DETACHED");
    FrameOutcome {
        exit: true,
        exit_reason: Some(AttachEnd::Detached { reason }),
        ..FrameOutcome::default()
    }
}

/// A `go-to-directory` reply (`docs/spec/L3.md` §4). The driver owns the
/// overlay stack and the pending request id, so the handler only hands the
/// reply up; matching it against the pending request happens there.
fn directory_listing_outcome(
    request_id: u32,
    result: phux_protocol::wire::frame::DirectoryListingResult,
) -> FrameOutcome {
    FrameOutcome {
        directory_listing: Some((request_id, result)),
        ..FrameOutcome::default()
    }
}

/// phux-4li.5: reconcile-on-attach reply path. The driver sends
/// `GET_METADATA { request_id }` immediately after ATTACHED;
/// the server replies with `MetadataValue { request_id, value }`.
/// Match by id, decode the layout envelope, and adopt its topology
/// while preserving this client's valid active window and per-window
/// focus. `value: None` means "no persisted layout" — keep the
/// single-pane bootstrap untouched.
fn handle_metadata_value<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    request_id: u32,
    value: Option<Vec<u8>>,
) -> Result<FrameOutcome, AttachError> {
    // ADR-0040: a pending per-Terminal `phux.agent/v1` GET reply.
    // `value: None` (key absent) clears any stale record.
    if let Some(terminal) = ctx.agent_meta.pending.remove(&request_id) {
        let changed = ctx.agent_meta.apply(&terminal, value.as_deref());
        if changed {
            note_agent_change(ctx.panes, ctx.focused_resource.as_ref(), &terminal);
        }
        return Ok(FrameOutcome {
            agent_meta_changed: changed,
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
                // phux-e9fd: the persisted layout just replaced the
                // single-pane bootstrap, so every leaf's rect moved.
                // Without the reflow each restored pane keeps the
                // attach-time winsize the server derived from the
                // outer viewport and paints a row short.
                reflow_panes: true,
                attach_panes,
                ..FrameOutcome::default()
            })
        }
        Err(err) => Err(layout_decode_refusal(&err)),
    }
}

/// phux-4li.5: broadcast reconcile. Another attached client
/// mutated `phux.tui.layout/v1`; decode + adopt topology + repaint.
/// ADR-0049: the sender's serialized focus is never authoritative.
/// Tombstones (`value: None`) are treated as "layout reset" —
/// fall back to the single-pane bootstrap so the next render
/// doesn't try to draw against a stale tree.
fn handle_metadata_changed<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    scope: &Scope,
    key: &str,
    value: Option<Vec<u8>>,
) -> Result<FrameOutcome, AttachError> {
    if key == RESOURCE_AGENT_KEY {
        return Ok(apply_agent_broadcast(ctx, scope, value));
    }
    // phux-foz.5: the config-reload doorbell. Value bytes are an
    // opaque nonce (only there to defeat the server's equal-bytes
    // SET dedup); a tombstone is not a reload request.
    if key == CONFIG_RELOAD_KEY && matches!(scope, Scope::Global) {
        return Ok(FrameOutcome {
            config_reload: value.is_some(),
            ..FrameOutcome::default()
        });
    }
    if key == SESSION_KEEP_EMPTY_KEY && matches!(scope, Scope::Global) {
        return Ok(apply_keep_empty_broadcast(ctx, value.as_deref()));
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
                // phux-e9fd: a peer's topology change reshapes our
                // tiles too. The peer sized the PTYs against ITS
                // content rect, which is only ours when both
                // clients run the same viewport and chrome.
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

/// ADR-0040: a `phux.agent/v1` broadcast for a subscribed pane.
/// A tombstone (`value: None`, the `DELETE_METADATA` path) clears
/// the record and the label falls back to the OSC title.
fn apply_agent_broadcast<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    scope: &Scope,
    value: Option<Vec<u8>>,
) -> FrameOutcome {
    let Scope::Resource(terminal) = scope else {
        return FrameOutcome::default();
    };
    // phux-k0cw: a record for a pane THIS client does not
    // hold belongs to a peer session. It must not enter the
    // local `AgentMetaIndex`, because
    // `sync_agent_meta_subscriptions` retains that index
    // against the local pane set and would evict it on the
    // next sweep — the record would flicker in and vanish.
    if !ctx.panes.contains_key(terminal) {
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
        ..FrameOutcome::default()
    }
}

/// Replace the local workspace with a decoded envelope's topology and report
/// the leaves this client has never seen.
///
/// Also re-anchors the driver's focused-pane mirror onto the active window's
/// client-local reconciled focus. The correlated GET or session-key guard has
/// already established ownership; missing replicas are discovered after adoption.
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

/// phux-4li.12: split-pane reply path. Look up the parked
/// `PendingSplit` by request id; on Ok apply the split + seed the
/// new `PaneSlot` + broadcast the envelope. On Err log + bell.
fn handle_terminal_spawned<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    request_id: u32,
    result: SpawnResult,
) -> Result<FrameOutcome, AttachError> {
    // phux-4li.15: a parked new-window takes priority — its reply
    // opens a window on the spawned pane instead of splitting the
    // active one. Request ids are unique across both maps.
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
    match result {
        SpawnResult::Ok(new_id) => apply_split_spawned(ctx, new_id, &pending),
        SpawnResult::Err(SpawnError::GroupNotFound) => {
            // v0.1 clients only ever target DEFAULT_GROUP_ID,
            // which the server always exposes; this branch
            // means a server-side L2 invariant changed under
            // us. Log loudly + bell.
            tracing::warn!(
                request_id,
                "ResourceSpawned: server reports GroupNotFound for DEFAULT group",
            );
            let _ = actions::write_bell(ctx.out);
            Ok(FrameOutcome::default())
        }
        SpawnResult::Err(SpawnError::SpawnFailed(reason)) => {
            tracing::warn!(
                request_id,
                reason = %reason,
                "ResourceSpawned: server-side spawn failed",
            );
            let _ = actions::write_bell(ctx.out);
            Ok(FrameOutcome::default())
        }
        // SpawnError is #[non_exhaustive] — catch future
        // variants so newer servers don't take the client down.
        SpawnResult::Err(other) => {
            tracing::warn!(
                request_id,
                error = ?other,
                "ResourceSpawned: unknown spawn error variant",
            );
            let _ = actions::write_bell(ctx.out);
            Ok(FrameOutcome::default())
        }
        // SpawnResult is also #[non_exhaustive].
        _ => {
            tracing::warn!(request_id, "ResourceSpawned: unknown SpawnResult variant");
            Ok(FrameOutcome::default())
        }
    }
}

/// Fold a successfully spawned split into the active window: apply the parked
/// intent, seed the new pane's slot, and move focus onto it.
fn apply_split_spawned<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    new_id: ResourceId,
    pending: &PendingSplit,
) -> Result<FrameOutcome, AttachError> {
    let Some(active_ls) = ctx.workspace.active_window_mut() else {
        tracing::warn!("ResourceSpawned: no active window to apply split into");
        let _ = actions::write_bell(ctx.out);
        return Ok(FrameOutcome::default());
    };
    let new_state = match apply_spawned_ok(active_ls, new_id.clone(), pending) {
        Ok(new_state) => new_state,
        Err(err) => {
            tracing::warn!(
                error = %err,
                terminal = ?new_id,
                "apply_spawned_ok failed; dropping spawned terminal",
            );
            let _ = actions::write_bell(ctx.out);
            return Ok(FrameOutcome::default());
        }
    };
    *active_ls = new_state;
    // phux-x2hm: a split un-zooms (tmux parity). The
    // new pane needs its tile, and the reflow_panes
    // diff below is taken against the now-cleared
    // (real, tiled) view.
    // phux-r82.7: unless the parked intent asked to
    // zoom the spawned pane (`placement = "zoomed"`
    // plugin panes) — then the new pane fills the
    // window and un-zooming reveals it tiled beside
    // its anchor.
    *ctx.zoomed = if pending.zoom_on_spawn {
        Some(new_id.clone())
    } else {
        None
    };
    // Seed pane metadata so the first bootstrap lands
    // on a warm rendering slot. Vacant-or-occupied —
    // never overwrite existing frontend metadata.
    if let std::collections::hash_map::Entry::Vacant(v) = ctx.panes.entry(new_id) {
        v.insert(PaneSlot::new()?);
    }
    // Move focus to the freshly spawned pane —
    // tmux-compatible (apply_split already sets
    // focus inside the returned state).
    ctx.focused_resource.clone_from(
        &ctx.workspace
            .active_window()
            .and_then(|ls| ls.focus.clone()),
    );
    // Re-anchor predictive echo to the freshly
    // focused pane (phux-7ry0). The split leaves the
    // predict layer holding the previous pane's
    // viewport + cursor; a keystroke before the new
    // pane's first snapshot would otherwise echo at
    // the old pane's coordinates (mid-screen ghost).
    if let Some(fid) = ctx.focused_resource.as_ref() {
        reanchor_predict_to_pane(ctx.predict, ctx.panes, fid);
    }
    Ok(FrameOutcome {
        layout_replaced: true,
        emit_set_metadata: true,
        // phux-tnh: the split shrank the sibling
        // and added a leaf; emit per-leaf resizes
        // so the server learns the real split dims
        // instead of leaving panes at spawn size.
        reflow_panes: true,
        ..FrameOutcome::default()
    })
}

/// phux-4li.12: a Terminal closed. Fold it out of the layout if
/// it's a known leaf, drop its `PaneSlot` regardless. If we
/// initiated the kill (or it died on us spontaneously), the
/// server still broadcasts this so every attached client folds
/// in lockstep.
fn handle_terminal_closed<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal_id: &ResourceId,
    exit_status: Option<i32>,
    reason: CloseReason,
) -> FrameOutcome {
    tracing::info!(
        terminal = ?terminal_id,
        exit_status = ?exit_status,
        ?reason,
        "ResourceClosed",
    );
    // phux-i0e8.2.2: was this close one WE asked for (kill-pane /
    // kill-window)? Drain the marker unconditionally — every close
    // consumes at most one expectation, whatever its exit status —
    // so a later spontaneous death of a re-used id still notifies.
    let expected = ctx.expected_closes.remove(terminal_id);
    // An AgentSession occupies no slot and no leaf: its close (its own, or
    // the cascade of its parent closing) removes only its chrome row. The
    // parent pane, if it is still open, is untouched; if the parent closed
    // too, the parent's own close frame folds the layout.
    if ctx.is_agent_session(terminal_id) {
        return FrameOutcome {
            chrome_dirty: true,
            ..FrameOutcome::default()
        };
    }
    // Always drop the slot — even for unknown leaves (could be
    // a spawn-failure cleanup race or a stale id from before
    // an attach).
    ctx.panes.remove(terminal_id);
    // Find the window holding this leaf (panes can live in any
    // window, not just the active one) and fold it out there.
    let owner = ctx.workspace.windows.iter().position(|w| {
        w.state
            .tree
            .as_ref()
            .map(layout::leaves)
            .unwrap_or_default()
            .contains(terminal_id)
    });
    let Some(idx) = owner else {
        return FrameOutcome::default();
    };
    let new_state = match apply_terminal_closed(&ctx.workspace.windows[idx].state, terminal_id) {
        Ok(new_state) => new_state,
        Err(err) => {
            // The leaf vanished from the tree between the lookup
            // and the fold (a race), or the window emptied. Drop
            // quietly — the slot is already gone.
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
    // phux-4r1: consumer-owned detach policy (ADR-0015 L1).
    // The server reports the fact (RESOURCE_CLOSED) and stops
    // there; deciding whether *this* client detaches is the
    // TUI's call. When the last pane closed there is nothing
    // left to render or to route input to, so detach. For
    // v0.1 single-pane this is behaviorally identical to the
    // old server-baked "EOF ⇒ DETACHED" (the seed pane closes
    // ⇒ client exits), but now multi-Terminal-ready: closing
    // one of several panes folds it out and keeps the attach
    // alive. ADR-0105: a keep-empty session outlives its last
    // pane, so the attach stays and shows the empty state.
    if ctx.workspace.windows.is_empty() && *ctx.keep_empty_session {
        return last_pane_closed_keep_empty(ctx, terminal_id, exit_status, expected);
    }
    if ctx.workspace.windows.is_empty() {
        tracing::info!("ResourceClosed folded the last pane; detaching");
        return FrameOutcome {
            exit: true,
            // phux-i0e8.2.2: carry the dead pane's status up
            // so the CLI can explain the exit on the cooked
            // terminal — an OOM-killed shell must not look
            // like phux crashed.
            exit_reason: Some(AttachEnd::LastPaneClosed { exit_status }),
            ..FrameOutcome::default()
        };
    }
    // Re-anchor `focused_resource` onto the (possibly new)
    // active window's focus. `apply_terminal_closed` sets
    // a surviving window's focus to the first DFS leaf;
    // a pruned active window hands focus to its successor.
    *ctx.focused_resource = ctx
        .workspace
        .active_window()
        .and_then(|ls| ls.focus.clone());
    FrameOutcome {
        layout_replaced: true,
        emit_set_metadata: true,
        // phux-tnh: the survivor's Rect grew; tell the
        // server so its PTY winsize grows too.
        reflow_panes: true,
        notices: pane_exit_notices(terminal_id, exit_status, expected),
        ..FrameOutcome::default()
    }
}

/// ADR-0105: the last pane of a keep-empty session closed. The session is
/// still on the server, so the client stays attached with nothing focused
/// and the driver paints the empty state. The stored layout now names only
/// dead panes, so it is tombstoned rather than left for the next attach.
fn last_pane_closed_keep_empty<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal_id: &ResourceId,
    exit_status: Option<i32>,
    expected: bool,
) -> FrameOutcome {
    tracing::info!("ResourceClosed folded the last pane of a keep-empty session; staying attached");
    *ctx.focused_resource = None;
    *ctx.zoomed = None;
    FrameOutcome {
        layout_replaced: true,
        clear_layout: true,
        notices: pane_exit_notices(terminal_id, exit_status, expected),
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

/// phux-i0e8.2.2: survivors get a transient Warn notice naming the dead pane
/// and its exit shape. Silent for a clean exit 0 (the user typed `exit`;
/// nothing is wrong) and for a close this client itself requested.
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

/// Dispatch one pushed agent event (ADR-0033 `SUBSCRIBE_EVENTS` stream).
///
/// Lifecycle/activity events share the subscribed agent-event stream,
/// but most do not affect the interactive client's projection. They
/// remain valid server traffic: ignoring them must not tear down an
/// otherwise healthy attach.
fn handle_agent_event<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    frame: FrameKind,
    route: &KernelRoute,
) -> FrameOutcome {
    match frame {
        // A live-spawned `AgentSession` under one of our panes: the kernel
        // just declared it, so attach it as a record stream (the same
        // per-resource attach a layout-discovered pane gets) and let the
        // chrome grow its row once the stream publishes.
        FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::ResourceSpawned { .. },
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
                    ..
                },
        } => fold_terminal_control(ctx, &terminal, lifecycle, input_holder),
        FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::Asked { .. },
        } => fold_agent_ask(ctx, terminal),
        FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::CwdChanged { cwd },
        } => fold_cwd_changed(ctx, &terminal, cwd),
        FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::CommandFinished { exit_code },
        } => fold_command_finished(ctx, &terminal, exit_code),
        // phux-k0cw: the pane set of ANOTHER session changed. This client
        // holds a server-wide `SUBSCRIBE_EVENTS { terminal: None }`, so the
        // server announces every spawn and close — which is precisely what
        // makes enumerate-then-subscribe race-free and keeps the whole
        // cross-session sidebar inside the existing wire (ADR-0030).
        FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::ResourceSpawned { .. } | AgentEvent::ResourceClosed { .. },
        } if !ctx.panes.contains_key(&terminal) && !ctx.is_agent_session(&terminal) => {
            FrameOutcome {
                foreign_pane_set_dirty: true,
                ..FrameOutcome::default()
            }
        }
        _ => FrameOutcome::default(),
    }
}

/// ADR-0033: fold a supervisory `TerminalControl` broadcast's lifecycle +
/// lease-holder into the pane's slot so the next paint renders the "FROZEN" /
/// "wheel" badge.
///
/// phux-i0e8.2.1: a holder TRANSITION on the FOCUSED pane also raises a
/// transient status-bar notice — the badge shows the steady state; the notice
/// calls out the moment the wheel moved. The first `TerminalControl` a slot
/// ever sees is the attach-time initial state (the server re-states the lease
/// on subscribe), not a transition, so it stays silent.
fn fold_terminal_control<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal: &ResourceId,
    lifecycle: ResourceLifecycle,
    input_holder: Option<ClientId>,
) -> FrameOutcome {
    let Some(slot) = ctx.panes.get_mut(terminal) else {
        // A control event for a pane we have no slot for yet (it can
        // precede the first snapshot). Harmless to drop — the lease is
        // server-authoritative and the next event re-states it.
        return FrameOutcome::default();
    };
    let initial_state = !slot.control_seen;
    slot.control_seen = true;
    let holder_changed = slot.input_holder != input_holder;
    slot.lifecycle = lifecycle;
    slot.input_holder = input_holder;
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

/// phux-foz.1 / ADR-0035: an agent in `terminal` is waiting on a human
/// answer. Mirror the `TerminalControl` fold above: raise the pane's
/// attention flag so the next chrome paint renders the window-tab `!`
/// marker and the status-bar `[ ASK ]` hint. The flag clears when the
/// user sends key/paste input to the pane (see
/// `pane_state::clear_attention_on_input`); a repeated `Asked` while
/// already flagged changes nothing, so no repaint is requested for it.
fn fold_agent_ask<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal: ResourceId,
) -> FrameOutcome {
    let Some(slot) = ctx.panes.get_mut(&terminal) else {
        // phux-k0cw: no slot means either a pane whose snapshot has
        // not landed yet, or — now that this client subscribes
        // server-wide — a pane in ANOTHER session whose agent is
        // blocked on a human. Both route out as `foreign_attention`:
        // the roster and queue want it, and the local pane map has
        // nowhere to put it. The ADR-0036 detector coalesces repeated
        // markers, so a genuinely-early local ask still re-raises
        // once the slot exists.
        return FrameOutcome {
            foreign_attention: Some(terminal),
            ..FrameOutcome::default()
        };
    };
    if slot.attention {
        return FrameOutcome::default();
    }
    slot.attention = true;
    FrameOutcome {
        chrome_dirty: true,
        ..FrameOutcome::default()
    }
}

/// phux-foz.4: the pane's shell changed directory (kernel-observed,
/// announced at prompt boundaries / output settle). Fold it into the
/// slot so the status-bar `cwd` widget tracks the focused pane;
/// `chrome_dirty` only when the value actually moved, and the chrome
/// refresh itself no-ops for an unfocused pane's change.
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

/// phux-foz.4: a command finished in the pane; record its OSC-133
/// exit code for the status-bar `exit` widget. `None` is recorded
/// too — "the last command reported no code" honestly blanks the
/// widget rather than pinning a stale code.
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

/// phux-ijuj: ERROR never terminates the attach. SPEC §9 puts
/// termination on `DETACHED` plus transport close, and the same
/// `ErrorCode` is emitted both fatally and non-fatally by the same
/// server, so no client-side "which codes are fatal" table can be
/// sound. This arm is therefore total over `Error` and total in its
/// result: it degrades, it never tears down. `FrameKind::Error`
/// carries no terminal id, so an uncorrelated per-pane failure cannot
/// be attributed to a pane — the notice names the code instead.
fn handle_error_frame(request_id: Option<u32>, code: ErrorCode, message: &str) -> FrameOutcome {
    // Request-correlated errors are normally consumed by
    // `Connection`'s request table. A raced reply that reaches the
    // attached dispatcher is still direction-valid and must not
    // mutate or retire terminal state.
    if request_id.is_some() {
        return FrameOutcome::default();
    }
    // phux-i0e8.2.1 (second consumer, closing phux-i0e8.2's otherwise
    // orphaned lifecycle event): a spontaneous, uncorrelated
    // `ERROR { SATELLITE_UNREACHABLE }` is the hub announcing a
    // degraded-federation transition — part of the fleet just became
    // invisible. It keeps its own wording; `phux status`'s
    // degradation line remains the CLI view of the same state, not
    // the TUI representation.
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

/// The `ERROR` arm: a correlated refusal of a parked satellite-session
/// attach (phux-c2td.3) decides that window; anything else is the ordinary
/// error notice.
fn error_frame_outcome<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    request_id: Option<u32>,
    code: ErrorCode,
    message: String,
) -> Result<FrameOutcome, AttachError> {
    if let Some(pending) = request_id.and_then(|id| take_pending_adopt(ctx, id)) {
        return handle_window_adopt_reply(ctx, &pending, Some(message));
    }
    Ok(handle_error_frame(request_id, code, &message))
}

/// The `COMMAND_RESULT` arm: the reply to a parked satellite-session attach
/// (phux-c2td.3) decides whether its window opens; any other reply that
/// reached the dispatcher instead of its awaiter is dropped, inert (see the
/// arm's comment in [`dispatch_frame`]).
fn command_result_outcome<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    request_id: u32,
    result: phux_protocol::wire::frame::CommandResult,
) -> Result<FrameOutcome, AttachError> {
    if let Some(pending) = take_pending_adopt(ctx, request_id) {
        let refusal = match result {
            phux_protocol::wire::frame::CommandResult::Error { message, .. } => Some(message),
            _ => None,
        };
        return handle_window_adopt_reply(ctx, &pending, refusal);
    }
    tracing::debug!(
        request_id,
        "dropping CommandResult with no matching pending request"
    );
    Ok(FrameOutcome::default())
}

/// phux-c2td.3: take the parked satellite-session window behind
/// `request_id`, if that is what the id names. A spawn-parked window (no
/// `adopt`) stays parked for its `RESOURCE_SPAWNED`.
fn take_pending_adopt<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    request_id: u32,
) -> Option<PendingWindow> {
    if ctx
        .pending_windows
        .get(&request_id)
        .is_none_or(|pending| pending.adopt.is_none())
    {
        return None;
    }
    ctx.pending_windows.remove(&request_id)
}

/// phux-c2td.3: apply the reply to a satellite-session window's
/// `ATTACH_RESOURCE`.
///
/// Success opens the window on the adopted pane, makes it active, focuses
/// the pane, and asks for the layout broadcast and a reflow — the same
/// follow-up a spawned new window gets. Its slot normally exists already,
/// seeded by the pane's `BOOTSTRAP_BEGIN`. A refusal opens nothing and saves
/// nothing: it bells and says which host and session could not be opened,
/// because a dead window in the shared layout would be the worse outcome.
fn handle_window_adopt_reply<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    pending: &PendingWindow,
    refusal: Option<String>,
) -> Result<FrameOutcome, AttachError> {
    let Some(pane) = pending.adopt.clone() else {
        return Ok(FrameOutcome::default());
    };
    if let Some(reason) = refusal {
        tracing::warn!(window = %pending.name, %reason, "satellite window attach refused");
        let _ = actions::write_bell(ctx.out);
        let host = pane.host().map_or_else(String::new, ToString::to_string);
        return Ok(FrameOutcome {
            notices: vec![Notice::warn(format!(
                "could not open {} on satellite {host}: {reason}",
                pending.name
            ))],
            ..FrameOutcome::default()
        });
    }
    ctx.workspace.add_window(pending.name.clone(), pane.clone());
    if let std::collections::hash_map::Entry::Vacant(slot) = ctx.panes.entry(pane) {
        slot.insert(PaneSlot::new()?);
    }
    *ctx.focused_resource = ctx
        .workspace
        .active_window()
        .and_then(|ls| ls.focus.clone());
    Ok(FrameOutcome {
        layout_replaced: true,
        emit_set_metadata: true,
        reflow_panes: true,
        ..FrameOutcome::default()
    })
}

/// phux-4li.15: apply a `RESOURCE_SPAWNED` reply for a parked
/// `new-window` action. On success it appends a window seeded on the
/// freshly spawned pane (making it active), seeds the pane's slot, and
/// re-anchors `focused_resource`. The follow-up flags mirror the split path:
/// `layout_replaced` triggers a full repaint, `emit_set_metadata`
/// broadcasts the new workspace to siblings, and `reflow_panes` sizes the
/// new full-window pane.
pub(super) fn handle_window_spawned<W: crate::attach::RenderSink>(
    out: &mut W,
    workspace: &mut Workspace,
    focused_resource: &mut Option<ResourceId>,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    pending: &PendingWindow,
    result: SpawnResult,
) -> Result<FrameOutcome, AttachError> {
    match result {
        // A pane spawned on a satellite through the hub streams to no one
        // until it is attached (the relay drops automatic spawn output no
        // proxy observes), and that attach can be refused. Hand the window
        // back to be parked on the attach, the satellite-session open path:
        // it opens only when the attach succeeds.
        SpawnResult::Ok(new_id) if !new_id.is_local() => Ok(FrameOutcome {
            adopt_windows: vec![PendingWindow {
                name: pending.name.clone(),
                adopt: Some(new_id),
            }],
            ..FrameOutcome::default()
        }),
        SpawnResult::Ok(new_id) => {
            workspace.add_window(pending.name.clone(), new_id.clone());
            if let std::collections::hash_map::Entry::Vacant(v) = panes.entry(new_id) {
                v.insert(PaneSlot::new()?);
            }
            *focused_resource = workspace.active_window().and_then(|ls| ls.focus.clone());
            Ok(FrameOutcome {
                layout_replaced: true,
                emit_set_metadata: true,
                reflow_panes: true,
                ..FrameOutcome::default()
            })
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

/// phux-17u: resolve the focused session's display name from an
/// `ATTACHED` snapshot for the status-bar `session-name` widget.
///
/// The snapshot carries `sessions: Vec<SessionInfo>` plus a
/// `focused_session` id; the name is the `SessionInfo` whose `id`
/// matches. Returns the empty string when the focused session isn't in
/// the list — which shouldn't happen (the focused session is always one
/// of the snapshot's own sessions), but an empty widget is a safer
/// degradation than a panic.
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

/// Which session a layout-coordination key names, for keys ADR-0019 reserves
/// (`phux.tui.layout/v1[/<session>]`, scoped to the default Group).
///
/// `None` ⇒ not a layout key we can attribute.
///
/// phux-k0cw replaced the old boolean `is_layout_key`. Its doc comment said
/// matching the family was sufficient because "a client only ever receives
/// broadcasts for the key it subscribed to (its own session)" — an invariant
/// the cross-session sidebar removes, which is exactly why the caller must now
/// know WHOSE layout arrived before deciding to adopt it.
pub(super) fn layout_key_scope_session(scope: &Scope, key: &str) -> Option<LayoutKeyOwner> {
    if !matches!(scope, Scope::Group(id) if *id == DEFAULT_GROUP_ID) {
        return None;
    }
    layout_key_session(key)
}

/// Adopt a decoded workspace's topology without adopting its sender's focus.
///
/// Focus and the active-window index are client-local (ADR-0019 decision 6,
/// reaffirmed by ADR-0049). For each incoming window, preserve the local focus
/// at the same index when that terminal remains a leaf; otherwise choose the
/// first depth-first leaf. Preserve the local active index when the new window
/// count permits it, otherwise clamp deterministically.
///
/// The foreign-session guard remains workspace-scoped. An incoming non-empty
/// tree is foreign only when none of its leaves belongs to the current local
/// workspace or pane-slot set. Checking all known panes, rather than only the
/// currently focused pane, lets a sibling legitimately remove that focused leaf
/// without making the surviving topology look foreign.
/// Whether a persisted layout names at least one pane and none of them is in
/// the `ATTACHED` snapshot (ADR-0105). Every pane alive at attach time is in
/// the snapshot, so such a layout is a stale tree of dead panes, left by a
/// keep-empty session that lost its last window. Adopting it would hide the
/// empty state behind panes that never bootstrap.
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

/// Preserve a valid local focus while adopting `state`'s tree topology.
///
/// The focus decoded from metadata is deliberately ignored. If this client has
/// no focus for the window, or its focused leaf disappeared, the first leaf in
/// depth-first order is the deterministic ADR-0019 fallback.
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
        // phux-17u: the widget reads the name of the focused session,
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
