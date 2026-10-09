//! The attached session's long-lived state and the `tokio::select!` step
//! that drives it.
//!
//! The handlers live beside this file: one module per wake-up source
//! (input, frames, paint, inventory, bootstrap, panes, peers) so this
//! type is not a single 3k-line impl.
//!
//! [`SessionLoop`] owns every local the attach loop carries across
//! iterations; one named handler per wake-up source (`on_stdin`,
//! `on_server_frame`, `on_resize`, `on_status_tick`, ...) turns the
//! `select!` into a readable dispatch over what just happened. Every
//! session-scoped field is rebuilt on each [`SessionLoop::new`], so a
//! re-attach starts from a clean slate (no stale pane mirror, no
//! carried-over predict queue).

#![allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]

pub(super) use std::collections::{HashMap, HashSet};
pub(super) use std::sync::atomic::{AtomicBool, Ordering};
pub(super) use std::time::Duration;

pub(super) use phux_client_core::engine::ghostty::GhosttyAdapter;
pub(super) use phux_client_core::history::HistoryCacheConfig;
pub(super) use phux_client_core::session::SessionKernel;
pub(super) use phux_protocol::caps::ServerFeature;
pub(super) use phux_protocol::ids::{ClientId, ResourceId, SatelliteHost};
pub(super) use phux_protocol::wire::frame::{
    AttachTarget, CONFIG_RELOAD_KEY, Command, CommandResult, CommandValue, FrameKind,
    SESSION_NAME_KEY, Scope,
};
pub(super) use tokio::signal::unix::{Signal, SignalKind, signal};

pub(super) use crate::attach::actions::{ParkedAdopt, PendingSplit, PendingWindow};
pub(super) use crate::attach::connection::{Connection, NegotiatedBootstrap};
pub(super) use crate::attach::input::StdinParser;
pub(super) use crate::attach::input_dispatch::{
    DispatchCtx, DragGrab, PendingSessionRename, ReattachTarget, dispatch_input_events,
    encode_layout_or_log, sync_overlays_to_focused_pane,
};

/// The QUIC connection keeps one of its 128 bidi streams for control.
pub(super) const MAX_PENDING_STREAM_BINDS: usize = 127;
pub(super) use crate::attach::chrome_ctx::{ChromeCtx, PaneScene};
pub(super) use crate::attach::onboarding::{AttachClaim, AttachMoment};
pub(super) use crate::attach::outcome::{AttachEnd, AttachError};
pub(super) use crate::attach::paint::{
    SidebarReservation, StatusBarPaint, content_rect, paint_chrome_in_place, paint_full_frame,
    sidebar_reservation,
};
pub(super) use crate::attach::pane_state::{
    AttentionNavigation, VcsIndex, reanchor_predict_to_pane,
};
pub(super) use crate::attach::path_picker;
pub(super) use crate::attach::plugin_actions::{self, PluginRunResult};
pub(super) use crate::attach::repaint::{PaintPacer, RepaintAccumulator, RepaintLevel};
pub(super) use crate::attach::server_frame::{
    FrameEnv, FrameOutcome, ProjectTagFrame, attach_participants, handle_server_frame,
};
pub(super) use crate::attach::session_mirror::SessionMirror;
pub(super) use crate::attach::tty_input::TtyInput;
pub(super) use crate::predict::{PredictionState, PredictiveConfig};
pub(super) use crate::render::chrome::sidebar::SidebarPainter;
pub(super) use crate::render::chrome::status_bar::{Notice, StatusBarPainter};
pub(super) use crate::render::overlay::{OverlayState, ToastOverlay};
pub(super) use crate::settings::TuiSettings;
pub(super) use phux_client::layout_ops::{DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, layout_key};

pub(super) use super::chrome::{mark_focused_seen, refresh_window_chrome};

