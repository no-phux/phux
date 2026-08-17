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
use phux_client_core::session::{EffectBuffer as KernelEffectBuffer, SessionKernel};
#[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::{ClientId, SessionId, TerminalId};
use phux_protocol::wire::frame::{AttachTarget, CONFIG_RELOAD_KEY, Command, FrameKind, Scope};
use tokio::signal::unix::{Signal, SignalKind, signal};

use crate::agent_meta::AgentRecord;
use crate::attach::actions::{PendingSplit, PendingWindow};
use crate::attach::connection::{Connection, NegotiatedBootstrap};
use crate::attach::input::StdinParser;
use crate::attach::input_dispatch::{
    DispatchCtx, DragGrab, ReattachTarget, dispatch_input_events, encode_layout_or_log,
    sync_overlays_to_focused_pane,
};
use crate::attach::onboarding::{AttachClaim, AttachMoment};
use crate::attach::outcome::{AttachEnd, AttachError};
use crate::attach::paint::{
    SidebarEdge, SidebarReservation, StatusBarPaint, content_rect, paint_chrome_in_place,
    paint_full_frame, sidebar_reservation,
};
use crate::attach::pane_state::{
    AttentionNavigation, PaneSlot, VcsIndex, reanchor_predict_to_pane,
};
use crate::attach::plugin_actions::{self, PluginActionEntry, PluginRunResult};
use crate::attach::plugin_panes::{self, PluginPaneEntry};
use crate::attach::repaint::{PaintPacer, RepaintAccumulator, RepaintLevel};
use crate::attach::server_frame::{AgentMetaIndex, FrameOutcome, handle_server_frame};
use crate::attach::tty_input::TtyInput;
use crate::layout::Workspace;
use crate::layout_ops::{DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, layout_key};
use crate::predict::{Overlay, PredictionState, PredictiveConfig};
use crate::render::ChromeBreakpoints;
use crate::render::chrome::sidebar::SidebarPainter;
use crate::render::chrome::status_bar::{Notice, StatusBarPainter};
use crate::render::overlay::{OverlayState, ToastOverlay};
use phux_config::SidebarPosition;

use super::chrome::{mark_focused_seen, peer_inputs, refresh_window_chrome};
use super::config_ui::{
    apply_initial_notice, build_resolver_from, build_status_bar_painter, handle_config_reload,
    keybind_error_line, push_which_key_overlay, update_which_key_deadline,
};
use super::entry::{
    LoopExit, detached_loop_exit, finish_onboarding_claim, finish_return_onboarding_after_paint,
    seed_sidebar_enabled,
};
use super::main_loop::{
    FRAME_COALESCE_CAP, coalesce_defer_flags, frame_defers_paint, frame_paint_target,
};
use super::overlay_paint::{paint_active_overlay, refresh_fleet_if_open};
use super::session_io::{
    send_attach, send_terminal_replies, send_unless_peer_gone, should_emit_frame_ack,
    take_terminal_replies,
};
use super::subscriptions::{
    apply_foreign_agent_reply, apply_foreign_layout_reply, prune_foreign_agents,
    sync_agent_meta_subscriptions, sync_foreign_agent_subscriptions,
    sync_foreign_layout_subscriptions,
};
use super::terminal::{
    desired_mouse_capture, sync_hover_tracking, sync_mouse_capture, terminal_reset_on_signal,
};
use super::viewport::{
    HOST_CELL_PX_FALLBACK, current_viewport, current_viewport_or_default, emit_view_reflow,
    host_cell_px, view_rects, viewport_resize_frame,
};

/// Window before a parser-pending bare ESC is interpreted as the Escape
/// key, anchored to when the ESC became pending (see
/// [`SessionLoop::esc_deadline`]). The client reads stdin from the *outer*
/// terminal, which writes a key's full `ESC [`/`ESC O` sequence in one burst
/// — a split only happens at a read-buffer boundary — so a short window
/// suffices to disambiguate. It must stay short: a modal-editor user pays
/// this window on EVERY bare Escape, and the inner application (vim's
/// `ttimeoutlen`, readline's `keyseq-timeout`) then stacks its own on top.
/// tmux installs ship `escape-time 0..10` for the same reason; 10ms keeps
/// Escape under the perception floor while still absorbing split sequences.
const ESC_FLUSH_IDLE: Duration = Duration::from_millis(10);

/// Safety valve for an application that enters DEC synchronized output and
/// never leaves it. Normal TUI transactions last milliseconds.
const SYNC_OUTPUT_WATCHDOG: Duration = Duration::from_secs(1);

/// What one [`SessionLoop::step`] decided about the loop's future.
pub(super) enum Step {
    /// Nothing ended; park on the wake-up sources again.
    Continue,
    /// The attach is over (detach, disconnect, or a session switch).
    Exit(LoopExit),
}

/// What one handled server frame decided about the burst it arrived in.
enum FrameStep {
    /// Frame handled; move on to the next frame in the burst.
    Done,
    /// The engine rejected a generation and a replacement bootstrap is in
    /// flight; skip the rest of this frame's handling.
    Rebootstrap,
    /// The attach is over; unwind out of the burst.
    Exit(LoopExit),
}

/// The peer-session caches the roster, the window picker, and the
/// agent-fleet dashboard project from.
///
/// One struct rather than ten parallel locals: every field here is written
/// by the same peer sweep and read by the same [`peer_inputs`] projection,
/// so they are refreshed, pruned, and reset together.
#[derive(Default)]
struct PeerCaches {
    /// phux-4li.20: cache of the server's session graph, refreshed from
    /// every ATTACHED snapshot. The `<leader> a` session picker reads
    /// this to list peer sessions; `focused_session` marks the row the
    /// client is currently attached to (excluded from the picker).
    sessions: Vec<phux_protocol::wire::info::SessionInfo>,
    /// The session this client is attached to, once ATTACHED has named it.
    focused_session: Option<SessionId>,
    /// phux-foz.8: peer sessions' persisted layouts, fetched right after the
    /// session graph lands (one `GET_METADATA` per peer, correlated through
    /// `foreign_layout_pending`). The window picker reads the cache to render
    /// one-step cross-session window rows; sessions with no entry fall back
    /// to the plain "switch to this session" row. Attach-time snapshot only —
    /// we do not subscribe to peers' layout keys.
    foreign_layouts: HashMap<SessionId, Workspace>,
    /// In-flight peer-layout GETs, by request id.
    foreign_layout_pending: HashMap<u32, SessionId>,
    /// phux-jpqd: the `phux.agent/v1` records of FOREIGN panes, so the
    /// agent-fleet dashboard shows a peer session's agent glyph/state without
    /// attaching there. Populated lazily: when a peer's layout lands
    /// (`apply_foreign_layout_reply`), the driver fires one `GET_METADATA` per
    /// `TerminalId` in that workspace on the pane's agent key, correlated
    /// through `foreign_agent_pending`. Keyed by foreign terminal id; pruned
    /// to the union of all cached foreign layouts' leaves on each fold so it
    /// stays bounded. No subscription — a one-shot read, same lazy-query
    /// shape as the foreign layouts above (ADR-0018 / ADR-0030).
    foreign_agents: HashMap<TerminalId, AgentRecord>,
    /// In-flight foreign agent-record GETs, by request id.
    foreign_agent_pending: HashMap<u32, TerminalId>,
    /// phux-k0cw: which peer keys this connection has already subscribed to.
    /// Send-once bookkeeping, not teardown: L3 has no `UNSUBSCRIBE_METADATA`
    /// verb, so a subscription lives as long as the connection and re-sending
    /// one would just be noise on the wire.
    foreign_layout_subscribed: HashSet<SessionId>,
    /// The per-pane half of the same send-once bookkeeping.
    foreign_agent_subscribed: HashSet<TerminalId>,
    /// phux-k0cw: peer panes whose agent has asked for a human (an ADR-0035
    /// `Asked` for a Terminal outside this client's pane set). The local
    /// equivalent is `PaneSlot::attention`, which a foreign pane has no slot
    /// to carry, so the flag lives here and is pruned with the peer records.
    foreign_attention: HashSet<TerminalId>,
    /// phux-k0cw.10: the peer sweep owes the first paint its silence. Set at
    /// construction and consumed at the ONE drain in the frame burst, so
    /// bootstrap sends no peer traffic until this session has actually
    /// painted.
    ///
    /// Why a flag rather than a call at bootstrap: a session switch re-enters
    /// `main_loop` through the same bootstrap, and the server drops every
    /// subscription with the old attach, so a switch rebuilds all of this from
    /// empty. Sweeping before the loop therefore puts N peer GET/SUBSCRIBE
    /// pairs — plus M per-pane pairs once the layouts land — ahead of the
    /// `TERMINAL_SNAPSHOT` burst that produces the first paint, and the switch
    /// pays for the roster's freshness in exactly the moment the roster exists
    /// to make fast. One flag covers both entries because both run this code.
    sweep_pending: bool,
}

impl PeerCaches {
    /// The peer-wide projection zones 1 and 3 of the sidebar strip render
    /// from.
    fn inputs(&self) -> crate::attach::sidebar_zones::PeerInputs<'_> {
        peer_inputs(
            &self.sessions,
            self.focused_session,
            &self.foreign_layouts,
            &self.foreign_agents,
            &self.foreign_attention,
        )
    }
}

/// Everything the driver derives from the on-disk config at attach time.
///
/// Loaded once, before any user input can reach the loop: opening a
/// discovery surface must never perform config I/O under the user's
/// fingers. The in-place reload (phux-foz.5) swaps the same pieces.
struct ConfigSeed {
    /// phux-nz4.5: status-bar painter, or `None` when the config named no bar.
    status_bar: Option<StatusBarPainter>,
    /// phux-r82.5: palette rows + manifest `keys` merged into the prefix table.
    plugin_actions: Vec<PluginActionEntry>,
    /// phux-r82.7: the hostable pane entries committing `plugin-pane`.
    plugin_panes: Vec<PluginPaneEntry>,
    /// The plugin-merged keybindings snapshot.
    keybindings: Option<phux_config::KeybindingsCfg>,
    /// phux-4li.5: the keybind resolver built from that snapshot.
    resolver: Option<phux_config::keybind::Resolver>,
    /// phux-ahv.4: single source of truth for chrome + overlay colors.
    theme: crate::render::Theme,
    /// `[sidebar] enabled` — the FIRST attach's seed for the runtime toggle.
    sidebar_enabled: bool,
    /// `[sidebar] width`, in columns.
    sidebar_width: u16,
    /// `[sidebar] position`, folded to the reservation's edge.
    sidebar_edge: SidebarEdge,
    /// phux-huhi: the responsive-chrome thresholds.
    chrome_breakpoints: ChromeBreakpoints,
    /// The global `mouse` gate the `RawModeGuard` install used.
    mouse_capture_cfg: bool,
    /// phux-foz.2: whether the which-key popup is armed at all.
    which_key_enabled: bool,
    /// How long the resolver may sit at a prefix before the popup shows.
    which_key_delay: Duration,
}

impl ConfigSeed {
    /// Load the layered config and derive every driver-visible piece of it.
    ///
    /// Load failures fall back to defaults so a malformed config never blocks
    /// attach — the user still gets a working pane mirror.
    fn load() -> Self {
        let loaded_cfg = phux_config::loader::load().ok();
        let cfg = loaded_cfg.as_ref();
        let (plugin_actions, plugin_panes) = load_plugin_entries(cfg);
        let keybindings = cfg.map(|c| merged_keybindings(c, &plugin_actions));
        let mut status_bar = build_status_bar_painter();
        let resolver = build_lenient_resolver(keybindings.as_ref(), &mut status_bar);
        let theme = cfg.map_or_else(crate::render::Theme::default, |c| {
            crate::render::Theme::from_cfg(&c.theme)
        });
        // phux-foz.1: the attention hint's chip color comes from the theme's
        // `attention` slot rather than a hardcoded SGR in the painter.
        if let Some(sb) = status_bar.as_mut() {
            sb.set_attention_color(theme.attention);
        }
        let sidebar_cfg = cfg.map(|c| c.sidebar.clone());
        Self {
            status_bar,
            plugin_actions,
            plugin_panes,
            which_key_enabled: keybindings.as_ref().is_some_and(|kb| kb.which_key),
            which_key_delay: Duration::from_millis(
                keybindings.as_ref().map_or(600, |kb| kb.which_key_delay_ms),
            ),
            keybindings,
            resolver,
            theme,
            sidebar_enabled: sidebar_cfg.as_ref().is_some_and(|c| c.enabled),
            sidebar_width: sidebar_cfg.as_ref().map_or(20, |c| c.width),
            sidebar_edge: match sidebar_cfg.as_ref().map(|c| c.position) {
                Some(SidebarPosition::Right) => SidebarEdge::Right,
                _ => SidebarEdge::Left,
            },
            chrome_breakpoints: cfg.map_or_else(ChromeBreakpoints::default, |c| {
                ChromeBreakpoints::from_cfg(&c.chrome)
            }),
            mouse_capture_cfg: cfg.is_none_or(|c| c.defaults.mouse),
        }
    }
}

/// phux-r82.5 / phux-r82.7: snapshot the enabled plugins' manifests once at
/// driver start (same policy as the keybindings snapshot — no config I/O
/// under user fingers), then derive both the action entries (palette rows +
/// manifest `keys` merged into the prefix table, user config winning every
/// conflict) and the hostable pane entries (palette rows committing
/// `plugin-pane`; placement `split`/`tab`/`zoomed` — overlay is deferred and
/// dropped with a warning). A broken manifest is skipped with a warning;
/// manifests resolve relative to the canonical config path, the same
/// resolution `phux config run` uses.
fn load_plugin_entries(
    cfg: Option<&phux_config::Config>,
) -> (Vec<PluginActionEntry>, Vec<PluginPaneEntry>) {
    let manifests: Vec<phux_config::plugin::PluginManifest> = cfg
        .map(|cfg| {
            phux_config::plugin::load_enabled_manifests(
                &phux_config::loader::config_path(),
                &cfg.plugins,
            )
        })
        .unwrap_or_default();
    (
        plugin_actions::entries_from_manifests(&manifests),
        plugin_panes::entries_from_manifests(&manifests),
    )
}

