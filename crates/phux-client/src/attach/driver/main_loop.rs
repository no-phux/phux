//! The `tokio::select!` session loop and its frame-coalescing policy.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use phux_client_core::engine::ghostty::GhosttyAdapter;
use phux_client_core::history::HistoryCacheConfig;
use phux_client_core::session::{EffectBuffer as KernelEffectBuffer, SessionKernel};
#[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::{ClientId, TerminalId};
use phux_protocol::wire::frame::{AttachTarget, CONFIG_RELOAD_KEY, Command, FrameKind, Scope};
use tokio::io::AsyncReadExt;
use tokio::signal::unix::{SignalKind, signal};

use crate::agent_meta::AgentRecord;
use crate::attach::actions::{PendingSplit, PendingWindow};
use crate::attach::connection::Connection;
use crate::attach::exec_widgets::spawn_exec_feed_runners;
use crate::attach::input::StdinParser;
use crate::attach::input_dispatch::{
    DispatchCtx, ReattachTarget, dispatch_input_events, encode_layout_or_log,
    sync_overlays_to_focused_pane,
};
use crate::attach::outcome::{AttachEnd, AttachError};
use crate::attach::paint::{
    SidebarEdge, StatusBarPaint, content_rect, paint_bar_after_pane, paint_chrome_in_place,
    paint_full_frame, sidebar_reservation,
};
use crate::attach::pane_state::{
    AttentionNavigation, PaneSlot, VcsIndex, reanchor_predict_to_pane,
};
use crate::attach::plugin_actions::{self, PluginActionEntry, PluginRunResult};
use crate::attach::plugin_panes;
use crate::attach::repaint::{RepaintAccumulator, RepaintLevel};
use crate::attach::server_frame::{AgentMetaIndex, handle_server_frame};
use crate::layout::Workspace;
use crate::layout_ops::{DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, layout_key};
use crate::predict::{Overlay, PredictionState, PredictiveConfig};
use crate::render::ChromeBreakpoints;
use crate::render::chrome::sidebar::SidebarPainter;
use crate::render::chrome::status_bar::{Notice, StatusBarPainter};
use crate::render::overlay::OverlayState;
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
/// key, anchored to when the ESC became pending (see `esc_deadline` in
/// `main_loop`). The client reads stdin from the *outer* terminal, which
/// writes a key's full `ESC [`/`ESC O` sequence in one burst — a split
/// only happens at a read-buffer boundary — so a short window suffices to
/// disambiguate. It must stay short: a modal-editor user pays this window
/// on EVERY bare Escape, and the inner application (vim's `ttimeoutlen`,
/// readline's `keyseq-timeout`) then stacks its own on top. tmux installs
/// ship `escape-time 0..10` for the same reason; 10ms keeps Escape under
/// the perception floor while still absorbing split sequences.
const ESC_FLUSH_IDLE: Duration = Duration::from_millis(10);

/// phux-jhv8: upper bound on how many already-queued frames one `recv`
/// wake-up drains before painting. A back-to-back output burst (nvim
/// startup) is a few dozen frames; the cap only guards against a server
/// that streams without pause starving the stdin/signal `select!` arms.
const FRAME_COALESCE_CAP: usize = 1024;

/// Safety valve for an application that enters DEC synchronized output and
/// never leaves it. Normal TUI transactions last milliseconds.
const SYNC_OUTPUT_WATCHDOG: Duration = Duration::from_secs(1);

/// The terminal a frame would repaint under normal handling, if any — the
/// `vt_write` + render pair a coalesced burst can defer to a later same-pane
/// frame (phux-jhv8). Output and snapshot frames carry pane content; every
/// other frame (layout, lifecycle, control) paints through its own path or
/// not at all, so it never defers (returns `None`).
const fn frame_paint_target(frame: &FrameKind) -> Option<&TerminalId> {
    match frame {
        FrameKind::TerminalOutput { terminal_id, .. } => Some(terminal_id),
        _ => None,
    }
}

/// Per-frame paint-deferral mask for a coalesced burst (phux-jhv8).
///
/// `targets[i]` is the pane frame `i` would repaint (`None` for control
/// frames). The result is `true` at `i` iff some later frame repaints the
/// *same* pane — meaning frame `i`'s paint is redundant and can be skipped
/// (its `vt_write` still applies). Each pane's LAST frame is therefore never
/// deferred, so every touched pane settles exactly once and none is left
/// stale; control frames (`None`) never defer.
fn coalesce_defer_flags<T>(
    items: &[T],
    target: impl for<'a> Fn(&'a T) -> Option<&'a TerminalId>,
) -> Vec<bool> {
    let paint_count = items.iter().filter(|item| target(item).is_some()).count();
    let mut seen = HashSet::with_capacity(paint_count);
    let mut deferred = Vec::with_capacity(items.len());
    for item in items.iter().rev() {
        deferred.push(target(item).is_some_and(|pane| !seen.insert(pane)));
    }
    deferred.reverse();
    deferred
}

/// Apply the per-pane last-wins coalescing decision.
const fn frame_defers_paint(deferred_by_coalesce: bool, _frame: &FrameKind) -> bool {
    deferred_by_coalesce
}