pub(super) use super::config_ui::{
    adopt_config_reload, apply_initial_notice, push_which_key_overlay, update_which_key_deadline,
};
pub(super) use super::entry::{
    CarriedSidebar, EntryPick, LoopExit, detached_loop_exit, finish_onboarding_claim,
    finish_return_onboarding_after_paint, seed_sidebar_enabled,
};
pub(super) use super::main_loop::{
    FRAME_COALESCE_CAP, coalesce_defer_flags, frame_defers_paint, frame_paint_target,
};
pub(super) use super::overlay_paint::paint_active_overlay;
pub(super) use super::session_io::{
    peer_gone, send_attach, send_unless_peer_gone, should_emit_frame_ack,
};
pub(super) use super::subscriptions::{PeerWatch, sync_agent_meta_subscriptions};
pub(super) use super::terminal::{
    desired_mouse_capture, sync_hover_tracking, sync_mouse_capture, terminal_reset_on_signal,
};
pub(super) use super::viewport::{
    HOST_CELL_PX_FALLBACK, current_viewport, current_viewport_or_default,
    emit_bootstrap_workspace_reflow, emit_moved_tiles, emit_view_reflow, host_cell_px,
    resize_cell_px, view_rects, viewport_resize_frame,
};

#[cfg(test)]
#[path = "../loop_state_tests.rs"]
mod tests;

/// How long a host-inventory `GET_STATE` may stay unanswered before the
/// notices held for it surface anyway (hub relay deadline 30 s + margin).
pub(super) const HOST_INVENTORY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(35);

/// How long a grey satellite pane waits before asking the hub which hosts
/// are back. Anchored so busy output cannot postpone it.
pub(super) const SATELLITE_PROBE_INTERVAL: Duration = Duration::from_secs(5);

/// Bar ticks (1 s each) to keep polling the background update check's cache.
pub(super) const UPDATE_POLL_TICKS: u8 = 8;

/// Rename the matching cached session in place. Identity (`SessionId`) is
/// unchanged; only the display label moves.
pub(super) fn apply_graph_rename(
    sessions: &mut [phux_protocol::wire::info::SessionInfo],
    current: &str,
    new_name: &str,
) {
    if let Some(session) = sessions.iter_mut().find(|session| session.name == current) {
        new_name.clone_into(&mut session.name);
    }
}

/// The held `SatelliteUnreachable` notices the inventory reply does not
/// explain (by exact diagnostic or by naming the row's host); those are news.
pub(super) fn unexplained_unreachable_notices(
    held: Vec<String>,
    hosts: &[phux_protocol::wire::info::HostInventory],
) -> Vec<String> {
    held.into_iter()
        .filter(|notice| {
            !hosts
                .iter()
                .any(|row| unreachable_row_explains(row, notice))
        })
        .collect()
}

/// True when `notice` is the diagnostic this unreachable host already reported.
pub(super) fn unreachable_row_explains(
    row: &phux_protocol::wire::info::HostInventory,
    notice: &str,
) -> bool {
    let Some(diagnostic) = row.unreachable.as_deref() else {
        return false;
    };
    diagnostic == notice || notice.starts_with(&format!("satellite {} is unreachable", row.host))
}

/// Held `SatelliteUnreachable` diagnostics as status notices.
pub(super) fn federation_notices(messages: Vec<String>) -> Vec<Notice> {
    messages
        .into_iter()
        .map(|message| Notice::warn(format!("federation degraded: {message}")))
        .collect()
}

/// What one host inventory said about each satellite.
#[derive(Debug, Default)]
pub(super) struct HostAnswers {
    /// Satellites the inventory reached.
    reachable: Vec<SatelliteHost>,
    /// Satellites it could not list.
    unreachable: Vec<SatelliteHost>,
}

/// Partition an inventory into the hosts it reached and the ones it could not list.
pub(super) fn host_answers(rows: &[phux_protocol::wire::info::HostInventory]) -> HostAnswers {
    let (reachable, unreachable): (Vec<_>, Vec<_>) =
        rows.iter().partition(|row| row.is_reachable());
    let names = |rows: Vec<&phux_protocol::wire::info::HostInventory>| {
        rows.into_iter().map(|row| row.host.clone()).collect()
    };
    HostAnswers {
        reachable: names(reachable),
        unreachable: names(unreachable),
    }
}

