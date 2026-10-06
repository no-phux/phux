//! The attached session's long-lived state and the `tokio::select!` step
//! that drives it.
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

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use phux_client_core::engine::ghostty::GhosttyAdapter;
use phux_client_core::history::HistoryCacheConfig;
use phux_client_core::session::SessionKernel;
use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::{ClientId, ResourceId, SatelliteHost};
use phux_protocol::wire::frame::{
    AttachTarget, CONFIG_RELOAD_KEY, Command, CommandResult, CommandValue, FrameKind,
    SESSION_NAME_KEY, Scope,
};
use tokio::signal::unix::{Signal, SignalKind, signal};

use crate::attach::actions::{ParkedAdopt, PendingSplit, PendingWindow};
use crate::attach::connection::{Connection, NegotiatedBootstrap};
use crate::attach::input::StdinParser;
use crate::attach::input_dispatch::{
    DispatchCtx, DragGrab, PendingSessionRename, ReattachTarget, dispatch_input_events,
    encode_layout_or_log, sync_overlays_to_focused_pane,
};

/// The QUIC connection keeps one of its 128 bidi streams for control.
const MAX_PENDING_STREAM_BINDS: usize = 127;
use crate::attach::chrome_ctx::{ChromeCtx, PaneScene};
use crate::attach::onboarding::{AttachClaim, AttachMoment};
use crate::attach::outcome::{AttachEnd, AttachError};
use crate::attach::paint::{
    SidebarReservation, StatusBarPaint, content_rect, paint_chrome_in_place, paint_full_frame,
    sidebar_reservation,
};
use crate::attach::pane_state::{AttentionNavigation, VcsIndex, reanchor_predict_to_pane};
use crate::attach::path_picker;
use crate::attach::plugin_actions::{self, PluginRunResult};
use crate::attach::repaint::{PaintPacer, RepaintAccumulator, RepaintLevel};
use crate::attach::server_frame::{
    FrameEnv, FrameOutcome, attach_participants, handle_server_frame,
};
use crate::attach::session_mirror::SessionMirror;
use crate::attach::tty_input::TtyInput;
use crate::predict::{PredictionState, PredictiveConfig};
use crate::render::chrome::sidebar::SidebarPainter;
use crate::render::chrome::status_bar::{Notice, StatusBarPainter};
use crate::render::overlay::{OverlayState, ToastOverlay};
use crate::settings::TuiSettings;
use phux_client::layout_ops::{DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, layout_key};

use super::chrome::{mark_focused_seen, refresh_window_chrome};

use super::config_ui::{
    adopt_config_reload, apply_initial_notice, push_which_key_overlay, update_which_key_deadline,
};
use super::entry::{
    CarriedSidebar, EntryPick, LoopExit, detached_loop_exit, finish_onboarding_claim,
    finish_return_onboarding_after_paint, seed_sidebar_enabled,
};
use super::main_loop::{
    FRAME_COALESCE_CAP, coalesce_defer_flags, frame_defers_paint, frame_paint_target,
};
use super::overlay_paint::paint_active_overlay;
use super::session_io::{peer_gone, send_attach, send_unless_peer_gone, should_emit_frame_ack};
use super::subscriptions::{PeerWatch, sync_agent_meta_subscriptions};
use super::terminal::{
    desired_mouse_capture, sync_hover_tracking, sync_mouse_capture, terminal_reset_on_signal,
};
use super::viewport::{
    HOST_CELL_PX_FALLBACK, current_viewport, current_viewport_or_default,
    emit_bootstrap_workspace_reflow, emit_moved_tiles, emit_view_reflow, host_cell_px,
    resize_cell_px, view_rects, viewport_resize_frame,
};

#[cfg(test)]
#[path = "loop_state_tests.rs"]
mod tests;

/// How long a host-inventory `GET_STATE` may stay unanswered before the
/// notices held for it surface anyway (hub relay deadline 30 s + margin).
const HOST_INVENTORY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(35);

/// How long a grey satellite pane waits before asking the hub which hosts
/// are back. Anchored so busy output cannot postpone it.
const SATELLITE_PROBE_INTERVAL: Duration = Duration::from_secs(5);

/// Bar ticks (1 s each) to keep polling the background update check's cache.
const UPDATE_POLL_TICKS: u8 = 8;