/// Drive the `tokio::select!` loop until detach or a session switch.
///
/// `initial_attached` is the `FrameKind::Attached` frame that
/// [`wait_for_attached`] already pulled off the wire; we replay it
/// through `handle_server_frame` so the focused-pane bookkeeping lives
/// in one place. Subsequent bootstrap and `TERMINAL_OUTPUT` frames come off the
/// wire as usual.
///
/// phux-eb0: returns a [`LoopExit`] so the outer loop in
/// [`run_with_stdout_predict`] can re-attach to another session without
/// dropping the transport or leaving raw mode. Every session-scoped local
/// in this function is rebuilt on each entry, so a re-attach starts from a
/// clean slate (no stale pane mirror, no carried-over predict queue).
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
#[allow(
    clippy::too_many_lines,
    reason = "tokio::select! arms inflate function length; splitting would require carrying ~10 mutable locals through helpers"
)]
#[allow(
    clippy::cognitive_complexity,
    reason = "select! arms + phux-4li.5 outcome dispatch; ditto"
)]
#[allow(
    clippy::too_many_arguments,
    reason = "per-entry knobs from attach_session's outer loop; foz-6 onboarding + foz-8 window pick + jpqd cross-session pane pick"
)]
pub(super) async fn main_loop<W: crate::attach::RenderSink>(
    conn: &mut Connection,
    initial_attached: FrameKind,
    predict_cfg: PredictiveConfig,
    out: &mut W,
    // phux-fysb: the off-loop StdoutSink's backpressure flag. When the writer
    // drops a stale backlog under a slow terminal it sets this; we repaint the
    // latest state from scratch (a self-contained full frame supersedes the
    // dropped diffs). `None` for the synchronous test sink.
    needs_resync: Option<&AtomicBool>,
    // Whether this connection negotiated `OutputMode::StateSync`. Gates the
    // per-frame `FRAME_ACK`: only a state-sync consumer's acks are tracked
    // server-side, so a raw consumer skips them (see `should_emit_frame_ack`).
    wants_state_sync: bool,
    // First-use moment consumed by this loop entry. Session switches receive
    // `None`, so they never repeat attach guidance.
    mut onboarding_claim: Option<crate::attach::onboarding::AttachClaim>,
    // phux-i0e8.2.3: transient status-bar notice to seed at attach time —
    // the reconnect loop's "re-attached after server restart". Applied to
    // the painter right after the bootstrap chrome refresh, so the first
    // bar paint (driven by the initial TERMINAL_SNAPSHOT burst) shows it;
    // expiry rides the ordinary 1 s status_tick. `None` on a first attach
    // and on session switches.
    initial_notice: Option<Notice>,
    // phux-foz.8: window index to select once this session's persisted
    // layout loads. Set by the outer loop when a one-step cross-session
    // window pick (`switch-session { name, window }`) drove the re-attach;
    // `None` on a plain attach/switch. Resolved (and consumed) on the
    // first layout reconcile; out-of-range degrades to the session's own
    // restored focus with a warning.
    initial_window: Option<usize>,
    // phux-jpqd: DFS leaf ordinal to focus (within `initial_window`) once
    // this session's layout loads — the pane half of a one-step
    // cross-session PANE pick (`switch-session { name, window, pane }`,
    // the agent-fleet foreign rows). `None` on a plain switch or a
    // window-only pick; resolved alongside `initial_window` and, like it,
    // degrades to a logged no-op if out of range.
    initial_pane: Option<usize>,
    // The window sidebar's on/off state carried in from the previous
    // `main_loop` entry when a `switch-session` drove this one. `None` on the
    // first attach — `[sidebar] enabled` seeds it; `Some(v)` on every
    // in-process switch, so a `toggle-sidebar` the user made survives moving
    // between spaces. Only the toggle is carried: the strip's width and edge
    // stay pure config, re-derived per entry.
    carried_sidebar_enabled: Option<bool>,
) -> Result<LoopExit, AttachError> {
    let onboarding_moment = onboarding_claim
        .as_ref()
        .map_or(crate::attach::onboarding::AttachMoment::None, |claim| {
            claim.moment()
        });
    // phux-4li.4: hold N client-side Terminals keyed by `TerminalId`,
    // not the single Terminal of the wave-A driver. Each pane's metadata slot
    // is allocated lazily from authoritative bootstrap geometry.
    let negotiated = conn.negotiated_bootstrap().ok_or_else(|| {
        AttachError::Protocol("attach loop started before bootstrap negotiation".to_owned())
    })?;
    let terminal_reply_supported = negotiated
        .server_features
        .contains(ServerFeature::TerminalReply);
    // phux-a5xj: does this server build a spawned pane at the geometry we
    // name, rather than at its own default? Fixed for the life of the
    // connection, like the reply bit above.
    let spawn_initial_size_supported = negotiated
        .server_features
        .contains(ServerFeature::SpawnInitialSize);
    let history_config = HistoryCacheConfig {
        request_max_bytes: negotiated.limits.max_history_page_bytes(),
        ..HistoryCacheConfig::default()
    };
    let mut engine_kernel = SessionKernel::with_history_config(
        GhosttyAdapter::new(negotiated.limits),
        negotiated.profile,
        history_config,
    );
    let mut kernel_effects = KernelEffectBuffer::new();
    // `Workspace` mirror (initialized as a single window holding one
    // pane when `ATTACHED` lands; see `handle_server_frame`) is the
    // source of truth for which leaves are live and where they sit in
    // the outer viewport. The renderer and layout helpers operate on the
    // active window (`workspace.active_window()`); the workspace
    // dimension is what gets persisted to L3.
    let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
    let mut workspace = Workspace::default();
    let mut focused_pane: Option<TerminalId> = None;
    // phux-oih5.4: one-entry focus MRU, local to this attached client. It is
    // deliberately outside Workspace so layout metadata never persists or
    // shares focus history (ADR-0019 decision 6).
    let mut focus_history = crate::attach::focus::FocusHistory::default();
    // ADR-0033: this client's own server-assigned ClientId, captured from
    // ATTACHED. Used to render "you hold the wheel" vs another client in the
    // supervisory badge. `None` until ATTACHED lands.
    let mut own_client_id: Option<ClientId> = None;
    // phux-x2hm: pane-zoom view state (driver-local, like focus). `Some(id)`
    // ⇒ pane `id` is zoomed to fill the window; render/reflow then run against
    // `workspace.render_window(zoomed)` (a synthetic single-leaf layout)
    // instead of the real tiled tree, which is left untouched for mutation.
    let mut zoomed: Option<TerminalId> = None;
    // phux-4li.5: request-id allocator for L3 GET correlation. (The
    // keybind resolver is built below, from the plugin-merged
    // keybindings snapshot.)
    let mut layout_get_request_id: Option<u32> = None;
    let mut next_request_id: u32 = 1;
    // phux-4li.12: in-flight `split-pane` actions parked by request id.
    // Populated by `run_action` when it dispatches SPAWN_TERMINAL;
    // drained by `handle_server_frame`'s TerminalSpawned arm when the
    // reply arrives. The map is small (one entry per outstanding
    // user-triggered split) so a HashMap is overkill for cap but
    // matches the layout-key request-id pattern.
    let mut pending_splits: HashMap<u32, PendingSplit> = HashMap::new();
    // phux-4li.15: in-flight `new-window` actions parked by request id,
    // same lifecycle as `pending_splits`. The TerminalSpawned arm checks
    // this map first; a hit opens a new window on the spawned pane.
    let mut pending_windows: HashMap<u32, PendingWindow> = HashMap::new();
    // ADR-0040 (phux-3ert): the structured agent-identity index. Each pane
    // gets a one-shot `GET_METADATA` + a live `SUBSCRIBE_METADATA` on
    // `phux.agent/v1` (see `sync_agent_meta_subscriptions`); decoded records
    // feed the window labels so the sidebar/tab strip renders agent
    // name/state from structured data, with the OSC title as the fallback.
    let mut agent_meta = AgentMetaIndex::default();
    // phux-p4vp: pane cwd + branch memo behind the sidebar's branch line.
    // Seeded from every ATTACHED snapshot; read at chrome-refresh time.
    let mut vcs = VcsIndex::default();
    // phux-nz4.5: status-bar painter, built from the on-disk config.
    // Load failures fall back to an empty bar so a malformed config
    // never blocks attach — the user still gets a working pane mirror.
    let mut status_bar = build_status_bar_painter();
    // Cache the keybindings so opening discovery surfaces never performs
    // config I/O under user fingers. Load the config once and split out the
    // keybindings snapshot and color theme; failures fall back to defaults.
    let loaded_cfg = phux_config::loader::load().ok();
    // phux-r82.5 / phux-r82.7: snapshot the enabled plugins' manifests once
    // at driver start (same policy as the keybindings snapshot — no config
    // I/O under user fingers), then derive both the action entries (palette
    // rows + manifest `keys` merged into the prefix table below, user config
    // winning every conflict) and the hostable pane entries (palette rows
    // committing `plugin-pane`; placement `split`/`tab`/`zoomed` — overlay
    // is deferred and dropped with a warning). A broken manifest is skipped
    // with a warning; manifests resolve relative to the canonical config
    // path, the same resolution `phux config run` uses. Both derived
    // vectors are `mut` because the in-place config reload (phux-foz.5)
    // swaps them when a reload succeeds.
    let plugin_manifests: Vec<phux_config::plugin::PluginManifest> = loaded_cfg
        .as_ref()
        .map(|cfg| {
            phux_config::plugin::load_enabled_manifests(
                &phux_config::loader::config_path(),
                &cfg.plugins,
            )
        })
        .unwrap_or_default();
    let mut plugin_actions: Vec<PluginActionEntry> =
        plugin_actions::entries_from_manifests(&plugin_manifests);
    let mut plugin_panes: Vec<plugin_panes::PluginPaneEntry> =
        plugin_panes::entries_from_manifests(&plugin_manifests);
    // The plugin-events channel: spawned plugin-action tasks report
    // completion here; the select! arm below toasts failures. Sender is
    // lent to `DispatchCtx` each batch.
    let (plugin_tx, mut plugin_rx) = tokio::sync::mpsc::unbounded_channel::<PluginRunResult>();
    let mut keybindings_snapshot: Option<phux_config::KeybindingsCfg> =
        loaded_cfg.as_ref().map(|c| {
            let mut kb = c.keybindings.clone();
            plugin_actions::merge_plugin_bindings(&mut kb, &plugin_actions);
            kb
        });
    // phux-4li.5: keybind resolver, built from the plugin-merged snapshot
    // so a manifest `keys` chord resolves exactly like a user binding.
    // The resolver consumes `InputEvent::Key` events *before* they would
    // be forwarded to the focused pane; a chord that resolves to an
    // action mutates the active window here and never reaches the
    // server's input pipe.
    // phux-i0e8.3.4: the build is lenient — whenever a snapshot exists a
    // resolver exists, and each diagnostic disables exactly one binding.
    // Diagnostics surface as a status-bar error line naming the chord
    // (unless the bar is already showing a config error, which subsumes
    // any keybinding problem).
    let mut resolver: Option<phux_config::keybind::Resolver> = None;
    if let Some(kb) = keybindings_snapshot.as_ref() {
        let (built, diags) = build_resolver_from(kb);
        resolver = Some(built);
        if !diags.is_empty()
            && !status_bar
                .as_ref()
                .is_some_and(StatusBarPainter::is_error_line)
        {
            status_bar = Some(StatusBarPainter::error_line(keybind_error_line(&diags)));
        }
    }
    // phux-ahv.4: single source of truth for chrome + overlay colors,
    // owned alongside the keybindings snapshot and threaded into the
    // overlay render path via `DispatchCtx`.
    let mut theme: crate::render::Theme = loaded_cfg
        .as_ref()
        .map_or_else(crate::render::Theme::default, |c| {
            crate::render::Theme::from_cfg(&c.theme)
        });
    // phux-foz.1: the attention hint's chip color comes from the theme's
    // `attention` slot rather than a hardcoded SGR in the painter.
    if let Some(sb) = status_bar.as_mut() {
        sb.set_attention_color(theme.attention);
    }
    // phux-r82.6: spawn one bounded interval runner per `exec` widget. The
    // runners execute off-loop and write into the widgets' shared caches;
    // the bar's normal repaint tick picks changed cells up, so the render
    // loop never blocks on a widget command. The guard aborts the tasks
    // (and via kill_on_drop, their children) when this attach loop ends.
    let _exec_runners = spawn_exec_feed_runners(
        status_bar
            .as_ref()
            .map(StatusBarPainter::exec_feeds)
            .unwrap_or_default(),
    );
    // phux-4h5a: window-sidebar render state, driver-local like `zoomed`. The
    // `[sidebar]` config seeds the initial enabled flag, width, and edge; the
    // `toggle-sidebar` action flips `sidebar_enabled` at runtime. Each frame
    // `sidebar_reservation()` folds these into an `Option<SidebarReservation>`
    // that threads to every layout site, so panes, dividers, reflow, mouse, and
    // the strip itself agree on the same inset. Default-off keeps the disabled
    // path byte-identical.
    let sidebar_cfg = loaded_cfg.as_ref().map(|c| c.sidebar.clone());
    // A `switch-session` re-enters this function, so `[sidebar] enabled` seeds
    // the flag only on the FIRST attach; a carried runtime value wins after
    // that (see `seed_sidebar_enabled`).
    let mut sidebar_enabled = seed_sidebar_enabled(
        carried_sidebar_enabled,
        sidebar_cfg.as_ref().is_some_and(|c| c.enabled),
    );
    let sidebar_width = sidebar_cfg.as_ref().map_or(20, |c| c.width);
    let sidebar_edge = match sidebar_cfg.as_ref().map(|c| c.position) {
        Some(SidebarPosition::Right) => SidebarEdge::Right,
        _ => SidebarEdge::Left,
    };
    // phux-huhi: the responsive-chrome thresholds, snapshotted from
    // `[chrome]` beside the theme and the sidebar geometry. One value for the
    // whole attach: the sidebar-yield fold below, the `toggle-sidebar`
    // refusal in `input_dispatch`, and every overlay's `centered_panel` read
    // this, so "compact" cannot mean two things on the same frame.
    let mut chrome_breakpoints = loaded_cfg
        .as_ref()
        .map_or_else(ChromeBreakpoints::default, |c| {
            ChromeBreakpoints::from_cfg(&c.chrome)
        });
    // The strip painter, themed like the status bar. Fed `window_infos` from
    // the same snapshot that drives the tab strip; caches so an unchanged
    // repaint emits nothing.
    let mut sidebar_painter = SidebarPainter::new(theme);
    // phux-5ke.4: overlay state — initially empty. Pushed onto by the
    // `show-help` action; drained by `OverlayState::handle_key` when
    // the active overlay returns `Dismiss`. While active, key events
    // route to the overlay (no pane forwarding) and pane stdout flushes
    // are suppressed (ADR-0020 §Decision invariant 5).
    let mut overlays = OverlayState::new();
    // phux-huhi: stamp the configured breakpoints once, before anything can
    // be pushed. `OverlayState::push` hands them to each overlay from here,
    // so no overlay construction site names a threshold.
    overlays.set_breakpoints(chrome_breakpoints);
    // phux-oih5.16: one client-local return point for attention navigation.
    // Cycling never overwrites it; return consumes it. It is deliberately
    // absent from Workspace/L3 metadata and resets on re-attach.
    let mut attention_navigation = AttentionNavigation::default();
    // ADR-0048: the in-flight divider drag. `None` between drags; a press
    // on a divider records the grabbed split, motion re-tunes it, release
    // clears it. Lives across dispatch batches (press and release land in
    // different `select!` wakeups), so it is owned here and lent to
    // `DispatchCtx` by reference each batch.
    let mut drag: Option<crate::attach::input_dispatch::DragGrab> = None;
    // phux-npb3 (ADR-0048 decision 3 follow-up): per-pane mouse opt-out.
    // `set-pane mouse off` puts the focused pane in this set; the dispatcher
    // then never synthesizes INPUT_MOUSE for it, and the sync at the top of
    // each loop iteration drops the outer-terminal mouse-tracking DECSET
    // whenever the focused pane is opted out — so the host's raw mouse
    // handling (native selection etc.) returns for that pane alone while
    // sibling panes keep drag-to-resize. Client-local; nothing on the wire.
    // `mouse_capture_cfg` mirrors the global `mouse` gate the RawModeGuard
    // install used: with `mouse = false` capture stays off unconditionally.
    let mouse_capture_cfg = loaded_cfg.as_ref().is_none_or(|c| c.defaults.mouse);
    let mut mouse_optout: std::collections::HashSet<TerminalId> = std::collections::HashSet::new();
    // Track the current outer-terminal viewport so the painter knows
    // which row is "bottom". Initialized to a sensible default and
    // updated by SIGWINCH; the server doesn't drive client-side
    // viewport (clients own their chrome per DESIGN §8.5).
    let mut viewport_dims: (u16, u16) =
        current_viewport().map_or((80, 24), |v| (v.cols.max(1), v.rows.max(1)));
    // Host per-cell pixel size for the INPUT_MOUSE cells→pixels scaling
    // (SPEC input.md §3.1). Tracked next to `viewport_dims` and refreshed
    // on the same SIGWINCH edge — a monitor change can move the window to
    // a display with a different cell size (phux-yyex).
    let mut cell_px_dims: (u16, u16) =
        current_viewport().map_or(HOST_CELL_PX_FALLBACK, |v| host_cell_px(&v));
    let mut session_name = String::new();
    // phux-4li.20: cache of the server's session graph, refreshed from
    // every ATTACHED snapshot. The `<leader> a` session picker reads
    // this to list peer sessions; `focused_session` marks the row the
    // client is currently attached to (excluded from the picker).
    let mut sessions: Vec<phux_protocol::wire::info::SessionInfo> = Vec::new();
    let mut focused_session: Option<phux_protocol::ids::SessionId> = None;
    // phux-foz.8: peer sessions' persisted layouts, fetched right after the
    // session graph lands (one GET_METADATA per peer, correlated through
    // `foreign_layout_pending`). The window picker reads the cache to render
    // one-step cross-session window rows; sessions with no entry fall back
    // to the plain "switch to this session" row. Attach-time snapshot only —
    // we do not subscribe to peers' layout keys.
    let mut foreign_layouts: HashMap<phux_protocol::ids::SessionId, Workspace> = HashMap::new();
    let mut foreign_layout_pending: HashMap<u32, phux_protocol::ids::SessionId> = HashMap::new();
    // phux-jpqd: the `phux.agent/v1` records of FOREIGN panes, so the
    // agent-fleet dashboard shows a peer session's agent glyph/state without
    // attaching there. Populated lazily: when a peer's layout lands
    // (`apply_foreign_layout_reply`), the driver fires one GET_METADATA per
    // `TerminalId` in that workspace on the pane's agent key, correlated
    // through `foreign_agent_pending`. Keyed by foreign terminal id; pruned
    // to the union of all cached foreign layouts' leaves on each fold so it
    // stays bounded. No subscription — a one-shot read, same lazy-query
    // shape as the foreign layouts above (ADR-0018 / ADR-0030).
    let mut foreign_agents: HashMap<TerminalId, AgentRecord> = HashMap::new();
    let mut foreign_agent_pending: HashMap<u32, TerminalId> = HashMap::new();
    // phux-k0cw: which peer keys this connection has already subscribed to.
    // Send-once bookkeeping, not teardown: L3 has no UNSUBSCRIBE_METADATA
    // verb, so a subscription lives as long as the connection and re-sending
    // one would just be noise on the wire.
    let mut foreign_layout_subscribed: std::collections::HashSet<phux_protocol::ids::SessionId> =
        std::collections::HashSet::new();
    let mut foreign_agent_subscribed: std::collections::HashSet<TerminalId> =
        std::collections::HashSet::new();
    // phux-k0cw: peer panes whose agent has asked for a human (an ADR-0035
    // `Asked` for a Terminal outside this client's pane set). The local
    // equivalent is `PaneSlot::attention`, which a foreign pane has no slot
    // to carry, so the flag lives here and is pruned with the peer records.
    let mut foreign_attention: std::collections::HashSet<TerminalId> =
        std::collections::HashSet::new();
    // phux-k0cw.10: the peer sweep owes the first paint its silence. Set here
    // and consumed at the ONE drain below, so bootstrap sends no peer traffic
    // until this session has actually painted.
    //
    // Why a flag rather than a call at bootstrap: a session switch re-enters
    // `main_loop` through the same bootstrap, and the server drops every
    // subscription with the old attach, so a switch rebuilds all of this from
    // empty. Sweeping before the loop therefore puts N peer GET/SUBSCRIBE
    // pairs — plus M per-pane pairs once the layouts land — ahead of the
    // `TERMINAL_SNAPSHOT` burst that produces the first paint, and the switch
    // pays for the roster's freshness in exactly the moment the roster exists
    // to make fast. One flag covers both entries because both run this code.
    let mut peer_sweep_pending = true;
    // phux-foz.8: the deferred window select of a one-step cross-session
    // pick, consumed on the first layout reconcile below. phux-jpqd:
    // `pending_pane` is the DFS leaf ordinal focused after the window
    // select resolves — the pane half of a one-step cross-session pick.
    let mut pending_window = initial_window;
    let mut pending_pane = initial_pane;
    let mut parser = StdinParser::new();
    // Predictive local echo (phux-9gw.1). State is updated alongside
    // every keystroke and drained on every TERMINAL_OUTPUT; when
    // `predict_cfg.enabled == false` every `predict_key` returns
    // `Disabled` so the overlay never paints.
    let mut predict = PredictionState::new(predict_cfg, 80, 24);
    let overlay = Overlay;
    let mut stdin = tokio::io::stdin();
    let mut stdin_buf = [0u8; 4096];
    let mut sigwinch = signal(SignalKind::window_change()).map_err(AttachError::Io)?;
    // `phux-roz`: SIGINT/SIGTERM/SIGHUP handlers run terminal cleanup
    // before exiting non-zero. SIGKILL is uncatchable; deferring
    // alt-screen entry until after handshake covers most real failure
    // modes for that case.
    let mut sigint = signal(SignalKind::interrupt()).map_err(AttachError::Io)?;
    let mut sigterm = signal(SignalKind::terminate()).map_err(AttachError::Io)?;
    let mut sighup = signal(SignalKind::hangup()).map_err(AttachError::Io)?;
    let mut detach_pending = false;
    // Bare-ESC disambiguation deadline, anchored to the iteration where the
    // parser first went pending. Re-creating the sleep each loop pass (the
    // pre-anchor behavior) restarted the full window whenever ANY other arm
    // fired first — under a steady output stream (status-line clock, shell
    // highlight repaints) a lone Escape could be deferred far past the
    // intended window. `None` ⇔ nothing pending.
    let mut esc_deadline: Option<tokio::time::Instant> = None;
    // phux-foz.2: which-key popup arming. When the resolver sits at the
    // pending-prefix state (`<prefix>` pressed, continuation awaited) for
    // `which_key_delay` without a follow-up chord, the loop pushes a
    // which-key overlay listing the prefix-table continuations. Config
    // comes from the same `[keybindings]` snapshot the action finder uses;
    // with no loaded config there is no resolver (and so no prefix to
    // hesitate on), so the popup is naturally inert. `None` ⇔ not armed.
    // Same anchored-deadline pattern as `esc_deadline`: the deadline is
    // set once when the pending state is first observed and survives
    // unrelated arms firing, so a busy output stream cannot starve it.
    let mut which_key_enabled = keybindings_snapshot.as_ref().is_some_and(|kb| kb.which_key);
    let mut which_key_delay = Duration::from_millis(
        keybindings_snapshot
            .as_ref()
            .map_or(600, |kb| kb.which_key_delay_ms),
    );
    let mut which_key_deadline: Option<tokio::time::Instant> = None;
    // phux-eb0: set by `apply_action_effects` when the user commits a
    // `switch-session`. Checked after each input-dispatch batch; a value
    // here makes `main_loop` return `LoopExit::SwitchTo` so the outer
    // loop re-attaches to the named session on the same connection.
    let mut switch_request: Option<ReattachTarget> = None;
    // phux-foz.5: set by `apply_action_effects` when the user commits a
    // `reload-config` (palette or bound chord). Checked after each
    // input-dispatch batch; the driver then re-runs the layered config
    // loader and swaps its config-derived state in place — or keeps the
    // old state and toasts the error. The `phux config reload` CLI
    // doorbell reaches the same handler via `FrameOutcome::config_reload`.
    let mut reload_request = false;
    // phux-i0e8.2.2: Terminals whose close THIS client requested
    // (kill-pane / kill-window). The action dispatcher parks ids here at
    // the kill seam; the `TerminalClosed` arm drains them to suppress the
    // pane-exit notice for a death the user themselves ordered.
    let mut expected_closes: HashSet<TerminalId> = HashSet::new();

    // Replay the `ATTACHED` frame so the focused-pane bookkeeping in
    // `handle_server_frame` runs exactly once, in one place. The sidebar
    // reservation for this bootstrap frame (recomputed per-iteration in the
    // loop below to track `toggle-sidebar`).
    let sidebar = sidebar_reservation(
        viewport_dims.0,
        sidebar_enabled,
        sidebar_width,
        sidebar_edge,
        chrome_breakpoints.min_pane_cols,
    );
    let outcome = handle_server_frame(
        &mut engine_kernel,
        &mut kernel_effects,
        out,
        initial_attached,
        &mut panes,
        &mut workspace,
        &mut focused_pane,
        &mut zoomed,
        &mut session_name,
        focused_session,
        status_bar.as_mut(),
        sidebar,
        viewport_dims,
        &mut predict,
        &overlay,
        layout_get_request_id,
        &mut pending_splits,
        &mut pending_windows,
        &mut expected_closes,
        &mut agent_meta,
        overlays.is_active(),
        // Single replayed frame — no burst to coalesce, paint it.
        false,
    )?;
    if outcome.exit {
        let end = outcome
            .exit_reason
            .unwrap_or(AttachEnd::Detached { reason: None });
        return Ok(detached_loop_exit(end, false));
    }
    // phux-e9fd: size every bootstrap pane's PTY to the rect this client will
    // actually paint it into, before anything else runs.
    //
    // The server sizes each pane from the ATTACH viewport
    // (`apply_attach_viewport`), which is the client's OUTER terminal —
    // chrome included. The client paints panes into `content_rect`, which is
    // one row shorter whenever a status bar is docked. Without this call the
    // mirror is a row taller than the rect it is clipped into, so the pane's
    // bottom line is never painted and the bar looks like it overwrote it.
    // The self-heal users notice — resize, split, toggle the sidebar — is
    // just the first reflow that DID emit `TERMINAL_RESIZE`.
    //
    // The server side already defers the off-by-one here in as many words
    // ("the client's concern via the post-attach `TERMINAL_RESIZE` reflow
    // path"); this is that path, and until now nothing called it. An empty
    // `prev_rects` makes `compute_reflow` report every leaf as changed — its
    // documented first-attach rule — so each pane is sized exactly once.
    emit_view_reflow(
        conn,
        &workspace,
        zoomed.as_ref(),
        &HashMap::new(),
        content_rect(
            viewport_dims,
            status_bar.as_ref().map(StatusBarPainter::position),
            sidebar,
        ),
    )
    .await?;
    vcs.apply_snapshot(outcome.pane_cwds);
    if let Some((list, focused)) = outcome.sessions {
        sessions = list;
        focused_session = Some(focused);
    }
    // phux-k0cw.10: the peer sweep belongs HERE in reading order — this is
    // where the session graph it reads (`sessions` / `focused_session`) has
    // just been folded from the ATTACHED replay above — but it is issued from
    // the ONE drain in the recv arm instead, carried there by
    // `peer_sweep_pending`, so the first paint never queues behind peer
    // traffic. Both are loop state, so the deferred call sweeps the same graph
    // a call here would have.
    //
    // What it does when it runs, unchanged: phux-foz.8 fetches each peer
    // session's persisted layout so the window picker can list foreign windows
    // as one-step jump rows, and phux-k0cw SUBSCRIBEs the same keys so the
    // roster tracks peers live rather than showing an attach-time snapshot
    // that silently rots. Fire-and-forget either way — replies drain through
    // the recv arm, and a peer with nothing persisted never replies with a
    // value and simply keeps its fallback row.
    // ADR-0033: cache our own ClientId (for the "you hold the wheel" badge) and
    // opt into the agent-event stream so `TerminalControl` broadcasts (lease +
    // lifecycle) reach this client. Server-scoped (`terminal: None`) so we see
    // control events for every pane, not just one.
    if outcome.own_client_id.is_some() {
        own_client_id = outcome.own_client_id;
    }
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
    if outcome.subscribe_layout
        && let Some(session) = focused_session
    {
        // phux-4li.5: ask the server for any persisted layout, then
        // subscribe to future mutations. Both frames are best-effort —
        // if the server rejects them with an ERROR (we'd see one in a
        // later loop iteration) we just stay in the single-pane
        // bootstrap. phux-jy4t: keyed per session so we restore THIS
        // session's layout, not whatever sibling wrote the key last.
        let key = layout_key(session);
        let req_id = next_request_id;
        layout_get_request_id = Some(req_id);
        next_request_id = next_request_id.wrapping_add(1);
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
    sync_agent_meta_subscriptions(
        conn,
        panes.keys().cloned().collect(),
        &mut agent_meta,
        &mut next_request_id,
    )
    .await?;
    // phux-4li.17: seed the window/tab strip from the bootstrap layout so
    // the first bootstrap-driven bar paint shows the window.
    // phux-4h5a: the sidebar painter tracks the same window list so the strip's
    // tab list stays current whenever the bar's does.
    {
        refresh_window_chrome(
            status_bar.as_mut(),
            &mut sidebar_painter,
            &workspace,
            &panes,
            focused_pane.as_ref(),
            zoomed.as_ref(),
            own_client_id,
            &agent_meta,
            &mut vcs,
            // phux-k0cw: the peer sweep has not answered yet at bootstrap, so
            // zones 1 and 3 start empty and fill as the replies land. That is
            // the intended shape: the queue holds at zero rows rather than
            // animating to correctness in the user's peripheral vision on
            // every attach.
            peer_inputs(
                &sessions,
                focused_session,
                &foreign_layouts,
                &foreign_agents,
                &foreign_attention,
            ),
        );
    }

    // phux-i0e8.2.3: seed the post-reconnect notice now that the session is
    // attached and the bar painter exists. The first bar paint — driven by
    // the initial TERMINAL_SNAPSHOT burst that follows ATTACHED — picks it
    // up, and the ordinary 1 s status_tick expires it, so "re-attached
    // after server restart" is visible inside the live TUI instead of on
    // the cooked terminal the alt screen replaced.
    let return_notice_available = initial_notice.is_none()
        && onboarding_moment == crate::attach::onboarding::AttachMoment::Return;
    let initial_notice = initial_notice.or_else(|| {
        return_notice_available.then(|| Notice::info(crate::attach::onboarding::RETURN_NOTICE))
    });
    let notice_accepted = apply_initial_notice(status_bar.as_mut(), initial_notice);
    if onboarding_moment == crate::attach::onboarding::AttachMoment::Return
        && (!return_notice_available || !notice_accepted)
    {
        onboarding_claim.take();
    }

    // The introduction floats over the live pane after bootstrap. It is a
    // passthrough notice: the first key dismisses it and continues through the
    // normal resolver/pane route, so guidance never taxes the user's intent.
    if onboarding_moment == crate::attach::onboarding::AttachMoment::Intro {
        overlays.push(Box::new(crate::render::overlay::ToastOverlay::passthrough(
            crate::attach::onboarding::ONBOARDING_TITLE,
            crate::attach::onboarding::hint_lines(keybindings_snapshot.as_ref()),
            &theme,
        )));
        paint_active_overlay(
            out,
            &overlays,
            &workspace,
            &mut panes,
            &engine_kernel,
            focused_pane.as_ref(),
            zoomed.as_ref(),
            viewport_dims,
            status_bar.as_mut(),
            sidebar,
            Some(&mut sidebar_painter),
            &session_name,
            &theme,
        );
        let paint_accepted = out.flush().is_ok();
        finish_onboarding_claim(onboarding_claim.take(), paint_accepted);
    }

    loop {
        // phux-4h5a: fold the driver-local sidebar render state into the
        // per-frame reservation threaded to every layout site this iteration.
        // `toggle-sidebar` flips `sidebar_enabled`; the change takes effect on
        // the next iteration. `None` (the default) keeps `content_rect` the
        // full pane viewport, so the whole path is byte-identical when the
        // sidebar is off.
        let sidebar = sidebar_reservation(
            viewport_dims.0,
            sidebar_enabled,
            sidebar_width,
            sidebar_edge,
            chrome_breakpoints.min_pane_cols,
        );
        // phux-npb3: capture follows focus. Re-derive the outer-terminal
        // mouse-tracking DECSET from the focused pane's opt-out state every
        // iteration — one call site covers every way focus or the set can
        // change (set-pane, click-to-focus, keybind navigation, spawn/close
        // reflows). `sync_mouse_capture` is a no-op when nothing changed, so
        // the steady-state cost is one bool compare. Closed panes are pruned
        // so a recycled TerminalId can never inherit a stale opt-out.
        if !mouse_optout.is_empty() {
            mouse_optout.retain(|id| panes.contains_key(id));
        }
        // The attention ladder's `seen` half: the pane the user is looking at
        // has, by definition, been looked at. One hash lookup per iteration —
        // and it covers EVERY way focus can move (click, keybind, split,
        // window switch, a peer's layout broadcast) without a call at each
        // site. A later agent-state change on an unfocused pane re-arms the
        // bit (see `server_frame::note_agent_change`), which is what lets a
        // background agent's `done` climb back above the working ones.
        //
        // The FLIP is a chrome trigger, not a silent side effect. The focus
        // action that made this pane focused ran in the PREVIOUS iteration, and
        // it recomputed the chrome while `seen` was still false — so the strip
        // it painted still carries the filled "look at me" diamond, bold,
        // pinned above every working agent, about the very pane the user is now
        // looking at. Nothing else recomputes `agent_entries` (the status tick
        // paints only the bar), so without this the row keeps lying until some
        // unrelated chrome event happens to fire — indefinitely, in a
        // single-agent session. That defeats the ladder's central promise:
        // visiting a pane demotes it.
        if mark_focused_seen(&mut panes, focused_pane.as_ref()) {
            let chrome_changed = refresh_window_chrome(
                status_bar.as_mut(),
                &mut sidebar_painter,
                &workspace,
                &panes,
                focused_pane.as_ref(),
                zoomed.as_ref(),
                own_client_id,
                &agent_meta,
                &mut vcs,
                peer_inputs(
                    &sessions,
                    focused_session,
                    &foreign_layouts,
                    &foreign_agents,
                    &foreign_attention,
                ),
            );
            // ADR-0029: demoting a ladder row touches no pane interior, so this
            // is an in-place CHROME paint, never a full-frame clear. Gated on
            // the painter's own change report, so a focus change that moves no
            // agent row costs zero bytes.
            if chrome_changed
                && !overlays.is_active()
                && let Some(ls) = workspace.render_window(zoomed.as_ref()).as_deref()
            {
                let painted = paint_chrome_in_place(
                    out,
                    ls,
                    &panes,
                    focused_pane.as_ref(),
                    viewport_dims,
                    status_bar.as_mut(),
                    sidebar,
                    Some(&mut sidebar_painter),
                    &session_name,
                );
                finish_return_onboarding_after_paint(
                    &mut onboarding_claim,
                    status_bar.as_ref(),
                    painted,
                );
            }
        }
        let want_capture =
            desired_mouse_capture(mouse_capture_cfg, focused_pane.as_ref(), &mouse_optout);
        sync_mouse_capture(out, want_capture).map_err(AttachError::Io)?;
        // phux-wrnm: hover reporting follows the overlay stack the same way
        // capture follows focus — raised while a context menu wants to track
        // the pointer with no button held, dropped as soon as it closes.
        sync_hover_tracking(out, overlays.wants_pointer_hover()).map_err(AttachError::Io)?;
        // phux-fysb: the off-loop stdout writer dropped a stale backlog under
        // a slow terminal. Repaint the latest state from scratch — a
        // self-contained full frame (or overlay) supersedes the dropped
        // diffs. `swap(false)` clears the flag, but any set re-armed by THIS
        // repaint's own flushes is preserved for the next iteration. Checked
        // before parking so a resync that landed during the prior arm is
        // serviced promptly.
        if needs_resync.is_some_and(|flag| flag.swap(false, Ordering::AcqRel)) {
            if overlays.is_active() {
                let painted = paint_active_overlay(
                    out,
                    &overlays,
                    &workspace,
                    &mut panes,
                    &engine_kernel,
                    focused_pane.as_ref(),
                    zoomed.as_ref(),
                    viewport_dims,
                    status_bar.as_mut(),
                    sidebar,
                    Some(&mut sidebar_painter),
                    &session_name,
                    &theme,
                );
                finish_return_onboarding_after_paint(
                    &mut onboarding_claim,
                    status_bar.as_ref(),
                    painted,
                );
            } else if let Some(ls) = workspace.render_window(zoomed.as_ref()).as_deref() {
                let painted = paint_full_frame(
                    out,
                    ls,
                    &mut panes,
                    &engine_kernel,
                    focused_pane.as_ref(),
                    viewport_dims,
                    status_bar.as_mut(),
                    sidebar,
                    Some(&mut sidebar_painter),
                    &session_name,
                );
                finish_return_onboarding_after_paint(
                    &mut onboarding_claim,
                    status_bar.as_ref(),
                    painted,
                );
            }
        }

        // Arm the bare-ESC idle timer only when the parser has pending
        // state, anchored to the first iteration that saw it (the deadline
        // survives other arms firing — see `esc_deadline`). When no flush
        // is pending we substitute a never-resolving future so the select!
        // arm parks forever; this keeps the steady-state cost at one
        // always-`Pending` future and avoids unused-`Option` branches
        // inside `select!`.
        if parser.has_pending() {
            esc_deadline.get_or_insert_with(|| tokio::time::Instant::now() + ESC_FLUSH_IDLE);
        } else {
            esc_deadline = None;
        }
        let flush_sleep: std::pin::Pin<Box<dyn Future<Output = ()>>> = match esc_deadline {
            Some(deadline) => Box::pin(tokio::time::sleep_until(deadline)),
            None => Box::pin(std::future::pending::<()>()),
        };

        // phux-foz.2: (dis)arm the which-key deadline from the resolver's
        // CURRENT pending state. An early continuation chord (dispatched
        // in the stdin arm) leaves the resolver non-pending, so the next
        // pass through here disarms the timer before it can fire — the
        // popup is suppressed without any explicit cancellation call.
        update_which_key_deadline(
            &mut which_key_deadline,
            resolver
                .as_ref()
                .is_some_and(phux_config::keybind::Resolver::pending_at_prefix),
            which_key_enabled,
            overlays.is_active(),
            tokio::time::Instant::now(),
            which_key_delay,
        );
        let which_key_sleep: std::pin::Pin<Box<dyn Future<Output = ()>>> = match which_key_deadline
        {
            Some(deadline) => Box::pin(tokio::time::sleep_until(deadline)),
            None => Box::pin(std::future::pending::<()>()),
        };

        // phux-nz4.5: per-bar repaint cadence. Driven by the slowest
        // widget that wants periodic refresh (currently floor-1s via the
        // `time` widget). Empty bar ⇒ `Pending` forever so this select!
        // arm never fires.
        let status_tick: std::pin::Pin<Box<dyn Future<Output = ()>>> = match status_bar
            .as_ref()
            .and_then(StatusBarPainter::min_poll_interval)
        {
            Some(interval) => Box::pin(tokio::time::sleep(interval)),
            None => Box::pin(std::future::pending::<()>()),
        };

        // Synchronized-output transactions intentionally span arbitrary
        // socket reads, so their deadline is pane state rather than a
        // per-batch timer. A stuck producer gets one bounded recovery paint;
        // later bytes re-arm suppression if mode 2026 is still set.
        let sync_output_sleep: std::pin::Pin<Box<dyn Future<Output = ()>>> = panes
            .values()
            .filter_map(|slot| slot.sync_output_since)
            .map(|since| since + SYNC_OUTPUT_WATCHDOG)
            .min()
            .map_or_else(
                || Box::pin(std::future::pending::<()>()) as _,
                |deadline| Box::pin(tokio::time::sleep_until(deadline)) as _,
            );

        tokio::select! {
            biased;

            // Stdin is polled before inbound frames so a local keystroke
            // is dispatched promptly rather than waiting behind an output
            // burst. One read is bounded by `stdin_buf`; the inbound arm is
            // bounded by `FRAME_COALESCE_CAP`, so neither starves the other.
            n = stdin.read(&mut stdin_buf) => {
                let n = n.map_err(AttachError::Io)?;
                if n == 0 {
                    // Stdin EOF — outer terminal closed. Detach cleanly.
                    if !detach_pending {
                        conn.send(&FrameKind::Detach).await?;
                        detach_pending = true;
                    }
                    continue;
                }
                let events = parser.feed(&stdin_buf[..n]);
                // Capture the pre-dispatch view so zoom and sidebar toggles can
                // diff against it and resize each changed pane's PTY. Taken
                // before dispatch mutates either piece of view geometry.
                let prev_zoomed = zoomed.clone();
                let prev_sidebar = sidebar;
                let prev_view_rects = view_rects(
                    &workspace,
                    prev_zoomed.as_ref(),
                    content_rect(
                        viewport_dims,
                        status_bar.as_ref().map(StatusBarPainter::position),
                        sidebar,
                    ),
                    viewport_dims,
                );
                // phux-foz.9: the agents-section row -> window mapping,
                // snapshotted from the strip painter so a click on an
                // agent row hit-tests against exactly what was painted.
                let sidebar_targets = sidebar_painter.click_targets();
                let mut ctx = DispatchCtx {
                    engine_kernel: &mut engine_kernel,
                    resolver: resolver.as_mut(),
                    focus_history: focus_history.clone(),
                    workspace: &mut workspace,
                    viewport: viewport_dims,
                    cell_px: cell_px_dims,
                    next_request_id: &mut next_request_id,
                    spawn_initial_size_supported,
                    pending_splits: &mut pending_splits,
                    pending_windows: &mut pending_windows,
                    expected_closes: &mut expected_closes,
                    overlays: &mut overlays,
                    keybindings: keybindings_snapshot.as_ref(),
                    theme: &theme,
                    sessions: &sessions,
                    foreign_layouts: &foreign_layouts,
                    foreign_agents: &foreign_agents,
                    focused_session,
                    session_name: &mut session_name,
                    switch_request: &mut switch_request,
                    zoomed: &mut zoomed,
                    sidebar,
                    sidebar_enabled: &mut sidebar_enabled,
                    sidebar_width,
                    chrome: chrome_breakpoints,
                    sidebar_targets: &sidebar_targets,
                    bar: status_bar.as_ref().map(StatusBarPainter::position),
                    status_bar: status_bar.as_ref(),
                    drag: &mut drag,
                    mouse_optout: &mut mouse_optout,
                    attention_navigation: &mut attention_navigation,
                    plugin_actions: &plugin_actions,
                    plugin_panes: &plugin_panes,
                    plugin_tx: Some(&plugin_tx),
                    reload_request: &mut reload_request,
                    agent_meta: &agent_meta.records,
                    vcs: &mut vcs,
                };
                let layout_changed = dispatch_input_events(
                    out,
                    conn,
                    events,
                    &mut focused_pane,
                    &mut detach_pending,
                    &mut predict,
                    &overlay,
                    &mut panes,
                    &mut ctx,
                )
                .await?;
                focus_history = ctx.focus_history;
                // phux-4h5a: a `toggle-sidebar` in this batch flipped
                // `sidebar_enabled`. Re-fold it into the reservation so the
                // reflow + repaint below tile into the NEW content rect this
                // iteration rather than waiting a frame.
                let sidebar = sidebar_reservation(viewport_dims.0, sidebar_enabled, sidebar_width, sidebar_edge, chrome_breakpoints.min_pane_cols);
                // phux-eb0: a committed `switch-session` ends this loop so
                // the outer driver re-attaches. Return BEFORE any repaint
                // — the new session's ATTACHED + snapshot will repaint.
                if let Some(target) = switch_request.take() {
                    return Ok(LoopExit::SwitchTo {
                        target,
                        sidebar_enabled,
                    });
                }
                // Zoom and sidebar toggles both change pane geometry. Resize
                // every affected PTY before repainting so applications reflow
                // to the same rectangle the client is about to render.
                if zoomed != prev_zoomed || sidebar != prev_sidebar {
                    emit_view_reflow(
                        conn,
                        &workspace,
                        zoomed.as_ref(),
                        &prev_view_rects,
                        content_rect(
                        viewport_dims,
                        status_bar.as_ref().map(StatusBarPainter::position),
                        sidebar,
                    ),
                    )
                    .await?;
                }
                if layout_changed {
                    // ADR-0040: an input action may have split/closed panes;
                    // keep the agent-metadata watches in step with the set.
                    sync_agent_meta_subscriptions(
                        conn,
                        panes.keys().cloned().collect(),
                        &mut agent_meta,
                        &mut next_request_id,
                    )
                    .await?;
                    refresh_window_chrome(
                        status_bar.as_mut(),
                        &mut sidebar_painter,
                        &workspace,
                        &panes,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        own_client_id,
                        &agent_meta,
                    &mut vcs,
                    peer_inputs(
                        &sessions,
                        focused_session,
                        &foreign_layouts,
                        &foreign_agents,
                        &foreign_attention,
                    ),
                    );
                    // phux-5ke.4: on overlay dismiss the dispatcher
                    // sets layout_changed=true; the full-frame repaint
                    // below restores pane content under the now-gone
                    // modal. When the overlay is still active (e.g.
                    // a push happened in the same batch) we skip the
                    // pane repaint and go straight to overlay paint.
                    if !overlays.is_active()
                        && let Some(ls) = workspace.render_window(zoomed.as_ref()).as_deref()
                    {
                        let painted = paint_full_frame(
                            out,
                            ls,
                            &mut panes,
                            &engine_kernel,
                            focused_pane.as_ref(),
                            viewport_dims,
                            status_bar.as_mut(),
                            sidebar,
                            Some(&mut sidebar_painter),
                            &session_name,
                        );
                        finish_return_onboarding_after_paint(
                            &mut onboarding_claim,
                            status_bar.as_ref(),
                            painted,
                        );
                    }
                }
                if overlays.is_active() {
                    let painted = paint_active_overlay(
                        out,
                        &overlays,
                        &workspace,
                        &mut panes,
                        &engine_kernel,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                        &theme,
                    );
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
                    );
                }
                // phux-foz.5: a `reload-config` committed in this batch
                // (palette row or bound chord). Runs LAST in the arm so
                // its repaint reflects the new theme/bar.
                if reload_request {
                    reload_request = false;
                    let painted = handle_config_reload(
                        out,
                        &mut keybindings_snapshot,
                        &mut resolver,
                        &mut theme,
                        &mut chrome_breakpoints,
                        &mut status_bar,
                        &mut sidebar_painter,
                        &mut plugin_actions,
                        &mut plugin_panes,
                        &mut which_key_enabled,
                        &mut which_key_delay,
                        &mut overlays,
                        &workspace,
                        &mut panes,
                        &engine_kernel,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        own_client_id,
                        &agent_meta,
                        &mut vcs,
                        peer_inputs(
                            &sessions,
                            focused_session,
                            &foreign_layouts,
                            &foreign_agents,
                            &foreign_attention,
                        ),
                        viewport_dims,
                        sidebar,
                        &session_name,
                    );
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
                    );
                }
            }

            // Inbound frames are drained in a `FRAME_COALESCE_CAP`-bounded
            // batch so a redraw burst paints once; bounded so it cannot
            // starve the stdin arm polled above it.
            frame = conn.recv() => {
                match frame {
                    Ok(first) => {
                        // phux-jhv8: drain every frame already queued so a
                        // back-to-back output burst (nvim startup, a
                        // full-screen redraw) applies all its vt_writes and
                        // paints ONCE — on the final frame — instead of a
                        // render + blocking flush per frame. The non-blocking
                        // try_recv stops the moment the socket would block, so
                        // a lone frame keeps the old one-frame-one-paint path.
                        let mut batch = vec![first];
                        while batch.len() < FRAME_COALESCE_CAP {
                            match conn.try_recv() {
                                Ok(Some(more)) => batch.push(more),
                                // Socket drained, or a clean EOF the next
                                // `recv()` will surface as Disconnected.
                                Ok(None) | Err(AttachError::Disconnected) => break,
                                Err(err) => return Err(err),
                            }
                        }
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
                        // Declared HERE, inside the frame arm, deliberately:
                        // the stdin / ESC-flush arms shadow `sidebar` with a
                        // freshly recomputed reservation so a same-iteration
                        // `toggle-sidebar` takes effect, and a drain hoisted
                        // outside the `select!` would capture the stale outer
                        // one. This arm does not shadow it.
                        let mut repaint = RepaintAccumulator::default();
                        for (frame_idx, f) in batch.into_iter().enumerate() {
                        // phux-foz.8: a peer session's persisted-layout GET
                        // reply. Picker/fleet display data only — decode into
                        // the cache and skip the general frame handler (whose
                        // MetadataValue arm would drop the unmatched id).
                        // phux-jpqd: once a peer's pane tree is known, fetch
                        // each pane's agent record (prune stale first) so the
                        // fleet dashboard's foreign rows carry agent state,
                        // then refresh a live fleet in place.
                        let f = match f {
                            FrameKind::MetadataValue { request_id, value }
                                if foreign_layout_pending.contains_key(&request_id) =>
                            {
                                if let Some(session) = foreign_layout_pending.remove(&request_id) {
                                    apply_foreign_layout_reply(
                                        &mut foreign_layouts,
                                        session,
                                        value.as_deref(),
                                    );
                                    prune_foreign_agents(
                                        &mut foreign_agents,
                                        &mut foreign_agent_subscribed,
                                        &foreign_layouts,
                                    );
                                    if let Some(ws) = foreign_layouts.get(&session) {
                                        sync_foreign_agent_subscriptions(
                                            conn,
                                            ws,
                                            &mut next_request_id,
                                            &mut foreign_agent_pending,
                                            &mut foreign_agent_subscribed,
                                        )
                                        .await?;
                                    }
                                    // ADR-0029 §2: raise, drain once (below the
                                    // loop). A peer's layout reply arrives with
                                    // one agent-record reply per foreign pane
                                    // right behind it; refreshing inline would
                                    // re-project (and repaint) the dashboard
                                    // once per reply.
                                    repaint.raise_fleet();
                                }
                                continue;
                            }
                            // phux-jpqd: a foreign pane's agent-record GET
                            // reply. Fold into the fleet cache and refresh a
                            // live fleet; same intercept shape as the layout
                            // reply (the general handler would drop it).
                            FrameKind::MetadataValue { request_id, value }
                                if foreign_agent_pending.contains_key(&request_id) =>
                            {
                                if let Some(id) = foreign_agent_pending.remove(&request_id) {
                                    apply_foreign_agent_reply(
                                        &mut foreign_agents,
                                        id,
                                        value.as_deref(),
                                    );
                                    repaint.raise_fleet();
                                }
                                continue;
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
                            } if foreign_layout_pending.contains_key(&request_id)
                                || foreign_agent_pending.contains_key(&request_id) =>
                            {
                                foreign_layout_pending.remove(&request_id);
                                foreign_agent_pending.remove(&request_id);
                                continue;
                            }
                            other => other,
                        };
                        let defer_paint = frame_defers_paint(defer_flags[frame_idx], &f);
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
                        let prev_rects = workspace
                            .render_window(zoomed.as_ref())
                            .and_then(|ls| {
                                ls.tree.as_ref().map(|_| {
                                    crate::attach::multi_pane::compute_layout_in(
                                        ls.as_ref(),
                                        content_rect(
                                            viewport_dims,
                                            status_bar.as_ref().map(StatusBarPainter::position),
                                            sidebar,
                                        ),
                                        viewport_dims,
                                    )
                                    .rects
                                })
                            });
                        let focused_before_frame = focused_pane.clone();
                        let mut outcome = handle_server_frame(
                            &mut engine_kernel,
                            &mut kernel_effects,
                            out,
                            f,
                            &mut panes,
                            &mut workspace,
                            &mut focused_pane,
                            &mut zoomed,
                            &mut session_name,
                            focused_session,
                            status_bar.as_mut(),
                            sidebar,
                            viewport_dims,
                            &mut predict,
                            &overlay,
                            layout_get_request_id,
                            &mut pending_splits,
                            &mut pending_windows,
                            &mut expected_closes,
                            &mut agent_meta,
                            overlays.is_active(),
                            defer_paint,
                        )?;
                        send_terminal_replies(
                            conn,
                            take_terminal_replies(&mut outcome, terminal_reply_supported),
                        )
                        .await?;
                        focus_history.observe(focused_before_frame, focused_pane.as_ref());
                        focus_history.repair(focused_pane.as_ref(), &workspace);
                        if outcome.exit {
                            let end = outcome.exit_reason.unwrap_or(AttachEnd::Detached { reason: None });
                            return Ok(detached_loop_exit(end, detach_pending));
                        }
                        if outcome.resync_required {
                            if session_name.is_empty() {
                                return Err(AttachError::Protocol(
                                    "engine requested rebootstrap before ATTACHED named the session"
                                        .to_owned(),
                                ));
                            }
                            let attach_id =
                                send_attach(conn, AttachTarget::ByName(session_name.clone())).await?;
                            tracing::warn!(
                                attach_id,
                                session = %session_name,
                                "engine generation rejected; requested replacement bootstrap"
                            );
                            continue;
                        }
                        // A peer headless placement can add a layout leaf
                        // without this attached client being subscribed to the
                        // new Terminal. Attach each discovered leaf so its
                        // snapshot creates a PaneSlot and renders in place.
                        for terminal_id in &outcome.attach_panes {
                            let request_id = next_request_id;
                            next_request_id = next_request_id.wrapping_add(1);
                            send_unless_peer_gone(conn, &FrameKind::Command {
                                request_id,
                                command: Command::AttachTerminal {
                                    terminal_id: terminal_id.clone(),
                                },
                            })
                            .await?;
                        }
                        // phux-foz.7: did this frame change anything the
                        // agent-fleet dashboard projects (agent records,
                        // asked/lease state, layout/pane set, session
                        // graph)? Captured before the move-y outcome
                        // fields are consumed below; acted on after the
                        // per-frame handling (the fleet refresh block).
                        let fleet_dirty = outcome.chrome_dirty
                            || outcome.agent_meta_changed
                            || outcome.layout_replaced
                            || outcome.reflow_panes
                            || outcome.sessions.is_some();
                        // phux-k0cw: fold anything the frame said about a
                        // session OTHER than ours into the peer caches the
                        // roster and cross-session queue read.
                        //
                        // Both repaint kinds are raised, not just the fleet
                        // one: the peer state now feeds the always-on strip,
                        // so raising `fleet` alone would leave a peer's
                        // change invisible unless the fleet modal happened to
                        // be open (`refresh_fleet_if_open` returns
                        // `NotPublished` when it is not).
                        let layout_folded =
                            if let Some((session, value)) = outcome.foreign_layout {
                                apply_foreign_layout_reply(
                                    &mut foreign_layouts,
                                    session,
                                    value.as_deref(),
                                );
                                prune_foreign_agents(
                                        &mut foreign_agents,
                                        &mut foreign_agent_subscribed,
                                        &foreign_layouts,
                                    );
                                true
                            } else {
                                false
                            };
                        let agent_folded = if let Some((id, value)) = outcome.foreign_agent {
                            apply_foreign_agent_reply(&mut foreign_agents, id, value.as_deref());
                            true
                        } else {
                            false
                        };
                        // Only a NEW ask is a repaint reason; a repeated one
                        // changes nothing the strip renders.
                        let asked_folded = outcome
                            .foreign_attention
                            .is_some_and(|id| foreign_attention.insert(id));
                        // A peer spawn/close needs no fold of its own: the
                        // layouts are re-read on the next sweep, so flagging
                        // the repaint is enough.
                        let foreign_dirty = layout_folded
                            || agent_folded
                            || asked_folded
                            || outcome.foreign_pane_set_dirty;
                        if foreign_dirty {
                            repaint.raise_chrome();
                            repaint.raise_fleet();
                        }
                        finish_return_onboarding_after_paint(
                            &mut onboarding_claim,
                            status_bar.as_ref(),
                            outcome.status_bar_painted,
                        );
                        // ADR-0040: the frame may have added panes
                        // (TerminalSpawned, a peer's layout broadcast) or
                        // removed them (TerminalClosed). Re-sweep so every
                        // live pane has a `phux.agent/v1` watch; the len
                        // guard keeps the steady state zero-cost.
                        if panes.len() != agent_meta.subscribed.len() {
                            sync_agent_meta_subscriptions(
                                conn,
                                panes.keys().cloned().collect(),
                                &mut agent_meta,
                                &mut next_request_id,
                            )
                            .await?;
                        }
                        // phux-4li.20: refresh the cached session graph
                        // whenever an ATTACHED snapshot lands so the
                        // session picker lists the current peer set.
                        // phux-p4vp: the same snapshot refreshes the
                        // pane-cwd index behind the sidebar branch line.
                        vcs.apply_snapshot(outcome.pane_cwds);
                        // phux-foz.8: re-request the peers' persisted
                        // layouts against the fresh graph so the window
                        // picker's one-step rows track it; replies
                        // overwrite stale cache entries.
                        if let Some((list, focused)) = outcome.sessions {
                            sessions = list;
                            focused_session = Some(focused);
                            // phux-k0cw.10: a graph refresh in the SAME batch
                            // that satisfies the deferred bootstrap sweep does
                            // its whole job — same call, same arguments, and
                            // against a fresher graph. Clear the flag so the
                            // drain below does not re-send a GET per peer that
                            // this call already has in flight (the send-once
                            // `subscribed` set covers the SUBSCRIBE half, but
                            // nothing dedupes the GET).
                            peer_sweep_pending = false;
                            sync_foreign_layout_subscriptions(
                                conn,
                                &sessions,
                                focused_session,
                                &mut next_request_id,
                                &mut foreign_layout_pending,
                                &mut foreign_layout_subscribed,
                            )
                            .await?;
                        }
                        // ADR-0033 / phux-foz.1: a `TerminalControl` or `Asked`
                        // event changed a pane's lease/lifecycle/attention. The
                        // event frame paints nothing, so refresh the chrome
                        // (supervisory badge, attention hint, window markers)
                        // and repaint here — but only when a painter input
                        // actually changed (`refresh_window_chrome` reports
                        // it), so an event that alters no visible state doesn't
                        // force a full-window repaint. (`own_client_id` is
                        // fixed for the life of this loop; it was captured at
                        // bootstrap.)
                        if outcome.chrome_dirty {
                            let chrome_changed = refresh_window_chrome(
                                status_bar.as_mut(),
                                &mut sidebar_painter,
                                &workspace,
                                &panes,
                                focused_pane.as_ref(),
                                zoomed.as_ref(),
                                own_client_id,
                                &agent_meta,
                            &mut vcs,
                            peer_inputs(
                                &sessions,
                                focused_session,
                                &foreign_layouts,
                                &foreign_agents,
                                &foreign_attention,
                            ),
                            );
                            // ADR-0029: nothing about a title / lease /
                            // attention change touches a pane interior, so this
                            // is a CHROME raise, not a full-frame clear.
                            if chrome_changed && !overlays.is_active() {
                                repaint.raise_chrome();
                            }
                        }
                        // phux-i0e8.2.1: drain the frame's transient notices
                        // into the painter's newest-wins slot; expiry rides
                        // the 1 s status_tick arm below. With no bar to paint
                        // on (no painter, an empty bar, or the persistent
                        // error line holding the row — the painter refuses
                        // those itself) the notice degrades to a tracing
                        // line rather than vanishing.
                        if !outcome.notices.is_empty() {
                            let now = std::time::Instant::now();
                            let mut notice_shown = false;
                            for notice in outcome.notices {
                                if let Some(sb) = status_bar.as_mut() {
                                    notice_shown |= sb.set_notice(notice, now);
                                } else {
                                    tracing::info!(
                                        severity = ?notice.severity,
                                        text = %notice.text,
                                        "status-bar notice dropped: no status bar configured",
                                    );
                                }
                            }
                            if notice_shown && !overlays.is_active() {
                                repaint.raise_chrome();
                            }
                        }
                        if let Some((terminal_id, stream_id, bootstrap_id, seq)) =
                            should_emit_frame_ack(wants_state_sync, outcome.ack)
                        {
                            send_unless_peer_gone(conn, &FrameKind::FrameAck {
                                terminal_id,
                                stream_id,
                                bootstrap_id,
                                seq,
                            })
                            .await?;
                        }
                        if let Some((
                            terminal_id,
                            stream_id,
                            bootstrap_id,
                            cursor,
                            max_bytes,
                            max_rows,
                        )) = outcome.history_request
                        {
                            send_unless_peer_gone(conn, &FrameKind::HistoryRequest {
                                terminal_id,
                                stream_id,
                                bootstrap_id,
                                cursor,
                                max_bytes,
                                max_rows,
                            })
                            .await?;
                        }
                        // phux-4li.12: a layout mutation triggered by a
                        // server frame (TerminalSpawned ok, TerminalClosed)
                        // requires the same `SET_METADATA` broadcast as
                        // a local action — see `ActionEffects.set_metadata`
                        // for the local-action path.
                        if outcome.emit_set_metadata
                            && let Some(session) = focused_session
                            && let Some(bytes) = encode_layout_or_log(&workspace)
                        {
                            let request_id = next_request_id;
                            next_request_id = next_request_id.wrapping_add(1);
                            send_unless_peer_gone(conn, &FrameKind::SetMetadata {
                                request_id,
                                scope: Scope::Group(DEFAULT_GROUP_ID),
                                key: layout_key(session),
                                value: bytes,
                            })
                            .await?;
                        }
                        // phux-tnh: a pane close/spawn changed surviving
                        // panes' dimensions. Diff the folded/split layout
                        // against the pre-frame rects and emit a
                        // TERMINAL_RESIZE per changed leaf — same path the
                        // SIGWINCH arm uses — so the server reflows each
                        // PTY (TIOCSWINSZ) and the shell redraws to fill.
                        // Without this the survivor of a close keeps its
                        // old small winsize ("survivor stays small").
                        // Sent BEFORE the repaint so the server's resync
                        // snapshot lands after the local mirror has grown.
                        if outcome.reflow_panes
                            && let Some(prev_rects) = &prev_rects
                            && let Some(ls) = workspace.render_window(zoomed.as_ref())
                            && ls.tree.is_some()
                        {
                            let new_content =
                                content_rect(
                        viewport_dims,
                        status_bar.as_ref().map(StatusBarPainter::position),
                        sidebar,
                    );
                            let diff = crate::attach::reflow::compute_reflow(
                                ls.as_ref(),
                                prev_rects,
                                new_content,
                            );
                            for (terminal_id, new_rect) in &diff.changed {
                                send_unless_peer_gone(conn, &FrameKind::TerminalResize {
                                    terminal_id: terminal_id.clone(),
                                    cols: new_rect.w,
                                    rows: new_rect.h,
                                })
                                .await?;
                            }
                        }
                        if outcome.layout_replaced {
                            // phux-foz.8: a one-step cross-session window
                            // pick drove this attach; the multi-window
                            // layout just landed, so resolve the deferred
                            // select against it before the repaint below.
                            // Out-of-range (a peer mutated the layout
                            // between pick and load) keeps the session's
                            // restored focus with a warning.
                            if let Some(idx) = pending_window.take() {
                                if workspace.select(idx) {
                                    let next_focus = workspace
                                        .active_window()
                                        .and_then(|ls| ls.focus.clone());
                                    focus_history.transition(&mut focused_pane, next_focus);
                                    // phux-jpqd: the pane half of a
                                    // one-step cross-session pane pick — move
                                    // focus onto the target DFS leaf of the
                                    // just-selected window. Out-of-range
                                    // (peer mutated the layout) keeps the
                                    // window's restored focus, logged.
                                    if let Some(ord) = pending_pane.take() {
                                        if let Some(leaf) = workspace
                                            .active_window()
                                            .and_then(|ls| ls.tree.as_ref())
                                            .map(crate::layout::leaves)
                                            .and_then(|leaves| leaves.get(ord).cloned())
                                        {
                                            if let Some(ls) = workspace.active_window_mut()
                                            {
                                                ls.focus = Some(leaf.clone());
                                            }
                                            focus_history
                                                .transition(&mut focused_pane, Some(leaf));
                                        } else {
                                            tracing::warn!(
                                                window = idx,
                                                pane = ord,
                                                "cross-session pane pick out of range; keeping window focus",
                                            );
                                        }
                                    }
                                    if let Some(fid) = focused_pane.as_ref() {
                                        reanchor_predict_to_pane(&mut predict, &panes, fid);
                                    }
                                } else {
                                    tracing::warn!(
                                        index = idx,
                                        windows = workspace.windows.len(),
                                        "cross-session window pick out of range; keeping restored focus",
                                    );
                                }
                            }
                            // phux-4li.5: layout changed under us
                            // (either the GET reply or a peer's broadcast).
                            // Trigger a full repaint: clear screen + paint
                            // dividers + re-render every pane.
                            // phux-5ke.4: while an overlay is up, defer
                            // the repaint — the dismiss path always
                            // triggers paint_full_frame, and the
                            // libghostty mirror is already updated.
                            refresh_window_chrome(
                                status_bar.as_mut(),
                                &mut sidebar_painter,
                                &workspace,
                                &panes,
                                focused_pane.as_ref(),
                                zoomed.as_ref(),
                                own_client_id,
                                &agent_meta,
                            &mut vcs,
                            peer_inputs(
                                &sessions,
                                focused_session,
                                &foreign_layouts,
                                &foreign_agents,
                                &foreign_attention,
                            ),
                            );
                            // phux-z6wt: this arm fires for a peer's layout
                            // broadcast and for the TerminalSpawned/
                            // TerminalClosed reflow — neither goes through
                            // SIGWINCH, so the phux-d26y fan-out never ran
                            // for them. A surviving copy-mode overlay would
                            // keep clamping against the pane size it opened
                            // with, silently dropping or clipping a copy.
                            // Recompute the focused pane's rect the same way
                            // the SIGWINCH arm does and hand it to every
                            // surviving overlay before the repaint below.
                            sync_overlays_to_focused_pane(
                                &mut overlays,
                                &workspace,
                                zoomed.as_ref(),
                                focused_pane.as_ref(),
                                viewport_dims,
                                status_bar.as_ref().map(StatusBarPainter::position),
                                sidebar,
                            );
                            // The pane rects moved: only a full-viewport
                            // repaint (ED2 + every pane + dividers) is a
                            // coherent base. ADR-0029: raise, drain once.
                            if !overlays.is_active() {
                                repaint.raise_full();
                            }
                            // The GET reply is single-use; clear the pending
                            // request id so a stray late MetadataValue can't
                            // trample state. Gated on `layout_get_answered`,
                            // NOT on `layout_replaced`: the latter is also
                            // raised for pane damage during bootstrap, and
                            // clearing on that dropped the real reply.
                            if outcome.layout_get_answered {
                                layout_get_request_id = None;
                            }
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
                            let chrome_changed = refresh_window_chrome(
                                status_bar.as_mut(),
                                &mut sidebar_painter,
                                &workspace,
                                &panes,
                                focused_pane.as_ref(),
                                zoomed.as_ref(),
                                own_client_id,
                                &agent_meta,
                            &mut vcs,
                            peer_inputs(
                                &sessions,
                                focused_session,
                                &foreign_layouts,
                                &foreign_agents,
                                &foreign_attention,
                            ),
                            );
                            if chrome_changed && !overlays.is_active() {
                                repaint.raise_chrome();
                            }
                        }
                        // phux-foz.5: the `phux config reload` doorbell
                        // rang (a subscribed `phux.config.reload/v1`
                        // broadcast). Re-read our own config file and swap
                        // the config-derived state in place — same handler
                        // as the `reload-config` action; failures keep the
                        // previous config and toast.
                        if outcome.config_reload {
                            let painted = handle_config_reload(
                                out,
                                &mut keybindings_snapshot,
                                &mut resolver,
                                &mut theme,
                                &mut chrome_breakpoints,
                                &mut status_bar,
                                &mut sidebar_painter,
                                &mut plugin_actions,
                                &mut plugin_panes,
                                &mut which_key_enabled,
                                &mut which_key_delay,
                                &mut overlays,
                                &workspace,
                                &mut panes,
                                &engine_kernel,
                                focused_pane.as_ref(),
                                zoomed.as_ref(),
                                own_client_id,
                                &agent_meta,
                                &mut vcs,
                                peer_inputs(
                                    &sessions,
                                    focused_session,
                                    &foreign_layouts,
                                    &foreign_agents,
                                    &foreign_attention,
                                ),
                                viewport_dims,
                                sidebar,
                                &session_name,
                            );
                            finish_return_onboarding_after_paint(
                                &mut onboarding_claim,
                                status_bar.as_ref(),
                                painted,
                            );
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
                        // batch, and this arm used to fire nine times inside it —
                        // nine full-screen clears in one iteration, in exactly
                        // the view that exists for watching agents. The
                        // accumulator collapses them into ONE refresh at the
                        // drain below.
                        if fleet_dirty {
                            repaint.raise_fleet();
                        }
                        }
                        // ADR-0029 §2: the ONE drain. Every loop-level repaint
                        // trigger in this batch has raised; the highest level
                        // wins and paints exactly once. `Chrome` repaints the
                        // sidebar strip + status bar in place (no ED2, no pane
                        // re-render); `Full` clears and recomposites because
                        // the pane rects moved under us.
                        let drained = repaint.drain();
                        // The overlay half of the same drain. A no-op unless a
                        // live fleet list is actually in the overlay stack, so
                        // the raise costs nothing when the dashboard is closed.
                        if drained.fleet_dirty {
                            let painted = refresh_fleet_if_open(
                                out,
                                &mut overlays,
                                &workspace,
                                &mut panes,
                                &engine_kernel,
                                focused_pane.as_ref(),
                                zoomed.as_ref(),
                                viewport_dims,
                                status_bar.as_mut(),
                                sidebar,
                                &mut sidebar_painter,
                                &session_name,
                                &theme,
                                &sessions,
                                focused_session,
                                &agent_meta.records,
                                &mut vcs,
                                &foreign_layouts,
                                &foreign_agents,
                            );
                            finish_return_onboarding_after_paint(
                                &mut onboarding_claim,
                                status_bar.as_ref(),
                                painted,
                            );
                        }
                        if !overlays.is_active()
                            && let Some(ls) = workspace.render_window(zoomed.as_ref()).as_deref()
                        {
                            let status_bar_painted = match drained.level {
                                RepaintLevel::None => StatusBarPaint::NotPublished,
                                RepaintLevel::Chrome => paint_chrome_in_place(
                                    out,
                                    ls,
                                    &panes,
                                    focused_pane.as_ref(),
                                    viewport_dims,
                                    status_bar.as_mut(),
                                    sidebar,
                                    Some(&mut sidebar_painter),
                                    &session_name,
                                ),
                                RepaintLevel::Full => paint_full_frame(
                                    out,
                                    ls,
                                    &mut panes,
                                    &engine_kernel,
                                    focused_pane.as_ref(),
                                    viewport_dims,
                                    status_bar.as_mut(),
                                    sidebar,
                                    Some(&mut sidebar_painter),
                                    &session_name,
                                ),
                            };
                            finish_return_onboarding_after_paint(
                                &mut onboarding_claim,
                                status_bar.as_ref(),
                                status_bar_painted,
                            );
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
                        if peer_sweep_pending {
                            peer_sweep_pending = false;
                            sync_foreign_layout_subscriptions(
                                conn,
                                &sessions,
                                focused_session,
                                &mut next_request_id,
                                &mut foreign_layout_pending,
                                &mut foreign_layout_subscribed,
                            )
                            .await?;
                        }
                    }
                    Err(AttachError::Disconnected) if detach_pending => {
                        // Server closed the socket without a `DETACHED`
                        // frame — treat it as a clean shutdown because
                        // the user requested detach. Otherwise the loop
                        // bubbles the disconnect up unchanged. No frame
                        // arrived, so there is no stated reason to carry.
                        return Ok(detached_loop_exit(
                            AttachEnd::Detached { reason: None },
                            true,
                        ));
                    }
                    Err(err) => return Err(err),
                }
            }

            // Bound the failure mode of an application that omits `?2026l`.
            // Expose the latest complete mirror once, then let subsequent
            // output re-arm the transaction watchdog.
            () = sync_output_sleep => {
                let now = tokio::time::Instant::now();
                let mut expired = false;
                for slot in panes.values_mut() {
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
                if expired
                    && !overlays.is_active()
                    && let Some(ls) = workspace.render_window(zoomed.as_ref()).as_deref()
                {
                    let painted = paint_full_frame(
                        out,
                        ls,
                        &mut panes,
                        &engine_kernel,
                        focused_pane.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                    );
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
                    );
                }
            }

            // Bare-ESC idle timeout. Only armed when the parser has
            // pending state; resolves an ambiguous lone ESC into the
            // Escape key (see input::StdinParser::flush docs).
            () = flush_sleep => {
                let events = parser.flush();
                // phux-x2hm: a flushed bare-ESC chord can also resolve to
                // A flushed event may complete `toggle-zoom` or
                // `toggle-sidebar`; capture the old view for the same reflow
                // handshake as the stdin arm.
                let prev_zoomed = zoomed.clone();
                let prev_sidebar = sidebar;
                let prev_view_rects = view_rects(
                    &workspace,
                    prev_zoomed.as_ref(),
                    content_rect(
                        viewport_dims,
                        status_bar.as_ref().map(StatusBarPainter::position),
                        sidebar,
                    ),
                    viewport_dims,
                );
                // phux-foz.9: same agents-row snapshot as the stdin arm.
                let sidebar_targets = sidebar_painter.click_targets();
                let mut ctx = DispatchCtx {
                    engine_kernel: &mut engine_kernel,
                    resolver: resolver.as_mut(),
                    focus_history: focus_history.clone(),
                    workspace: &mut workspace,
                    viewport: viewport_dims,
                    cell_px: cell_px_dims,
                    next_request_id: &mut next_request_id,
                    spawn_initial_size_supported,
                    pending_splits: &mut pending_splits,
                    pending_windows: &mut pending_windows,
                    expected_closes: &mut expected_closes,
                    overlays: &mut overlays,
                    keybindings: keybindings_snapshot.as_ref(),
                    theme: &theme,
                    sessions: &sessions,
                    foreign_layouts: &foreign_layouts,
                    foreign_agents: &foreign_agents,
                    focused_session,
                    session_name: &mut session_name,
                    switch_request: &mut switch_request,
                    zoomed: &mut zoomed,
                    sidebar,
                    sidebar_enabled: &mut sidebar_enabled,
                    sidebar_width,
                    chrome: chrome_breakpoints,
                    sidebar_targets: &sidebar_targets,
                    bar: status_bar.as_ref().map(StatusBarPainter::position),
                    status_bar: status_bar.as_ref(),
                    drag: &mut drag,
                    mouse_optout: &mut mouse_optout,
                    attention_navigation: &mut attention_navigation,
                    plugin_actions: &plugin_actions,
                    plugin_panes: &plugin_panes,
                    plugin_tx: Some(&plugin_tx),
                    reload_request: &mut reload_request,
                    agent_meta: &agent_meta.records,
                    vcs: &mut vcs,
                };
                let layout_changed = dispatch_input_events(
                    out,
                    conn,
                    events,
                    &mut focused_pane,
                    &mut detach_pending,
                    &mut predict,
                    &overlay,
                    &mut panes,
                    &mut ctx,
                )
                .await?;
                focus_history = ctx.focus_history;
                // phux-4h5a: re-fold a `toggle-sidebar` flip into the
                // reservation, same as the stdin arm, so the same-iteration
                // repaint tiles into the new content rect.
                let sidebar = sidebar_reservation(viewport_dims.0, sidebar_enabled, sidebar_width, sidebar_edge, chrome_breakpoints.min_pane_cols);
                // phux-eb0: same switch-on-commit check as the stdin arm.
                // A bare-ESC flush can carry the final chord of a
                // `<leader> a` selection committed via Enter.
                if let Some(target) = switch_request.take() {
                    return Ok(LoopExit::SwitchTo {
                        target,
                        sidebar_enabled,
                    });
                }
                if zoomed != prev_zoomed || sidebar != prev_sidebar {
                    emit_view_reflow(
                        conn,
                        &workspace,
                        zoomed.as_ref(),
                        &prev_view_rects,
                        content_rect(
                        viewport_dims,
                        status_bar.as_ref().map(StatusBarPainter::position),
                        sidebar,
                    ),
                    )
                    .await?;
                }
                if layout_changed {
                    // ADR-0040: keep the agent-metadata watches in step
                    // with a pane set changed by this flush's actions.
                    sync_agent_meta_subscriptions(
                        conn,
                        panes.keys().cloned().collect(),
                        &mut agent_meta,
                        &mut next_request_id,
                    )
                    .await?;
                    refresh_window_chrome(
                        status_bar.as_mut(),
                        &mut sidebar_painter,
                        &workspace,
                        &panes,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        own_client_id,
                        &agent_meta,
                    &mut vcs,
                    peer_inputs(
                        &sessions,
                        focused_session,
                        &foreign_layouts,
                        &foreign_agents,
                        &foreign_attention,
                    ),
                    );
                }
                if layout_changed
                    && !overlays.is_active()
                    && let Some(ls) = workspace.render_window(zoomed.as_ref()).as_deref()
                {
                    let painted = paint_full_frame(
                        out,
                        ls,
                        &mut panes,
                        &engine_kernel,
                        focused_pane.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                    );
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
                    );
                }
                if overlays.is_active() {
                    let painted = paint_active_overlay(
                        out,
                        &overlays,
                        &workspace,
                        &mut panes,
                        &engine_kernel,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                        &theme,
                    );
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
                    );
                }
                // phux-foz.5: same reload-on-commit check as the stdin
                // arm — a bare-ESC flush can carry the final chord of a
                // palette selection committing `reload-config`.
                if reload_request {
                    reload_request = false;
                    let painted = handle_config_reload(
                        out,
                        &mut keybindings_snapshot,
                        &mut resolver,
                        &mut theme,
                        &mut chrome_breakpoints,
                        &mut status_bar,
                        &mut sidebar_painter,
                        &mut plugin_actions,
                        &mut plugin_panes,
                        &mut which_key_enabled,
                        &mut which_key_delay,
                        &mut overlays,
                        &workspace,
                        &mut panes,
                        &engine_kernel,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        own_client_id,
                        &agent_meta,
                        &mut vcs,
                        peer_inputs(
                            &sessions,
                            focused_session,
                            &foreign_layouts,
                            &foreign_agents,
                            &foreign_attention,
                        ),
                        viewport_dims,
                        sidebar,
                        &session_name,
                    );
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
                    );
                }
            }

            // phux-foz.2: which-key idle timeout. Armed only while the
            // resolver sits at the pending-prefix state (see the update
            // above); fires once per hesitation. Pushing the popup does
            // not touch the resolver — the pending prefix stays live, so
            // the next chord executes exactly as if the popup never
            // appeared (the dispatcher's passthrough branch dismisses it
            // on the way through).
            () = which_key_sleep => {
                which_key_deadline = None;
                if push_which_key_overlay(
                    &mut overlays,
                    resolver.as_ref(),
                    keybindings_snapshot.as_ref(),
                    &theme,
                ) {
                    let painted = paint_active_overlay(
                        out,
                        &overlays,
                        &workspace,
                        &mut panes,
                        &engine_kernel,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                        &theme,
                    );
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
                    );
                }
            }

            // SIGWINCH — terminal was resized. Read the new viewport
            // and ship a VIEWPORT_RESIZE upstream (SPEC §7.1 / §10.5).
            // The server uses this to recompute layout and update the
            // attached pane's dims. On query failure we fall back to a
            // sane default (logged) rather than skip the frame — the
            // server still benefits from knowing a resize happened.
            _ = sigwinch.recv() => {
                let prev_dims = viewport_dims;
                let viewport = current_viewport_or_default();
                viewport_dims = (viewport.cols.max(1), viewport.rows.max(1));
                cell_px_dims = host_cell_px(&viewport);
                // Bound predict to the FOCUSED pane's current grid, not the
                // whole viewport — predictions are pane-local (phux-7ry0). The
                // pane grids resize on the server's resize-ack snapshot, which
                // re-syncs predict again; this just keeps the transient
                let (predict_cols, predict_rows) = focused_pane
                    .as_ref()
                    .and_then(|fid| panes.get(fid))
                    .map_or((viewport.cols, viewport.rows), |slot| slot.geometry);
                predict.set_viewport(predict_cols, predict_rows);
                conn.send(&viewport_resize_frame(viewport)).await?;

                // Emit one TERMINAL_RESIZE per leaf whose (w, h) actually
                // changed so the server ioctls TIOCSWINSZ on each PTY. This
                // covers the single-pane case too — `Workspace::single` seeds
                // a one-leaf tree, so the `tree.is_some()` guard only skips a
                // workspace with no panes at all, and a lone pane still needs
                // sizing to the chrome-inset content rect.
                if let Some(ls) = workspace.render_window(zoomed.as_ref())
                    && ls.tree.is_some()
                {
                    let bar = status_bar.as_ref().map(StatusBarPainter::position);
                    // phux-4h5a: size each PTY to the inset content rect (the
                    // pane area after the status bar + sidebar reservation),
                    // not the full viewport — otherwise an enabled sidebar
                    // resizes panes to the full width while they paint inset.
                    let prev_content = content_rect(prev_dims, bar, sidebar);
                    let new_content = content_rect(viewport_dims, bar, sidebar);
                    let prev_rects =
                        crate::attach::multi_pane::compute_layout_in(ls.as_ref(), prev_content, prev_dims)
                            .rects;
                    let diff = crate::attach::reflow::compute_reflow(
                        ls.as_ref(),
                        &prev_rects,
                        new_content,
                    );
                    if diff.too_small {
                        tracing::warn!(
                            cols = viewport_dims.0,
                            rows = viewport_dims.1,
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
                }
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
                if overlays.dismiss_stale_on_resize() {
                    tracing::debug!("resize: dropped a pinned overlay whose geometry went stale");
                }
                if overlays.is_active() {
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
                    // Same choke point the `layout_replaced` arm uses for
                    // the non-SIGWINCH triggers (a peer's layout broadcast,
                    // TerminalSpawned/TerminalClosed reflow).
                    sync_overlays_to_focused_pane(
                        &mut overlays,
                        &workspace,
                        zoomed.as_ref(),
                        focused_pane.as_ref(),
                        viewport_dims,
                        status_bar.as_ref().map(StatusBarPainter::position),
                        sidebar,
                    );
                    let painted = paint_active_overlay(
                        out,
                        &overlays,
                        &workspace,
                        &mut panes,
                        &engine_kernel,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                        &theme,
                    );
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
                    );
                } else {
                    let _ = out.flush();
                }
            }

            // phux-nz4.5: periodic status-bar repaint (e.g. for the
            // `time` widget). Only fires when at least one widget has a
            // `poll_interval`. Paints in place — no pane re-render, no
            // full-screen redraw.
            () = status_tick => {
                // phux-i0e8.2.1: expire the transient notice on the tick that
                // carries the bar's repaint cadence. The clear invalidates the
                // painter's cache, so the paint below restores the widget row.
                // Runs even while an overlay is up (the bar repaints on
                // overlay dismiss, and a stale notice must not resurface).
                if let Some(sb) = status_bar.as_mut() {
                    let _ = sb.clear_expired_notice(std::time::Instant::now());
                }
                // phux-5ke.4: an overlay above the bar would get
                // partially overwritten by the bar paint; skip ticks
                // while a modal is up.
                if !overlays.is_active() {
                    // Restore the cursor to wherever the focused pane left it
                    // so an idle tick doesn't strand the cursor in the bar.
                    let focused_cursor = focused_pane.as_ref()
                        .and_then(|fid| panes.get(fid))
                        .and_then(|slot| slot.renderer.last_cursor());
                    // phux-9xn / phux-gxy: ALWAYS provide a fallback
                    // origin. When focused_pane is None (e.g. ATTACHED
                    // hasn't seeded yet) the old code passed None →
                    // paint_bar_after_pane emitted no CUP → cursor
                    // stranded at the bar's last cell every tick.
                    let bar = status_bar.as_ref().map(StatusBarPainter::position);
                    let content = content_rect(viewport_dims, bar, sidebar);
                    let fallback_origin = Some(
                        focused_pane
                            .as_ref()
                            .and_then(|fid| {
                                workspace.render_window(zoomed.as_ref()).and_then(|ls| {
                                    crate::attach::multi_pane::compute_layout_in(
                                        ls.as_ref(),
                                        content,
                                        viewport_dims,
                                    )
                                    .rects
                                    .get(fid)
                                    .copied()
                                })
                            })
                            .map_or((0, 0), |r| (r.x, r.y)),
                    );
                    tracing::trace!(
                        focused_pane_set = focused_pane.is_some(),
                        has_cursor = focused_cursor.is_some(),
                        "status_tick: repaint bar"
                    );
                    let painted = paint_bar_after_pane(
                        status_bar.as_mut(),
                        out,
                        viewport_dims,
                        sidebar,
                        &session_name,
                        focused_cursor,
                        fallback_origin,
                        // Idle tick: nothing clobbered the bar row. The
                        // painter's content cache repaints only when a
                        // widget (e.g. the clock) actually changed.
                        false,
                    );
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
                    );
                }
            }

            // phux-r82.5: a spawned plugin action finished. Successes just
            // log (no modal to dismiss on the happy path); failures push a
            // dismissable toast carrying the captured output, so a broken
            // plugin is *seen* without ever having blocked the input loop.
            // The channel can't close while this loop holds `plugin_tx`,
            // so the `Some` pattern always matches when the arm fires.
            Some(result) = plugin_rx.recv() => {
                tracing::info!(
                    plugin = %result.plugin_id,
                    action = %result.action_id,
                    ok = plugin_actions::run_succeeded(&result),
                    "plugin action finished",
                );
                if let Some((title, lines)) = plugin_actions::failure_toast(&result) {
                    overlays.push(Box::new(crate::render::overlay::ToastOverlay::new(
                        title, lines, &theme,
                    )));
                    let painted = paint_active_overlay(
                        out,
                        &overlays,
                        &workspace,
                        &mut panes,
                        &engine_kernel,
                        focused_pane.as_ref(),
                        zoomed.as_ref(),
                        viewport_dims,
                        status_bar.as_mut(),
                        sidebar,
                        Some(&mut sidebar_painter),
                        &session_name,
                        &theme,
                    );
                    finish_return_onboarding_after_paint(
                        &mut onboarding_claim,
                        status_bar.as_ref(),
                        painted,
                    );
                }
            }

            // SIGINT — restore the terminal explicitly (Drop wouldn't
            // fire on `exit(130)`), then exit with the shell-conventional
            // 130. `phux-roz`: this is the path that fires when the user
            // hits Ctrl-C in the outer shell after `phux attach` has
            // entered the alt screen.
            _ = sigint.recv() => {
                terminal_reset_on_signal();
                #[allow(clippy::exit, reason = "signal-driven graceful exit; Drop won't run")]
                std::process::exit(130);
            }

            // SIGTERM — `kill <pid>` from a sibling tool, supervisor, or
            // the user's tmux/screen wrapping us. Same cleanup, exit 143.
            _ = sigterm.recv() => {
                terminal_reset_on_signal();
                #[allow(clippy::exit, reason = "signal-driven graceful exit; Drop won't run")]
                std::process::exit(143);
            }

            // SIGHUP — controlling terminal went away. Restore and exit
            // 129. There is no live outer terminal to clean up, but the
            // termios restore is harmless on a dead tty and keeps the
            // cleanup path uniform.
            _ = sighup.recv() => {
                terminal_reset_on_signal();
                #[allow(clippy::exit, reason = "signal-driven graceful exit; Drop won't run")]
                std::process::exit(129);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn coalesce_defers_every_pane_frame_but_its_last() {
        // phux-jhv8: in a coalesced burst, every output frame for a pane
        // defers EXCEPT that pane's final frame, which settles the screen.
        let p = |id| Some(TerminalId::Local { id });
        // Single-pane burst: only the last frame paints.
        assert_eq!(
            coalesce_defer_flags(&[p(2), p(2), p(2)], Option::as_ref),
            vec![true, true, false]
        );
        // A lone frame never defers (preserves the one-frame-one-paint path).
        assert_eq!(coalesce_defer_flags(&[p(2)], Option::as_ref), vec![false]);
    }

    #[test]
    fn coalesce_keys_deferral_per_pane_not_globally() {
        // Two panes interleaved: each pane's LAST frame paints, so neither is
        // left stale even when the burst ends on the other pane's output.
        let p = |id| Some(TerminalId::Local { id });
        // A(defer, later A) B(defer, later B) A(last A) B(last B)
        assert_eq!(
            coalesce_defer_flags(&[p(1), p(2), p(1), p(2)], Option::as_ref),
            vec![true, true, false, false]
        );
        // Burst ending on a non-focused pane B must still paint A's last frame.
        assert_eq!(
            coalesce_defer_flags(&[p(1), p(1), p(2)], Option::as_ref),
            vec![true, false, false]
        );
    }

    #[test]
    fn output_honors_coalescing_decision() {
        let output = FrameKind::TerminalOutput {
            terminal_id: TerminalId::Local { id: 1 },
            stream_id: phux_protocol::StreamId::new(1).expect("stream"),
            bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
            seq: 1,
            bytes: bytes::Bytes::new(),
        };
        assert!(frame_defers_paint(true, &output));
        assert!(!frame_defers_paint(false, &output));
    }

    #[test]
    fn coalesce_control_frames_never_defer() {
        // `None` (a non-painting control frame) never defers, and never
        // counts as a later same-pane paint for the frames before it.
        let p = |id| Some(TerminalId::Local { id });
        assert_eq!(
            coalesce_defer_flags(&[p(1), None, p(1)], Option::as_ref),
            vec![true, false, false]
        );
        assert_eq!(
            coalesce_defer_flags(&[None, None], Option::as_ref),
            vec![false, false]
        );
        let empty: [Option<TerminalId>; 0] = [];
        assert_eq!(
            coalesce_defer_flags(&empty, Option::as_ref),
            Vec::<bool>::new()
        );
    }
}