/// The satellite panes a frame's parked windows and splits were spawned as;
/// each proves its satellite answered a relayed spawn.
pub(super) fn spawned_satellite_panes(parked: &[ParkedAdopt]) -> Vec<ResourceId> {
    parked
        .iter()
        .filter_map(ParkedAdopt::pane)
        .cloned()
        .collect()
}

/// The panes this client spawned for windows/splits still awaiting attach.
pub(super) fn parked_spawned_panes(
    windows: &HashMap<u32, PendingWindow>,
    splits: &HashMap<u32, PendingSplit>,
) -> Vec<crate::attach::actions::SpawnedPane> {
    let windows = windows.values().filter_map(PendingWindow::spawned_pane);
    let splits = splits.values().filter_map(|split| split.adopt.as_ref());
    windows.chain(splits).cloned().collect()
}

/// Request ids of windows and splits whose spawn has not answered yet.
pub(super) fn unanswered_spawns(
    windows: &HashMap<u32, PendingWindow>,
    splits: &HashMap<u32, PendingSplit>,
) -> HashSet<u32> {
    let windows = windows
        .iter()
        .filter(|(_, window)| window.adopt.is_none())
        .map(|(id, _)| *id);
    let splits = splits
        .iter()
        .filter(|(_, split)| split.adopt.is_none() && split.open_existing.is_none())
        .map(|(id, _)| *id);
    windows.chain(splits).collect()
}

/// Whether a host-inventory request sent at `since` is past the deadline.
pub(super) fn host_inventory_overdue(
    since: Option<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    since.is_some_and(|sent| now.saturating_duration_since(sent) > HOST_INVENTORY_DEADLINE)
}

/// Window before a parser-pending bare ESC is taken as the Escape key. The
/// outer terminal writes a key's full sequence in one burst, so a short window
/// suffices; it must stay short because modal-editor users pay it on every
/// Escape (tmux ships `escape-time 0..10` for the same reason).
pub(super) const ESC_FLUSH_IDLE: Duration = Duration::from_millis(10);

/// Safety valve for an application that enters DEC synchronized output and
/// never leaves it. Normal TUI transactions last milliseconds.
pub(super) const SYNC_OUTPUT_WATCHDOG: Duration = Duration::from_secs(1);

/// What one [`SessionLoop::step`] decided about the loop's future.
#[allow(
    clippy::large_enum_variant,
    reason = "LoopExit is the attach-ending payload; boxing it would scatter every match"
)]
pub(super) enum Step {
    /// Nothing ended; park on the wake-up sources again.
    Continue,
    /// The attach is over (detach, disconnect, or a session switch).
    Exit(LoopExit),
}

/// What one handled server frame decided about the burst it arrived in.
#[allow(
    clippy::large_enum_variant,
    reason = "LoopExit is the attach-ending payload; boxing it would scatter every match"
)]
pub(super) enum FrameStep {
    /// Frame handled; move on to the next frame in the burst.
    Done,
    /// The engine rejected a generation and a replacement bootstrap is in
    /// flight; skip the rest of this frame's handling.
    Rebootstrap,
    /// The attach is over; unwind out of the burst.
    Exit(LoopExit),
}

/// A `sleep_until` future for an armed deadline, or a never-resolving one.
async fn sleep_until_or_pending(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending::<()>().await,
    }
}

/// The relative-delay twin of [`sleep_until_or_pending`].
pub(super) fn sleep_for_or_pending(interval: Option<Duration>) -> impl Future<Output = ()> {
    // Anchor relative delays when armed, not when select first polls them.
    let deadline = interval.map(|interval| tokio::time::Instant::now() + interval);
    sleep_until_or_pending(deadline)
}