/// Cache the keybindings so opening discovery surfaces never performs config
/// I/O under user fingers, with the plugin manifests' `keys` merged in.
fn merged_keybindings(
    cfg: &phux_config::Config,
    plugin_actions: &[PluginActionEntry],
) -> phux_config::KeybindingsCfg {
    let mut kb = cfg.keybindings.clone();
    plugin_actions::merge_plugin_bindings(&mut kb, plugin_actions);
    kb
}

/// phux-4li.5: build the keybind resolver from the plugin-merged snapshot so
/// a manifest `keys` chord resolves exactly like a user binding. The resolver
/// consumes `InputEvent::Key` events *before* they would be forwarded to the
/// focused pane; a chord that resolves to an action mutates the active window
/// in the driver and never reaches the server's input pipe.
///
/// phux-i0e8.3.4: the build is lenient — whenever a snapshot exists a
/// resolver exists, and each diagnostic disables exactly one binding.
/// Diagnostics surface as a status-bar error line naming the chord (unless
/// the bar is already showing a config error, which subsumes any keybinding
/// problem).
fn build_lenient_resolver(
    keybindings: Option<&phux_config::KeybindingsCfg>,
    status_bar: &mut Option<StatusBarPainter>,
) -> Option<phux_config::keybind::Resolver> {
    let kb = keybindings?;
    let (built, diags) = build_resolver_from(kb);
    if !diags.is_empty()
        && !status_bar
            .as_ref()
            .is_some_and(StatusBarPainter::is_error_line)
    {
        *status_bar = Some(StatusBarPainter::error_line(keybind_error_line(&diags)));
    }
    Some(built)
}

/// A `sleep_until` future for an armed deadline, or a never-resolving future
/// when nothing is armed — this keeps the steady-state cost at one
/// always-`Pending` future and avoids unused-`Option` branches inside
/// `select!`.
fn sleep_until_or_pending(
    deadline: Option<tokio::time::Instant>,
) -> std::pin::Pin<Box<dyn Future<Output = ()>>> {
    match deadline {
        Some(deadline) => Box::pin(tokio::time::sleep_until(deadline)),
        None => Box::pin(std::future::pending::<()>()),
    }
}

/// The relative-delay twin of [`sleep_until_or_pending`], for a cadence
/// rather than an anchored deadline.
fn sleep_for_or_pending(interval: Option<Duration>) -> std::pin::Pin<Box<dyn Future<Output = ()>>> {
    match interval {
        Some(interval) => Box::pin(tokio::time::sleep(interval)),
        None => Box::pin(std::future::pending::<()>()),
    }
}

/// Restore the terminal explicitly (Drop wouldn't fire on `exit()`), then
/// exit with the shell-conventional code for the signal.
#[allow(clippy::exit, reason = "signal-driven graceful exit; Drop won't run")]
fn exit_on_signal(code: i32) -> ! {
    terminal_reset_on_signal();
    std::process::exit(code);
}

/// phux-jhv8: drain every frame already queued so a back-to-back output burst
/// (nvim startup, a full-screen redraw) applies all its `vt_write`s and paints
/// ONCE — on the final frame — instead of a render + blocking flush per frame.
/// The non-blocking `try_recv` stops the moment the socket would block, so a
/// lone frame keeps the old one-frame-one-paint path.
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

/// Whether an input event is the kind that expects output back, and so should
/// arm the pacer's reply grace.
///
/// Pointer MOTION does not. The mouse is reported to the server under
/// `?1002h` from attach (`driver::terminal`), so a divider drag, a selection
/// sweep, or just crossing the window emits a continuous stream of ordinary
/// `InputEvent`s — at well over 50 a second, each one refreshing a 20ms
/// grace. That kept the grace permanently alive and pacing permanently off
/// for the whole client. A drag's own feedback (the divider, the selection
/// highlight) is chrome painted locally and never went through the pacer
/// anyway; what motion does NOT do is make a pane echo something back.
///
/// Press and release do expect a reply — a click into a pane running a mouse
/// aware program gets one — so only motion is excluded.
pub(super) const fn input_expects_a_reply(event: &phux_protocol::input::InputEvent) -> bool {
    match event {
        phux_protocol::input::InputEvent::Mouse(mouse) => !matches!(
            mouse.action,
            phux_protocol::input::mouse::MouseAction::Motion
        ),
        // Keys, focus changes and pastes all expect output back. So does any
        // future atom: `InputEvent` is `non_exhaustive`, and a new one is far
        // likelier to be something the user did that expects a reply than
        // another continuous pointer stream — so the default favours latency,
        // and an atom that behaves like motion should be named above.
        _ => true,
    }
}

/// Whether an inbound burst must discharge the pacer's withheld debt before
/// it parks, rather than leaving it to the `paint_deadline` select arm.
///
/// Both inputs are cases the timer cannot be relied on for, and the FIRST is
/// the one that was missing (phux-l96p.3 review): an admitted burst has just
/// pushed `next_allowed` out to `now + interval`, so `deadline_passed` is
/// false by construction on that pass — and the `paint_deadline` arm sits
/// third in a `biased` select behind `conn.recv()`, which a saturating
/// producer keeps permanently ready. Without the `paint_now` term a pane that
/// emitted once during a refused window and then went quiet stayed unpainted
/// for as long as any other pane kept talking.
pub(super) const fn burst_settles_debt(paint_now: bool, deadline_passed: bool) -> bool {
    paint_now || deadline_passed
}

/// phux-foz.7: did this frame change anything the agent-fleet dashboard
/// projects (agent records, asked/lease state, layout/pane set, session
/// graph)? Read before the move-y outcome fields are consumed, acted on after
/// the per-frame handling.
const fn fleet_projection_dirty(outcome: &FrameOutcome) -> bool {
    outcome.chrome_dirty
        || outcome.agent_meta_changed
        || outcome.layout_replaced
        || outcome.reflow_panes
        || outcome.sessions.is_some()
}

/// Every local the attach loop carries across `select!` iterations.
///
/// phux-4li.4: `panes` holds N client-side Terminals keyed by `TerminalId`,
/// not the single Terminal of the wave-A driver. Each pane's metadata slot is
/// allocated lazily from authoritative bootstrap geometry.
#[allow(
    clippy::struct_excessive_bools,
    reason = "parallel driver-local view/lifecycle flags; a bitset would obscure every read site"
)]
pub(super) struct SessionLoop {
    /// Does this server answer `TERMINAL_REPLY`? Fixed for the connection.
    terminal_reply_supported: bool,
    /// phux-a5xj: does this server build a spawned pane at the geometry we
    /// name, rather than at its own default? Fixed for the life of the
    /// connection, like the reply bit above.
    spawn_initial_size_supported: bool,
    /// Whether the server advertised `ACKNOWLEDGED_INPUT`, the bit the
    /// ADR-0053 paste journal needs before it may route a batch.
    acknowledged_input_supported: bool,
    /// ADR-0053: the acknowledged-input replay journal, shared with the CLI's
    /// reconnect loop so an operation unresolved when a socket died is
    /// replayed by the next attach under its original operation id. `None`
    /// on UDS dials.
    input_replay:
        Option<std::rc::Rc<std::cell::RefCell<crate::attach::input_replay::InputReplayJournal>>>,
    /// Whether this connection negotiated `OutputMode::StateSync`. Gates the
    /// per-frame `FRAME_ACK`: only a state-sync consumer's acks are tracked
    /// server-side, so a raw consumer skips them (see `should_emit_frame_ack`).
    wants_state_sync: bool,