/// Rename the matching cached session in place. Identity (`SessionId`) is
/// unchanged; only the display label moves.
fn apply_graph_rename(
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
fn unexplained_unreachable_notices(
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

fn unreachable_row_explains(row: &phux_protocol::wire::info::HostInventory, notice: &str) -> bool {
    let Some(diagnostic) = row.unreachable.as_deref() else {
        return false;
    };
    diagnostic == notice || notice.starts_with(&format!("satellite {} is unreachable", row.host))
}

/// Held `SatelliteUnreachable` diagnostics as status notices.
fn federation_notices(messages: Vec<String>) -> Vec<Notice> {
    messages
        .into_iter()
        .map(|message| Notice::warn(format!("federation degraded: {message}")))
        .collect()
}

/// What one host inventory said about each satellite.
#[derive(Debug, Default)]
struct HostAnswers {
    /// Satellites the inventory reached.
    reachable: Vec<SatelliteHost>,
    /// Satellites it could not list.
    unreachable: Vec<SatelliteHost>,
}

fn host_answers(rows: &[phux_protocol::wire::info::HostInventory]) -> HostAnswers {
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
fn spawned_satellite_panes(parked: &[ParkedAdopt]) -> Vec<ResourceId> {
    parked
        .iter()
        .filter_map(ParkedAdopt::pane)
        .cloned()
        .collect()
}

/// The panes this client spawned for windows/splits still awaiting attach.
fn parked_spawned_panes(
    windows: &HashMap<u32, PendingWindow>,
    splits: &HashMap<u32, PendingSplit>,
) -> Vec<crate::attach::actions::SpawnedPane> {
    let windows = windows.values().filter_map(PendingWindow::spawned_pane);
    let splits = splits.values().filter_map(|split| split.adopt.as_ref());
    windows.chain(splits).cloned().collect()
}

/// Request ids of windows and splits whose spawn has not answered yet.
fn unanswered_spawns(
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
fn host_inventory_overdue(since: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    since.is_some_and(|sent| now.saturating_duration_since(sent) > HOST_INVENTORY_DEADLINE)
}

/// Window before a parser-pending bare ESC is taken as the Escape key. The
/// outer terminal writes a key's full sequence in one burst, so a short window
/// suffices; it must stay short because modal-editor users pay it on every
/// Escape (tmux ships `escape-time 0..10` for the same reason).
const ESC_FLUSH_IDLE: Duration = Duration::from_millis(10);

/// Safety valve for an application that enters DEC synchronized output and
/// never leaves it. Normal TUI transactions last milliseconds.
const SYNC_OUTPUT_WATCHDOG: Duration = Duration::from_secs(1);

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
enum FrameStep {
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
fn sleep_for_or_pending(interval: Option<Duration>) -> impl Future<Output = ()> {
    // Anchor relative delays when armed, not when select first polls them.
    let deadline = interval.map(|interval| tokio::time::Instant::now() + interval);
    sleep_until_or_pending(deadline)
}

/// Restore the terminal explicitly (Drop wouldn't fire on `exit()`), then
/// exit with the shell-conventional code for the signal.
#[allow(clippy::exit, reason = "signal-driven graceful exit; Drop won't run")]
fn exit_on_signal(code: i32) -> ! {
    terminal_reset_on_signal();
    std::process::exit(code);
}

/// Drain every frame already queued so an output burst applies all its writes
/// and paints once, on the final frame. Stops the moment the socket would block.
fn drain_frame_batch(
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
const fn fleet_projection_dirty(outcome: &FrameOutcome) -> bool {
    outcome.chrome_dirty
        || outcome.agent_meta_changed
        || outcome.layout_replaced
        || outcome.reflow_panes
        || outcome.sessions.is_some()
}

/// Warn notices for every replay report that did not end in delivery.
fn undelivered_notices(
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

    // ---- small shared projections -------------------------------------

    /// The per-frame sidebar reservation; `None` keeps the full viewport.
    const fn sidebar(&self) -> Option<SidebarReservation> {
        sidebar_reservation(
            self.viewport_dims.0,
            self.sidebar_enabled,
            self.settings.sidebar.width,
            self.settings.sidebar.edge,
            self.settings.chrome.min_pane_cols,
        )
    }

    /// The row the status bar reserves, if any.
    fn bar(&self) -> Option<crate::render::chrome::status_bar::Position> {
        self.settings
            .status_bar
            .as_ref()
            .map(StatusBarPainter::position)
    }

    /// The residual rect panes tile into once the bar and strip are folded off.
    fn content(&self, sidebar: Option<SidebarReservation>) -> crate::layout::Rect {
        content_rect(self.viewport_dims, self.bar(), sidebar)
    }

    /// The single chrome-refresh chokepoint, with this driver's inputs bound.
    fn refresh_chrome(&mut self) -> bool {
        let rows = crate::attach::agent_rows::agent_session_rows(&self.mirror.engine_kernel);
        self.review
            .observe_streams(&rows, self.mirror.focused_resource.as_ref());
        let mut changed = self.project_window_chrome(self.sidebar(), &rows);
        let (tab_drop, sidebar_drop) = self.drag.as_ref().map_or((None, None), |drag| {
            (drag.tab_drop_at(), drag.sidebar_drop_at())
        });
        if let Some(status_bar) = self.settings.status_bar.as_mut() {
            changed |= status_bar.set_drop_index(tab_drop);
        }
        changed |= self.sidebar_painter.set_drop_index(sidebar_drop);
        changed
    }

    /// Project the window tabs, agent rows, and peer zones into the chrome
    /// painters; true when any painter input changed.
    fn project_window_chrome(
        &mut self,
        sidebar: Option<SidebarReservation>,
        rows: &crate::attach::agent_rows::AgentSessionRows,
    ) -> bool {
        let mut chrome = ChromeCtx::new(
            &mut self.settings,
            &mut self.sidebar_painter,
            &self.mirror.session_name,
            self.viewport_dims,
            sidebar,
        );
        let scene = PaneScene {
            workspace: &self.mirror.workspace,
            panes: &self.mirror.panes,
            focused: self.mirror.focused_resource.as_ref(),
            zoomed: self.mirror.zoomed.as_ref(),
        };
        refresh_window_chrome(
            &mut chrome,
            scene,
            self.own_client_id,
            &self.mirror.agent_meta,
            &mut self.vcs,
            rows,
            self.peers.inputs(&self.review),
        )
    }

    /// Commit attach onboarding once its notice has reached the render sink.
    fn finish_paint(&mut self, painted: StatusBarPaint) {
        finish_return_onboarding_after_paint(
            &mut self.onboarding_claim,
            self.settings.status_bar.as_ref(),
            painted,
        );
    }

    /// Paint the view at `level` (`Chrome` in place, `Full` recomposite),
    /// unless an overlay owns the screen.
    fn repaint_view<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        level: RepaintLevel,
    ) {
        if self.overlays.is_active() {
            return;
        }
        if let Some(painted) = self.paint_view(out, sidebar, level) {
            self.finish_paint(painted);
            if matches!(level, RepaintLevel::Full) {
                self.clear_visible_delivery_fences_after_paint();
            }
        }
    }

    /// The paint half of [`Self::repaint_view`]. `None` ⇒ there was no window
    /// to render.
    fn paint_view<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        level: RepaintLevel,
    ) -> Option<StatusBarPaint> {
        let Some(ls) = self
            .mirror
            .workspace
            .render_window(self.mirror.zoomed.as_ref())
        else {
            return self.paint_empty_state(out, sidebar, level);
        };
        let mut chrome = ChromeCtx::new(
            &mut self.settings,
            &mut self.sidebar_painter,
            &self.mirror.session_name,
            self.viewport_dims,
            sidebar,
        );
        let focus = self.mirror.paint_focus();
        let focused = focus.as_ref();
        Some(match level {
            RepaintLevel::None => StatusBarPaint::NotPublished,
            RepaintLevel::Chrome => {
                paint_chrome_in_place(out, ls.as_ref(), &self.mirror.panes, focused, &mut chrome)
            }
            RepaintLevel::Full => paint_full_frame(
                out,
                ls.as_ref(),
                &mut self.mirror.panes,
                &self.mirror.engine_kernel,
                focused,
                &mut chrome,
            ),
        })
    }

    /// ADR-0105: a keep-empty session with no window paints its empty state.
    fn paint_empty_state<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        level: RepaintLevel,
    ) -> Option<StatusBarPaint> {
        let showing = self.mirror.keep_empty_session && self.mirror.workspace.windows.is_empty();
        if !showing || matches!(level, RepaintLevel::None) {
            return None;
        }
        let new_window = phux_config::keybind::ResolvedAction {
            action: "new-window".to_owned(),
            args: std::collections::BTreeMap::new(),
        };
        let chord = crate::attach::action_registry::bound_chord_for(
            self.settings.keybindings.as_ref(),
            &new_window,
        );
        let lines = crate::attach::paint::empty_session_lines(chord.as_deref());
        let mut chrome = ChromeCtx::new(
            &mut self.settings,
            &mut self.sidebar_painter,
            &self.mirror.session_name,
            self.viewport_dims,
            sidebar,
        );
        Some(crate::attach::paint::paint_empty_session(
            out,
            &mut chrome,
            &lines,
        ))
    }

    /// Paint the active overlay layer over the current pane composition.
    fn paint_overlay<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        let painted = self.paint_overlay_layer(out, sidebar);
        self.finish_paint(painted);
    }

    /// The paint half of [`Self::paint_overlay`]; committing the onboarding
    /// claim is the caller's.
    fn paint_overlay_layer<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) -> StatusBarPaint {
        let base = self
            .mirror
            .workspace
            .render_window(self.mirror.zoomed.as_ref());
        let mut chrome = ChromeCtx::new(
            &mut self.settings,
            &mut self.sidebar_painter,
            &self.mirror.session_name,
            self.viewport_dims,
            sidebar,
        );
        let focus = self.mirror.paint_focus();
        paint_active_overlay(
            out,
            &self.overlays,
            base.as_deref(),
            &mut self.mirror.panes,
            &self.mirror.engine_kernel,
            focus.as_ref(),
            &mut chrome,
        )
    }

    /// Repaint the overlay stack when the live overlay tagged `key` took the
    /// fresh `items`; a no-op when it is not open.
    fn refresh_live_overlay<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        key: &str,
        items: &[crate::render::overlay::SelectItem],
    ) {
        if self.overlays.refresh_items(key, items) {
            self.paint_overlay(out, sidebar);
        }
    }

    /// Open the directory picker on the listing the latest `go-to-directory`
    /// asked for; a reply to an older request is dropped.
    fn open_directory_picker<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        reply: Option<(u32, phux_protocol::wire::frame::DirectoryListingResult)>,
    ) {
        let Some((request_id, result)) = reply else {
            return;
        };
        let Some(pending) = self
            .pending_directory
            .take_if(|pending| pending.request_id == request_id)
        else {
            tracing::debug!(request_id, "dropping stale DIRECTORY_LISTING");
            return;
        };
        let picker = crate::render::overlay::SelectList::new(
            crate::attach::directory_picker::picker_title(&result, &pending.host),
            crate::attach::directory_picker::picker_items(&result, &pending.host),
            &self.settings.theme,
        );
        // Only over its own placeholder: if the user cancelled it or opened
        // something else on top, the reply is dropped.
        if !self.overlays.replace_pending(request_id, Box::new(picker)) {
            tracing::debug!(request_id, "dropping DIRECTORY_LISTING: placeholder gone");
            return;
        }
        self.paint_overlay(out, sidebar);
    }

    /// Re-run the layered config loader and swap the config-derived state in
    /// place; failures keep the previous config and toast the error.
    fn reload_config<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        let reloaded = adopt_config_reload(
            &mut self.settings,
            &mut self.overlays,
            &mut self.sidebar_painter,
            &phux_config::loader::config_path(),
        );
        if reloaded {
            // A reload rebuilds the sidebar painter cache-cold, so the
            // cross-session zones must be re-projected with it or the strip
            // comes back with an empty queue and roster until the next push.
            let rows = crate::attach::agent_rows::agent_session_rows(&self.mirror.engine_kernel);
            self.project_window_chrome(sidebar, &rows);
        }
        let has_window = self
            .mirror
            .workspace
            .render_window(self.mirror.zoomed.as_ref())
            .is_some();
        // A failed reload always leaves its toast on top.
        let painted = if self.overlays.is_active() {
            self.paint_overlay_layer(out, sidebar)
        } else if reloaded && has_window {
            self.paint_view(out, sidebar, RepaintLevel::Full)
                .unwrap_or(StatusBarPaint::NotPublished)
        } else {
            StatusBarPaint::NotPublished
        };
        self.finish_paint(painted);
    }

    /// ADR-0040: keep every live pane's `phux.agent/v1` watch in step with the
    /// pane set.
    async fn sync_agent_meta(&mut self, conn: &mut Connection) -> Result<(), AttachError> {
        sync_agent_meta_subscriptions(
            conn,
            self.mirror.panes.keys().cloned().collect(),
            &mut self.mirror.agent_meta,
            &mut self.next_request_id,
        )
        .await
    }

    /// Fetch and subscribe each peer session's persisted layout (for the
    /// picker's one-step rows and the live roster), then the peer agent
    /// records, serving host, and host inventory. Fire-and-forget.
    async fn sweep_peer_layouts(&mut self, conn: &mut Connection) -> Result<(), AttachError> {
        self.peers
            .sweep_layouts(conn, &mut self.next_request_id)
            .await?;
        // Agent watches must not wait for a persisted TUI layout.
        self.reconcile_peer_agents(conn).await?;
        // The fleet's other half. Rides the same deferred
        // sweep, so it costs the first paint nothing.
        self.request_serving_host(conn).await?;
        self.request_host_inventory(conn).await
    }

    /// Read the server's identity after first paint without blocking the frame loop.
    async fn request_serving_host(&mut self, conn: &mut Connection) -> Result<(), AttachError> {
        if !self.whoami_supported || self.peers.serving_host_attempted {
            return Ok(());
        }
        self.peers.serving_host_attempted = true;
        let request_id = self.take_request_id();
        self.peers.serving_host_pending = Some(request_id);
        super::session_io::send_unless_peer_gone(
            conn,
            &FrameKind::GetMetadata {
                request_id,
                scope: Scope::Global,
                key: phux_protocol::wire::frame::WHOAMI_KEY.to_owned(),
            },
        )
        .await
    }

    /// Invalid or unsupported identity leaves the honest `this server` label.
    fn fold_serving_host(&mut self, value: Option<&[u8]>) {
        use phux_protocol::wire::frame::{WHOAMI_SCHEMA_VERSION, WhoamiRecord};
        self.peers.serving_host_pending = None;
        self.peers.serving_host = value
            .and_then(|bytes| serde_json::from_slice::<WhoamiRecord>(bytes).ok())
            .filter(|r| r.schema_version == WHOAMI_SCHEMA_VERSION && !r.host.trim().is_empty())
            // The hosts provider's spelling, so the machine header does not
            // flip between `mac.local` and `mac` when its listing lands.
            .map(|r| crate::attach::hosts::short_host_label(&r.host).to_owned());
        self.peers.chrome_dirty = true;
    }

    /// Ask for the federation host inventory (one `GET_STATE`). Skipped
    /// without the feature or while one is already in flight.
    async fn request_host_inventory(&mut self, conn: &mut Connection) -> Result<(), AttachError> {
        if !self.host_sessions_supported || self.peers.hosts_pending.is_some() {
            return Ok(());
        }
        let request_id = self.take_request_id();
        self.peers.hosts_pending = Some(request_id);
        self.peers.hosts_pending_since = Some(std::time::Instant::now());
        super::session_io::send_unless_peer_gone(
            conn,
            &FrameKind::Command {
                request_id,
                command: Command::GetState {
                    scope: phux_protocol::wire::frame::StateScope::Server,
                },
            },
        )
        .await
    }

    /// Fold a host-inventory reply. A refusal keeps the previous inventory.
    /// Held unreachable notices the reply explains are dropped; the rest
    /// surface. Returns which satellites it reached and which it could not.
    fn fold_host_inventory(
        &mut self,
        result: &phux_protocol::wire::frame::CommandResult,
        repaint: &mut RepaintAccumulator,
    ) -> HostAnswers {
        let held = self.end_host_inventory_request();
        let explained_by: &[phux_protocol::wire::info::HostInventory] = match result {
            phux_protocol::wire::frame::CommandResult::OkWith(
                phux_protocol::wire::frame::CommandValue::State(snapshot),
            ) => {
                self.peers.hosts = snapshot.hosts().to_vec();
                let sessions_changed = self.peers.sessions != snapshot.sessions;
                if sessions_changed {
                    self.peers.sessions.clone_from(&snapshot.sessions);
                }
                // Sweep only when the graph the sweep reads actually moved.
                if sessions_changed || self.snapshot_graph_changed(snapshot) {
                    self.peers.sweep_pending = true;
                }
                self.adopt_snapshot_graph(snapshot);
                self.peers.chrome_dirty = true;
                self.session_picker_dirty = true;
                &self.peers.hosts
            }
            _ => &[],
        };
        let answers = host_answers(explained_by);
        let surfaced = unexplained_unreachable_notices(held, explained_by);
        self.apply_notices(federation_notices(surfaced), repaint);
        answers
    }

    /// Close the in-flight host-inventory request and hand back the
    /// notices held for it.
    fn end_host_inventory_request(&mut self) -> Vec<String> {
        self.peers.hosts_pending = None;
        self.peers.hosts_pending_since = None;
        std::mem::take(&mut self.peers.held_unreachable)
    }

    /// True when a snapshot carries windows or resources not yet adopted.
    /// Empty lists are not a change (inventory replies often omit the graph).
    fn snapshot_graph_changed(
        &self,
        snapshot: &phux_protocol::wire::info::SessionSnapshot,
    ) -> bool {
        (!snapshot.windows.is_empty() && self.peers.windows != snapshot.windows)
            || (!snapshot.resources.is_empty() && self.peers.resources != snapshot.resources)
    }

    /// Cache windows/resources from a snapshot when it actually carries them.
    fn adopt_snapshot_graph(&mut self, snapshot: &phux_protocol::wire::info::SessionSnapshot) {
        if !snapshot.windows.is_empty() {
            self.peers.windows.clone_from(&snapshot.windows);
        }
        if !snapshot.resources.is_empty() {
            self.peers.resources.clone_from(&snapshot.resources);
        }
    }

    /// Fold windows/resources carried on an ATTACHED outcome.
    fn fold_inventory(
        &mut self,
        inventory: Option<(
            Vec<phux_protocol::wire::info::WindowInfo>,
            Vec<phux_protocol::wire::info::ResourceInfo>,
        )>,
    ) {
        let Some((windows, resources)) = inventory else {
            return;
        };
        if !windows.is_empty() {
            self.peers.windows = windows;
        }
        if !resources.is_empty() {
            self.peers.resources = resources;
        }
    }

    /// Apply a `phux.session.name/v1` broadcast to the cached graph and
    /// (when it names this client's session) the status-bar name.
    fn fold_session_rename(
        &mut self,
        outcome: &mut FrameOutcome,
        repaint: &mut RepaintAccumulator,
    ) {
        let Some((current, new_name)) = outcome.session_rename.take() else {
            return;
        };
        apply_graph_rename(&mut self.peers.sessions, &current, &new_name);
        self.peers.chrome_dirty = true;
        self.session_picker_dirty = true;
        self.note_chrome_change(repaint);
    }

    /// The `GET_STATE` barrier after a local rename: the snapshot is
    /// authoritative, so a refused write leaves the current name in place.
    fn confirm_session_rename(&mut self, result: &CommandResult, repaint: &mut RepaintAccumulator) {
        match result {
            CommandResult::OkWith(CommandValue::State(snapshot)) => {
                let Some(pending) = self.rename_pending.take() else {
                    return;
                };
                self.peers.sessions.clone_from(&snapshot.sessions);
                self.adopt_snapshot_graph(snapshot);
                if let Some(id) = pending.session_id.or(self.peers.focused_session) {
                    let roster: Vec<_> = snapshot
                        .sessions
                        .iter()
                        .map(|session| phux_client::rename::NamedSession {
                            id: session.id,
                            name: session.name.as_str(),
                        })
                        .collect();
                    if let Some(info) = snapshot.sessions.iter().find(|session| session.id == id) {
                        self.mirror.session_name.clone_from(&info.name);
                    }
                    if let Some(reason) =
                        phux_client::rename::barrier_verdict(&roster, id, &pending.new_name)
                            .refusal_reason()
                    {
                        self.apply_notices(
                            vec![Notice::warn(format!(
                                "could not rename session to {}: {reason}",
                                pending.new_name
                            ))],
                            repaint,
                        );
                    }
                }
                self.peers.chrome_dirty = true;
                self.session_picker_dirty = true;
                self.note_chrome_change(repaint);
            }
            CommandResult::Error { message, .. } => {
                self.fail_session_rename(message, repaint);
            }
            _ => {
                self.fail_session_rename("the server did not confirm the session rename", repaint);
            }
        }
    }

    /// A refused or unanswered rename barrier: keep the current name.
    fn fail_session_rename(&mut self, message: &str, repaint: &mut RepaintAccumulator) {
        let pending = self.rename_pending.take();
        let text = pending.as_ref().map_or_else(
            || format!("could not rename session: {message}"),
            |pending| {
                format!(
                    "could not rename session to {}: {message}",
                    pending.new_name
                )
            },
        );
        self.apply_notices(vec![Notice::warn(text)], repaint);
    }

    /// Past [`HOST_INVENTORY_DEADLINE`], free the inventory slot and surface
    /// every notice held for it.
    fn expire_overdue_host_inventory(&mut self) {
        let now = std::time::Instant::now();
        if !host_inventory_overdue(self.peers.hosts_pending_since, now) {
            return;
        }
        let held = self.end_host_inventory_request();
        self.show_notices(federation_notices(held));
    }

    /// Hand one inbound frame to the shared server-frame handler.
    fn handle_frame<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        frame: FrameKind,
        sidebar: Option<SidebarReservation>,
        defer_paint: bool,
    ) -> Result<FrameOutcome, AttachError> {
        let retired_terminal = match &frame {
            FrameKind::ResourceClosed { terminal_id, .. } => Some(terminal_id.clone()),
            _ => None,
        };
        let env = FrameEnv {
            focused_session: self.peers.focused_session,
            status_bar: self.settings.status_bar.as_mut(),
            sidebar,
            viewport_dims: self.viewport_dims,
            pending_layout_request: self.layout_get_request_id,
            overlay_active: self.overlays.is_active(),
            defer_paint,
        };
        let mut outcome = handle_server_frame(&mut self.mirror, env, out, frame)?;
        for terminal_id in &outcome.authoritative_damage {
            let fenced = self
                .input_replay
                .as_ref()
                .is_some_and(|journal| journal.borrow().delivery_fenced(terminal_id));
            if fenced {
                self.delivery_fence_paint_pending
                    .insert(terminal_id.clone());
                if outcome.painted_output.as_ref() != Some(terminal_id) {
                    self.pacer.withhold(terminal_id);
                }
            }
        }
        if let Some(terminal_id) = outcome.painted_output.as_ref() {
            self.clear_delivery_fence_after_paint(terminal_id);
        }
        if let Some(terminal_id) = retired_terminal {
            self.review.forget(&terminal_id);
            self.delivery_fence_paint_pending.remove(&terminal_id);
            if let Some(journal) = self.input_replay.as_ref() {
                let reports = journal
                    .borrow_mut()
                    .retire_terminal(&terminal_id, "the terminal closed");
                outcome.notices.extend(undelivered_notices(reports));
            }
        }
        if outcome.layout_get_answered {
            self.layout_read_complete = true;
            self.layout_get_request_id = None;
        }
        Ok(outcome)
    }

    // ---- bootstrap ----------------------------------------------------

    /// Replay the `ATTACHED` frame through `handle_server_frame` and set up
    /// the first paint. `Some(exit)` ⇒ the replayed frame ended the attach.
    pub(super) async fn bootstrap<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        initial_attached: FrameKind,
        initial_notice: Option<Notice>,
    ) -> Result<Option<LoopExit>, AttachError> {
        if conn.multistream_enabled()
            && let FrameKind::Attached { snapshot, .. } = &initial_attached
        {
            for terminal_id in attach_participants(snapshot) {
                conn.bind_terminal(&terminal_id).await?;
            }
        }
        let moment = self
            .onboarding_claim
            .as_ref()
            .map_or(AttachMoment::None, AttachClaim::moment);
        // The sidebar reservation for this bootstrap frame (recomputed
        // per-iteration in the loop below to track `toggle-sidebar`).
        let sidebar = self.sidebar();
        // Single replayed frame — no burst to coalesce, paint it.
        let outcome = self.handle_frame(out, initial_attached, sidebar, false)?;
        if outcome.exit {
            let end = outcome
                .exit_reason
                .unwrap_or(AttachEnd::Detached { reason: None });
            return Ok(Some(detached_loop_exit(end, false)));
        }
        // Defer outbound writes to the recv-arm drain: a last-pane
        // RESOURCE_CLOSED may already be queued and must fold first.
        self.bootstrap_outbound = Some(outcome.subscribe_layout);
        self.vcs.apply_snapshot(outcome.pane_cwds);
        if let Some((list, focused)) = outcome.sessions {
            self.peers.sessions = list;
            self.peers.focused_session = Some(focused);
        }
        self.fold_inventory(outcome.inventory);
        self.resolve_cross_session_pick();
        // The peer sweep is deferred to the first drain (`sweep_pending`) so
        // the first paint never queues behind peer traffic.
        if outcome.own_client_id.is_some() {
            self.own_client_id = outcome.own_client_id;
        }
        // Seed the tab strip and sidebar from the bootstrap layout; peer zones
        // start empty and fill as sweep replies land.
        self.refresh_chrome();
        self.seed_initial_notice(initial_notice, moment);
        self.show_intro(out, sidebar, moment);
        Ok(None)
    }

    /// Size every workspace pane to its current chrome-inset layout. Used
    /// after bootstrap, late stream binding, and every outer viewport vote.
    async fn size_workspace_panes(
        &self,
        conn: &mut Connection,
        sidebar: Option<SidebarReservation>,
    ) -> Result<(), AttachError> {
        emit_bootstrap_workspace_reflow(
            conn,
            &self.mirror.workspace,
            self.mirror.zoomed.as_ref(),
            self.content(sidebar),
            self.resize_cell_px,
        )
        .await?;
        self.size_floating_pane(conn, sidebar).await
    }

    /// ADR-0147: size the floating overlay's PTY to its box interior, which
    /// follows the content rect rather than any layout tile.
    async fn size_floating_pane(
        &self,
        conn: &mut Connection,
        sidebar: Option<SidebarReservation>,
    ) -> Result<(), AttachError> {
        let Some(id) = crate::attach::floating::floating_pane(&self.mirror.panes) else {
            return Ok(());
        };
        let inner = crate::attach::floating::floating_box(self.content(sidebar)).inner;
        if inner.w == 0 || inner.h == 0 || !conn.can_route_terminal(id) {
            return Ok(());
        }
        send_unless_peer_gone(
            conn,
            &FrameKind::ResizeTerminal {
                terminal_id: id.clone(),
                cols: inner.w,
                rows: inner.h,
                cell_px: self.resize_cell_px,
            },
        )
        .await
    }

    /// Open every subscription this attach lives on: agent events, the
    /// config-reload doorbell, the persisted layout key, and each bootstrap
    /// pane's `phux.agent/v1` record.
    async fn subscribe_bootstrap(
        &mut self,
        conn: &mut Connection,
        subscribe_layout: bool,
    ) -> Result<(), AttachError> {
        // Server-scoped (`terminal: None`) so we see control events for every
        // pane, not just one.
        conn.send(&FrameKind::SubscribeEvents {
            terminal: None,
            after_seq: None,
        })
        .await?;
        // The `phux config reload` doorbell.
        conn.send(&FrameKind::SubscribeMetadata {
            scope: Scope::Global,
            key: CONFIG_RELOAD_KEY.to_owned(),
        })
        .await?;
        // ADR-0105: follow the keep-empty mark, so a mark set or cleared after
        // attach still decides whether the last pane's close detaches.
        conn.send(&FrameKind::SubscribeMetadata {
            scope: Scope::Global,
            key: phux_protocol::wire::frame::SESSION_KEEP_EMPTY_KEY.to_owned(),
        })
        .await?;
        // Session renames, so the roster and status name follow them.
        conn.send(&FrameKind::SubscribeMetadata {
            scope: Scope::Global,
            key: SESSION_NAME_KEY.to_owned(),
        })
        .await?;
        if subscribe_layout && let Some(session) = self.peers.focused_session {
            // Fetch and watch this session's persisted layout (best effort).
            let key = layout_key(session);
            let req_id = self.take_request_id();
            self.layout_get_request_id = Some(req_id);
            self.layout_read_complete = false;
            conn.send(&FrameKind::GetMetadata {
                request_id: req_id,
                scope: Scope::Group(DEFAULT_GROUP_ID),
                key: key.clone(),
            })
            .await?;
            conn.send(&FrameKind::SubscribeMetadata {
                scope: Scope::Group(DEFAULT_GROUP_ID),
                key,
            })
            .await?;
        }
        // ADR-0040: read + watch every bootstrap pane's agent record.
        self.sync_agent_meta(conn).await?;
        self.adopt_input_replay(conn).await
    }

    /// Size the bootstrap PTYs and open the attach-lifetime subscriptions,
    /// from the recv-arm drain (see `bootstrap_outbound`).
    pub(super) async fn emit_deferred_bootstrap_outbound(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        let Some(subscribe_layout) = self.bootstrap_outbound.take() else {
            return Ok(());
        };
        let sidebar = self.sidebar();
        self.size_workspace_panes(conn, sidebar).await?;
        self.subscribe_bootstrap(conn, subscribe_layout).await
    }

    /// Install the ADR-0053 replay journal the CLI's reconnect loop owns.
    pub(super) fn set_input_replay(
        &mut self,
        journal: Option<
            std::rc::Rc<std::cell::RefCell<crate::attach::input_replay::InputReplayJournal>>,
        >,
    ) {
        self.input_replay = journal;
    }

    /// ADR-0053: adopt this connection into the acknowledged-input journal.
    /// Survivors are resent under their original operation ids; anything that
    /// cannot be replayed resolves as a status-bar notice.
    async fn adopt_input_replay(&mut self, conn: &mut Connection) -> Result<(), AttachError> {
        let Some(journal) = self.input_replay.clone() else {
            return Ok(());
        };
        let mut reports = journal
            .borrow_mut()
            .begin_connection(conn.server_id(), self.acknowledged_input_supported);
        let (more, replay_frames) = journal.borrow_mut().next_frames(&mut self.next_request_id);
        reports.extend(more);
        self.show_notices(undelivered_notices(reports));
        crate::attach::input_dispatch::send_replay_frames(conn, journal.as_ref(), &replay_frames)
            .await
    }

    /// ADR-0053: the reply to one of the journal's `APPLY_INPUT` attempts;
    /// non-delivery raises a notice and the next queued operation is sent.
    async fn resolve_input_replay(
        &mut self,
        conn: &mut Connection,
        request_id: u32,
        result: &phux_protocol::wire::frame::CommandResult,
        repaint: &mut RepaintAccumulator,
    ) -> Result<(), AttachError> {
        let Some(journal) = self.input_replay.clone() else {
            return Ok(());
        };
        let mut reports: Vec<_> = journal
            .borrow_mut()
            .resolve(request_id, result)
            .into_iter()
            .collect();
        let (more, next_frames) = journal.borrow_mut().next_frames(&mut self.next_request_id);
        reports.extend(more);
        if self.show_notices(undelivered_notices(reports)) {
            repaint.raise_chrome();
        }
        crate::attach::input_dispatch::send_replay_frames(conn, journal.as_ref(), &next_frames)
            .await
    }

    /// Seed the post-reconnect (or return-onboarding) notice now that the bar
    /// painter exists; the first bar paint shows it and the tick expires it.
    fn seed_initial_notice(&mut self, initial_notice: Option<Notice>, moment: AttachMoment) {
        let return_notice_available = initial_notice.is_none() && moment == AttachMoment::Return;
        let initial_notice = initial_notice.or_else(|| {
            return_notice_available.then(|| Notice::info(crate::attach::onboarding::RETURN_NOTICE))
        });
        let notice_accepted =
            apply_initial_notice(self.settings.status_bar.as_mut(), initial_notice);
        if moment == AttachMoment::Return && (!return_notice_available || !notice_accepted) {
            self.onboarding_claim.take();
        }
    }

    /// The introduction toast: passthrough, so the first key dismisses it and
    /// still reaches the resolver/pane.
    fn show_intro<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        moment: AttachMoment,
    ) {
        if moment != AttachMoment::Intro {
            return;
        }
        self.overlays.push(Box::new(ToastOverlay::passthrough(
            crate::attach::onboarding::ONBOARDING_TITLE,
            crate::attach::onboarding::hint_lines(
                self.settings.keybindings.as_ref(),
                self.sidebar_enabled,
            ),
            &self.settings.theme,
        )));
        // The intro commits when its paint reaches the sink, not when a
        // return notice is published, so it skips `finish_paint`.
        self.paint_overlay_layer(out, sidebar);
        let paint_accepted = out.flush().is_ok();
        finish_onboarding_claim(self.onboarding_claim.take(), paint_accepted);
    }

    // ---- one loop iteration -------------------------------------------

    /// Settle the per-iteration view state, then park on every wake-up
    /// source until one of them fires.
    pub(super) async fn step<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        needs_resync: Option<&AtomicBool>,
    ) -> Result<Step, AttachError> {
        let sidebar = self.sidebar();
        self.settle_iteration(out, sidebar, needs_resync)?;
        match self.select_next_event(conn, out, sidebar).await {
            // Any write into a server that already hung up (a SIGWINCH
            // resize racing a server crash, say) defers to the read side,
            // exactly as `send_unless_peer_gone` does: the next `recv`
            // drains what the server sent and then reports the EOF, which is
            // what the reconnect path and the explained endings key on.
            Err(err) if peer_gone(&err) && conn.write_hit_closed_peer() => {
                tracing::debug!(
                    ?err,
                    "write found the server gone; the read side names the ending"
                );
                Ok(Step::Continue)
            }
            stepped => stepped,
        }
    }

    /// Bring the outer terminal's modes, the attention ladder, and any
    /// dropped-backlog resync up to date before the loop parks.
    fn settle_iteration<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        needs_resync: Option<&AtomicBool>,
    ) -> Result<(), AttachError> {
        // Capture follows focus. Closed panes are pruned so a
        // recycled ResourceId can never inherit a stale opt-out.
        if !self.mouse_optout.is_empty() {
            self.mouse_optout
                .retain(|id| self.mirror.panes.contains_key(id));
        }
        self.settle_focus_seen(out, sidebar);
        // Re-derive outer mouse tracking from the focused pane's opt-out every
        // iteration; a no-op when nothing changed.
        let want_capture = desired_mouse_capture(
            self.settings.mouse_capture,
            self.mirror.focused_resource.as_ref(),
            &self.mouse_optout,
        );
        sync_mouse_capture(out, want_capture).map_err(AttachError::Io)?;
        // Hover reporting follows the overlay stack (context menus).
        sync_hover_tracking(out, self.overlays.wants_pointer_hover()).map_err(AttachError::Io)?;
        self.repaint_after_resync(out, sidebar, needs_resync);
        crate::attach::render_prof::tick();
        Ok(())
    }

    /// Mark the focused pane seen (demoting its attention-ladder row) and
    /// repaint chrome in place when that changed a row. Checked every
    /// iteration so every way focus moves is covered.
    fn settle_focus_seen<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        if !mark_focused_seen(
            &mut self.mirror.panes,
            &mut self.review,
            self.mirror.focused_resource.as_ref(),
        ) {
            return;
        }
        if self.refresh_chrome() {
            self.repaint_view(out, sidebar, RepaintLevel::Chrome);
        }
    }

    /// The stdout writer dropped a stale backlog: repaint from scratch.
    fn repaint_after_resync<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        needs_resync: Option<&AtomicBool>,
    ) {
        if !needs_resync.is_some_and(|flag| flag.swap(false, Ordering::AcqRel)) {
            return;
        }
        // The writer dropped bytes the renderers believe landed,
        // so no front buffer describes the screen any more.
        crate::attach::pane_state::invalidate_all_fronts(&mut self.mirror.panes);
        if self.overlays.is_active() {
            self.paint_overlay(out, sidebar);
        } else {
            self.repaint_view(out, sidebar, RepaintLevel::Full);
        }
    }

    /// Arm this iteration's timers and park on every wake-up source. Stdin is
    /// polled before inbound frames; both arms are bounded, so neither starves.
    async fn select_next_event<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) -> Result<Step, AttachError> {
        // Arm the bare-ESC timer only while a lone ESC is pending, anchored to
        // the first iteration that saw it.
        if self.parser.esc_pending() {
            self.esc_deadline
                .get_or_insert_with(|| tokio::time::Instant::now() + ESC_FLUSH_IDLE);
        } else {
            self.esc_deadline = None;
        }
        let flush_sleep = sleep_until_or_pending(self.esc_deadline);
        // (Dis)arm which-key from the resolver's current pending state.
        update_which_key_deadline(
            &mut self.which_key_deadline,
            self.settings
                .resolver
                .as_ref()
                .is_some_and(phux_config::keybind::Resolver::pending_at_prefix),
            self.settings.which_key.enabled,
            self.overlays.is_active(),
            tokio::time::Instant::now(),
            self.settings.which_key.delay,
        );
        let which_key_sleep = sleep_until_or_pending(self.which_key_deadline);
        // Status-bar repaint cadence; never fires for a bar with no polling widget.
        let status_tick = sleep_for_or_pending(
            self.settings
                .status_bar
                .as_ref()
                .and_then(StatusBarPainter::min_poll_interval),
        );
        // The pacer's settle deadline, armed only while a paint is owed.
        let paint_sleep = sleep_until_or_pending(self.pacer.deadline());
        let sync_output_sleep = sleep_until_or_pending(
            self.mirror
                .panes
                .values()
                .filter_map(|slot| slot.sync_output_since)
                .map(|since| since + SYNC_OUTPUT_WATCHDOG)
                .min(),
        );
        // Down satellite panes probe the host inventory on a fixed cadence.
        self.arm_satellite_probe(tokio::time::Instant::now());
        let satellite_probe = sleep_until_or_pending(self.satellite_probe_at);

        tokio::select! {
            biased;

            n = self.stdin.read(&mut self.stdin_buf) => {
                let started = std::time::Instant::now();
                let result = self.on_stdin(conn, out, sidebar, n).await;
                phux_client::perf::INPUT_WALL.record_elapsed(started);
                result
            },

            // Inbound frames, drained in a bounded batch so a burst paints once.
            frame = conn.recv() => {
                let started = std::time::Instant::now();
                let result = self.on_server_frame(conn, out, sidebar, frame).await;
                phux_client::perf::FRAMES_WALL.record_elapsed(started);
                result
            },

            // The pacer window expired: settle every withheld pane in one frame.
            () = paint_sleep => {
                self.settle_withheld_panes(out, sidebar);
                Ok(Step::Continue)
            }

            // An application that never sent `?2026l`: expose the mirror once.
            () = sync_output_sleep => {
                self.on_sync_output_timeout(out, sidebar);
                Ok(Step::Continue)
            }

            // Bare-ESC idle timeout.
            () = flush_sleep => self.on_esc_flush(conn, out, sidebar).await,

            // Which-key idle timeout. The pending prefix stays live, so the
            // next chord executes as if the popup never appeared.
            () = which_key_sleep => {
                self.on_which_key_timeout(out, sidebar);
                Ok(Step::Continue)
            }

            // SIGWINCH.
            _ = self.sigwinch.recv() => {
                self.on_resize(conn, out, sidebar).await?;
                Ok(Step::Continue)
            }

            // Periodic status-bar repaint.
            () = status_tick => {
                self.on_status_tick(out, sidebar);
                Ok(Step::Continue)
            }

            // Ask which grey satellite panes can be reattached.
            () = satellite_probe => {
                self.satellite_probe_at = None;
                self.request_host_inventory(conn).await?;
                Ok(Step::Continue)
            }

            // A spawned plugin action finished; failures toast.
            Some(result) = self.plugin_rx.recv() => {
                self.on_plugin_result(out, sidebar, &result);
                Ok(Step::Continue)
            }

            // ADR-0140: the hosts provider answered. Only a changed answer
            // repaints; the chrome drain is the same one a session-graph
            // change takes.
            Some(hosts) = self.hosts_rx.recv() => {
                self.on_hosts(out, sidebar, hosts);
                Ok(Step::Continue)
            }

            // SIGINT — restore the terminal explicitly (Drop wouldn't
            // fire on `exit(130)`), then exit with the shell-conventional
            // 130. `phux-roz`: this is the path that fires when the user
            // hits Ctrl-C in the outer shell after `phux attach` has
            // entered the alt screen.
            _ = self.sigint.recv() => exit_on_signal(130),

            // SIGTERM — `kill <pid>` from a sibling tool, supervisor, or
            // the user's tmux/screen wrapping us. Same cleanup, exit 143.
            _ = self.sigterm.recv() => exit_on_signal(143),

            _ = self.sighup.recv() => exit_on_signal(129),
        }
    }

    // ---- input ---------------------------------------------------------

    /// One stdin read: EOF detaches cleanly, bytes become an input batch.
    async fn on_stdin<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        read: std::io::Result<usize>,
    ) -> Result<Step, AttachError> {
        let n = read.map_err(AttachError::Io)?;
        if n == 0 {
            // Stdin EOF — outer terminal closed. Detach cleanly.
            if !self.detach_pending {
                conn.send(&FrameKind::Detach).await?;
                conn.unbind_all_terminals();
                self.pending_stream_binds.clear();
                self.detach_pending = true;
            }
            return Ok(Step::Continue);
        }
        let mut events = std::mem::take(&mut self.input_events);
        self.parser.feed_into(&self.stdin_buf[..n], &mut events);
        self.dispatch_batch(conn, out, sidebar, events).await
    }

    /// The bare-ESC flush runs the same batch handling as a stdin read.
    async fn on_esc_flush<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) -> Result<Step, AttachError> {
        let mut events = std::mem::take(&mut self.input_events);
        self.parser.flush_into(&mut events);
        self.dispatch_batch(conn, out, sidebar, events).await
    }

    /// Dispatch one batch of decoded input events, then reflow and repaint
    /// whatever it moved. The buffer comes back drained (see `input_events`).
    async fn dispatch_batch<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        mut events: Vec<phux_protocol::input::InputEvent>,
    ) -> Result<Step, AttachError> {
        // Common (a partial CSI, an idle flush); nothing below can change.
        if events.is_empty() {
            self.input_events = events;
            return Ok(Step::Continue);
        }
        // Computed before dispatch drains `events`; armed after, so it keys to
        // the pane focus landed on.
        let expects_reply = events.iter().any(input_expects_a_reply);
        // Pre-dispatch view, so zoom and sidebar toggles, and a resized or
        // rearranged window, can reflow PTYs.
        let prev_zoomed = self.mirror.zoomed.clone();
        let prev_sidebar = sidebar;
        let prev_active_window = self.mirror.workspace.active;
        let prev_view_rects = view_rects(
            &self.mirror.workspace,
            prev_zoomed.as_ref(),
            self.content(sidebar),
            self.viewport_dims,
        );
        // Batch this read's wire writes into one; released before parking.
        conn.cork();
        let dispatched = self.dispatch_input(conn, out, sidebar, &mut events).await;
        // Uncork on both paths so an error never strands emitted frames.
        let shipped = conn.uncork().await;
        self.input_events = events;
        let layout_changed = dispatched?;
        shipped?;
        // Reply grace keyed to the focused pane only, so a flood elsewhere
        // stays paced. Cleared by time, never by a paint.
        if expects_reply {
            self.pacer.note_input(
                self.mirror.focused_resource.as_ref(),
                tokio::time::Instant::now(),
            );
        }
        // A `toggle-sidebar` in this batch takes effect this iteration.
        let sidebar = self.sidebar();
        // A committed `switch-session` ends this loop before any repaint.
        if let Some(target) = self.switch_request.take() {
            return Ok(Step::Exit(LoopExit::SwitchTo {
                target,
                sidebar: CarriedSidebar {
                    enabled: self.sidebar_enabled,
                    width: (self.settings.sidebar.width != self.configured_sidebar_width)
                        .then_some(self.settings.sidebar.width),
                },
                orphan_kills: self.orphans_for_switch(),
                review: std::mem::take(&mut self.review),
            }));
        }
        // Resize PTYs when client-local geometry (zoom, sidebar) changed.
        if self.mirror.zoomed != prev_zoomed || sidebar != prev_sidebar {
            emit_view_reflow(
                conn,
                &self.mirror.workspace,
                self.mirror.zoomed.as_ref(),
                &prev_view_rects,
                self.content(sidebar),
                self.resize_cell_px,
            )
            .await?;
        } else if layout_changed
            && self.mirror.workspace.active == prev_active_window
            && !self.detach_pending
        {
            // This window's own layout moved (`resize-pane`, a divider drag,
            // a swap): resize exactly the panes whose tile changed. A switch
            // to another window is not a resize (bootstrap sized its panes),
            // and nothing may follow a sent `DETACH`.
            let rects = view_rects(
                &self.mirror.workspace,
                self.mirror.zoomed.as_ref(),
                self.content(sidebar),
                self.viewport_dims,
            );
            emit_moved_tiles(conn, &prev_view_rects, &rects, self.resize_cell_px).await?;
        }
        if layout_changed {
            // ADR-0040: an input action may have split/closed panes;
            // keep the agent-metadata watches in step with the set.
            self.sync_agent_meta(conn).await?;
            self.refresh_chrome();
            // Restores pane content under a just-dismissed overlay.
            self.repaint_view(out, sidebar, RepaintLevel::Full);
        }
        if self.overlays.is_active() {
            self.paint_overlay(out, sidebar);
        }
        // One `GET_STATE` per batch when an action wanted a fresher inventory.
        if std::mem::take(&mut self.host_refresh_request) {
            self.request_host_inventory(conn).await?;
        }
        // Last, so the repaint reflects the new theme/bar.
        if self.reload_request {
            self.reload_request = false;
            self.reload_config(out, sidebar);
        }
        // ADR-0140: leave for another machine. Detach first so the server
        // records a detach rather than a dropped client, then restore the
        // terminal exactly as a detach does and become `phux attach` there.
        if let Some((host, session)) = self.host_switch_request.take() {
            let _ = conn.send(&FrameKind::Detach).await;
            super::terminal::restore_terminal_for_handoff();
            crate::attach::hosts::exec_switch_host(&host, &session);
        }
        Ok(Step::Continue)
    }

    /// Build the dispatch context and run the batch through the resolver,
    /// the overlay stack, and the pane input pipe.
    async fn dispatch_input<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        events: &mut Vec<phux_protocol::input::InputEvent>,
    ) -> Result<bool, AttachError> {
        // Only a mouse batch can hit-test the sidebar; skip cloning its
        // targets otherwise.
        let sidebar_targets = if events
            .iter()
            .any(|ev| matches!(ev, phux_protocol::input::InputEvent::Mouse(_)))
        {
            self.sidebar_painter.click_targets()
        } else {
            crate::render::chrome::sidebar::SidebarTargets::default()
        };
        let mut ctx = DispatchCtx {
            control_dial: self.control_dial.as_deref(),
            engine_kernel: &mut self.mirror.engine_kernel,
            resolver: self.settings.resolver.as_mut(),
            focus_history: self.focus_history.clone(),
            workspace: &mut self.mirror.workspace,
            layout_read_complete: self.layout_read_complete,
            viewport: self.viewport_dims,
            cell_px: self.cell_px_dims,
            next_request_id: &mut self.next_request_id,
            spawn_initial_size_supported: self.spawn_initial_size_supported,
            pending_splits: &mut self.mirror.pending_splits,
            pending_windows: &mut self.mirror.pending_windows,
            pending_floating: &mut self.mirror.pending_floating,
            directory_support: self.directory_support,
            pending_directory: &mut self.pending_directory,
            path_query_supported: self.path.supported,
            pending_path: &mut self.path.pending,
            own_client_id: self.own_client_id,
            expected_closes: &mut self.mirror.expected_closes,
            pending_kills: &mut self.mirror.pending_resource_ops,
            overlays: &mut self.overlays,
            keybindings: self.settings.keybindings.as_ref(),
            theme: &self.settings.theme,
            peers: self.peers.inputs(&self.review),
            host_refresh_request: &mut self.host_refresh_request,
            session_name: &mut self.mirror.session_name,
            rename_pending: &mut self.rename_pending,
            rename_notice: &mut self.rename_notice,
            switch_request: &mut self.switch_request,
            detach_pending: &mut self.detach_pending,
            zoomed: &mut self.mirror.zoomed,
            sidebar,
            sidebar_enabled: &mut self.sidebar_enabled,
            sidebar_width: &mut self.settings.sidebar.width,
            chrome: self.settings.chrome,
            sidebar_targets: &sidebar_targets,
            bar: self
                .settings
                .status_bar
                .as_ref()
                .map(StatusBarPainter::position),
            status_bar: self.settings.status_bar.as_ref(),
            drag: &mut self.drag,
            mouse_optout: &mut self.mouse_optout,
            attention_navigation: &mut self.attention_navigation,
            plugin_actions: &self.settings.plugin_actions,
            plugin_panes: &self.settings.plugin_panes,
            plugin_tx: Some(&self.plugin_tx),
            reload_request: &mut self.reload_request,
            host_switch_request: &mut self.host_switch_request,
            agent_meta: &self.mirror.agent_meta.records,
            vcs: &mut self.vcs,
            input_replay: self.input_replay.as_deref(),
        };
        let mut layout_changed = dispatch_input_events(
            out,
            conn,
            events,
            &mut self.mirror.focused_resource,
            &mut self.mirror.predict,
            &mut self.mirror.panes,
            &mut ctx,
        )
        .await?;
        self.focus_history = ctx.focus_history;
        let reports = self
            .input_replay
            .as_ref()
            .map_or_else(Vec::new, |journal| journal.borrow_mut().take_reports());
        layout_changed |= self.show_notices(undelivered_notices(reports));
        if let Some(line) = self.rename_notice.take() {
            layout_changed |= self.show_notices([Notice::warn(line)]);
        }
        Ok(layout_changed)
    }

    // ---- inbound frames -------------------------------------------------

    /// One `recv` wake-up: a frame to handle, or the end of the connection.
    async fn on_server_frame<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        frame: Result<FrameKind, AttachError>,
    ) -> Result<Step, AttachError> {
        match frame {
            Ok(first) => self.handle_frame_burst(conn, out, sidebar, first).await,
            Err(AttachError::Disconnected) if self.detach_pending => {
                // The socket closed after we asked to detach: clean shutdown.
                Ok(Step::Exit(detached_loop_exit(
                    AttachEnd::Detached { reason: None },
                    true,
                )))
            }
            Err(err) => Err(err),
        }
    }

    /// Apply one coalesced burst of inbound frames and paint it exactly once.
    async fn handle_frame_burst<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        first: FrameKind,
    ) -> Result<Step, AttachError> {
        let batch = drain_frame_batch(conn, first)?;
        phux_client::perf::BURST_FRAMES.record(u64::try_from(batch.len()).unwrap_or(u64::MAX));
        if batch.len() == FRAME_COALESCE_CAP {
            phux_client::perf::BURST_CAPPED.incr();
        }
        // Per-pane last-wins: a frame defers its paint iff a later frame in
        // the burst repaints the same pane.
        let defer_flags = coalesce_defer_flags(&batch, frame_paint_target);
        // ADR-0029: triggers raise a level and the accumulator drains once,
        // so a burst of metadata changes costs one chrome paint.
        let mut repaint = RepaintAccumulator::default();
        // One pacing decision per burst: refused, every output frame defers
        // and the `paint_deadline` arm settles them in one composited frame.
        let now = tokio::time::Instant::now();
        let is_reply = self
            .pacer
            .observe_reply(now, batch.iter().filter_map(frame_paint_target));
        let paint_now = self.pacer.admit(now, is_reply);
        for (frame_idx, frame) in batch.into_iter().enumerate() {
            let Some(frame) = self.orphan_kills.observe(frame) else {
                continue;
            };
            if self.coordinate_multistream_frame(conn, &frame).await? {
                self.pending_attach_ready = Some(frame);
                continue;
            }
            let Some(frame) = self.intercept_peer_reply(conn, frame, &mut repaint).await? else {
                continue;
            };
            let defer_paint = !paint_now || frame_defers_paint(defer_flags[frame_idx], &frame);
            if !paint_now && let Some(target) = frame_paint_target(&frame) {
                self.pacer.withhold(target);
            }
            match self
                .apply_server_frame(conn, out, sidebar, frame, defer_paint, &mut repaint)
                .await?
            {
                FrameStep::Done | FrameStep::Rebootstrap => {}
                FrameStep::Exit(exit) => return Ok(Step::Exit(exit)),
            }
            if self.pending_attach_ready.is_some()
                && self.mirror.engine_kernel.attach_ready_pending() == Some(0)
                && let Some(ready) = self.pending_attach_ready.take()
            {
                match self
                    .apply_server_frame(conn, out, sidebar, ready, false, &mut repaint)
                    .await?
                {
                    FrameStep::Done | FrameStep::Rebootstrap => {}
                    FrameStep::Exit(exit) => return Ok(Step::Exit(exit)),
                }
            }
        }
        // Only a burst that did not end the attach may spend.
        self.emit_deferred_bootstrap_outbound(conn).await?;
        self.drain_repaint(out, sidebar, &mut repaint);
        // Settle the pacer's debt here when the timer cannot be trusted (see
        // [`burst_settles_debt`]). After `drain_repaint`, so a full repaint
        // has already cleared what it redrew.
        if burst_settles_debt(
            paint_now,
            self.pacer
                .deadline()
                .is_some_and(|at| at <= tokio::time::Instant::now()),
        ) {
            self.settle_withheld_panes(out, sidebar);
        }
        // The first paint is behind us: send the deferred peer sweep. Gated on
        // reaching the drain, not on a paint, so a quiet attach still sweeps.
        if self.peers.sweep_pending {
            self.peers.sweep_pending = false;
            self.sweep_peer_layouts(conn).await?;
        }
        Ok(Step::Continue)
    }

    /// Apply the QUIC stream lifecycle barriers carried by one control frame.
    /// Returns true when `ATTACH_READY` must be held until every Terminal
    /// stream publishes its own READY/CLOSED outcome.
    async fn coordinate_multistream_frame(
        &mut self,
        conn: &mut Connection,
        frame: &FrameKind,
    ) -> Result<bool, AttachError> {
        if matches!(frame, FrameKind::Attached { .. }) {
            self.pending_attach_ready = None;
            // A new aggregate generation: retired replies must not bind
            // streams or fold leaves.
            self.pending_stream_binds.clear();
            // Replies to the retired generation's commands can no longer be
            // attributed; a stale entry would fold a live leaf.
            self.mirror.pending_resource_ops.clear();
        }
        if let FrameKind::Error {
            request_id: Some(request_id),
            ..
        } = frame
        {
            self.pending_stream_binds.remove(request_id);
        }
        if let FrameKind::CommandResult { request_id, result } = frame
            && let Some(terminal_id) = self.pending_stream_binds.remove(request_id)
        {
            if conn.multistream_enabled()
                && matches!(result, phux_protocol::wire::frame::CommandResult::Ok)
            {
                conn.bind_terminal(&terminal_id).await?;
                // The bootstrap reflow skipped this pane while it had no
                // stream; size it now that it has one.
                self.bind_reflow_owed = true;
            }
            return Ok(false);
        }
        if !conn.multistream_enabled() {
            return Ok(false);
        }
        match frame {
            FrameKind::Attached { snapshot, .. } => {
                for terminal_id in attach_participants(snapshot) {
                    conn.bind_terminal(&terminal_id).await?;
                }
            }
            // A local spawn: bind its Terminal stream before the frame's
            // reflow. Satellite spawns bind after their ATTACH_RESOURCE reply.
            FrameKind::ResourceSpawned { request_id, result }
                if self.mirror.pending_splits.contains_key(request_id)
                    || self.mirror.pending_windows.contains_key(request_id)
                    || self.mirror.pending_floating.contains_key(request_id) =>
            {
                if let Some(terminal_id) = result.spawned_id()
                    && terminal_id.is_local()
                {
                    conn.bind_terminal(terminal_id).await?;
                }
            }
            FrameKind::AttachReady { .. } => {
                return Ok(self
                    .mirror
                    .engine_kernel
                    .attach_ready_pending()
                    .is_some_and(|pending| pending > 0));
            }
            _ => {}
        }
        Ok(false)
    }

    /// Fold a peer-scoped reply into the foreign caches (`None`), or hand the
    /// frame on to the general handler.
    async fn intercept_peer_reply(
        &mut self,
        conn: &mut Connection,
        frame: FrameKind,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Option<FrameKind>, AttachError> {
        let Some(frame) = self
            .intercept_sidebar_metadata(conn, frame, repaint)
            .await?
        else {
            return Ok(None);
        };
        self.intercept_inventory_reply(conn, frame, repaint).await
    }

    /// Correlate the sidebar's metadata reads independently of command replies.
    async fn intercept_sidebar_metadata(
        &mut self,
        conn: &mut Connection,
        frame: FrameKind,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Option<FrameKind>, AttachError> {
        match frame {
            FrameKind::MetadataValue { request_id, value }
                if self.peers.serving_host_pending == Some(request_id) =>
            {
                self.fold_serving_host(value.as_deref());
                Ok(None)
            }
            FrameKind::Error {
                request_id: Some(request_id),
                ..
            } if self.peers.serving_host_pending == Some(request_id) => {
                self.peers.serving_host_pending = None;
                Ok(None)
            }
            // A peer session's persisted-layout GET reply.
            FrameKind::MetadataValue { request_id, value }
                if self.peers.foreign_layout_pending.contains_key(&request_id) =>
            {
                self.fold_peer_layout(conn, request_id, value.as_deref(), repaint)
                    .await?;
                Ok(None)
            }
            // A foreign pane's agent-record GET reply.
            FrameKind::MetadataValue { request_id, value }
                if self.peers.foreign_agent_pending.contains_key(&request_id) =>
            {
                if let Some(id) = self.peers.foreign_agent_pending.remove(&request_id)
                    && self.fold_foreign_agent(&id, value.as_deref())
                {
                    self.peers.chrome_dirty = true;
                    repaint.raise_fleet();
                }
                Ok(None)
            }
            FrameKind::MetadataValue { request_id, value }
                if self.peers.foreign_asked_pending.contains_key(&request_id) =>
            {
                if let Some(id) = self.peers.foreign_asked_pending.remove(&request_id) {
                    let asked = value.as_deref() == Some(b"1");
                    let changed = if asked {
                        self.peers.foreign_attention.insert(id)
                    } else {
                        self.peers.foreign_attention.remove(&id)
                    };
                    if changed {
                        self.peers.chrome_dirty = true;
                        repaint.raise_fleet();
                    }
                }
                Ok(None)
            }
            // A refused peer read (`proto.md` §9): drop the pending entry so
            // the map does not grow and the ERROR does not reach the handler.
            FrameKind::Error {
                request_id: Some(request_id),
                ..
            } if self.peers.foreign_layout_pending.contains_key(&request_id)
                || self.peers.foreign_agent_pending.contains_key(&request_id)
                || self.peers.foreign_asked_pending.contains_key(&request_id) =>
            {
                self.peers.foreign_layout_pending.remove(&request_id);
                self.peers.foreign_agent_pending.remove(&request_id);
                self.peers.foreign_asked_pending.remove(&request_id);
                Ok(None)
            }
            other => Ok(Some(other)),
        }
    }

    /// Inventory and replay command replies share the connection, not projection state.
    async fn intercept_inventory_reply(
        &mut self,
        conn: &mut Connection,
        frame: FrameKind,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Option<FrameKind>, AttachError> {
        match frame {
            FrameKind::CommandResult { request_id, result }
                if self
                    .rename_pending
                    .as_ref()
                    .is_some_and(|pending| pending.barrier == request_id) =>
            {
                self.confirm_session_rename(&result, repaint);
                Ok(None)
            }
            FrameKind::Error {
                request_id: Some(request_id),
                message,
                ..
            } if self
                .rename_pending
                .as_ref()
                .is_some_and(|pending| pending.barrier == request_id) =>
            {
                self.fail_session_rename(&message, repaint);
                Ok(None)
            }
            // The reply to our own host-inventory GET_STATE.
            FrameKind::CommandResult { request_id, result }
                if self.peers.hosts_pending == Some(request_id) =>
            {
                // A satellite this inventory could not list
                // forgets its strays; one it reached gets their kills.
                let asked_at = self.peers.hosts_pending_since;
                let answers = self.fold_host_inventory(&result, repaint);
                self.replay_returned_satellites(conn, &answers).await?;
                self.retry_after_inventory(conn, &answers, asked_at).await?;
                Ok(None)
            }
            // Its refusal: keep the old inventory, free the slot, surface every
            // held notice.
            FrameKind::Error {
                request_id: Some(request_id),
                ..
            } if self.peers.hosts_pending == Some(request_id) => {
                let held = self.end_host_inventory_request();
                self.apply_notices(federation_notices(held), repaint);
                Ok(None)
            }
            // Un-correlated `SatelliteUnreachable` pushes while our inventory is
            // in flight are held (see `PeerWatch::held_unreachable`).
            FrameKind::Error {
                request_id: None,
                code: phux_protocol::wire::frame::ErrorCode::SatelliteUnreachable,
                message,
            } if self.peers.hosts_pending.is_some() => {
                // Grey the panes now. The inventory reply still
                // decides the notice, but the layout slot is already down.
                if crate::attach::pane_state::note_satellite_unreachable(
                    &mut self.mirror.panes,
                    &message,
                ) {
                    self.peers.chrome_dirty = true;
                }
                self.peers.held_unreachable.push(message);
                Ok(None)
            }
            // ADR-0053: the reply to a journal `APPLY_INPUT` attempt.
            FrameKind::CommandResult { request_id, result }
                if self
                    .input_replay
                    .as_ref()
                    .is_some_and(|journal| journal.borrow().owns(request_id)) =>
            {
                self.resolve_input_replay(conn, request_id, &result, repaint)
                    .await?;
                Ok(None)
            }
            other => Ok(Some(other)),
        }
    }

    /// Fold a peer layout reply, then sync that peer's agent-record watches.
    async fn fold_peer_layout(
        &mut self,
        conn: &mut Connection,
        request_id: u32,
        value: Option<&[u8]>,
        repaint: &mut RepaintAccumulator,
    ) -> Result<(), AttachError> {
        let Some(session) = self.peers.foreign_layout_pending.remove(&request_id) else {
            return Ok(());
        };
        self.peers.apply_layout_reply(session, value);
        self.reconcile_peer_agents(conn).await?;
        self.peers.chrome_dirty = true;
        repaint.raise_fleet();
        Ok(())
    }

    /// Prune and re-sync the foreign agent watches against the live foreign
    /// terminal set (from persisted layouts, or the server graph).
    async fn reconcile_peer_agents(&mut self, conn: &mut Connection) -> Result<(), AttachError> {
        self.peers
            .sweep_agents(conn, &mut self.next_request_id, &self.review)
            .await
    }

    /// Hand one frame to the server-frame handler and act on everything its
    /// outcome asks for.
    async fn apply_server_frame<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        frame: FrameKind,
        defer_paint: bool,
        repaint: &mut RepaintAccumulator,
    ) -> Result<FrameStep, AttachError> {
        // Pre-frame, zoom-honoring leaf rects, so a close/spawn can resize
        // survivors whose dims changed.
        let prev_rects = self.leaf_rects(sidebar);
        let focused_before_frame = self.mirror.focused_resource.clone();
        let outcome = self.handle_frame(out, frame, sidebar, defer_paint)?;
        self.focus_history
            .observe(focused_before_frame, self.mirror.focused_resource.as_ref());
        self.focus_history.repair(
            self.mirror.focused_resource.as_ref(),
            &self.mirror.workspace,
        );
        if outcome.exit {
            let end = outcome
                .exit_reason
                .unwrap_or(AttachEnd::Detached { reason: None });
            return Ok(FrameStep::Exit(detached_loop_exit(
                end,
                self.detach_pending,
            )));
        }
        if outcome.resync_required {
            return self.request_rebootstrap(conn).await;
        }
        self.fold_frame_outcome(conn, out, sidebar, outcome, prev_rects.as_ref(), repaint)
            .await?;
        Ok(FrameStep::Done)
    }

    /// Fold one non-terminal frame outcome into the loop: attach what it
    /// discovered, fold peer, rename, watch, and chrome changes, send the
    /// requests it owes, and raise (never paint) what the burst drain must
    /// repaint. `prev_rects` is the pre-frame leaf geometry a reflow diffs.
    async fn fold_frame_outcome<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        mut outcome: FrameOutcome,
        prev_rects: Option<&HashMap<ResourceId, crate::layout::Rect>>,
        repaint: &mut RepaintAccumulator,
    ) -> Result<(), AttachError> {
        self.attach_discovered_panes(conn, &outcome.attach_panes)
            .await?;
        let answered = spawned_satellite_panes(&outcome.adopt_spawned);
        self.attach_spawned_panes(conn, std::mem::take(&mut outcome.adopt_spawned))
            .await?;
        self.settle_orphans(conn, &mut outcome, &answered).await?;
        let fleet_dirty = fleet_projection_dirty(&outcome);
        self.fold_peer_outcome(conn, &mut outcome, repaint).await?;
        self.fold_session_rename(&mut outcome, repaint);
        self.finish_paint(outcome.status_bar_painted);
        self.resync_watches(conn, &mut outcome).await?;
        self.fold_chrome_and_notices(&mut outcome, repaint);
        self.open_directory_picker(out, sidebar, outcome.directory_listing.take());
        let reply = outcome.path_results.take();
        if path_picker::accept_reply(self.path.pending.as_ref(), &mut self.overlays, reply) {
            self.paint_overlay(out, sidebar);
        }
        self.emit_outcome_requests(conn, &mut outcome, sidebar, prev_rects)
            .await?;
        self.settle_frame_view(out, &outcome, sidebar, fleet_dirty, repaint);
        Ok(())
    }

    /// The per-leaf rect map of the zoom- and sidebar-honoring view, or
    /// `None` when there is no window or its tree is unseeded.
    fn leaf_rects(
        &self,
        sidebar: Option<SidebarReservation>,
    ) -> Option<HashMap<ResourceId, crate::layout::Rect>> {
        let ls = self
            .mirror
            .workspace
            .render_window(self.mirror.zoomed.as_ref())?;
        ls.tree.as_ref().map(|_| {
            crate::attach::multi_pane::compute_layout_in(
                ls.as_ref(),
                self.content(sidebar),
                self.viewport_dims,
            )
            .rects
        })
    }

    /// The engine rejected a generation: re-ATTACH in-connection while the
    /// frozen replica stays visible.
    async fn request_rebootstrap(
        &mut self,
        conn: &mut Connection,
    ) -> Result<FrameStep, AttachError> {
        if self.mirror.session_name.is_empty() {
            return Err(AttachError::Protocol(
                "engine requested rebootstrap before ATTACHED named the session".to_owned(),
            ));
        }
        conn.unbind_all_terminals();
        self.pending_stream_binds.clear();
        let attach_id =
            send_attach(conn, AttachTarget::ByName(self.mirror.session_name.clone())).await?;
        tracing::warn!(
            attach_id,
            session = %self.mirror.session_name,
            "engine generation rejected; requested replacement bootstrap"
        );
        Ok(FrameStep::Rebootstrap)
    }

    fn track_pending_stream_bind(
        &mut self,
        request_id: u32,
        terminal_id: ResourceId,
    ) -> Result<(), AttachError> {
        if self.pending_stream_binds.len() >= MAX_PENDING_STREAM_BINDS {
            return Err(AttachError::Protocol(format!(
                "QUIC Terminal stream cap exceeded ({MAX_PENDING_STREAM_BINDS})"
            )));
        }
        self.pending_stream_binds.insert(request_id, terminal_id);
        Ok(())
    }

    /// Attach layout leaves this client is not subscribed to yet (a peer's
    /// headless placement, a returned satellite).
    async fn attach_discovered_panes(
        &mut self,
        conn: &mut Connection,
        terminal_ids: &[ResourceId],
    ) -> Result<(), AttachError> {
        for terminal_id in terminal_ids {
            let request_id = self.take_request_id();
            // Correlate the refusal: it is the only evidence a restored leaf
            // names a resource that died with a previous server.
            self.mirror
                .pending_resource_ops
                .insert(request_id, terminal_id.clone());
            self.send_attach_resource(conn, request_id, terminal_id)
                .await?;
        }
        Ok(())
    }

    /// `ATTACH_RESOURCE` one pane under `request_id`, tracking the QUIC
    /// stream bind its affirmative reply opens.
    async fn send_attach_resource(
        &mut self,
        conn: &mut Connection,
        request_id: u32,
        terminal_id: &ResourceId,
    ) -> Result<(), AttachError> {
        if conn.multistream_enabled() {
            self.track_pending_stream_bind(request_id, terminal_id.clone())?;
        }
        let frame = FrameKind::Command {
            request_id,
            command: Command::AttachResource {
                terminal_id: terminal_id.clone(),
                role_policy: crate::attach::attach_role::pane_attach_role(),
            },
        };
        if let Err(error) = send_unless_peer_gone(conn, &frame).await {
            self.pending_stream_binds.remove(&request_id);
            return Err(error);
        }
        Ok(())
    }

    /// Park each window/split spawned on a satellite and attach its pane; the
    /// reply opens it or bells with the host name.
    async fn attach_spawned_panes(
        &mut self,
        conn: &mut Connection,
        parked: Vec<ParkedAdopt>,
    ) -> Result<(), AttachError> {
        for adopt in parked {
            let Some(terminal_id) = adopt.pane().cloned() else {
                continue;
            };
            let request_id = self.take_request_id();
            self.park_adopt(request_id, adopt);
            self.send_attach_resource(conn, request_id, &terminal_id)
                .await?;
        }
        Ok(())
    }

    /// Best-effort kill of spawned satellite panes whose attach was refused.
    async fn kill_orphaned_spawns(
        &mut self,
        conn: &mut Connection,
        panes: Vec<ResourceId>,
    ) -> Result<(), AttachError> {
        for frame in self
            .orphan_kills
            .kill_frames(panes, &mut self.next_request_id)
        {
            send_unless_peer_gone(conn, &frame).await?;
        }
        Ok(())
    }

    /// Act on the orphans one frame left: kill what is reachable, remember
    /// bound ones an unreachable satellite stranded, and kill strays on each
    /// satellite that just answered a spawn (`answered`).
    async fn settle_orphans(
        &mut self,
        conn: &mut Connection,
        outcome: &mut FrameOutcome,
        answered: &[ResourceId],
    ) -> Result<(), AttachError> {
        self.kill_orphaned_spawns(conn, std::mem::take(&mut outcome.kill_orphans))
            .await?;
        self.orphan_kills.record_unreachable(
            std::mem::take(&mut outcome.unreachable_strays),
            std::time::Instant::now(),
        );
        self.orphan_kills.forget_reissued(answered);
        let hosts: Vec<SatelliteHost> = answered
            .iter()
            .filter_map(ResourceId::host)
            .cloned()
            .collect();
        self.retry_stray_kills(conn, &hosts, std::time::Instant::now())
            .await
    }

    /// An inventory reply is the satellite recovery signal: unreachable hosts
    /// stay grey; reached hosts are un-greyed and re-attached in place.
    async fn replay_returned_satellites(
        &mut self,
        conn: &mut Connection,
        answers: &HostAnswers,
    ) -> Result<(), AttachError> {
        let mut changed = false;
        for host in &answers.unreachable {
            changed |= crate::attach::pane_state::mark_satellite_down(
                &mut self.mirror.panes,
                host.as_str(),
            );
        }
        let mut replay = Vec::new();
        for host in &answers.reachable {
            replay.extend(crate::attach::pane_state::satellite_panes_returned(
                &mut self.mirror.panes,
                host.as_str(),
            ));
        }
        if changed || !replay.is_empty() {
            self.peers.chrome_dirty = true;
        }
        self.attach_discovered_panes(conn, &replay).await
    }

    /// Arm [`SATELLITE_PROBE_INTERVAL`] while any satellite pane
    /// is down and no host inventory is already in flight.
    fn arm_satellite_probe(&mut self, now: tokio::time::Instant) {
        if self.host_sessions_supported
            && self.peers.hosts_pending.is_none()
            && self.mirror.panes.values().any(|slot| slot.satellite_down)
        {
            self.satellite_probe_at
                .get_or_insert(now + SATELLITE_PROBE_INTERVAL);
        } else {
            self.satellite_probe_at = None;
        }
    }

    async fn retry_after_inventory(
        &mut self,
        conn: &mut Connection,
        answers: &HostAnswers,
        asked_at: Option<std::time::Instant>,
    ) -> Result<(), AttachError> {
        self.orphan_kills.forget_hosts(&answers.unreachable);
        let Some(asked_at) = asked_at else {
            return Ok(());
        };
        self.retry_stray_kills(conn, &answers.reachable, asked_at)
            .await
    }

    /// Kill the strays on `hosts` (known to answer at `answered_at`), skipping
    /// any this client now references.
    async fn retry_stray_kills(
        &mut self,
        conn: &mut Connection,
        hosts: &[SatelliteHost],
        answered_at: std::time::Instant,
    ) -> Result<(), AttachError> {
        if hosts.is_empty() {
            return Ok(());
        }
        let due = self
            .orphan_kills
            .take_answered(hosts, answered_at, std::time::Instant::now());
        let strays = self.unreferenced_strays(due);
        for frame in self
            .orphan_kills
            .stray_kill_frames(strays, &mut self.next_request_id)
        {
            send_unless_peer_gone(conn, &frame).await?;
        }
        Ok(())
    }

    /// The strays nothing in this client references now; an adopted one is
    /// forgotten, not killed.
    fn unreferenced_strays(
        &self,
        strays: Vec<super::orphans::Stray>,
    ) -> Vec<super::orphans::Stray> {
        strays
            .into_iter()
            .filter(|stray| {
                let pane = stray.pane();
                let adopted = crate::attach::server_frame::pane_is_referenced(
                    &self.mirror.workspace,
                    &self.mirror.pending_windows,
                    &self.mirror.pending_splits,
                    pane,
                );
                if adopted {
                    tracing::debug!(?pane, "a stray satellite pane was adopted; not killing it");
                }
                !adopted
            })
            .collect()
    }

    /// Take over the orphan record from an earlier entry on this connection,
    /// stamped with this entry's `CONDITIONAL_KILL` bit.
    pub(super) fn set_orphan_kills(&mut self, mut kills: super::orphans::OrphanKills) {
        kills.set_conditional_kill(self.conditional_kill_supported);
        self.orphan_kills = kills;
    }

    /// Take over the review index an earlier entry on this
    /// connection handed out at a session switch.
    pub(super) fn set_review(&mut self, review: crate::attach::review::ReviewIndex) {
        self.review = review;
    }

    /// Fold a foreign agent record into the fleet cache and review index;
    /// true when either moved.
    fn fold_foreign_agent(&mut self, id: &ResourceId, value: Option<&[u8]>) -> bool {
        let cache_changed = self.peers.apply_agent_reply(id.clone(), value);
        let review_changed = self.review.observe_record(
            id,
            self.peers.foreign_agents.get(id),
            self.mirror.focused_resource.as_ref(),
        );
        cache_changed || review_changed
    }

    /// Hand the orphan record to the next loop entry; windows and splits
    /// still opening become strays (no kill is sent on the way out).
    fn orphans_for_switch(&mut self) -> super::orphans::OrphanKills {
        let mut kills = std::mem::take(&mut self.orphan_kills);
        kills.park_for_switch(
            parked_spawned_panes(&self.mirror.pending_windows, &self.mirror.pending_splits),
            unanswered_spawns(&self.mirror.pending_windows, &self.mirror.pending_splits),
            std::time::Instant::now(),
        );
        kills
    }

    /// Park a spawned satellite pane's window or split under `request_id`,
    /// in the map its kind's replies are looked up in.
    fn park_adopt(&mut self, request_id: u32, adopt: ParkedAdopt) {
        match adopt {
            ParkedAdopt::Window(window) => {
                self.mirror.pending_windows.insert(request_id, window);
            }
            ParkedAdopt::Split(split) => {
                self.mirror.pending_splits.insert(request_id, split);
            }
        }
    }

    /// Fold what the frame said about other sessions into the peer caches.
    /// Raises both chrome and fleet: peer state feeds the always-on strip.
    async fn fold_peer_outcome(
        &mut self,
        conn: &mut Connection,
        outcome: &mut FrameOutcome,
        repaint: &mut RepaintAccumulator,
    ) -> Result<(), AttachError> {
        let layout_folded = if let Some((session, value)) = outcome.foreign_layout.take() {
            self.peers.apply_layout_reply(session, value.as_deref());
            self.reconcile_peer_agents(conn).await?;
            true
        } else {
            false
        };
        let agent_folded = if let Some((id, value)) = outcome.foreign_agent.take() {
            self.fold_foreign_agent(&id, value.as_deref())
        } else {
            false
        };
        // Only a NEW ask is a repaint reason; a repeated one
        // changes nothing the strip renders.
        let asked_folded = outcome
            .foreign_attention
            .take()
            .is_some_and(|id| self.peers.foreign_attention.insert(id));
        let cleared_folded = outcome
            .foreign_attention_clear
            .take()
            .is_some_and(|id| self.peers.foreign_attention.remove(&id));
        // Lifecycle changes owe a real graph/layout sweep after this burst.
        self.peers.sweep_pending |= outcome.foreign_pane_set_dirty;
        if layout_folded
            || agent_folded
            || asked_folded
            || cleared_folded
            || outcome.foreign_pane_set_dirty
        {
            self.peers.chrome_dirty = true;
            repaint.raise_fleet();
        }
        Ok(())
    }

    /// Re-sweep the watches and caches an ATTACHED snapshot or a pane
    /// lifecycle change invalidated.
    async fn resync_watches(
        &mut self,
        conn: &mut Connection,
        outcome: &mut FrameOutcome,
    ) -> Result<(), AttachError> {
        // Keep a `phux.agent/v1` watch per live pane, once bootstrap outbound
        // has gone (see `bootstrap_outbound`).
        if self.bootstrap_outbound.is_none()
            && self.mirror.panes.len() != self.mirror.agent_meta.subscribed.len()
        {
            self.sync_agent_meta(conn).await?;
        }
        // The ATTACHED snapshot refreshes the pane-cwd index
        // behind the sidebar branch line.
        self.vcs
            .apply_snapshot(std::mem::take(&mut outcome.pane_cwds));
        // Refresh the cached session graph and re-sweep the peers against it.
        if let Some((list, focused)) = outcome.sessions.take() {
            self.peers.sessions = list;
            self.peers.focused_session = Some(focused);
            self.fold_inventory(std::mem::take(&mut outcome.inventory));
            // This sweep satisfies a pending deferred one; clearing the flag
            // avoids a duplicate GET per peer.
            self.peers.sweep_pending = false;
            self.sweep_peer_layouts(conn).await?;
        } else {
            self.fold_inventory(std::mem::take(&mut outcome.inventory));
        }
        Ok(())
    }

    /// Refresh the chrome and raise an in-place paint only when it changed.
    fn note_chrome_change(&mut self, repaint: &mut RepaintAccumulator) {
        if self.refresh_chrome() && !self.overlays.is_active() {
            repaint.raise_chrome();
        }
    }

    /// Fold the frame's chrome-dirtying signals and its transient notices.
    fn fold_chrome_and_notices(
        &mut self,
        outcome: &mut FrameOutcome,
        repaint: &mut RepaintAccumulator,
    ) {
        // A control/asked event changed chrome only (ADR-0029).
        if outcome.chrome_dirty {
            self.note_chrome_change(repaint);
        }
        if !outcome.notices.is_empty() {
            self.apply_notices(std::mem::take(&mut outcome.notices), repaint);
        }
    }

    /// Drain the frame's transient notices into the bar (expiry rides the
    /// status tick); with no bar they degrade to tracing.
    fn apply_notices(&mut self, notices: Vec<Notice>, repaint: &mut RepaintAccumulator) {
        if self.show_notices(notices) && !self.overlays.is_active() {
            repaint.raise_chrome();
        }
    }

    /// Send every request the handled frame asked the driver to emit.
    async fn emit_outcome_requests(
        &mut self,
        conn: &mut Connection,
        outcome: &mut FrameOutcome,
        sidebar: Option<SidebarReservation>,
        prev_rects: Option<&HashMap<ResourceId, crate::layout::Rect>>,
    ) -> Result<(), AttachError> {
        if let Some((terminal_id, stream_id, bootstrap_id, seq)) =
            should_emit_frame_ack(self.wants_state_sync, outcome.ack.take())
        {
            send_unless_peer_gone(
                conn,
                &FrameKind::FrameAck {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    seq,
                },
            )
            .await?;
        }
        if let Some((terminal_id, stream_id, bootstrap_id, cursor, max_bytes, max_rows)) =
            outcome.history_request.take()
        {
            send_unless_peer_gone(
                conn,
                &FrameKind::HistoryRequest {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    cursor,
                    max_bytes,
                    max_rows,
                },
            )
            .await?;
        }
        // A server-driven layout change broadcasts like a local action.
        if outcome.emit_set_metadata {
            self.broadcast_layout(conn).await?;
        }
        if outcome.clear_layout {
            self.clear_stored_layout(conn).await?;
        }
        // `ATTACHED` initially exposes a one-pane fallback. The persisted
        // multi-window layout lands later as this correlated metadata reply.
        // Reflow every restored window here, before `settle_frame_view`
        // schedules its full paint. Doing it earlier can only see the fallback;
        // doing it on first window selection turns that selection into a
        // corrective resize instead of an ordinary paint.
        let bind_reflow_owed = std::mem::take(&mut self.bind_reflow_owed);
        if outcome.layout_get_answered || bind_reflow_owed {
            self.size_workspace_panes(conn, sidebar).await?;
        } else if outcome.reflow_panes
            && let Some(prev_rects) = prev_rects
        {
            self.emit_reflow_resizes(conn, prev_rects, sidebar).await?;
        }
        // ADR-0147: a floating overlay just opened; state its box size, as a
        // split's reflow does for a new tile.
        if outcome.size_floating {
            self.size_floating_pane(conn, sidebar).await?;
        }
        Ok(())
    }

    /// Broadcast the local workspace on the session's layout key so sibling
    /// clients reconcile.
    async fn broadcast_layout(&mut self, conn: &mut Connection) -> Result<(), AttachError> {
        if !self.layout_read_complete {
            return Ok(());
        }
        let Some(session) = self.peers.focused_session else {
            return Ok(());
        };
        let Some(bytes) = encode_layout_or_log(&self.mirror.workspace) else {
            return Ok(());
        };
        let request_id = self.take_request_id();
        send_unless_peer_gone(
            conn,
            &FrameKind::SetMetadata {
                request_id,
                scope: Scope::Group(DEFAULT_GROUP_ID),
                key: layout_key(session),
                value: bytes,
            },
        )
        .await
    }

    /// ADR-0105: tombstone the stored layout once a keep-empty session's last
    /// pane closed.
    async fn clear_stored_layout(&mut self, conn: &mut Connection) -> Result<(), AttachError> {
        let Some(session) = self.peers.focused_session else {
            return Ok(());
        };
        let request_id = self.take_request_id();
        send_unless_peer_gone(
            conn,
            &FrameKind::DeleteMetadata {
                request_id,
                scope: Scope::Group(DEFAULT_GROUP_ID),
                key: layout_key(session),
            },
        )
        .await
    }

    /// A close/spawn changed survivors' dimensions: resize each changed leaf's
    /// PTY, before the repaint so the resync snapshot lands on the grown mirror.
    async fn emit_reflow_resizes(
        &self,
        conn: &mut Connection,
        prev_rects: &HashMap<ResourceId, crate::layout::Rect>,
        sidebar: Option<SidebarReservation>,
    ) -> Result<(), AttachError> {
        emit_view_reflow(
            conn,
            &self.mirror.workspace,
            self.mirror.zoomed.as_ref(),
            prev_rects,
            self.content(sidebar),
            self.resize_cell_px,
        )
        .await
    }

    /// Fold the frame's view-level consequences: a replaced layout, a changed
    /// agent record, a config-reload doorbell, and the fleet projection.
    fn settle_frame_view<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        outcome: &FrameOutcome,
        sidebar: Option<SidebarReservation>,
        fleet_dirty: bool,
        repaint: &mut RepaintAccumulator,
    ) {
        if outcome.layout_replaced {
            self.on_layout_replaced(sidebar, repaint);
        } else if outcome.layout_get_answered {
            // No persisted layout: still resolve a resource-identity pick
            // against the ATTACHED graph.
            self.resolve_cross_session_pick();
        }
        // ADR-0040: an agent record changed. Fold it into the review index even
        // when identical (so a repeat GET cannot re-arm a reviewed completion);
        // repaint chrome in place only when something moved.
        let review_changed = outcome.agent_meta_terminal.as_ref().is_some_and(|id| {
            self.review.observe_record(
                id,
                self.mirror.agent_meta.records.get(id),
                self.mirror.focused_resource.as_ref(),
            )
        });
        if outcome.agent_meta_changed || review_changed {
            self.note_chrome_change(repaint);
        }
        // The `phux config reload` doorbell rang.
        if outcome.config_reload {
            self.reload_config(out, sidebar);
        }
        // Raised, not called: the fleet refresh repaints over a full frame, so
        // the accumulator collapses a burst into one.
        if fleet_dirty {
            repaint.raise_fleet();
        }
    }

    /// The layout changed under us: raise a full repaint (deferred while an
    /// overlay is up; dismiss repaints).
    fn on_layout_replaced(
        &mut self,
        sidebar: Option<SidebarReservation>,
        repaint: &mut RepaintAccumulator,
    ) {
        self.resolve_cross_session_pick();
        self.refresh_chrome();
        // Overlays (copy mode) must adopt the focused pane's new rect.
        self.sync_overlays(sidebar);
        if !self.overlays.is_active() {
            repaint.raise_full();
        }
    }

    /// Apply a one-step cross-session pick: a `ResourceId` from the server
    /// graph, else the layout-backed window/pane indices.
    fn resolve_cross_session_pick(&mut self) {
        if let Some(id) = self.pick.resource.take() {
            self.pick.window = None;
            self.pick.pane = None;
            self.focus_pending_resource(&id);
            return;
        }
        let Some(idx) = self.pick.window.take() else {
            return;
        };
        if !self.mirror.workspace.select(idx) {
            tracing::warn!(
                index = idx,
                windows = self.mirror.workspace.windows.len(),
                "cross-session window pick out of range; keeping restored focus",
            );
            return;
        }
        let next_focus = self
            .mirror
            .workspace
            .active_window()
            .and_then(|ls| ls.focus.clone());
        self.focus_history
            .transition(&mut self.mirror.focused_resource, next_focus);
        if let Some(ord) = self.pick.pane.take() {
            self.focus_picked_leaf(idx, ord);
        }
        if let Some(fid) = self.mirror.focused_resource.as_ref() {
            reanchor_predict_to_pane(&mut self.mirror.predict, &self.mirror.panes, fid);
        }
    }

    /// Focus `id` in the current workspace, adopting a server-graph window
    /// when the TUI layout does not yet name it.
    fn focus_pending_resource(&mut self, id: &ResourceId) {
        if self.focus_resource_in_workspace(id) {
            return;
        }
        if !self.adopt_inventory_resource(id) {
            tracing::warn!(
                resource = %id,
                "cross-session resource pick not in workspace or inventory; keeping restored focus",
            );
            return;
        }
        if !self.focus_resource_in_workspace(id) {
            tracing::warn!(
                resource = %id,
                "cross-session resource pick adopted but not focusable",
            );
        }
    }

    fn focus_resource_in_workspace(&mut self, id: &ResourceId) -> bool {
        let Some(idx) = self.mirror.workspace.windows.iter().position(|window| {
            window
                .state
                .tree
                .as_ref()
                .is_some_and(|tree| crate::layout::leaves(tree).iter().any(|leaf| leaf == id))
        }) else {
            return false;
        };
        let _ = self.mirror.workspace.select(idx);
        if let Some(ls) = self.mirror.workspace.active_window_mut() {
            ls.focus = Some(id.clone());
        }
        self.focus_history
            .transition(&mut self.mirror.focused_resource, Some(id.clone()));
        reanchor_predict_to_pane(&mut self.mirror.predict, &self.mirror.panes, id);
        true
    }

    fn adopt_inventory_resource(&mut self, id: &ResourceId) -> bool {
        let Some(window) = self.peers.windows.iter().find(|window| {
            self.peers
                .focused_session
                .is_none_or(|session| window.session_id == session)
                && crate::attach::sidebar_zones::window_contains_terminal(
                    window,
                    &self.peers.resources,
                    id,
                )
        }) else {
            return false;
        };
        // `GET_STATE` carries no layout (ADR-0030): adopt the pane alone.
        let layout = crate::layout::LayoutNode::Leaf(id.clone());
        let name = window.name.clone();
        let inventory_leaves = crate::layout::leaves(&layout);
        if self.mirror.workspace.windows.len() == 1 {
            let bootstrap = self.mirror.workspace.windows[0]
                .state
                .tree
                .as_ref()
                .map(crate::layout::leaves)
                .unwrap_or_default();
            if bootstrap.len() == 1 && inventory_leaves.iter().any(|leaf| bootstrap.contains(leaf))
            {
                let w = &mut self.mirror.workspace.windows[0];
                w.name = name;
                w.state.tree = Some(layout);
                w.state.focus = Some(id.clone());
                self.mirror.workspace.active = 0;
                return true;
            }
        }
        self.mirror.workspace.add_window(name, id.clone());
        if let Some(ls) = self.mirror.workspace.active_window_mut() {
            ls.tree = Some(layout);
            ls.focus = Some(id.clone());
        }
        true
    }

    /// Focus leaf `ord` of the just-selected window; out of range keeps the
    /// restored focus.
    fn focus_picked_leaf(&mut self, idx: usize, ord: usize) {
        let picked = self
            .mirror
            .workspace
            .active_window()
            .and_then(|ls| ls.tree.as_ref())
            .map(crate::layout::leaves)
            .and_then(|leaves| leaves.get(ord).cloned());
        let Some(leaf) = picked else {
            tracing::warn!(
                window = idx,
                pane = ord,
                "cross-session pane pick out of range; keeping window focus",
            );
            return;
        };
        if let Some(ls) = self.mirror.workspace.active_window_mut() {
            ls.focus = Some(leaf.clone());
        }
        self.focus_history
            .transition(&mut self.mirror.focused_resource, Some(leaf));
    }

    /// ADR-0029 §2: the ONE drain. Every loop-level repaint trigger in this
    /// batch has raised; the highest level wins and paints exactly once.
    fn drain_repaint<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        repaint: &mut RepaintAccumulator,
    ) {
        if std::mem::take(&mut self.peers.chrome_dirty) {
            self.note_chrome_change(repaint);
        }
        let drained = repaint.drain();
        if drained.fleet_dirty {
            self.refresh_fleet(out, sidebar);
        }
        // The same in-place refresh for a session picker that
        // was opened before its host inventory landed.
        if std::mem::take(&mut self.session_picker_dirty) {
            self.refresh_session_picker(out, sidebar);
        }
        // A full repaint discharges every paint the pacer was holding.
        if matches!(drained.level, RepaintLevel::Full) {
            self.pacer.clear_pending();
        }
        self.repaint_view(out, sidebar, drained.level);
    }

    /// Paint every pane the pacer withheld, as one composited frame.
    /// Suppression is re-evaluated here: a pane now under an overlay or a
    /// sync-output transaction is dropped (both have their own recovery).
    fn settle_withheld_panes<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        let owed = self.pacer.take_pending();
        if owed.is_empty() {
            return;
        }
        // Arm the next window from the settle, not from the burst that filled
        // it, so a sustained producer paints on a steady cadence.
        self.pacer.rearm(tokio::time::Instant::now());
        if self.overlays.is_active() {
            return;
        }
        // ADR-0147: under the floating overlay only the overlay paints; the
        // rest repaint whole when it closes.
        let focus = self.mirror.paint_focus();
        let floating = crate::attach::floating::floating_pane(&self.mirror.panes).cloned();
        let live: Vec<ResourceId> = owed
            .into_iter()
            .filter(|id| {
                floating.as_ref().is_none_or(|floating| floating == id)
                    && self
                        .mirror
                        .panes
                        .get(id)
                        .is_some_and(|slot| slot.sync_output_since.is_none())
            })
            .collect();
        if live.is_empty() {
            return;
        }
        let painted = crate::attach::server_frame::paint_output_frame(
            crate::attach::server_frame::OutputFrame {
                out,
                kernel: &self.mirror.engine_kernel,
                panes: &mut self.mirror.panes,
                workspace: &self.mirror.workspace,
                zoomed: self.mirror.zoomed.as_ref(),
                focused_resource: focus.as_ref(),
                status_bar: self.settings.status_bar.as_mut(),
                sidebar,
                viewport_dims: self.viewport_dims,
                session_name: &self.mirror.session_name,
                predict: &mut self.mirror.predict,
            },
            &live,
        );
        self.finish_paint(painted);
        if self
            .mirror
            .focused_resource
            .as_ref()
            .is_some_and(|focused| live.contains(focused))
        {
            self.clear_focused_delivery_fence_after_paint();
        }
    }

    fn clear_focused_delivery_fence_after_paint(&mut self) {
        let Some(terminal_id) = self.mirror.focused_resource.clone() else {
            return;
        };
        self.clear_delivery_fence_after_paint(&terminal_id);
    }

    fn clear_visible_delivery_fences_after_paint(&mut self) {
        let Some(layout) = self
            .mirror
            .workspace
            .render_window(self.mirror.zoomed.as_ref())
        else {
            return;
        };
        let visible = layout
            .tree
            .as_ref()
            .map_or_else(Vec::new, crate::layout::leaves);
        for terminal_id in visible {
            self.clear_delivery_fence_after_paint(&terminal_id);
        }
    }

    fn clear_delivery_fence_after_paint(&mut self, terminal_id: &ResourceId) {
        if !self.delivery_fence_paint_pending.remove(terminal_id) {
            return;
        }
        if let Some(journal) = self.input_replay.as_ref() {
            journal.borrow_mut().clear_delivery_fence(terminal_id);
        }
    }

    /// Rebuild and repaint the session picker, if it is open.
    fn refresh_session_picker<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        if !self.overlays.is_active() {
            return;
        }
        let items = crate::attach::input_dispatch::session_picker_rows(
            &self.peers.inputs(&self.review),
            &self.mirror.workspace,
        );
        let key = crate::attach::input_dispatch::SESSION_PICKER_LIVE_KEY;
        self.refresh_live_overlay(out, sidebar, key, &items);
    }

    /// Rebuild and repaint the agent-fleet dashboard, if it is open.
    fn refresh_fleet<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        if !self.overlays.is_active() {
            return;
        }
        let items = crate::attach::fleet::live_fleet_items(
            &self.mirror.workspace,
            &self.mirror.panes,
            &crate::attach::agent_rows::agent_session_rows(&self.mirror.engine_kernel),
            &self.mirror.agent_meta.records,
            &mut self.vcs,
            &self.peers.inputs(&self.review),
        );
        let key = crate::attach::fleet::FLEET_LIVE_KEY;
        self.refresh_live_overlay(out, sidebar, key, &items);
    }

    // ---- timers, signals, and the periodic paints -----------------------

    /// An application that never sent `?2026l`: expose the mirror once.
    fn on_sync_output_timeout<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        let now = tokio::time::Instant::now();
        let mut expired = false;
        for slot in self.mirror.panes.values_mut() {
            if slot.sync_output_dirty
                && slot.sync_output_since.is_some_and(|since| {
                    now.saturating_duration_since(since) >= SYNC_OUTPUT_WATCHDOG
                })
            {
                slot.sync_output_since = None;
                slot.sync_output_dirty = false;
                expired = true;
            }
        }
        if expired {
            self.repaint_view(out, sidebar, RepaintLevel::Full);
        }
    }

    /// Push the which-key popup listing the pending prefix's continuations.
    fn on_which_key_timeout<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        self.which_key_deadline = None;
        if push_which_key_overlay(
            &mut self.overlays,
            self.settings.resolver.as_ref(),
            self.settings.keybindings.as_ref(),
            &self.settings.theme,
        ) {
            self.paint_overlay(out, sidebar);
        }
    }

    /// Adopt the outer terminal's new size: tell the server, reflow every
    /// PTY, and rebuild the viewport from the authoritative snapshot.
    async fn on_resize<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) -> Result<(), AttachError> {
        let viewport = current_viewport_or_default();
        self.viewport_dims = (viewport.cols.max(1), viewport.rows.max(1));
        self.cell_px_dims = host_cell_px(&viewport);
        // Predictions are pane-local: bound them to the focused pane's grid.
        let (predict_cols, predict_rows) = self
            .mirror
            .focused_resource
            .as_ref()
            .and_then(|fid| self.mirror.panes.get(fid))
            .map_or((viewport.cols, viewport.rows), |slot| slot.geometry);
        self.mirror.predict.set_viewport(predict_cols, predict_rows);
        self.resize_cell_px = resize_cell_px(self.sizes_panes_itself, &viewport);
        // ADR-0145: a client that sizes its own panes casts no vote, so the
        // outer size never reaches a pane before its tile does. An older
        // server still needs the vote for cell pixels (a pixel-only SIGWINCH
        // too); the pane targets below then reassert the tiles.
        if !self.sizes_panes_itself {
            conn.send(&viewport_resize_frame(viewport)).await?;
        }
        self.size_workspace_panes(conn, sidebar).await?;
        // Clear rather than repaint stale pre-resize mirrors; the server's
        // resync snapshot repopulates at the new size.
        let _ = out.write_all(b"\x1b[2J\x1b[H");
        crate::attach::pane_state::invalidate_all_fronts(&mut self.mirror.panes);
        // A pointer-pinned overlay (context menu) is stale now: drop it before
        // the repaint so it cannot capture keys invisibly.
        if self.overlays.dismiss_stale_on_resize() {
            tracing::debug!("resize: dropped a pinned overlay whose geometry went stale");
        }
        if self.overlays.is_active() {
            // Survivors adopt the focused pane's new size (copy mode clamps
            // against it).
            self.sync_overlays(sidebar);
            self.paint_overlay(out, sidebar);
        } else {
            let _ = out.flush();
        }
        Ok(())
    }

    /// Hand every surviving overlay the focused pane's current rect.
    fn sync_overlays(&mut self, sidebar: Option<SidebarReservation>) {
        let bar = self.bar();
        sync_overlays_to_focused_pane(
            &mut self.overlays,
            &self.mirror.workspace,
            self.mirror.zoomed.as_ref(),
            self.mirror.focused_resource.as_ref(),
            self.viewport_dims,
            bar,
            sidebar,
        );
    }

    /// The periodic status-bar repaint.
    fn on_status_tick<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        // Expire the transient notice (even under an overlay).
        if let Some(sb) = self.settings.status_bar.as_mut() {
            let _ = sb.clear_expired_notice(std::time::Instant::now());
        }
        self.expire_overdue_host_inventory();
        // Surface a background update check once it lands.
        self.poll_update_notice(out, sidebar);
        // An overlay would be partly overwritten by the bar paint.
        if self.overlays.is_active() {
            return;
        }
        // Restore the cursor to wherever the focused pane left it
        // so an idle tick doesn't strand the cursor in the bar.
        let focused_cursor = self
            .mirror
            .focused_resource
            .as_ref()
            .and_then(|fid| self.mirror.panes.get(fid))
            .and_then(|slot| slot.renderer.last_cursor());
        tracing::trace!(
            focused_pane_set = self.mirror.focused_resource.is_some(),
            has_cursor = focused_cursor.is_some(),
            "status_tick: repaint bar"
        );
        let fallback_origin = Some(self.bar_fallback_origin(sidebar));
        // A frame block opens only if the bar writes, so an unchanged tick
        // emits nothing.
        let mut chrome = ChromeCtx::new(
            &mut self.settings,
            &mut self.sidebar_painter,
            &self.mirror.session_name,
            self.viewport_dims,
            sidebar,
        );
        let painted = crate::attach::paint::close_frame_with_chrome(
            crate::attach::paint::FrameBlock::begin(out),
            &mut chrome,
            focused_cursor,
            fallback_origin,
            // The tick refreshes what the painter cannot observe (clock, exec).
            crate::render::chrome::status_bar::ComposePolicy::Always,
        );
        self.finish_paint(painted);
    }

    /// Where the cursor lands after a bar paint: the focused pane's origin,
    /// else (0, 0), so it is never stranded in the bar.
    fn bar_fallback_origin(&self, sidebar: Option<SidebarReservation>) -> (u16, u16) {
        let content = self.content(sidebar);
        self.mirror
            .focused_resource
            .as_ref()
            .and_then(|fid| {
                self.mirror
                    .workspace
                    .render_window(self.mirror.zoomed.as_ref())
                    .and_then(|ls| {
                        crate::attach::paint::tiled_rect(
                            ls.as_ref(),
                            content,
                            self.viewport_dims,
                            fid,
                        )
                    })
            })
            .map_or((0, 0), |r| (r.x, r.y))
    }

    /// ADR-0140: a hosts-provider answer; redraw machines and the picker.
    fn on_hosts<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        hosts: Vec<phux_core::host_list::HostJson>,
    ) {
        if self.peers.remote_hosts == hosts {
            return;
        }
        crate::attach::hosts::remember(&hosts);
        self.peers.remote_hosts = hosts;
        self.peers.chrome_dirty = true;
        self.session_picker_dirty = true;
        let mut repaint = RepaintAccumulator::default();
        self.drain_repaint(out, sidebar, &mut repaint);
    }

    /// A spawned plugin action finished: log it, and toast a failure.
    fn on_plugin_result<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        result: &PluginRunResult,
    ) {
        tracing::info!(
            plugin = %result.plugin_id,
            action = %result.action_id,
            ok = plugin_actions::run_succeeded(result),
            "plugin action finished",
        );
        if let Some((title, lines)) = plugin_actions::failure_toast(result) {
            self.overlays.push(Box::new(ToastOverlay::new(
                title,
                lines,
                &self.settings.theme,
            )));
            self.paint_overlay(out, sidebar);
        }
    }

    /// Surface the background update check once it lands, polled on the bar
    /// tick for [`UPDATE_POLL_TICKS`] ticks; waits while a modal is up.
    fn poll_update_notice<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        if self.update_poll_ticks == 0 || self.overlays.is_active() {
            return;
        }
        self.update_poll_ticks -= 1;
        let Some(notice) = crate::attach::update_notice::take_pending_notice() else {
            return;
        };
        self.update_poll_ticks = 0;
        self.overlays.push(Box::new(ToastOverlay::new(
            notice.title(),
            notice.body(),
            &self.settings.theme,
        )));
        self.paint_overlay(out, sidebar);
    }
}