/// Restore the terminal explicitly (Drop wouldn't fire on `exit()`), then
/// exit with the shell-conventional code for the signal.
#[allow(clippy::exit, reason = "signal-driven graceful exit; Drop won't run")]
pub(super) fn exit_on_signal(code: i32) -> ! {
    terminal_reset_on_signal();
    std::process::exit(code);
}

/// Drain every frame already queued so an output burst applies all its writes
/// and paints once, on the final frame. Stops the moment the socket would block.
pub(super) fn drain_frame_batch(
    conn: &mut Connection,
    first: FrameKind,
) -> Result<Vec<FrameKind>, AttachError> {
    let mut batch = vec![first];
    while batch.len() < FRAME_COALESCE_CAP {
        match conn.try_recv() {
            Ok(Some(more)) => batch.push(more),
            // Socket drained, or a clean EOF the next `recv()` will surface
            // as Disconnected.
            Ok(None) | Err(AttachError::Disconnected) => break,
            Err(err) => return Err(err),
        }
    }
    Ok(batch)
}

/// Whether an input event expects output back, and so should arm the pacer's
/// reply grace. Pointer motion does not: under `?1002h` a drag streams motion
/// events fast enough to keep the grace alive and pacing off forever.
pub(super) const fn input_expects_a_reply(event: &phux_protocol::input::InputEvent) -> bool {
    match event {
        phux_protocol::input::InputEvent::Mouse(mouse) => !matches!(
            mouse.action,
            phux_protocol::input::mouse::MouseAction::Motion
        ),
        // Keys, focus, pastes, and any future atom expect a reply.
        _ => true,
    }
}

/// Whether an inbound burst must discharge the pacer's withheld debt before it
/// parks. `paint_now` covers the case the timer cannot: an admitted burst just
/// pushed the deadline out, and a saturating `conn.recv()` would starve the
/// `paint_deadline` arm.
pub(super) const fn burst_settles_debt(paint_now: bool, deadline_passed: bool) -> bool {
    paint_now || deadline_passed
}

/// Did this frame change anything the fleet dashboard projects?
pub(super) const fn fleet_projection_dirty(outcome: &FrameOutcome) -> bool {
    outcome.chrome_dirty
        || outcome.agent_meta_changed
        || outcome.layout_replaced
        || outcome.reflow_panes
        || outcome.sessions.is_some()
}

/// Warn notices for every replay report that did not end in delivery.
pub(super) fn undelivered_notices(
    reports: impl IntoIterator<Item = crate::attach::input_replay::ReplayReport>,
) -> impl Iterator<Item = Notice> {
    reports
        .into_iter()
        .filter(|report| {
            !matches!(
                report.disposition,
                crate::attach::input_replay::ReplayDisposition::Delivered
            )
        })
        .map(|report| Notice::warn(report.notice_line()))
}

/// Every local the attach loop carries across `select!` iterations.
#[allow(
    clippy::struct_excessive_bools,
    reason = "parallel driver-local view/lifecycle flags; a bitset would obscure every read site"
)]
pub(super) struct SessionLoop {
    /// Original attach dial, reused for dedicated control-plane requests.
    control_dial: Option<Box<crate::attach::Dial>>,
    /// Whether the server builds a spawned pane at the geometry we name.
    spawn_initial_size_supported: bool,
    /// What the `go-to-directory` picker can list on this server.
    directory_support: crate::attach::directory_picker::DirectorySupport,
    /// The host path picker: whether the server answers, and the query in flight.
    path: path_picker::PickerState,
    /// Whether the server advertised `ACKNOWLEDGED_INPUT` (ADR-0053 journal).
    acknowledged_input_supported: bool,
    /// ADR-0053 replay journal, shared with the CLI reconnect loop so an
    /// unresolved operation is replayed under its original id. `None` on UDS.
    input_replay:
        Option<std::rc::Rc<std::cell::RefCell<crate::attach::input_replay::InputReplayJournal>>>,
    /// Authoritative output accepted while its pane was not actually painted.
    /// A fence clears only when the focused pane becomes visible or retires.
    delivery_fence_paint_pending: HashSet<ResourceId>,
    /// Whether this connection negotiated `OutputMode::StateSync`, which
    /// gates per-frame `FRAME_ACK`.
    wants_state_sync: bool,
    /// Control-stream `ATTACH_READY` held until all per-Terminal streams have
    /// delivered READY or CLOSED. QUIC has no cross-stream ordering.
    pending_attach_ready: Option<FrameKind>,
    /// `ATTACH_RESOURCE` requests waiting for an affirmative command result.
    /// A QUIC stream is never bound before the server registers membership.
    pending_stream_binds: HashMap<u32, ResourceId>,