    /// The client-side terminal engine every pane's replica lives in.
    engine_kernel: SessionKernel<GhosttyAdapter>,
    /// Scratch buffer the kernel drains its effects into.
    kernel_effects: KernelEffectBuffer,
    /// The per-pane mirrors, keyed by `TerminalId`.
    panes: HashMap<TerminalId, PaneSlot>,
    /// `Workspace` mirror (initialized as a single window holding one
    /// pane when `ATTACHED` lands; see `handle_server_frame`) is the
    /// source of truth for which leaves are live and where they sit in
    /// the outer viewport. The renderer and layout helpers operate on the
    /// active window (`workspace.active_window()`); the workspace
    /// dimension is what gets persisted to L3.
    workspace: Workspace,
    /// The pane keystrokes route to.
    focused_pane: Option<TerminalId>,
    /// phux-oih5.4: one-entry focus MRU, local to this attached client. It is
    /// deliberately outside Workspace so layout metadata never persists or
    /// shares focus history (ADR-0019 decision 6).
    focus_history: crate::attach::focus::FocusHistory,
    /// ADR-0033: this client's own server-assigned `ClientId`, captured from
    /// ATTACHED. Used to render "you hold the wheel" vs another client in the
    /// supervisory badge. `None` until ATTACHED lands.
    own_client_id: Option<ClientId>,
    /// phux-x2hm: pane-zoom view state (driver-local, like focus). `Some(id)`
    /// ⇒ pane `id` is zoomed to fill the window; render/reflow then run against
    /// `workspace.render_window(zoomed)` (a synthetic single-leaf layout)
    /// instead of the real tiled tree, which is left untouched for mutation.
    zoomed: Option<TerminalId>,
    /// phux-4li.5: the in-flight layout GET's request id, for L3 correlation.
    layout_get_request_id: Option<u32>,
    /// phux-4li.5: request-id allocator for L3 GET correlation.
    next_request_id: u32,
    /// phux-4li.12: in-flight `split-pane` actions parked by request id.
    /// Populated by `run_action` when it dispatches `SPAWN_TERMINAL`;
    /// drained by `handle_server_frame`'s `TerminalSpawned` arm when the
    /// reply arrives. The map is small (one entry per outstanding
    /// user-triggered split) so a `HashMap` is overkill for cap but
    /// matches the layout-key request-id pattern.
    pending_splits: HashMap<u32, PendingSplit>,
    /// phux-4li.15: in-flight `new-window` actions parked by request id,
    /// same lifecycle as `pending_splits`. The `TerminalSpawned` arm checks
    /// this map first; a hit opens a new window on the spawned pane.
    pending_windows: HashMap<u32, PendingWindow>,
    /// phux-i0e8.2.2: Terminals whose close THIS client requested
    /// (kill-pane / kill-window). The action dispatcher parks ids here at
    /// the kill seam; the `TerminalClosed` arm drains them to suppress the
    /// pane-exit notice for a death the user themselves ordered.
    expected_closes: HashSet<TerminalId>,
    /// ADR-0040 (phux-3ert): the structured agent-identity index. Each pane
    /// gets a one-shot `GET_METADATA` + a live `SUBSCRIBE_METADATA` on
    /// `phux.agent/v1` (see `sync_agent_meta_subscriptions`); decoded records
    /// feed the window labels so the sidebar/tab strip renders agent
    /// name/state from structured data, with the OSC title as the fallback.
    agent_meta: AgentMetaIndex,
    /// phux-p4vp: pane cwd + branch memo behind the sidebar's branch line.
    /// Seeded from every ATTACHED snapshot; read at chrome-refresh time.
    vcs: VcsIndex,
    /// phux-nz4.5: status-bar painter, built from the on-disk config.
    status_bar: Option<StatusBarPainter>,
    /// phux-r82.5: plugin palette rows, swapped by an in-place config reload.
    plugin_actions: Vec<PluginActionEntry>,
    /// phux-r82.7: hostable plugin panes, swapped alongside the actions.
    plugin_panes: Vec<PluginPaneEntry>,
    /// The plugin-events channel's sender: spawned plugin-action tasks report
    /// completion here. Lent to `DispatchCtx` each batch.
    plugin_tx: tokio::sync::mpsc::UnboundedSender<PluginRunResult>,
    /// The receiving half of the same channel; the `plugin_rx` select arm
    /// toasts failures.
    plugin_rx: tokio::sync::mpsc::UnboundedReceiver<PluginRunResult>,
    /// The plugin-merged keybindings snapshot the action finder reads.
    keybindings_snapshot: Option<phux_config::KeybindingsCfg>,
    /// phux-4li.5: the keybind resolver built from that snapshot.
    resolver: Option<phux_config::keybind::Resolver>,
    /// phux-ahv.4: single source of truth for chrome + overlay colors,
    /// owned alongside the keybindings snapshot and threaded into the
    /// overlay render path via `DispatchCtx`.
    theme: crate::render::Theme,
    /// The window-strip painter, themed like the status bar. Fed
    /// `window_infos` from the same snapshot that drives the tab strip;
    /// caches so an unchanged repaint emits nothing.
    sidebar_painter: SidebarPainter,
    /// phux-5ke.4: overlay state — initially empty. Pushed onto by the
    /// `show-help` action; drained by `OverlayState::handle_key` when
    /// the active overlay returns `Dismiss`. While active, key events
    /// route to the overlay (no pane forwarding) and pane stdout flushes
    /// are suppressed (ADR-0020 §Decision invariant 5).
    overlays: OverlayState,
    /// phux-oih5.16: one client-local return point for attention navigation.
    /// Cycling never overwrites it; return consumes it. It is deliberately
    /// absent from Workspace/L3 metadata and resets on re-attach.
    attention_navigation: AttentionNavigation,
    /// ADR-0048: the in-flight divider drag. `None` between drags; a press
    /// on a divider records the grabbed split, motion re-tunes it, release
    /// clears it. Lives across dispatch batches (press and release land in
    /// different `select!` wakeups).
    drag: Option<DragGrab>,
    /// phux-npb3 (ADR-0048 decision 3 follow-up): per-pane mouse opt-out.
    /// `set-pane mouse off` puts the focused pane in this set; the dispatcher
    /// then never synthesizes `INPUT_MOUSE` for it, and the sync at the top of
    /// each loop iteration drops the outer-terminal mouse-tracking DECSET
    /// whenever the focused pane is opted out — so the host's raw mouse
    /// handling (native selection etc.) returns for that pane alone while
    /// sibling panes keep drag-to-resize. Client-local; nothing on the wire.
    mouse_optout: HashSet<TerminalId>,
    /// Mirrors the global `mouse` gate the `RawModeGuard` install used: with
    /// `mouse = false` capture stays off unconditionally.
    mouse_capture_cfg: bool,
    /// phux-4h5a: the window sidebar's runtime on/off state, flipped by
    /// `toggle-sidebar`. Only the toggle is carried across a session switch:
    /// the strip's width and edge stay pure config, re-derived per entry.
    sidebar_enabled: bool,
    /// The configured strip width, in columns.
    sidebar_width: u16,
    /// The edge the strip docks to.
    sidebar_edge: SidebarEdge,
    /// phux-huhi: the responsive-chrome thresholds, snapshotted from
    /// `[chrome]` beside the theme and the sidebar geometry. One value for the
    /// whole attach: the sidebar-yield fold, the `toggle-sidebar` refusal in
    /// `input_dispatch`, and every overlay's `centered_panel` read this, so
    /// "compact" cannot mean two things on the same frame.
    chrome_breakpoints: ChromeBreakpoints,
    /// Track the current outer-terminal viewport so the painter knows
    /// which row is "bottom". Initialized to a sensible default and
    /// updated by SIGWINCH; the server doesn't drive client-side
    /// viewport (clients own their chrome per DESIGN §8.5).
    viewport_dims: (u16, u16),
    /// Host per-cell pixel size for the `INPUT_MOUSE` cells→pixels scaling
    /// (SPEC input.md §3.1). Tracked next to `viewport_dims` and refreshed
    /// on the same SIGWINCH edge — a monitor change can move the window to
    /// a display with a different cell size (phux-yyex).
    cell_px_dims: (u16, u16),
    /// The attached session's name, from ATTACHED.
    session_name: String,
    /// The peer-session caches the roster and window picker read.
    peers: PeerCaches,
    /// phux-foz.8: the deferred window select of a one-step cross-session
    /// pick, consumed on the first layout reconcile.
    pending_window: Option<usize>,
    /// phux-jpqd: the DFS leaf ordinal focused after the window select
    /// resolves — the pane half of a one-step cross-session pick.
    pending_pane: Option<usize>,
    /// The outer terminal's key/mouse decoder.
    parser: StdinParser,
    /// Predictive local echo (phux-9gw.1). State is updated alongside
    /// every keystroke and drained on every `TERMINAL_OUTPUT`; when
    /// `predict_cfg.enabled == false` every `predict_key` returns
    /// `Disabled` so the overlay never paints.
    predict: PredictionState,
    /// The predictive-echo overlay renderer.
    overlay: Overlay,
    /// The outer terminal's stdin, read on reactor readiness when the
    /// controlling tty can be opened privately (phux-l96p.4). See
    /// [`crate::attach::tty_input`] for the fallback ladder.
    stdin: TtyInput,
    /// One read's worth of stdin bytes.
    stdin_buf: [u8; 4096],
    /// One batch's worth of decoded input events (phux-l96p.4).
    ///
    /// Retained across reads so the keystroke path reuses one allocation
    /// instead of building a fresh `Vec` per read. Always drained by
    /// [`Self::dispatch_batch`]; a non-empty buffer between iterations would
    /// be a bug, not held state.
    input_events: Vec<phux_protocol::input::InputEvent>,
    /// Terminal resize notifications.
    sigwinch: Signal,
    /// `phux-roz`: SIGINT/SIGTERM/SIGHUP handlers run terminal cleanup
    /// before exiting non-zero. SIGKILL is uncatchable; deferring
    /// alt-screen entry until after handshake covers most real failure
    /// modes for that case.
    sigint: Signal,
    /// `kill <pid>` from a sibling tool, supervisor, or wrapper.
    sigterm: Signal,
    /// The controlling terminal going away.
    sighup: Signal,
    /// `true` once this client has asked the server to detach.
    detach_pending: bool,
    /// Bare-ESC disambiguation deadline, anchored to the iteration where the
    /// parser first went pending. Re-creating the sleep each loop pass (the
    /// pre-anchor behavior) restarted the full window whenever ANY other arm
    /// fired first — under a steady output stream (status-line clock, shell
    /// highlight repaints) a lone Escape could be deferred far past the
    /// intended window. `None` ⇔ nothing pending.
    esc_deadline: Option<tokio::time::Instant>,
    /// phux-l96p.3: the frame-rate governor for pane output.
    ///
    /// A burst that lands inside the previous frame's window applies its bytes
    /// to the mirrors but withholds the paint, recording the panes it touched;
    /// the `paint_deadline` arm below settles all of them in one composited
    /// frame when the window expires. See [`PaintPacer`] for why the coalescing
    /// drain alone was not enough.
    pacer: PaintPacer,
    /// phux-foz.2: whether the which-key popup is armed at all.
    which_key_enabled: bool,
    /// How long the resolver may sit at a prefix before the popup shows.
    which_key_delay: Duration,
    /// phux-foz.2: which-key popup arming. When the resolver sits at the
    /// pending-prefix state (`<prefix>` pressed, continuation awaited) for
    /// `which_key_delay` without a follow-up chord, the loop pushes a
    /// which-key overlay listing the prefix-table continuations. Config
    /// comes from the same `[keybindings]` snapshot the action finder uses;
    /// with no loaded config there is no resolver (and so no prefix to
    /// hesitate on), so the popup is naturally inert. `None` ⇔ not armed.
    /// Same anchored-deadline pattern as `esc_deadline`: the deadline is
    /// set once when the pending state is first observed and survives
    /// unrelated arms firing, so a busy output stream cannot starve it.
    which_key_deadline: Option<tokio::time::Instant>,
    /// phux-eb0: set by `apply_action_effects` when the user commits a
    /// `switch-session`. Checked after each input-dispatch batch; a value
    /// here makes the loop return `LoopExit::SwitchTo` so the outer
    /// loop re-attaches to the named session on the same connection.
    switch_request: Option<ReattachTarget>,
    /// phux-foz.5: set by `apply_action_effects` when the user commits a
    /// `reload-config` (palette or bound chord). Checked after each
    /// input-dispatch batch; the driver then re-runs the layered config
    /// loader and swaps its config-derived state in place — or keeps the
    /// old state and toasts the error. The `phux config reload` CLI
    /// doorbell reaches the same handler via `FrameOutcome::config_reload`.
    reload_request: bool,
    /// First-use moment consumed by this loop entry. Session switches receive
    /// `None`, so they never repeat attach guidance.
    onboarding_claim: Option<AttachClaim>,
}

impl SessionLoop {
    /// Build every session-scoped local for one attach entry.
    ///
    /// `carried_sidebar_enabled` is the window sidebar's on/off state carried
    /// in from the previous entry when a `switch-session` drove this one;
    /// `None` on the first attach — `[sidebar] enabled` seeds it, and a
    /// carried runtime value wins after that (see `seed_sidebar_enabled`).
    pub(super) fn new(
        negotiated: NegotiatedBootstrap,
        predict_cfg: PredictiveConfig,
        wants_state_sync: bool,
        onboarding_claim: Option<AttachClaim>,
        initial_window: Option<usize>,
        initial_pane: Option<usize>,
        carried_sidebar_enabled: Option<bool>,
    ) -> Result<Self, AttachError> {
        let history_config = HistoryCacheConfig {
            request_max_bytes: negotiated.limits.max_history_page_bytes(),
            ..HistoryCacheConfig::default()
        };
        let cfg = ConfigSeed::load();
        let (plugin_tx, plugin_rx) = tokio::sync::mpsc::unbounded_channel::<PluginRunResult>();
        // phux-huhi: stamp the configured breakpoints once, before anything
        // can be pushed. `OverlayState::push` hands them to each overlay from
        // here, so no overlay construction site names a threshold.
        let mut overlays = OverlayState::new();
        overlays.set_breakpoints(cfg.chrome_breakpoints);
        let viewport_dims = current_viewport().map_or((80, 24), |v| (v.cols.max(1), v.rows.max(1)));
        let cell_px_dims = current_viewport().map_or(HOST_CELL_PX_FALLBACK, |v| host_cell_px(&v));
        Ok(Self {
            acknowledged_input_supported: negotiated
                .server_features
                .contains(ServerFeature::AcknowledgedInput),
            input_replay: None,
            terminal_reply_supported: negotiated
                .server_features
                .contains(ServerFeature::TerminalReply),
            spawn_initial_size_supported: negotiated
                .server_features
                .contains(ServerFeature::SpawnInitialSize),
            wants_state_sync,
            engine_kernel: SessionKernel::with_history_config(
                GhosttyAdapter::new(negotiated.limits),
                negotiated.profile,
                history_config,
            ),
            kernel_effects: KernelEffectBuffer::new(),
            panes: HashMap::new(),
            workspace: Workspace::default(),
            focused_pane: None,
            focus_history: crate::attach::focus::FocusHistory::default(),
            own_client_id: None,
            zoomed: None,
            layout_get_request_id: None,
            next_request_id: 1,
            pending_splits: HashMap::new(),
            pending_windows: HashMap::new(),
            expected_closes: HashSet::new(),
            agent_meta: AgentMetaIndex::default(),
            vcs: VcsIndex::default(),
            sidebar_painter: SidebarPainter::new(cfg.theme),
            status_bar: cfg.status_bar,
            plugin_actions: cfg.plugin_actions,
            plugin_panes: cfg.plugin_panes,
            plugin_tx,
            plugin_rx,
            keybindings_snapshot: cfg.keybindings,
            resolver: cfg.resolver,
            theme: cfg.theme,
            overlays,
            attention_navigation: AttentionNavigation::default(),
            drag: None,
            mouse_optout: HashSet::new(),
            mouse_capture_cfg: cfg.mouse_capture_cfg,
            sidebar_enabled: seed_sidebar_enabled(carried_sidebar_enabled, cfg.sidebar_enabled),
            sidebar_width: cfg.sidebar_width,
            sidebar_edge: cfg.sidebar_edge,
            chrome_breakpoints: cfg.chrome_breakpoints,
            viewport_dims,
            cell_px_dims,
            session_name: String::new(),
            peers: PeerCaches {
                sweep_pending: true,
                ..PeerCaches::default()
            },
            pending_window: initial_window,
            pending_pane: initial_pane,
            parser: StdinParser::new(),
            predict: PredictionState::new(predict_cfg, 80, 24),
            overlay: Overlay,
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
            which_key_enabled: cfg.which_key_enabled,
            which_key_delay: cfg.which_key_delay,
            which_key_deadline: None,
            switch_request: None,
            reload_request: false,
            onboarding_claim,
        })
    }

    /// The `exec` widget feeds the driver spawns bounded interval runners for.
    pub(super) fn exec_feeds(&self) -> Vec<phux_config::widget::ExecFeed> {
        self.status_bar
            .as_ref()
            .map(StatusBarPainter::exec_feeds)
            .unwrap_or_default()
    }

    // ---- small shared projections -------------------------------------

    /// phux-4h5a: fold the driver-local sidebar render state into the
    /// per-frame reservation threaded to every layout site. `toggle-sidebar`
    /// flips `sidebar_enabled`; the change takes effect on the next
    /// iteration. `None` (the default) keeps `content_rect` the full pane
    /// viewport, so the whole path is byte-identical when the sidebar is off.
    fn sidebar(&self) -> Option<SidebarReservation> {
        sidebar_reservation(
            self.viewport_dims.0,
            self.sidebar_enabled,
            self.sidebar_width,
            self.sidebar_edge,
            self.chrome_breakpoints.min_pane_cols,
        )
    }

    /// The residual rect panes tile into once the bar and strip are folded off.
    fn content(&self, sidebar: Option<SidebarReservation>) -> crate::layout::Rect {
        content_rect(
            self.viewport_dims,
            self.status_bar.as_ref().map(StatusBarPainter::position),
            sidebar,
        )
    }

    /// The single chrome-refresh chokepoint, with this driver's inputs bound.
    fn refresh_chrome(&mut self) -> bool {
        refresh_window_chrome(
            self.status_bar.as_mut(),
            &mut self.sidebar_painter,
            &self.workspace,
            &self.panes,
            self.focused_pane.as_ref(),
            self.zoomed.as_ref(),
            self.own_client_id,
            &self.agent_meta,
            &mut self.vcs,
            self.peers.inputs(),
        )
    }

    /// Commit attach onboarding once its notice has reached the render sink.
    fn finish_paint(&mut self, painted: StatusBarPaint) {
        finish_return_onboarding_after_paint(
            &mut self.onboarding_claim,
            self.status_bar.as_ref(),
            painted,
        );
    }