    /// The attached session as this client mirrors it: the kernel, pane
    /// slots, layout, focus, and the frame-correlated bookkeeping.
    mirror: SessionMirror,
    /// One-entry focus MRU, deliberately outside Workspace so focus history
    /// never persists (ADR-0019).
    focus_history: crate::attach::focus::FocusHistory,
    /// This client's `ClientId` from ATTACHED, for the supervisory badge.
    own_client_id: Option<ClientId>,
    /// The in-flight layout GET's request id.
    layout_get_request_id: Option<u32>,
    /// Shared writes remain fenced until the initial correlated GET succeeds.
    layout_read_complete: bool,
    /// `Some(subscribe_layout)` until the first recv-arm drain. Bootstrap
    /// outbound traffic waits so a last-pane `RESOURCE_CLOSED` already
    /// buffered is applied first (writing earlier raced it into `BrokenPipe`).
    bootstrap_outbound: Option<bool>,
    /// Request-id allocator for L3 GET correlation.
    next_request_id: u32,
    /// Kills in flight for spawned satellite panes whose attach was refused.
    orphan_kills: super::orphans::OrphanKills,
    /// Per-identity review status; survives session switches.
    review: crate::attach::review::ReviewIndex,
    /// Whether the server supports `KILL_RESOURCE_IF`, used to retry stray
    /// satellite panes.
    conditional_kill_supported: bool,
    /// The `LIST_DIRECTORY` the directory picker is waiting on, with the host
    /// it reads; a reply with any other id is stale and dropped.
    pending_directory: Option<crate::attach::directory_picker::PendingDirectory>,
    /// Pane cwd + branch memo behind the sidebar's branch line.
    vcs: VcsIndex,
    /// Everything derived from the config file; the reloadable subset is
    /// swapped whole by `reload_config`.
    settings: TuiSettings,
    /// Plugin-action tasks report completion here.
    plugin_tx: tokio::sync::mpsc::UnboundedSender<PluginRunResult>,
    /// Receiving half; the `plugin_rx` arm toasts failures.
    plugin_rx: tokio::sync::mpsc::UnboundedReceiver<PluginRunResult>,
    /// A pane's QUIC stream bound after the bootstrap reflow skipped it
    /// (it had none yet); the next outcome drain sizes every restored leaf.
    bind_reflow_owed: bool,
    /// ADR-0140: the hosts provider's answers. The sender is kept here so
    /// the channel stays open (and the arm quiet) when no provider runs; the
    /// provider task stops once this receiver is dropped.
    _hosts_tx: tokio::sync::mpsc::UnboundedSender<Vec<phux_core::host_list::HostJson>>,
    hosts_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<phux_core::host_list::HostJson>>,
    /// phux-8n4w: bar ticks left to surface a background update check. The
    /// production entry kicks `attach::update_notice::spawn_background_refresh`
    /// before the loop; this poll reads its cache for a handful of ticks so a
    /// check that lands mid-session still reaches the user, without a second
    /// channel or a network call on the input loop.
    update_poll_ticks: u8,
    /// The window-strip painter; caches so an unchanged repaint emits nothing.
    sidebar_painter: SidebarPainter,
    /// Overlay stack. While active, keys route to the overlay and pane
    /// flushes are suppressed (ADR-0020).
    overlays: OverlayState,
    /// Client-local return point for attention navigation; resets on re-attach.
    attention_navigation: AttentionNavigation,
    /// ADR-0048 in-flight divider drag (spans dispatch batches).
    drag: Option<DragGrab>,
    /// Panes opted out of mouse (`set-pane mouse off`): no `INPUT_MOUSE`, and
    /// outer mouse tracking drops while one is focused. Client-local.
    mouse_optout: HashSet<ResourceId>,
    /// Runtime sidebar on/off (`toggle-sidebar`), carried across switches.
    sidebar_enabled: bool,
    /// `[sidebar] width` as configured; a switch carries the live width only
    /// when it differs.
    configured_sidebar_width: u16,
    /// The outer-terminal viewport, updated on SIGWINCH.
    viewport_dims: (u16, u16),
    /// Host per-cell pixel size for `INPUT_MOUSE` pixel scaling, refreshed
    /// with `viewport_dims`.
    cell_px_dims: (u16, u16),
    /// ADR-0145: the server applies per-pane cell size, so this client casts
    /// no window-size vote and sizes panes only with `RESIZE_TERMINAL`.
    sizes_panes_itself: bool,
    /// The cell size every `RESIZE_TERMINAL` carries (`None` when the server
    /// does not apply it or the host reports no pixel metrics).
    resize_cell_px: Option<(u16, u16)>,
    /// In-flight `rename-session` waiting on its `GET_STATE` barrier.
    rename_pending: Option<PendingSessionRename>,
    /// A rename refused by the shared policy before a write was sent.
    /// Drained onto the status bar at the end of the input batch.
    rename_notice: Option<String>,
    /// The peer-session caches the roster and window picker read.
    peers: PeerWatch,
    /// A one-step cross-session pick, resolved once the layout lands (the
    /// resource even without a persisted layout).
    pick: EntryPick,
    /// The outer terminal's key/mouse decoder.
    parser: StdinParser,
    /// The outer terminal's stdin (see [`crate::attach::tty_input`]).
    stdin: TtyInput,
    /// One read's worth of stdin bytes.
    stdin_buf: [u8; 4096],
    /// Decoded input events, reused across reads; always drained by
    /// [`Self::dispatch_batch`].
    input_events: Vec<phux_protocol::input::InputEvent>,
    /// Terminal resize notifications.
    sigwinch: Signal,
    /// SIGINT/SIGTERM/SIGHUP run terminal cleanup before exiting non-zero.
    sigint: Signal,
    /// `kill <pid>` from a sibling tool, supervisor, or wrapper.
    sigterm: Signal,
    /// The controlling terminal going away.
    sighup: Signal,
    /// `true` once this client has asked the server to detach.
    detach_pending: bool,
    /// Bare-ESC deadline, anchored when the parser first went pending so
    /// other arms firing cannot keep restarting it.
    esc_deadline: Option<tokio::time::Instant>,
    /// Frame-rate governor: bursts inside the previous frame's window apply
    /// but withhold the paint until `paint_deadline` (see [`PaintPacer`]).
    pacer: PaintPacer,
    /// Which-key popup deadline, armed when the resolver sits in the
    /// pending-prefix state; anchored like `esc_deadline`.
    which_key_deadline: Option<tokio::time::Instant>,
    /// A committed `switch-session`; the loop exits with `LoopExit::SwitchTo`.
    switch_request: Option<ReattachTarget>,
    /// A committed `reload-config` (also reached via the CLI doorbell).
    reload_request: bool,
    /// ADR-0140: a `switch-host` committed in the last batch, as
    /// `(host, session)`.
    host_switch_request: Option<(String, String)>,
    /// phux-c2td.3: did the server advertise
    /// [`ServerFeature::HostSessions`](phux_protocol::caps::ServerFeature::HostSessions)?
    /// Unset, the driver sends no host-inventory `GET_STATE` at all: an
    /// older server would answer without a `hosts` list, and the picker's
    /// ungrouped shape is the honest rendering of "this client cannot know
    /// what else is out there".
    host_sessions_supported: bool,
    whoami_supported: bool,
    /// An action wants a fresher host inventory; drained into one `GET_STATE`.
    host_refresh_request: bool,
    /// When to next ask which down satellites are back; anchored.
    satellite_probe_at: Option<tokio::time::Instant>,
    /// A fresh host inventory landed; an open session picker needs rebuilding.
    session_picker_dirty: bool,
    /// First-use moment for this entry; `None` on session switches.
    onboarding_claim: Option<AttachClaim>,
}

impl SessionLoop {
    pub(super) fn set_control_dial(&mut self, dial: crate::attach::Dial) {
        self.control_dial = Some(Box::new(dial));
    }

    /// Allocate the next L3/command correlation id.
    const fn take_request_id(&mut self) -> u32 {
        let id = self.next_request_id;
        self.next_request_id = id.wrapping_add(1);
        id
    }

    /// Put `notices` on the status bar, or trace them when there is no bar.
    /// True when any was shown.
    fn show_notices(&mut self, notices: impl IntoIterator<Item = Notice>) -> bool {
        let now = std::time::Instant::now();
        let mut shown = false;
        for notice in notices {
            if let Some(sb) = self.settings.status_bar.as_mut() {
                shown |= sb.set_notice(notice, now);
            } else {
                tracing::info!(
                    severity = ?notice.severity,
                    text = %notice.text,
                    "status-bar notice dropped: no status bar configured",
                );
            }
        }
        shown
    }

    /// Build every session-scoped local for one attach entry.
    /// `carried_sidebar` is the sidebar state carried by a `switch-session`.
    #[allow(
        clippy::too_many_lines,
        reason = "single constructor keeps all session-loop ownership visible"
    )]
    pub(super) fn new(
        negotiated: NegotiatedBootstrap,
        predict_cfg: PredictiveConfig,
        wants_state_sync: bool,
        onboarding_claim: Option<AttachClaim>,
        initial_pick: EntryPick,
        carried_sidebar: Option<CarriedSidebar>,
    ) -> Result<Self, AttachError> {
        let history_config = HistoryCacheConfig {
            request_max_bytes: negotiated.limits.max_history_page_bytes(),
            ..HistoryCacheConfig::default()
        };
        let mut settings = TuiSettings::load_tolerant();
        let configured_sidebar_width = settings.sidebar.width;
        if let Some(width) = carried_sidebar.and_then(|carried| carried.width) {
            settings.sidebar.width = width;
        }
        let server_features = negotiated.server_features;
        let (plugin_tx, plugin_rx) = tokio::sync::mpsc::unbounded_channel::<PluginRunResult>();
        let (hosts_tx, hosts_rx) = tokio::sync::mpsc::unbounded_channel();
        let origin = crate::attach::hosts::recorded_origin();
        if origin.is_some() {
            crate::attach::hosts::spawn_provider(&settings.hosts, hosts_tx.clone());
        }
        // phux-huhi: stamp the configured breakpoints once, before anything
        // can be pushed. `OverlayState::push` hands them to each overlay from
        // here, so no overlay construction site names a threshold.
        let mut overlays = OverlayState::new();
        overlays.set_breakpoints(settings.chrome);
        let viewport_dims = current_viewport().map_or((80, 24), |v| (v.cols.max(1), v.rows.max(1)));
        let cell_px_dims = current_viewport().map_or(HOST_CELL_PX_FALLBACK, |v| host_cell_px(&v));
        let sizes_panes_itself = negotiated
            .server_features_ext
            .contains(phux_protocol::ServerFeatureExt::ResizeCellPx);
        let resize_cell_px = current_viewport()
            .ok()
            .and_then(|v| resize_cell_px(sizes_panes_itself, &v));
        let conditional_kill_supported = server_features.contains(ServerFeature::ConditionalKill);
        let mut orphan_kills = super::orphans::OrphanKills::default();
        orphan_kills.set_conditional_kill(conditional_kill_supported);
        Ok(Self {
            control_dial: None,
            acknowledged_input_supported: server_features
                .contains(ServerFeature::AcknowledgedInput),
            input_replay: None,
            delivery_fence_paint_pending: HashSet::new(),
            spawn_initial_size_supported: server_features.contains(ServerFeature::SpawnInitialSize),
            directory_support: crate::attach::directory_picker::DirectorySupport::from_features(
                server_features,
            ),
            path: path_picker::PickerState::new(negotiated.server_features_ext),
            host_sessions_supported: server_features.contains(ServerFeature::HostSessions),
            whoami_supported: server_features.contains(ServerFeature::Whoami),
            host_refresh_request: false,
            satellite_probe_at: None,
            session_picker_dirty: false,
            wants_state_sync,
            pending_attach_ready: None,
            pending_stream_binds: HashMap::new(),
            mirror: SessionMirror::new(
                SessionKernel::with_history_config(
                    GhosttyAdapter::new(negotiated.limits),
                    negotiated.profile,
                    history_config,
                ),
                PredictionState::new(predict_cfg, 80, 24),
            ),
            focus_history: crate::attach::focus::FocusHistory::default(),
            own_client_id: None,
            layout_get_request_id: None,
            layout_read_complete: false,
            bootstrap_outbound: None,
            next_request_id: 1,
            orphan_kills,
            review: crate::attach::review::ReviewIndex::new(),
            conditional_kill_supported,
            pending_directory: None,
            vcs: VcsIndex::default(),
            sidebar_painter: SidebarPainter::new(settings.theme)
                .with_plugin_specs(settings.plugin_sidebar.clone()),
            plugin_tx,
            plugin_rx,
            bind_reflow_owed: false,
            _hosts_tx: hosts_tx,
            hosts_rx,
            update_poll_ticks: UPDATE_POLL_TICKS,
            overlays,
            attention_navigation: AttentionNavigation::default(),
            drag: None,
            mouse_optout: HashSet::new(),
            configured_sidebar_width,
            sidebar_enabled: seed_sidebar_enabled(
                carried_sidebar.map(|carried| carried.enabled),
                settings.sidebar.enabled,
            ),
            viewport_dims,
            cell_px_dims,
            sizes_panes_itself,
            resize_cell_px,
            rename_pending: None,
            rename_notice: None,
            peers: PeerWatch {
                sweep_pending: true,
                remote_hosts: if origin.is_some() {
                    crate::attach::hosts::last_known()
                } else {
                    Vec::new()
                },
                origin,
                ..PeerWatch::default()
            },
            pick: initial_pick,
            parser: StdinParser::new(),
            stdin: TtyInput::open(),
            stdin_buf: [0u8; 4096],
            input_events: Vec::new(),
            sigwinch: signal(SignalKind::window_change()).map_err(AttachError::Io)?,
            sigint: signal(SignalKind::interrupt()).map_err(AttachError::Io)?,
            sigterm: signal(SignalKind::terminate()).map_err(AttachError::Io)?,
            sighup: signal(SignalKind::hangup()).map_err(AttachError::Io)?,
            detach_pending: false,
            esc_deadline: None,
            pacer: PaintPacer::default(),
            which_key_deadline: None,
            switch_request: None,
            reload_request: false,
            host_switch_request: None,
            onboarding_claim,
            settings,
        })
    }

    /// The `exec` widget feeds the driver spawns bounded interval runners for.
    pub(super) fn exec_feeds(&self) -> Vec<phux_config::widget::ExecFeed> {
        self.settings
            .status_bar
            .as_ref()
            .map(StatusBarPainter::exec_feeds)
            .unwrap_or_default()
    }
}

mod bootstrap;
mod frames;
mod input;
mod inventory;
mod paint;
mod panes;
mod peers;
mod step;