    /// Paint the view at `level`, unless an overlay owns the screen.
    ///
    /// `Chrome` repaints the sidebar strip + status bar in place (no ED2, no
    /// pane re-render); `Full` clears and recomposites because the pane rects
    /// moved under us.
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
        let ls = self.workspace.render_window(self.zoomed.as_ref())?;
        Some(match level {
            RepaintLevel::None => StatusBarPaint::NotPublished,
            RepaintLevel::Chrome => paint_chrome_in_place(
                out,
                ls.as_ref(),
                &self.panes,
                self.focused_pane.as_ref(),
                self.viewport_dims,
                self.status_bar.as_mut(),
                sidebar,
                Some(&mut self.sidebar_painter),
                &self.session_name,
                &self.theme,
            ),
            RepaintLevel::Full => paint_full_frame(
                out,
                ls.as_ref(),
                &mut self.panes,
                &self.engine_kernel,
                self.focused_pane.as_ref(),
                self.viewport_dims,
                self.status_bar.as_mut(),
                sidebar,
                Some(&mut self.sidebar_painter),
                &self.session_name,
                &self.theme,
            ),
        })
    }

    /// Paint the active overlay layer over the current pane composition.
    fn paint_overlay<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        let painted = paint_active_overlay(
            out,
            &self.overlays,
            &self.workspace,
            &mut self.panes,
            &self.engine_kernel,
            self.focused_pane.as_ref(),
            self.zoomed.as_ref(),
            self.viewport_dims,
            self.status_bar.as_mut(),
            sidebar,
            Some(&mut self.sidebar_painter),
            &self.session_name,
            &self.theme,
        );
        self.finish_paint(painted);
    }

    /// Re-run the layered config loader and swap the config-derived state in
    /// place; failures keep the previous config and toast the error.
    fn reload_config<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        let painted = handle_config_reload(
            out,
            &mut self.keybindings_snapshot,
            &mut self.resolver,
            &mut self.theme,
            &mut self.chrome_breakpoints,
            &mut self.status_bar,
            &mut self.sidebar_painter,
            &mut self.plugin_actions,
            &mut self.plugin_panes,
            &mut self.which_key_enabled,
            &mut self.which_key_delay,
            &mut self.overlays,
            &self.workspace,
            &mut self.panes,
            &self.engine_kernel,
            self.focused_pane.as_ref(),
            self.zoomed.as_ref(),
            self.own_client_id,
            &self.agent_meta,
            &mut self.vcs,
            self.peers.inputs(),
            self.viewport_dims,
            sidebar,
            &self.session_name,
        );
        self.finish_paint(painted);
    }

    /// ADR-0040: keep every live pane's `phux.agent/v1` watch in step with the
    /// pane set.
    async fn sync_agent_meta(&mut self, conn: &mut Connection) -> Result<(), AttachError> {
        sync_agent_meta_subscriptions(
            conn,
            self.panes.keys().cloned().collect(),
            &mut self.agent_meta,
            &mut self.next_request_id,
        )
        .await
    }

    /// phux-foz.8 / phux-k0cw: fetch each peer session's persisted layout so
    /// the window picker can list foreign windows as one-step jump rows, and
    /// SUBSCRIBE the same keys so the roster tracks peers live rather than
    /// showing an attach-time snapshot that silently rots. Fire-and-forget:
    /// replies drain through the recv arm, and a peer with nothing persisted
    /// never replies with a value and simply keeps its fallback row.
    async fn sweep_peer_layouts(&mut self, conn: &mut Connection) -> Result<(), AttachError> {
        sync_foreign_layout_subscriptions(
            conn,
            &self.peers.sessions,
            self.peers.focused_session,
            &mut self.next_request_id,
            &mut self.peers.foreign_layout_pending,
            &mut self.peers.foreign_layout_subscribed,
        )
        .await
    }

    /// Hand one inbound frame to the shared server-frame handler.
    fn handle_frame<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        frame: FrameKind,
        sidebar: Option<SidebarReservation>,
        defer_paint: bool,
    ) -> Result<FrameOutcome, AttachError> {
        handle_server_frame(
            &mut self.engine_kernel,
            &mut self.kernel_effects,
            out,
            frame,
            &mut self.panes,
            &mut self.workspace,
            &mut self.focused_pane,
            &mut self.zoomed,
            &mut self.session_name,
            self.peers.focused_session,
            self.status_bar.as_mut(),
            sidebar,
            self.viewport_dims,
            &mut self.predict,
            &self.overlay,
            self.layout_get_request_id,
            &mut self.pending_splits,
            &mut self.pending_windows,
            &mut self.expected_closes,
            &mut self.agent_meta,
            self.overlays.is_active(),
            defer_paint,
        )
    }

    // ---- bootstrap ----------------------------------------------------

    /// Replay the `ATTACHED` frame and issue everything the first paint owes.
    ///
    /// `initial_attached` is the `FrameKind::Attached` frame that
    /// `wait_for_attached` already pulled off the wire; we replay it through
    /// `handle_server_frame` so the focused-pane bookkeeping lives in one
    /// place. Subsequent bootstrap and `TERMINAL_OUTPUT` frames come off the
    /// wire as usual. `Some(exit)` ⇒ the replayed frame ended the attach.
    pub(super) async fn bootstrap<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        initial_attached: FrameKind,
        initial_notice: Option<Notice>,
    ) -> Result<Option<LoopExit>, AttachError> {
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
        self.size_bootstrap_panes(conn, sidebar).await?;
        self.vcs.apply_snapshot(outcome.pane_cwds);
        if let Some((list, focused)) = outcome.sessions {
            self.peers.sessions = list;
            self.peers.focused_session = Some(focused);
        }
        // phux-k0cw.10: the peer sweep belongs HERE in reading order — this is
        // where the session graph it reads (`sessions` / `focused_session`) has
        // just been folded from the ATTACHED replay above — but it is issued from
        // the ONE drain in the recv arm instead, carried there by
        // `peers.sweep_pending`, so the first paint never queues behind peer
        // traffic. Both are loop state, so the deferred call sweeps the same graph
        // a call here would have.
        //
        // ADR-0033: cache our own ClientId (for the "you hold the wheel" badge) and
        // opt into the agent-event stream so `TerminalControl` broadcasts (lease +
        // lifecycle) reach this client.
        if outcome.own_client_id.is_some() {
            self.own_client_id = outcome.own_client_id;
        }
        self.subscribe_bootstrap(conn, outcome.subscribe_layout)
            .await?;
        // phux-4li.17: seed the window/tab strip from the bootstrap layout so
        // the first bootstrap-driven bar paint shows the window.
        // phux-4h5a: the sidebar painter tracks the same window list so the strip's
        // tab list stays current whenever the bar's does.
        //
        // phux-k0cw: the peer sweep has not answered yet at bootstrap, so
        // zones 1 and 3 start empty and fill as the replies land. That is
        // the intended shape: the queue holds at zero rows rather than
        // animating to correctness in the user's peripheral vision on
        // every attach.
        self.refresh_chrome();
        self.seed_initial_notice(initial_notice, moment);
        self.show_intro(out, sidebar, moment);
        Ok(None)
    }

    /// phux-e9fd: size every bootstrap pane's PTY to the rect this client will
    /// actually paint it into, before anything else runs.
    ///
    /// The server sizes each pane from the ATTACH viewport
    /// (`apply_attach_viewport`), which is the client's OUTER terminal —
    /// chrome included. The client paints panes into `content_rect`, which is
    /// one row shorter whenever a status bar is docked. Without this call the
    /// mirror is a row taller than the rect it is clipped into, so the pane's
    /// bottom line is never painted and the bar looks like it overwrote it.
    /// The self-heal users notice — resize, split, toggle the sidebar — is
    /// just the first reflow that DID emit `TERMINAL_RESIZE`.
    ///
    /// The server side already defers the off-by-one here in as many words
    /// ("the client's concern via the post-attach `TERMINAL_RESIZE` reflow
    /// path"); this is that path, and until now nothing called it. An empty
    /// `prev_rects` makes `compute_reflow` report every leaf as changed — its
    /// documented first-attach rule — so each pane is sized exactly once.
    async fn size_bootstrap_panes(
        &self,
        conn: &mut Connection,
        sidebar: Option<SidebarReservation>,
    ) -> Result<(), AttachError> {
        emit_view_reflow(
            conn,
            &self.workspace,
            self.zoomed.as_ref(),
            &HashMap::new(),
            self.content(sidebar),
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
        conn.send(&FrameKind::SubscribeEvents { terminal: None })
            .await?;
        // phux-foz.5: watch the config-reload doorbell so a `phux config
        // reload` from any shell reaches this client as a METADATA_CHANGED
        // broadcast (the config itself never crosses the wire — we re-read
        // our own file). Torn down implicitly on detach like every metadata
        // subscription.
        conn.send(&FrameKind::SubscribeMetadata {
            scope: Scope::Global,
            key: CONFIG_RELOAD_KEY.to_owned(),
        })
        .await?;
        if subscribe_layout && let Some(session) = self.peers.focused_session {
            // phux-4li.5: ask the server for any persisted layout, then
            // subscribe to future mutations. Both frames are best-effort —
            // if the server rejects them with an ERROR (we'd see one in a
            // later loop iteration) we just stay in the single-pane
            // bootstrap. phux-jy4t: keyed per session so we restore THIS
            // session's layout, not whatever sibling wrote the key last.
            let key = layout_key(session);
            let req_id = self.next_request_id;
            self.layout_get_request_id = Some(req_id);
            self.next_request_id = self.next_request_id.wrapping_add(1);
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
        // ADR-0040: read + watch every bootstrap pane's `phux.agent/v1` record
        // so window labels can prefer structured agent identity from the first
        // paint. The same sweep re-runs whenever the pane set changes.
        self.sync_agent_meta(conn).await?;
        self.adopt_input_replay(conn).await
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
    /// Every operation still queued from before the reconnect (or the
    /// session-switch drain) is re-decided against this connection's server
    /// incarnation: survivors are resent under their ORIGINAL operation ids —
    /// the server's dedupe cache is what makes that honest — and everything
    /// that cannot be replayed (expired, incarnation changed, feature gone)
    /// resolves loudly as a status-bar notice instead of silently dropping
    /// or doubling.
    async fn adopt_input_replay(&mut self, conn: &mut Connection) -> Result<(), AttachError> {
        let Some(journal) = self.input_replay.clone() else {
            return Ok(());
        };
        let mut reports = journal
            .borrow_mut()
            .begin_connection(conn.server_id(), self.acknowledged_input_supported);
        let (more, replay_frame) = journal.borrow_mut().next_frame(&mut self.next_request_id);
        reports.extend(more);
        let now = std::time::Instant::now();
        for report in reports {
            if matches!(
                report.disposition,
                crate::attach::input_replay::ReplayDisposition::Delivered
            ) {
                continue;
            }
            let line = report.notice_line();
            if let Some(sb) = self.status_bar.as_mut() {
                let _ = sb.set_notice(crate::render::chrome::status_bar::Notice::warn(line), now);
            } else {
                tracing::warn!(line = %line, "acknowledged paste stranded");
            }
        }
        if let Some(frame) = replay_frame {
            conn.send(&frame).await?;
        }
        Ok(())
    }

    /// ADR-0053: the reply to one of the journal's own `APPLY_INPUT`
    /// attempts. Delivery is silent; anything else raises a notice, and the
    /// next queued operation (if any) goes on the wire behind the resolution.
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
        let (more, next_frame) = journal.borrow_mut().next_frame(&mut self.next_request_id);
        reports.extend(more);
        let now = std::time::Instant::now();
        for report in reports {
            if matches!(
                report.disposition,
                crate::attach::input_replay::ReplayDisposition::Delivered
            ) {
                continue;
            }
            let line = report.notice_line();
            let shown = self.status_bar.as_mut().is_some_and(|sb| {
                sb.set_notice(
                    crate::render::chrome::status_bar::Notice::warn(line.clone()),
                    now,
                )
            });
            if shown {
                repaint.raise_chrome();
            } else {
                tracing::warn!(line = %line, "acknowledged paste outcome");
            }
        }
        if let Some(frame) = next_frame {
            super::session_io::send_unless_peer_gone(conn, &frame).await?;
        }
        Ok(())
    }

    /// phux-i0e8.2.3: seed the post-reconnect notice now that the session is
    /// attached and the bar painter exists. The first bar paint — driven by
    /// the initial `TERMINAL_SNAPSHOT` burst that follows ATTACHED — picks it
    /// up, and the ordinary 1 s `status_tick` expires it, so "re-attached
    /// after server restart" is visible inside the live TUI instead of on
    /// the cooked terminal the alt screen replaced.
    fn seed_initial_notice(&mut self, initial_notice: Option<Notice>, moment: AttachMoment) {
        let return_notice_available = initial_notice.is_none() && moment == AttachMoment::Return;
        let initial_notice = initial_notice.or_else(|| {
            return_notice_available.then(|| Notice::info(crate::attach::onboarding::RETURN_NOTICE))
        });
        let notice_accepted = apply_initial_notice(self.status_bar.as_mut(), initial_notice);
        if moment == AttachMoment::Return && (!return_notice_available || !notice_accepted) {
            self.onboarding_claim.take();
        }
    }

    /// The introduction floats over the live pane after bootstrap. It is a
    /// passthrough notice: the first key dismisses it and continues through the
    /// normal resolver/pane route, so guidance never taxes the user's intent.
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
            crate::attach::onboarding::hint_lines(self.keybindings_snapshot.as_ref()),
            &self.theme,
        )));
        paint_active_overlay(
            out,
            &self.overlays,
            &self.workspace,
            &mut self.panes,
            &self.engine_kernel,
            self.focused_pane.as_ref(),
            self.zoomed.as_ref(),
            self.viewport_dims,
            self.status_bar.as_mut(),
            sidebar,
            Some(&mut self.sidebar_painter),
            &self.session_name,
            &self.theme,
        );
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
        self.select_next_event(conn, out, sidebar).await
    }

    /// Bring the outer terminal's modes, the attention ladder, and any
    /// dropped-backlog resync up to date before the loop parks.
    fn settle_iteration<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        needs_resync: Option<&AtomicBool>,
    ) -> Result<(), AttachError> {
        // phux-npb3: capture follows focus. Closed panes are pruned so a
        // recycled TerminalId can never inherit a stale opt-out.
        if !self.mouse_optout.is_empty() {
            self.mouse_optout.retain(|id| self.panes.contains_key(id));
        }
        self.settle_focus_seen(out, sidebar);
        // Re-derive the outer-terminal mouse-tracking DECSET from the focused
        // pane's opt-out state every iteration — one call site covers every
        // way focus or the set can change (set-pane, click-to-focus, keybind
        // navigation, spawn/close reflows). `sync_mouse_capture` is a no-op
        // when nothing changed, so the steady-state cost is one bool compare.
        let want_capture = desired_mouse_capture(
            self.mouse_capture_cfg,
            self.focused_pane.as_ref(),
            &self.mouse_optout,
        );
        sync_mouse_capture(out, want_capture).map_err(AttachError::Io)?;
        // phux-wrnm: hover reporting follows the overlay stack the same way
        // capture follows focus — raised while a context menu wants to track
        // the pointer with no button held, dropped as soon as it closes.
        sync_hover_tracking(out, self.overlays.wants_pointer_hover()).map_err(AttachError::Io)?;
        self.repaint_after_resync(out, sidebar, needs_resync);
        crate::attach::render_prof::tick();
        Ok(())
    }

    /// The attention ladder's `seen` half: the pane the user is looking at
    /// has, by definition, been looked at. One hash lookup per iteration —
    /// and it covers EVERY way focus can move (click, keybind, split,
    /// window switch, a peer's layout broadcast) without a call at each
    /// site. A later agent-state change on an unfocused pane re-arms the
    /// bit (see `server_frame::note_agent_change`), which is what lets a
    /// background agent's `done` climb back above the working ones.
    ///
    /// The FLIP is a chrome trigger, not a silent side effect. The focus
    /// action that made this pane focused ran in the PREVIOUS iteration, and
    /// it recomputed the chrome while `seen` was still false — so the strip
    /// it painted still carries the filled "look at me" diamond, bold,
    /// pinned above every working agent, about the very pane the user is now
    /// looking at. Nothing else recomputes `agent_entries` (the status tick
    /// paints only the bar), so without this the row keeps lying until some
    /// unrelated chrome event happens to fire — indefinitely, in a
    /// single-agent session. That defeats the ladder's central promise:
    /// visiting a pane demotes it.
    ///
    /// ADR-0029: demoting a ladder row touches no pane interior, so this
    /// is an in-place CHROME paint, never a full-frame clear. Gated on
    /// the painter's own change report, so a focus change that moves no
    /// agent row costs zero bytes.
    fn settle_focus_seen<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        if !mark_focused_seen(&mut self.panes, self.focused_pane.as_ref()) {
            return;
        }
        if self.refresh_chrome() {
            self.repaint_view(out, sidebar, RepaintLevel::Chrome);
        }
    }

    /// phux-fysb: the off-loop stdout writer dropped a stale backlog under
    /// a slow terminal. Repaint the latest state from scratch — a
    /// self-contained full frame (or overlay) supersedes the dropped
    /// diffs. `swap(false)` clears the flag, but any set re-armed by THIS
    /// repaint's own flushes is preserved for the next iteration. Checked
    /// before parking so a resync that landed during the prior arm is
    /// serviced promptly.
    fn repaint_after_resync<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        needs_resync: Option<&AtomicBool>,
    ) {
        if !needs_resync.is_some_and(|flag| flag.swap(false, Ordering::AcqRel)) {
            return;
        }
        if self.overlays.is_active() {
            self.paint_overlay(out, sidebar);
        } else {
            self.repaint_view(out, sidebar, RepaintLevel::Full);
        }
    }

    /// Arm this iteration's timers and park on every wake-up source.
    ///
    /// Stdin is polled before inbound frames so a local keystroke is
    /// dispatched promptly rather than waiting behind an output burst. One
    /// read is bounded by `stdin_buf`; the inbound arm is bounded by
    /// `FRAME_COALESCE_CAP`, so neither starves the other.
    async fn select_next_event<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) -> Result<Step, AttachError> {
        // Arm the bare-ESC idle timer only when a lone ESC is actually
        // waiting to be disambiguated, anchored to the first iteration that
        // saw it (the deadline survives other arms firing — see
        // `esc_deadline`). phux-l96p.4: this used to arm for ANY in-progress
        // sequence, but `flush` only emits from `State::Escape`; every other
        // state's timer fired, produced nothing, and cost the loop a wake-up
        // and an empty dispatch batch.
        if self.parser.esc_pending() {
            self.esc_deadline
                .get_or_insert_with(|| tokio::time::Instant::now() + ESC_FLUSH_IDLE);
        } else {
            self.esc_deadline = None;
        }
        let flush_sleep = sleep_until_or_pending(self.esc_deadline);
        // phux-foz.2: (dis)arm the which-key deadline from the resolver's
        // CURRENT pending state. An early continuation chord (dispatched
        // in the stdin arm) leaves the resolver non-pending, so the next
        // pass through here disarms the timer before it can fire — the
        // popup is suppressed without any explicit cancellation call.
        update_which_key_deadline(
            &mut self.which_key_deadline,
            self.resolver
                .as_ref()
                .is_some_and(phux_config::keybind::Resolver::pending_at_prefix),
            self.which_key_enabled,
            self.overlays.is_active(),
            tokio::time::Instant::now(),
            self.which_key_delay,
        );
        let which_key_sleep = sleep_until_or_pending(self.which_key_deadline);
        // phux-nz4.5: per-bar repaint cadence. Driven by the slowest
        // widget that wants periodic refresh (currently floor-1s via the
        // `time` widget). Empty bar ⇒ `Pending` forever so this select!
        // arm never fires.
        let status_tick = sleep_for_or_pending(
            self.status_bar
                .as_ref()
                .and_then(StatusBarPainter::min_poll_interval),
        );
        // Synchronized-output transactions intentionally span arbitrary
        // socket reads, so their deadline is pane state rather than a
        // per-batch timer. A stuck producer gets one bounded recovery paint;
        // later bytes re-arm suppression if mode 2026 is still set.
        // phux-l96p.3: the frame pacer's settle deadline. Armed only while
        // some pane's paint is owed, so an idle attach adds no timer at all.
        let paint_sleep = sleep_until_or_pending(self.pacer.deadline());
        let sync_output_sleep = sleep_until_or_pending(
            self.panes
                .values()
                .filter_map(|slot| slot.sync_output_since)
                .map(|since| since + SYNC_OUTPUT_WATCHDOG)
                .min(),
        );

        tokio::select! {
            biased;

            n = self.stdin.read(&mut self.stdin_buf) => self.on_stdin(conn, out, sidebar, n).await,

            // Inbound frames are drained in a `FRAME_COALESCE_CAP`-bounded
            // batch so a redraw burst paints once; bounded so it cannot
            // starve the stdin arm polled above it.
            frame = conn.recv() => self.on_server_frame(conn, out, sidebar, frame).await,

            // phux-l96p.3: the withheld frames' window expired. Settle every
            // pane whose paint was held back, in ONE composited frame. Polled
            // above the other timers (but below stdin and inbound frames) so a
            // settle cannot be starved by a chatty widget tick.
            () = paint_sleep => {
                self.settle_withheld_panes(out, sidebar);
                Ok(Step::Continue)
            }

            // Bound the failure mode of an application that omits `?2026l`.
            // Expose the latest complete mirror once, then let subsequent
            // output re-arm the transaction watchdog.
            () = sync_output_sleep => {
                self.on_sync_output_timeout(out, sidebar);
                Ok(Step::Continue)
            }

            // Bare-ESC idle timeout. Only armed when the parser has
            // pending state; resolves an ambiguous lone ESC into the
            // Escape key (see input::StdinParser::flush docs).
            () = flush_sleep => self.on_esc_flush(conn, out, sidebar).await,

            // phux-foz.2: which-key idle timeout. Armed only while the
            // resolver sits at the pending-prefix state (see the update
            // above); fires once per hesitation. Pushing the popup does
            // not touch the resolver — the pending prefix stays live, so
            // the next chord executes exactly as if the popup never
            // appeared (the dispatcher's passthrough branch dismisses it
            // on the way through).
            () = which_key_sleep => {
                self.on_which_key_timeout(out, sidebar);
                Ok(Step::Continue)
            }

            // SIGWINCH — terminal was resized. Read the new viewport
            // and ship a VIEWPORT_RESIZE upstream (SPEC §7.1 / §10.5).
            // The server uses this to recompute layout and update the
            // attached pane's dims. On query failure we fall back to a
            // sane default (logged) rather than skip the frame — the
            // server still benefits from knowing a resize happened.
            _ = self.sigwinch.recv() => {
                self.on_resize(conn, out, sidebar).await?;
                Ok(Step::Continue)
            }

            // phux-nz4.5: periodic status-bar repaint (e.g. for the
            // `time` widget). Only fires when at least one widget has a
            // `poll_interval`. Paints in place — no pane re-render, no
            // full-screen redraw.
            () = status_tick => {
                self.on_status_tick(out, sidebar);
                Ok(Step::Continue)
            }

            // phux-r82.5: a spawned plugin action finished. Successes just
            // log (no modal to dismiss on the happy path); failures push a
            // dismissable toast carrying the captured output, so a broken
            // plugin is *seen* without ever having blocked the input loop.
            // The channel can't close while this loop holds `plugin_tx`,
            // so the `Some` pattern always matches when the arm fires.
            Some(result) = self.plugin_rx.recv() => {
                self.on_plugin_result(out, sidebar, &result);
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

            // SIGHUP — controlling terminal went away. Restore and exit
            // 129. There is no live outer terminal to clean up, but the
            // termios restore is harmless on a dead tty and keeps the
            // cleanup path uniform.
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
                self.detach_pending = true;
            }
            return Ok(Step::Continue);
        }
        // Decode into the retained buffer, then hand it to the dispatcher by
        // move so the borrow checker sees one owner; `dispatch_batch` returns
        // it drained-but-allocated.
        let mut events = std::mem::take(&mut self.input_events);
        self.parser.feed_into(&self.stdin_buf[..n], &mut events);
        self.dispatch_batch(conn, out, sidebar, events).await
    }

    /// The bare-ESC flush runs the same batch handling as a stdin read: a
    /// flushed event may complete `toggle-zoom` or `toggle-sidebar`
    /// (phux-x2hm), can carry the final chord of a `<leader> a` selection
    /// committed via Enter (phux-eb0), and can commit `reload-config` from a
    /// palette selection (phux-foz.5).
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
    /// whatever it moved.
    ///
    /// Takes the event buffer by move and hands it back drained, so the
    /// keystroke path reuses one allocation (see `input_events`).
    async fn dispatch_batch<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        mut events: Vec<phux_protocol::input::InputEvent>,
    ) -> Result<Step, AttachError> {
        // phux-l96p.4: an empty batch is common — a read that carries only the
        // first bytes of a `CSI` sequence decodes to nothing, and so does an
        // idle flush that finds no lone ESC. Nothing below can move any state
        // without an event to move it, so everything below (a tiling diff, a
        // `DispatchCtx` with its cloned focus history and sidebar targets, and
        // an unconditional overlay repaint) was pure per-read waste.
        if events.is_empty() {
            self.input_events = events;
            return Ok(Step::Continue);
        }
        // phux-l96p.3: whether this batch is the kind of input that expects
        // output back. Computed BEFORE dispatch (which drains `events`) and
        // armed after it, so the pane it is keyed to is the one focus landed
        // on — a click that moves focus marks the pane the user just picked,
        // not the one they left.
        let expects_reply = events.iter().any(input_expects_a_reply);
        // Capture the pre-dispatch view so zoom and sidebar toggles can
        // diff against it and resize each changed pane's PTY. Taken
        // before dispatch mutates either piece of view geometry.
        let prev_zoomed = self.zoomed.clone();
        let prev_sidebar = sidebar;
        let prev_view_rects = view_rects(
            &self.workspace,
            prev_zoomed.as_ref(),
            self.content(sidebar),
            self.viewport_dims,
        );
        // phux-l96p.4: batch this batch's wire writes. One keystroke still
        // costs one write; a read that decoded several events (auto-repeat,
        // arrow spam, a mouse drag burst) now costs one write rather than one
        // per event. The cork spans only the synchronous dispatch of bytes we
        // have already read and is released before the loop parks, so it is a
        // batching cork and never a linger.
        conn.cork();
        let dispatched = self.dispatch_input(conn, out, sidebar, &mut events).await;
        // Uncork on BOTH paths: a dispatch that errored half way through must
        // not strand the frames it did emit in the cork buffer, and the next
        // `send` must not find the writer still corked.
        let shipped = conn.uncork().await;
        self.input_events = events;
        let layout_changed = dispatched?;
        shipped?;
        // Arm the pacer's reply grace now that dispatch has settled focus.
        // Keyed to the focused pane: the output that must not wait is the
        // reply from the pane the user acted on, and lifting pacing for every
        // OTHER pane would un-pace a flood elsewhere on the screen — which is
        // the coalescing the pacer exists to do. Cleared by TIME, never by a
        // paint: a reply is often several frames.
        if expects_reply {
            self.pacer
                .note_input(self.focused_pane.as_ref(), tokio::time::Instant::now());
        }
        // phux-4h5a: a `toggle-sidebar` in this batch flipped
        // `sidebar_enabled`. Re-fold it into the reservation so the
        // reflow + repaint below tile into the NEW content rect this
        // iteration rather than waiting a frame.
        let sidebar = self.sidebar();
        // phux-eb0: a committed `switch-session` ends this loop so
        // the outer driver re-attaches. Return BEFORE any repaint
        // — the new session's ATTACHED + snapshot will repaint.
        if let Some(target) = self.switch_request.take() {
            return Ok(Step::Exit(LoopExit::SwitchTo {
                target,
                sidebar_enabled: self.sidebar_enabled,
            }));
        }
        // Zoom and sidebar toggles both change pane geometry. Resize
        // every affected PTY before repainting so applications reflow
        // to the same rectangle the client is about to render.
        if self.zoomed != prev_zoomed || sidebar != prev_sidebar {
            emit_view_reflow(
                conn,
                &self.workspace,
                self.zoomed.as_ref(),
                &prev_view_rects,
                self.content(sidebar),
            )
            .await?;
        }
        if layout_changed {
            // ADR-0040: an input action may have split/closed panes;
            // keep the agent-metadata watches in step with the set.
            self.sync_agent_meta(conn).await?;
            self.refresh_chrome();
            // phux-5ke.4: on overlay dismiss the dispatcher
            // sets layout_changed=true; the full-frame repaint
            // here restores pane content under the now-gone
            // modal. When the overlay is still active (e.g.
            // a push happened in the same batch) we skip the
            // pane repaint and go straight to overlay paint.
            self.repaint_view(out, sidebar, RepaintLevel::Full);
        }
        if self.overlays.is_active() {
            self.paint_overlay(out, sidebar);
        }
        // phux-foz.5: a `reload-config` committed in this batch
        // (palette row or bound chord). Runs LAST in the arm so
        // its repaint reflects the new theme/bar.
        if self.reload_request {
            self.reload_request = false;
            self.reload_config(out, sidebar);
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
        // phux-foz.9: the agents-section row -> window mapping,
        // snapshotted from the strip painter so a click on an
        // agent row hit-tests against exactly what was painted.
        //
        // phux-l96p.4: `sidebar_targets` is read at exactly one site —
        // `route_sidebar_click`, off a mouse event — and building it clones a
        // String per roster row. A keyboard batch can never reach that site,
        // so it gets the empty snapshot instead of paying for one.
        let sidebar_targets = if events
            .iter()
            .any(|ev| matches!(ev, phux_protocol::input::InputEvent::Mouse(_)))
        {
            self.sidebar_painter.click_targets()
        } else {
            crate::render::chrome::sidebar::SidebarTargets::default()
        };
        let mut ctx = DispatchCtx {
            engine_kernel: &mut self.engine_kernel,
            resolver: self.resolver.as_mut(),
            focus_history: self.focus_history.clone(),
            workspace: &mut self.workspace,
            viewport: self.viewport_dims,
            cell_px: self.cell_px_dims,
            next_request_id: &mut self.next_request_id,
            spawn_initial_size_supported: self.spawn_initial_size_supported,
            pending_splits: &mut self.pending_splits,
            pending_windows: &mut self.pending_windows,
            expected_closes: &mut self.expected_closes,
            overlays: &mut self.overlays,
            keybindings: self.keybindings_snapshot.as_ref(),
            theme: &self.theme,
            sessions: &self.peers.sessions,
            foreign_layouts: &self.peers.foreign_layouts,
            foreign_agents: &self.peers.foreign_agents,
            focused_session: self.peers.focused_session,
            session_name: &mut self.session_name,
            switch_request: &mut self.switch_request,
            zoomed: &mut self.zoomed,
            sidebar,
            sidebar_enabled: &mut self.sidebar_enabled,
            sidebar_width: self.sidebar_width,
            chrome: self.chrome_breakpoints,
            sidebar_targets: &sidebar_targets,
            bar: self.status_bar.as_ref().map(StatusBarPainter::position),
            status_bar: self.status_bar.as_ref(),
            drag: &mut self.drag,
            mouse_optout: &mut self.mouse_optout,
            attention_navigation: &mut self.attention_navigation,
            plugin_actions: &self.plugin_actions,
            plugin_panes: &self.plugin_panes,
            plugin_tx: Some(&self.plugin_tx),
            reload_request: &mut self.reload_request,
            agent_meta: &self.agent_meta.records,
            vcs: &mut self.vcs,
            input_replay: self.input_replay.as_deref(),
        };
        let layout_changed = dispatch_input_events(
            out,
            conn,
            events,
            &mut self.focused_pane,
            &mut self.detach_pending,
            &mut self.predict,
            &self.overlay,
            &mut self.panes,
            &mut ctx,
        )
        .await?;
        self.focus_history = ctx.focus_history;
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
                // Server closed the socket without a `DETACHED`
                // frame — treat it as a clean shutdown because
                // the user requested detach. Otherwise the loop
                // bubbles the disconnect up unchanged. No frame
                // arrived, so there is no stated reason to carry.
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
        // Per-pane last-wins: a frame defers its paint iff a
        // LATER frame in the burst repaints the same pane, so
        // every touched pane (focused or not) settles exactly
        // once on its final frame. No pane is left stale, and
        // the hot single-pane case collapses to one paint.
        let defer_flags = coalesce_defer_flags(&batch, frame_paint_target);
        // ADR-0029 §2: the loop-level repaint triggers in this
        // batch RAISE a level instead of painting inline, and
        // the accumulator is drained ONCE below. A burst of
        // twenty `MetadataChanged` frames (a live agent
        // detector publishing state transitions across nine
        // panes) therefore collapses into a single in-place
        // sidebar paint rather than twenty full-screen clears.
        // Declared HERE, inside the frame handler, deliberately:
        // the stdin / ESC-flush path shadows `sidebar` with a
        // freshly recomputed reservation so a same-iteration
        // `toggle-sidebar` takes effect, and a drain hoisted
        // outside the `select!` would capture the stale outer
        // one. This path does not shadow it.
        let mut repaint = RepaintAccumulator::default();
        // phux-l96p.3: one pacing decision for the whole burst. Admitted, the
        // burst behaves exactly as before (per-pane last-wins coalescing, then
        // one paint). Refused, EVERY output frame in it defers: the bytes still
        // reach the mirrors, the panes they touched are recorded, and the
        // `paint_deadline` arm settles them together when the window expires.
        // Deciding once per burst rather than once per frame is what makes the
        // settle a single composited frame.
        let now = tokio::time::Instant::now();
        // One pass over the burst answers both questions the pacer needs: does
        // this carry output for the pane the user last acted on (so it is a
        // reply, not a flood), and how long did that reply take (the sample
        // the grace is sized from).
        let is_reply = self
            .pacer
            .observe_reply(now, batch.iter().filter_map(frame_paint_target));
        let paint_now = self.pacer.admit(now, is_reply);
        for (frame_idx, frame) in batch.into_iter().enumerate() {
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
        }
        self.drain_repaint(out, sidebar, &mut repaint);
        // phux-l96p.3: settle whatever the pacer is still holding, rather
        // than leaving it to the `paint_deadline` arm. Two ways to get here,
        // and BOTH are cases the timer cannot be relied on for:
        //
        // * This burst was admitted. `admit` has just pushed `next_allowed`
        //   out to `now + interval`, so the deadline check below can never
        //   fire on this pass — and the `paint_deadline` arm sits third in a
        //   `biased` select behind `conn.recv()`, which a saturating producer
        //   keeps permanently ready. A pane that emitted one line during an
        //   earlier refused window and then went quiet would stay unpainted
        //   for as long as any other pane kept talking. The debt belongs in
        //   this frame.
        // * Handling the burst itself outran the window, same starvation
        //   shape without the admitted-burst part.
        //
        // Placed after `drain_repaint` so a `RepaintLevel::Full` in this batch
        // has already cleared the debt it just redrew.
        if burst_settles_debt(
            paint_now,
            self.pacer
                .deadline()
                .is_some_and(|at| at <= tokio::time::Instant::now()),
        ) {
            self.settle_withheld_panes(out, sidebar);
        }
        // phux-k0cw.10: the first paint is behind us, so the
        // peer sweep can go out now. Placed after the drain,
        // not before it, so the frames it sends never sit
        // between a snapshot burst and the paint that burst
        // produces.
        //
        // Conditioned on reaching the drain rather than on
        // `drained.level`: a batch that paints nothing still
        // means the burst is drained and the loop is idle
        // enough to spend, and gating on a paint that a quiet
        // attach may never produce would strand the roster
        // empty for the whole session. The zones already
        // tolerate this arriving late — zone 1 holds at zero
        // rows until the first full fold and zone 3 renders
        // nothing until a roster entry exists.
        //
        // The per-pane agent sweep needs no deferral of its
        // own: it hangs off the layout replies this sweep
        // asks for, so it lands strictly later by
        // construction.
        if self.peers.sweep_pending {
            self.peers.sweep_pending = false;
            self.sweep_peer_layouts(conn).await?;
        }
        Ok(Step::Continue)
    }

    /// Fold a peer-scoped reply into the foreign caches, or hand the frame
    /// on to the general handler.
    ///
    /// `None` ⇒ the frame was consumed here. The general handler's
    /// `MetadataValue` arm would drop these unmatched request ids, so they
    /// never reach it.
    async fn intercept_peer_reply(
        &mut self,
        conn: &mut Connection,
        frame: FrameKind,
        repaint: &mut RepaintAccumulator,
    ) -> Result<Option<FrameKind>, AttachError> {
        match frame {
            // phux-foz.8: a peer session's persisted-layout GET reply.
            // Picker/fleet display data only — decode into the cache and skip
            // the general frame handler.
            FrameKind::MetadataValue { request_id, value }
                if self.peers.foreign_layout_pending.contains_key(&request_id) =>
            {
                self.fold_peer_layout(conn, request_id, value.as_deref(), repaint)
                    .await?;
                Ok(None)
            }
            // phux-jpqd: a foreign pane's agent-record GET
            // reply. Fold into the fleet cache and refresh a
            // live fleet; same intercept shape as the layout
            // reply (the general handler would drop it).
            FrameKind::MetadataValue { request_id, value }
                if self.peers.foreign_agent_pending.contains_key(&request_id) =>
            {
                if let Some(id) = self.peers.foreign_agent_pending.remove(&request_id) {
                    apply_foreign_agent_reply(&mut self.peers.foreign_agents, id, value.as_deref());
                    repaint.raise_fleet();
                }
                Ok(None)
            }
            // phux-h5hj.12: the same two lookups for the
            // *refusal* shape. `proto.md` §9 lets a server
            // answer a request it will not serve with a
            // correlated ERROR instead of the reply frame,
            // and a peer session's Group is exactly the kind
            // of scope a policy refuses. Without this arm the
            // pending entry is never removed: the row stays
            // blank for the life of the attach, the map grows
            // by one per refused read, and the ERROR falls
            // through to `handle_server_frame` as if it were
            // an unrelated notice. Dropping the entry is the
            // whole fix — a refused read has no value to
            // apply, and the fleet projection already renders
            // a session it knows nothing about.
            FrameKind::Error {
                request_id: Some(request_id),
                ..
            } if self.peers.foreign_layout_pending.contains_key(&request_id)
                || self.peers.foreign_agent_pending.contains_key(&request_id) =>
            {
                self.peers.foreign_layout_pending.remove(&request_id);
                self.peers.foreign_agent_pending.remove(&request_id);
                Ok(None)
            }
            // ADR-0053: the reply to one of the journal's own APPLY_INPUT
            // attempts, consumed here — the same intercept shape as the
            // foreign-layout replies above — because the attached-phase frame
            // handler has no COMMAND_RESULT arm.
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

    /// phux-jpqd: once a peer's pane tree is known, fetch each pane's agent
    /// record (prune stale first) so the fleet dashboard's foreign rows carry
    /// agent state, then refresh a live fleet in place.
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
        apply_foreign_layout_reply(&mut self.peers.foreign_layouts, session, value);
        prune_foreign_agents(
            &mut self.peers.foreign_agents,
            &mut self.peers.foreign_agent_subscribed,
            &self.peers.foreign_layouts,
        );
        if let Some(ws) = self.peers.foreign_layouts.get(&session) {
            sync_foreign_agent_subscriptions(
                conn,
                ws,
                &mut self.next_request_id,
                &mut self.peers.foreign_agent_pending,
                &mut self.peers.foreign_agent_subscribed,
            )
            .await?;
        }
        // ADR-0029 §2: raise, drain once (after the burst). A peer's layout
        // reply arrives with one agent-record reply per foreign pane right
        // behind it; refreshing inline would re-project (and repaint) the
        // dashboard once per reply.
        repaint.raise_fleet();
        Ok(())
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
        // phux-tnh: snapshot the current per-leaf rects
        // BEFORE the frame may fold (close) or split the
        // layout, so a TerminalClosed/Spawned can diff
        // against them and resize survivors whose dims
        // changed. Only meaningful in multi-pane mode;
        // skipped (no cost) on the single-pane hot path.
        // phux-x2hm: snapshot the zoom-honoring rects so a
        // close/spawn diffs against what is actually on screen;
        // a TerminalSpawned-ok un-zooms (sets `zoomed = None`)
        // inside `handle_server_frame`, so the post-frame view
        // below correctly reflows every pane back to its tile.
        let prev_rects = self.leaf_rects(sidebar);
        let focused_before_frame = self.focused_pane.clone();
        let mut outcome = self.handle_frame(out, frame, sidebar, defer_paint)?;
        send_terminal_replies(
            conn,
            take_terminal_replies(&mut outcome, self.terminal_reply_supported),
        )
        .await?;
        self.focus_history
            .observe(focused_before_frame, self.focused_pane.as_ref());
        self.focus_history
            .repair(self.focused_pane.as_ref(), &self.workspace);
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
        self.attach_discovered_panes(conn, &outcome.attach_panes)
            .await?;
        let fleet_dirty = fleet_projection_dirty(&outcome);
        self.fold_peer_outcome(&mut outcome, repaint);
        self.finish_paint(outcome.status_bar_painted);
        self.resync_watches(conn, &mut outcome).await?;
        self.fold_chrome_and_notices(&mut outcome, repaint);
        self.emit_outcome_requests(conn, &mut outcome, sidebar, prev_rects.as_ref())
            .await?;
        self.settle_frame_view(out, &outcome, sidebar, fleet_dirty, repaint);
        Ok(FrameStep::Done)
    }

    /// The per-leaf rect map of the zoom- and sidebar-honoring view, or
    /// `None` when there is no window or its tree is unseeded.
    fn leaf_rects(
        &self,
        sidebar: Option<SidebarReservation>,
    ) -> Option<HashMap<TerminalId, crate::layout::Rect>> {
        let ls = self.workspace.render_window(self.zoomed.as_ref())?;
        ls.tree.as_ref().map(|_| {
            crate::attach::multi_pane::compute_layout_in(
                ls.as_ref(),
                self.content(sidebar),
                self.viewport_dims,
            )
            .rects
        })
    }

    /// The engine rejected a generation after emitting a typed resync status;
    /// issue a fresh in-connection ATTACH while the frozen published replica
    /// stays visible.
    async fn request_rebootstrap(&self, conn: &mut Connection) -> Result<FrameStep, AttachError> {
        if self.session_name.is_empty() {
            return Err(AttachError::Protocol(
                "engine requested rebootstrap before ATTACHED named the session".to_owned(),
            ));
        }
        let attach_id = send_attach(conn, AttachTarget::ByName(self.session_name.clone())).await?;
        tracing::warn!(
            attach_id,
            session = %self.session_name,
            "engine generation rejected; requested replacement bootstrap"
        );
        Ok(FrameStep::Rebootstrap)
    }

    /// A peer headless placement can add a layout leaf without this attached
    /// client being subscribed to the new Terminal. Attach each discovered
    /// leaf so its snapshot creates a `PaneSlot` and renders in place.
    async fn attach_discovered_panes(
        &mut self,
        conn: &mut Connection,
        terminal_ids: &[TerminalId],
    ) -> Result<(), AttachError> {
        for terminal_id in terminal_ids {
            let request_id = self.next_request_id;
            self.next_request_id = self.next_request_id.wrapping_add(1);
            send_unless_peer_gone(
                conn,
                &FrameKind::Command {
                    request_id,
                    command: Command::AttachTerminal {
                        terminal_id: terminal_id.clone(),
                    },
                },
            )
            .await?;
        }
        Ok(())
    }

    /// phux-k0cw: fold anything the frame said about a session OTHER than
    /// ours into the peer caches the roster and cross-session queue read.
    ///
    /// Both repaint kinds are raised, not just the fleet one: the peer state
    /// now feeds the always-on strip, so raising `fleet` alone would leave a
    /// peer's change invisible unless the fleet modal happened to be open
    /// (`refresh_fleet_if_open` returns `NotPublished` when it is not).
    fn fold_peer_outcome(&mut self, outcome: &mut FrameOutcome, repaint: &mut RepaintAccumulator) {
        let layout_folded = if let Some((session, value)) = outcome.foreign_layout.take() {
            apply_foreign_layout_reply(&mut self.peers.foreign_layouts, session, value.as_deref());
            prune_foreign_agents(
                &mut self.peers.foreign_agents,
                &mut self.peers.foreign_agent_subscribed,
                &self.peers.foreign_layouts,
            );
            true
        } else {
            false
        };
        let agent_folded = if let Some((id, value)) = outcome.foreign_agent.take() {
            apply_foreign_agent_reply(&mut self.peers.foreign_agents, id, value.as_deref());
            true
        } else {
            false
        };
        // Only a NEW ask is a repaint reason; a repeated one
        // changes nothing the strip renders.
        let asked_folded = outcome
            .foreign_attention
            .take()
            .is_some_and(|id| self.peers.foreign_attention.insert(id));
        // A peer spawn/close needs no fold of its own: the
        // layouts are re-read on the next sweep, so flagging
        // the repaint is enough.
        if layout_folded || agent_folded || asked_folded || outcome.foreign_pane_set_dirty {
            repaint.raise_chrome();
            repaint.raise_fleet();
        }
    }

    /// Re-sweep the watches and caches an ATTACHED snapshot or a pane
    /// lifecycle change invalidated.
    async fn resync_watches(
        &mut self,
        conn: &mut Connection,
        outcome: &mut FrameOutcome,
    ) -> Result<(), AttachError> {
        // ADR-0040: the frame may have added panes
        // (TerminalSpawned, a peer's layout broadcast) or
        // removed them (TerminalClosed). Re-sweep so every
        // live pane has a `phux.agent/v1` watch; the len
        // guard keeps the steady state zero-cost.
        if self.panes.len() != self.agent_meta.subscribed.len() {
            self.sync_agent_meta(conn).await?;
        }
        // phux-p4vp: the ATTACHED snapshot refreshes the pane-cwd index
        // behind the sidebar branch line.
        self.vcs
            .apply_snapshot(std::mem::take(&mut outcome.pane_cwds));
        // phux-4li.20: refresh the cached session graph
        // whenever an ATTACHED snapshot lands so the
        // session picker lists the current peer set.
        // phux-foz.8: re-request the peers' persisted
        // layouts against the fresh graph so the window
        // picker's one-step rows track it; replies
        // overwrite stale cache entries.
        if let Some((list, focused)) = outcome.sessions.take() {
            self.peers.sessions = list;
            self.peers.focused_session = Some(focused);
            // phux-k0cw.10: a graph refresh in the SAME batch
            // that satisfies the deferred bootstrap sweep does
            // its whole job — same call, same arguments, and
            // against a fresher graph. Clear the flag so the
            // drain below does not re-send a GET per peer that
            // this call already has in flight (the send-once
            // `subscribed` set covers the SUBSCRIBE half, but
            // nothing dedupes the GET).
            self.peers.sweep_pending = false;
            self.sweep_peer_layouts(conn).await?;
        }
        Ok(())
    }

    /// Refresh the chrome and schedule an in-place paint when a painter input
    /// actually changed, so an event that alters no visible state doesn't
    /// force a repaint.
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
        // ADR-0033 / phux-foz.1: a `TerminalControl` or `Asked`
        // event changed a pane's lease/lifecycle/attention. The
        // event frame paints nothing, so refresh the chrome
        // (supervisory badge, attention hint, window markers)
        // and repaint here. ADR-0029: nothing about a title /
        // lease / attention change touches a pane interior, so
        // this is a CHROME raise, not a full-frame clear.
        // (`own_client_id` is fixed for the life of this loop;
        // it was captured at bootstrap.)
        if outcome.chrome_dirty {
            self.note_chrome_change(repaint);
        }
        if !outcome.notices.is_empty() {
            self.apply_notices(std::mem::take(&mut outcome.notices), repaint);
        }
    }

    /// phux-i0e8.2.1: drain the frame's transient notices
    /// into the painter's newest-wins slot; expiry rides
    /// the 1 s `status_tick` arm. With no bar to paint
    /// on (no painter, an empty bar, or the persistent
    /// error line holding the row — the painter refuses
    /// those itself) the notice degrades to a tracing
    /// line rather than vanishing.
    fn apply_notices(&mut self, notices: Vec<Notice>, repaint: &mut RepaintAccumulator) {
        let now = std::time::Instant::now();
        let mut notice_shown = false;
        for notice in notices {
            if let Some(sb) = self.status_bar.as_mut() {
                notice_shown |= sb.set_notice(notice, now);
            } else {
                tracing::info!(
                    severity = ?notice.severity,
                    text = %notice.text,
                    "status-bar notice dropped: no status bar configured",
                );
            }
        }
        if notice_shown && !self.overlays.is_active() {
            repaint.raise_chrome();
        }
    }

    /// Send every request the handled frame asked the driver to emit.
    async fn emit_outcome_requests(
        &mut self,
        conn: &mut Connection,
        outcome: &mut FrameOutcome,
        sidebar: Option<SidebarReservation>,
        prev_rects: Option<&HashMap<TerminalId, crate::layout::Rect>>,
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
        // phux-4li.12: a layout mutation triggered by a
        // server frame (TerminalSpawned ok, TerminalClosed)
        // requires the same `SET_METADATA` broadcast as
        // a local action — see `ActionEffects.set_metadata`
        // for the local-action path.
        if outcome.emit_set_metadata {
            self.broadcast_layout(conn).await?;
        }
        if outcome.reflow_panes
            && let Some(prev_rects) = prev_rects
        {
            self.emit_reflow_resizes(conn, prev_rects, sidebar).await?;
        }
        Ok(())
    }

    /// Broadcast the local workspace on the session's layout key so sibling
    /// clients reconcile.
    async fn broadcast_layout(&mut self, conn: &mut Connection) -> Result<(), AttachError> {
        let Some(session) = self.peers.focused_session else {
            return Ok(());
        };
        let Some(bytes) = encode_layout_or_log(&self.workspace) else {
            return Ok(());
        };
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
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

    /// phux-tnh: a pane close/spawn changed surviving
    /// panes' dimensions. Diff the folded/split layout
    /// against the pre-frame rects and emit a
    /// `TERMINAL_RESIZE` per changed leaf — same path the
    /// SIGWINCH arm uses — so the server reflows each
    /// PTY (TIOCSWINSZ) and the shell redraws to fill.
    /// Without this the survivor of a close keeps its
    /// old small winsize ("survivor stays small").
    /// Sent BEFORE the repaint so the server's resync
    /// snapshot lands after the local mirror has grown.
    async fn emit_reflow_resizes(
        &self,
        conn: &mut Connection,
        prev_rects: &HashMap<TerminalId, crate::layout::Rect>,
        sidebar: Option<SidebarReservation>,
    ) -> Result<(), AttachError> {
        let Some(ls) = self.workspace.render_window(self.zoomed.as_ref()) else {
            return Ok(());
        };
        if ls.tree.is_none() {
            return Ok(());
        }
        let diff =
            crate::attach::reflow::compute_reflow(ls.as_ref(), prev_rects, self.content(sidebar));
        for (terminal_id, new_rect) in &diff.changed {
            send_unless_peer_gone(
                conn,
                &FrameKind::TerminalResize {
                    terminal_id: terminal_id.clone(),
                    cols: new_rect.w,
                    rows: new_rect.h,
                },
            )
            .await?;
        }
        Ok(())
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
            self.on_layout_replaced(outcome.layout_get_answered, sidebar, repaint);
        }
        // ADR-0040: a `phux.agent/v1` record changed (GET
        // reply or subscribed broadcast). The window labels
        // and the sidebar's agents section derive from it, so
        // recompose the chrome and schedule an IN-PLACE chrome
        // paint.
        //
        // This arm used to call `paint_full_frame`
        // UNCONDITIONALLY — no gate on whether a painter input
        // actually changed, unlike the `chrome_dirty` arm. That
        // was invisible only because nothing ever wrote the
        // record, so the arm never fired. With a server-side
        // agent-state detector publishing transitions, an
        // ungated `paint_full_frame` here is an `ESC[2J`
        // full-screen clear per transition. Both halves of the
        // fix are required: gate on `refresh_window_chrome`'s
        // change report, AND route to the in-place chrome
        // painter via the accumulator.
        if outcome.agent_meta_changed {
            self.note_chrome_change(repaint);
        }
        // phux-foz.5: the `phux config reload` doorbell
        // rang (a subscribed `phux.config.reload/v1`
        // broadcast). Re-read our own config file and swap
        // the config-derived state in place — same handler
        // as the `reload-config` action; failures keep the
        // previous config and toast.
        if outcome.config_reload {
            self.reload_config(out, sidebar);
        }
        // phux-foz.7: the agent-fleet dashboard is a live
        // projection — while it is open, a frame that
        // changed fleet-projected state (an agent record,
        // an ADR-0035 Asked, a pane spawn/close, a layout
        // or session-graph change) rebuilds its rows and
        // repaints the overlay layer. Push, not poll:
        // nothing runs when no such frame lands.
        //
        // RAISED, not called: `refresh_fleet_if_open` repaints
        // the overlay over a `paint_full_frame` base, so a call
        // per frame is an `ESC[2J` per frame. Nine panes
        // publishing an agent-state transition coalesce into one
        // batch, and this used to fire nine times inside it —
        // nine full-screen clears in one iteration, in exactly
        // the view that exists for watching agents. The
        // accumulator collapses them into ONE refresh at the
        // drain.
        if fleet_dirty {
            repaint.raise_fleet();
        }
    }

    /// phux-4li.5: the layout changed under us (either the GET reply or a
    /// peer's broadcast). Trigger a full repaint: clear screen + paint
    /// dividers + re-render every pane. phux-5ke.4: while an overlay is up,
    /// defer the repaint — the dismiss path always triggers
    /// `paint_full_frame`, and the libghostty mirror is already updated.
    fn on_layout_replaced(
        &mut self,
        layout_get_answered: bool,
        sidebar: Option<SidebarReservation>,
        repaint: &mut RepaintAccumulator,
    ) {
        self.resolve_cross_session_pick();
        self.refresh_chrome();
        // phux-z6wt: this path fires for a peer's layout
        // broadcast and for the TerminalSpawned/
        // TerminalClosed reflow — neither goes through
        // SIGWINCH, so the phux-d26y fan-out never ran
        // for them. A surviving copy-mode overlay would
        // keep clamping against the pane size it opened
        // with, silently dropping or clipping a copy.
        // Recompute the focused pane's rect the same way
        // the SIGWINCH arm does and hand it to every
        // surviving overlay before the repaint below.
        self.sync_overlays(sidebar);
        // The pane rects moved: only a full-viewport
        // repaint (ED2 + every pane + dividers) is a
        // coherent base. ADR-0029: raise, drain once.
        if !self.overlays.is_active() {
            repaint.raise_full();
        }
        // The GET reply is single-use; clear the pending
        // request id so a stray late MetadataValue can't
        // trample state. Gated on `layout_get_answered`,
        // NOT on `layout_replaced`: the latter is also
        // raised for pane damage during bootstrap, and
        // clearing on that dropped the real reply.
        if layout_get_answered {
            self.layout_get_request_id = None;
        }
    }

    /// phux-foz.8: a one-step cross-session window pick drove this attach;
    /// the multi-window layout just landed, so resolve the deferred select
    /// against it before the repaint. Out-of-range (a peer mutated the layout
    /// between pick and load) keeps the session's restored focus with a
    /// warning.
    fn resolve_cross_session_pick(&mut self) {
        let Some(idx) = self.pending_window.take() else {
            return;
        };
        if !self.workspace.select(idx) {
            tracing::warn!(
                index = idx,
                windows = self.workspace.windows.len(),
                "cross-session window pick out of range; keeping restored focus",
            );
            return;
        }
        let next_focus = self
            .workspace
            .active_window()
            .and_then(|ls| ls.focus.clone());
        self.focus_history
            .transition(&mut self.focused_pane, next_focus);
        if let Some(ord) = self.pending_pane.take() {
            self.focus_picked_leaf(idx, ord);
        }
        if let Some(fid) = self.focused_pane.as_ref() {
            reanchor_predict_to_pane(&mut self.predict, &self.panes, fid);
        }
    }

    /// phux-jpqd: the pane half of a one-step cross-session pane pick — move
    /// focus onto the target DFS leaf of the just-selected window.
    /// Out-of-range (peer mutated the layout) keeps the window's restored
    /// focus, logged.
    fn focus_picked_leaf(&mut self, idx: usize, ord: usize) {
        let picked = self
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
        if let Some(ls) = self.workspace.active_window_mut() {
            ls.focus = Some(leaf.clone());
        }
        self.focus_history
            .transition(&mut self.focused_pane, Some(leaf));
    }

    /// ADR-0029 §2: the ONE drain. Every loop-level repaint trigger in this
    /// batch has raised; the highest level wins and paints exactly once.
    fn drain_repaint<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        repaint: &mut RepaintAccumulator,
    ) {
        let drained = repaint.drain();
        // The overlay half of the same drain. A no-op unless a
        // live fleet list is actually in the overlay stack, so
        // the raise costs nothing when the dashboard is closed.
        if drained.fleet_dirty {
            self.refresh_fleet(out, sidebar);
        }
        // A full repaint force-redraws every pane, so it discharges every
        // paint the pacer was still holding. Settling them afterwards would
        // repaint rows that are already correct.
        if matches!(drained.level, RepaintLevel::Full) {
            self.pacer.clear_pending();
        }
        self.repaint_view(out, sidebar, drained.level);
    }

    /// phux-l96p.3: paint every pane whose paint the pacer withheld, as ONE
    /// composited frame.
    ///
    /// Reached from the `paint_deadline` select arm when the window expires,
    /// and from the end of an admitted burst — see `handle_frame_burst` for
    /// why the timer alone cannot be trusted to get there.
    ///
    /// Suppression is re-evaluated here rather than inherited from the frames
    /// that were withheld: an overlay may have opened, or a pane may have
    /// entered a synchronized-output transaction, since. Both have their own
    /// recovery (the overlay repaints the view on dismiss; the sync-output
    /// watchdog exposes a stuck transaction), so a pane suppressed at settle
    /// time is dropped rather than held indefinitely.
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
        let live: Vec<TerminalId> = owed
            .into_iter()
            .filter(|id| {
                self.panes
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
                kernel: &self.engine_kernel,
                panes: &mut self.panes,
                workspace: &self.workspace,
                zoomed: self.zoomed.as_ref(),
                focused_pane: self.focused_pane.as_ref(),
                status_bar: self.status_bar.as_mut(),
                sidebar,
                viewport_dims: self.viewport_dims,
                session_name: &self.session_name,
                predict: &mut self.predict,
                overlay: &self.overlay,
            },
            &live,
        );
        self.finish_paint(painted);
    }

    /// Rebuild and repaint the agent-fleet dashboard, if it is open.
    fn refresh_fleet<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        let painted = refresh_fleet_if_open(
            out,
            &mut self.overlays,
            &self.workspace,
            &mut self.panes,
            &self.engine_kernel,
            self.focused_pane.as_ref(),
            self.zoomed.as_ref(),
            self.viewport_dims,
            self.status_bar.as_mut(),
            sidebar,
            &mut self.sidebar_painter,
            &self.session_name,
            &self.theme,
            &self.peers.sessions,
            self.peers.focused_session,
            &self.agent_meta.records,
            &mut self.vcs,
            &self.peers.foreign_layouts,
            &self.peers.foreign_agents,
        );
        self.finish_paint(painted);
    }

    // ---- timers, signals, and the periodic paints -----------------------

    /// Bound the failure mode of an application that omits `?2026l`: expose
    /// the latest complete mirror once, then let subsequent output re-arm the
    /// transaction watchdog.
    fn on_sync_output_timeout<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        let now = tokio::time::Instant::now();
        let mut expired = false;
        for slot in self.panes.values_mut() {
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
            self.resolver.as_ref(),
            self.keybindings_snapshot.as_ref(),
            &self.theme,
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
        let prev_dims = self.viewport_dims;
        let viewport = current_viewport_or_default();
        self.viewport_dims = (viewport.cols.max(1), viewport.rows.max(1));
        self.cell_px_dims = host_cell_px(&viewport);
        // Bound predict to the FOCUSED pane's current grid, not the
        // whole viewport — predictions are pane-local (phux-7ry0). The
        // pane grids resize on the server's resize-ack snapshot, which
        // re-syncs predict again; this just keeps the transient
        let (predict_cols, predict_rows) = self
            .focused_pane
            .as_ref()
            .and_then(|fid| self.panes.get(fid))
            .map_or((viewport.cols, viewport.rows), |slot| slot.geometry);
        self.predict.set_viewport(predict_cols, predict_rows);
        conn.send(&viewport_resize_frame(viewport)).await?;
        self.emit_resize_reflow(conn, prev_dims, sidebar).await?;
        // phux-a7fz: do not repaint stale pre-resize mirrors into the
        // new viewport. The server resize path sends an authoritative
        // resync snapshot; painting the old grid first races with the
        // shell's prompt redraw and leaves duplicated right prompts on
        // resize-heavy shells. Clear immediately, then let the snapshot
        // repopulate the viewport at the new dimensions.
        let _ = out.write_all(b"\x1b[2J\x1b[H");
        // phux-fsb: an overlay that pinned its box to a pointer cell
        // (the context menu) is now addressing cells that may not
        // exist. Drop it BEFORE the repaint below, so this frame is
        // the one that erases it — leaving it up would keep an
        // invisible overlay capturing every keystroke, with Enter
        // committing its selected row (`Close pane`, if that is where
        // the selection sat) against a pane the user cannot see it
        // pointing at. Reflowing overlays are untouched.
        if self.overlays.dismiss_stale_on_resize() {
            tracing::debug!("resize: dropped a pinned overlay whose geometry went stale");
        }
        if self.overlays.is_active() {
            // phux-d26y / phux-z6wt: the survivors keep their state
            // but must adopt the focused pane's NEW size before they
            // are painted. Copy-mode is the one that cares: it
            // clamps its cursor and picks Line mode's right edge
            // from pane dimensions captured when it opened, so
            // without this a copy after a resize either resolves to
            // nothing (a stale-large corner the engine cannot
            // address) or stops at the old edge (stale-small). Runs
            // after the stale sweep above, so an overlay about to be
            // dropped is never handed geometry it will not use.
            // Same choke point the `layout_replaced` path uses for
            // the non-SIGWINCH triggers (a peer's layout broadcast,
            // TerminalSpawned/TerminalClosed reflow).
            self.sync_overlays(sidebar);
            self.paint_overlay(out, sidebar);
        } else {
            let _ = out.flush();
        }
        Ok(())
    }

    /// Emit one `TERMINAL_RESIZE` per leaf whose (w, h) actually
    /// changed so the server ioctls TIOCSWINSZ on each PTY. This
    /// covers the single-pane case too — `Workspace::single` seeds
    /// a one-leaf tree, so the `tree.is_some()` guard only skips a
    /// workspace with no panes at all, and a lone pane still needs
    /// sizing to the chrome-inset content rect.
    async fn emit_resize_reflow(
        &self,
        conn: &mut Connection,
        prev_dims: (u16, u16),
        sidebar: Option<SidebarReservation>,
    ) -> Result<(), AttachError> {
        let Some(ls) = self.workspace.render_window(self.zoomed.as_ref()) else {
            return Ok(());
        };
        if ls.tree.is_none() {
            return Ok(());
        }
        let bar = self.status_bar.as_ref().map(StatusBarPainter::position);
        // phux-4h5a: size each PTY to the inset content rect (the
        // pane area after the status bar + sidebar reservation),
        // not the full viewport — otherwise an enabled sidebar
        // resizes panes to the full width while they paint inset.
        let prev_content = content_rect(prev_dims, bar, sidebar);
        let new_content = self.content(sidebar);
        let prev_rects =
            crate::attach::multi_pane::compute_layout_in(ls.as_ref(), prev_content, prev_dims)
                .rects;
        let diff = crate::attach::reflow::compute_reflow(ls.as_ref(), &prev_rects, new_content);
        if diff.too_small {
            tracing::warn!(
                cols = self.viewport_dims.0,
                rows = self.viewport_dims.1,
                "viewport too small for current layout; rendering may be garbled",
            );
        }
        for (terminal_id, new_rect) in &diff.changed {
            conn.send(&FrameKind::TerminalResize {
                terminal_id: terminal_id.clone(),
                cols: new_rect.w,
                rows: new_rect.h,
            })
            .await?;
        }
        Ok(())
    }

    /// Hand every surviving overlay the focused pane's current rect.
    fn sync_overlays(&mut self, sidebar: Option<SidebarReservation>) {
        sync_overlays_to_focused_pane(
            &mut self.overlays,
            &self.workspace,
            self.zoomed.as_ref(),
            self.focused_pane.as_ref(),
            self.viewport_dims,
            self.status_bar.as_ref().map(StatusBarPainter::position),
            sidebar,
        );
    }

    /// The periodic status-bar repaint.
    fn on_status_tick<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        // phux-i0e8.2.1: expire the transient notice on the tick that
        // carries the bar's repaint cadence. The clear invalidates the
        // painter's cache, so the paint below restores the widget row.
        // Runs even while an overlay is up (the bar repaints on
        // overlay dismiss, and a stale notice must not resurface).
        if let Some(sb) = self.status_bar.as_mut() {
            let _ = sb.clear_expired_notice(std::time::Instant::now());
        }
        // phux-5ke.4: an overlay above the bar would get
        // partially overwritten by the bar paint; skip ticks
        // while a modal is up.
        if self.overlays.is_active() {
            return;
        }
        // Restore the cursor to wherever the focused pane left it
        // so an idle tick doesn't strand the cursor in the bar.
        let focused_cursor = self
            .focused_pane
            .as_ref()
            .and_then(|fid| self.panes.get(fid))
            .and_then(|slot| slot.renderer.last_cursor());
        tracing::trace!(
            focused_pane_set = self.focused_pane.is_some(),
            has_cursor = focused_cursor.is_some(),
            "status_tick: repaint bar"
        );
        let fallback_origin = Some(self.bar_fallback_origin(sidebar));
        // The tick used to end in an unconditional cursor placement and flush
        // even when the composed row was byte-identical — every second, for
        // the life of the attach, a wake of the stdout writer thread to say
        // that nothing changed. Wrapping the tick in a frame block makes the
        // no-change case emit literally nothing: the block only opens if the
        // bar actually writes, and a block that never opened performs no
        // cursor tail, no epilogue, and no flush.
        let painted = crate::attach::paint::close_frame_with_chrome(
            crate::attach::paint::FrameBlock::begin(out),
            self.status_bar.as_mut(),
            self.viewport_dims,
            sidebar,
            &self.session_name,
            focused_cursor,
            fallback_origin,
            // The tick exists precisely to refresh what the painter cannot
            // observe (the clock, an `exec` widget's cache), so it always
            // composes. Unchanged output still emits nothing: the row cache
            // suppresses the write and the frame block never opens.
            crate::render::chrome::status_bar::ComposePolicy::Always,
        );
        self.finish_paint(painted);
    }

    /// phux-9xn / phux-gxy: ALWAYS provide a fallback origin. When
    /// `focused_pane` is None (e.g. ATTACHED hasn't seeded yet) the old code
    /// passed None → `paint_bar_after_pane` emitted no CUP → cursor stranded
    /// at the bar's last cell every tick.
    fn bar_fallback_origin(&self, sidebar: Option<SidebarReservation>) -> (u16, u16) {
        let content = self.content(sidebar);
        self.focused_pane
            .as_ref()
            .and_then(|fid| {
                self.workspace
                    .render_window(self.zoomed.as_ref())
                    .and_then(|ls| {
                        // Through the memoized tiling: this runs on every 1 s
                        // status tick, and the layout it asks about is the
                        // same one the last paint tiled.
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
            self.overlays
                .push(Box::new(ToastOverlay::new(title, lines, &self.theme)));
            self.paint_overlay(out, sidebar);
        }
    }
}
