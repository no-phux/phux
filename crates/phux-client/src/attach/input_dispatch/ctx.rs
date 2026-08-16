//! The mutable dispatch context (`DispatchCtx`) and the in-flight
//! divider-drag state (`DragGrab`).

//! Input dispatcher: translates parser-emitted events into wire frames
//! or layout-action effects.
//!
//! Owns the resolver-intercept path (prefix chord → `ResolvedAction` →
//! mutate the active window of the `Workspace`), the predict overlay's
//! keystroke feed, and the parked-spawn bookkeeping (`PendingSplit` /
//! `PendingWindow`) that bridges a local `split-pane` / `new-window`
//! chord to its remote `SPAWN_TERMINAL` reply.

use std::collections::{HashMap, HashSet};

use phux_protocol::TerminalId;

use crate::attach::actions::{PendingSplit, PendingWindow};
use crate::attach::focus::FocusHistory;
use crate::attach::paint::SidebarReservation;
use crate::attach::pane_state::AttentionNavigation;
use crate::attach::plugin_actions::{PluginActionEntry, PluginRunResult};
use crate::attach::plugin_panes::PluginPaneEntry;
use crate::layout::{SplitDir, Workspace};
use crate::render::overlay::OverlayState;
use crate::render::{ChromeBreakpoints, Theme};

use super::effects::ReattachTarget;

/// Mutable context the input-dispatch path needs to update on a chord
/// that resolves to a layout action (phux-4li.5). Bundles the items
/// that would otherwise inflate `dispatch_input_events`'s argument
/// list past clippy's threshold.
pub(in crate::attach) struct DispatchCtx<'a> {
    /// Connection-owned engine replicas used for terminal queries and local scrolling.
    pub engine_kernel: &'a mut crate::attach::pane_state::AttachKernel,
    /// Keybind resolver state. `None` when the on-disk config failed
    /// to parse; the dispatcher then forwards every key to the focused
    /// pane unchanged.
    pub resolver: Option<&'a mut phux_config::keybind::Resolver>,
    /// Client-local focus transition/MRU bookkeeping.
    pub focus_history: FocusHistory,
    /// Client-side multi-window mirror. Pane actions operate on the
    /// active window ([`Workspace::active_window_mut`]); the whole
    /// workspace is what gets serialized to L3 on a `SET_METADATA`.
    pub workspace: &'a mut Workspace,
    /// Outer-viewport `(cols, rows)`. Used by `apply_resize` to convert
    /// `amount` (cells) to a ratio delta.
    pub viewport: (u16, u16),
    /// Host per-cell pixel size `(width, height)`, derived from the outer
    /// terminal's winsize pixel fields exactly the way the server derives
    /// its cell size from our `VIEWPORT_RESIZE` (`pixel / cells`, floored —
    /// SPEC L1 §9.2.1), so the two ends quantize mouse positions with the
    /// same geometry. Falls back to the same 8x16 the server seeds when the
    /// host reports no pixels. Never zero on either axis.
    ///
    /// SPEC input.md §3.1: `INPUT_MOUSE` positions on the wire are
    /// Terminal-local surface-space PIXELS; the dispatcher routes in cells
    /// and scales by this at the send boundary only (phux-yyex).
    pub cell_px: (u16, u16),
    /// Monotonic source of new request ids. We don't currently issue
    /// per-action correlated requests (the only side-channel today is
    /// the layout `SET_METADATA`, which doesn't need a reply), but we
    /// reserve the counter for future `SPAWN`/kill wiring.
    pub next_request_id: &'a mut u32,
    /// phux-a5xj: did the server advertise
    /// [`ServerFeature::SpawnInitialSize`](phux_protocol::caps::ServerFeature::SpawnInitialSize)?
    /// When set, a spawn carries the tile the new leaf will occupy so the
    /// pane bootstraps at its real geometry instead of at the server default
    /// and then being reflowed. Unset against an older server, where the
    /// field would be skipped by length anyway — omitting it keeps the
    /// frame byte-identical to what that server has always decoded.
    pub spawn_initial_size_supported: bool,
    /// phux-4li.12: parked split actions awaiting their
    /// `TERMINAL_SPAWNED` reply. `run_action` inserts;
    /// `handle_server_frame` removes.
    pub pending_splits: &'a mut HashMap<u32, PendingSplit>,
    /// phux-4li.15: parked `new-window` actions awaiting their
    /// `TERMINAL_SPAWNED` reply. Same lifecycle as `pending_splits`,
    /// keyed in the same request-id space.
    pub pending_windows: &'a mut HashMap<u32, PendingWindow>,
    /// phux-i0e8.2.2: Terminals whose close THIS client requested
    /// (kill-pane / kill-window soft-kill). [`apply_action_effects`]
    /// parks the target ids here at the kill-dispatch seam; the
    /// `TerminalClosed` arm of `handle_server_frame` drains a matching
    /// id and suppresses the pane-exit notice — the user ordered that
    /// death, so reporting it would be noise.
    pub expected_closes: &'a mut HashSet<TerminalId>,
    /// phux-5ke.4: overlay stack. When non-empty the dispatcher routes
    /// key events to the active overlay (no resolver, no predict, no
    /// pane forwarding) and discovery actions push onto it.
    pub overlays: &'a mut OverlayState,
    /// Snapshot of the on-disk keybindings, captured at driver start.
    /// The action finder uses it to show each live chord. `None` when
    /// config load failed (rows then show as unbound).
    pub keybindings: Option<&'a phux_config::KeybindingsCfg>,
    /// phux-ahv.4: chrome + overlay color theme, resolved from
    /// `[theme]` config at driver start. Overlays snapshot it at
    /// construction (action finder, `rename-window`) so their painted
    /// colors flow from a single source of truth.
    pub theme: &'a Theme,
    /// phux-4li.20: the server's session graph, cached from the latest
    /// `ATTACHED` snapshot. The `session-picker` action builds its rows
    /// from this list. Empty until the first snapshot lands (the picker then
    /// still offers its new-session row).
    pub sessions: &'a [phux_protocol::wire::info::SessionInfo],
    /// phux-foz.8: peer sessions' persisted L3 workspaces, fetched by the
    /// driver right after ATTACH (one `GET_METADATA` per peer on the
    /// per-session layout key). The `<leader> w` window picker reads this
    /// to list a foreign session's windows as one-step jump rows
    /// (`switch-session { name, window }`); a session with no entry (no
    /// persisted layout, reply not landed yet, or created after attach)
    /// falls back to the plain "switch to this session" row. Attach-time
    /// snapshot — peers' later mutations are not tracked (the post-switch
    /// select degrades to a logged no-op if the index went stale).
    pub foreign_layouts: &'a HashMap<phux_protocol::ids::SessionId, Workspace>,
    /// phux-jpqd: the `phux.agent/v1` records the driver fetched for
    /// **foreign** panes — one one-shot `GET_METADATA` per `TerminalId` in a
    /// peer session's cached [`Self::foreign_layouts`] workspace, keyed by
    /// that terminal id. The `agent-fleet` dashboard reads this so a foreign
    /// session's pane rows show agent glyph/state without attaching there.
    /// Empty until a peer's layout lands and its per-pane replies arrive; a
    /// pane with no entry renders `?`/"no agent" (no live subscription, so
    /// no asked flag or cwd/branch).
    pub foreign_agents: &'a HashMap<TerminalId, crate::agent_meta::AgentRecord>,
    /// phux-4li.20: id of the session this client is attached to. The
    /// picker places this row first and marks it `current`; selecting it
    /// dismisses the picker without reattaching. `None` before the first
    /// snapshot.
    pub focused_session: Option<phux_protocol::ids::SessionId>,
    /// phux-eb0: the name of the session this client is attached to,
    /// resolved from the latest ATTACHED snapshot. A `switch-session`
    /// targeting this name without a window/pane target is a silent no-op
    /// (guarded in [`apply_action_effects`]). Empty before the first snapshot.
    ///
    /// Mutable so the `rename-session` action can optimistically update it
    /// the moment the user commits a rename: the client sends the
    /// `RENAME_SESSION` command and reflects the new name in its own status
    /// bar immediately, rather than waiting a round-trip. The server is
    /// authoritative — the next `ATTACHED` snapshot overwrites this with the
    /// server's value (and is how other attached clients learn the rename).
    pub session_name: &'a mut String,
    /// phux-eb0: out-channel for a committed `switch-session { name }`.
    /// `apply_action_effects` sets this to `Some(target)` when the user
    /// picks a peer session; the driver's `main_loop` reads it after the
    /// dispatch batch and returns `LoopExit::SwitchTo(target)` so the
    /// outer loop re-attaches. Cleared by the driver each iteration.
    pub switch_request: &'a mut Option<ReattachTarget>,
    /// phux-x2hm: the driver's pane-zoom state — `Some(id)` when pane `id`
    /// is zoomed to fill the window. `apply_action_effects` flips this for a
    /// `toggle-zoom` action; the driver reads it (via `Workspace::render_window`)
    /// to render/reflow the zoomed pane.
    pub zoomed: &'a mut Option<TerminalId>,
    /// phux-4h5a: the active sidebar reservation, or `None` when the sidebar is
    /// disabled. The `resize-pane` min-cell gate tiles into the inset content
    /// rect so the underflow check matches the width panes actually paint into
    /// when a sidebar is docked.
    pub sidebar: Option<SidebarReservation>,
    /// phux-4h5a: the driver's sidebar on/off state. `toggle-sidebar` flips
    /// this (via `ActionEffects::toggle_sidebar`); the driver re-folds it into
    /// the per-frame `sidebar` reservation after dispatch so the toggle repaint
    /// reflects the new state. Owned by the driver like `zoomed`.
    pub sidebar_enabled: &'a mut bool,
    /// The configured sidebar width in columns, whether or not the strip
    /// is currently shown. `toggle-sidebar` needs it to answer "would
    /// turning this on actually change anything at this terminal size?"
    /// before flipping a flag whose effect the driver would then fold
    /// away — see the `toggle-sidebar` arm of [`run_action`].
    pub sidebar_width: u16,
    /// phux-huhi: the attach's `[chrome]` breakpoints. `toggle-sidebar`
    /// consults [`ChromeBreakpoints::min_pane_cols`] for the same
    /// "would this actually change anything?" arithmetic the driver's
    /// [`sidebar_reservation`] fold uses, so the keypress and the layout
    /// cannot disagree about whether the strip fits.
    ///
    /// [`sidebar_reservation`]: crate::attach::paint::sidebar_reservation
    pub chrome: ChromeBreakpoints,
    /// phux-k0cw: the sidebar's click-resolution table for the frame on
    /// screen — the same one the strip painter rendered from
    /// ([`crate::render::chrome::sidebar::SidebarPainter::click_targets`]).
    /// It carries both the counts `hit_test` derives the row shape from and
    /// the per-row targets a queue or roster click commits. Default (all
    /// zero, no targets) in fixtures that don't exercise the sidebar.
    pub sidebar_targets: &'a crate::render::chrome::sidebar::SidebarTargets,
    /// The status bar's row reservation this frame (`None` when no bar;
    /// the painter's `Position` otherwise — phux-foz.8). Mouse routing
    /// folds this into the same `content_rect(viewport, bar, sidebar)` the
    /// paint path uses so a click hit-tests against the rects actually on
    /// screen, including the one-row downshift under a top-docked bar.
    pub bar: Option<crate::render::chrome::status_bar::Position>,
    /// phux-foz.12: the driver's status-bar painter, lent read-only so a
    /// click on the bar row can hit-test the window tabs against the
    /// exact strip the painter last painted
    /// ([`StatusBarPainter::window_hit_at`]). `None` when no bar is
    /// configured — `bar` is then `None` too and the row is not claimed —
    /// or in fixtures that don't exercise bar clicks (the row is still
    /// claimed as chrome; every click on it is a no-op).
    pub status_bar: Option<&'a crate::render::chrome::status_bar::StatusBarPainter>,
    /// ADR-0048: the in-flight divider drag, or `None` when no divider is
    /// grabbed. A press on a divider cell records the grabbed split here;
    /// subsequent button-motion events re-tune that split's ratio from the
    /// pointer position; a release clears it. Owned by `main_loop` (it
    /// must survive across dispatch batches) and threaded in by reference.
    pub drag: &'a mut Option<DragGrab>,
    /// phux-npb3 (ADR-0048 decision 3 follow-up): panes that opted out of
    /// client mouse handling via `set-pane mouse off`. Client-local state,
    /// owned by `main_loop` like `drag` and lent in by reference. Two
    /// consumers: the dispatcher skips synthesizing `INPUT_MOUSE` (and the
    /// local wheel-scroll) for an opted-out pane, and the driver drops the
    /// outer-terminal mouse-tracking DECSET whenever the focused pane is in
    /// this set — so the host terminal's raw mouse handling returns for that
    /// pane without forcing the whole session to `mouse = false`.
    pub mouse_optout: &'a mut std::collections::HashSet<TerminalId>,
    /// phux-oih5.16: driver-owned, client-local attention excursion state.
    /// The first `next-attention` saves an origin; later cycles preserve it,
    /// and `return-from-attention` consumes it. Never serialized or shared.
    pub attention_navigation: &'a mut AttentionNavigation,
    /// phux-r82.5: enabled plugins' manifest `[[actions]]`, snapshotted at
    /// driver start (same lifecycle as `keybindings`). The command palette
    /// appends one namespaced row per entry under a "Plugin" header.
    pub plugin_actions: &'a [PluginActionEntry],
    /// phux-r82.7: enabled plugins' hostable manifest `[[panes]]`
    /// (placement `split`/`tab`/`zoomed`; overlay is deferred), snapshotted
    /// at driver start alongside `plugin_actions`. The command palette
    /// appends one namespaced row per entry; a dispatched `plugin-pane`
    /// looks its argv + placement up here.
    pub plugin_panes: &'a [PluginPaneEntry],
    /// phux-r82.5: sender half of the driver's plugin-events channel. A
    /// dispatched `plugin-action` spawns the child-process run off the
    /// input loop and reports completion here; the driver's `select!`
    /// surfaces failures as a toast. `None` in unit tests (no runtime).
    pub plugin_tx: Option<&'a tokio::sync::mpsc::UnboundedSender<PluginRunResult>>,
    /// phux-foz.5: out-channel for a dispatched `reload-config`. Set by
    /// [`apply_action_effects`]; the driver reads it after the dispatch
    /// batch and re-runs the layered config loader, swapping its
    /// config-derived state in place (or keeping the old state and
    /// surfacing the error when the re-read fails). Same driver-owned
    /// out-channel shape as `switch_request` — the reload cannot happen
    /// inside dispatch because the resolver/theme/keybindings borrows in
    /// this ctx ARE the state being replaced.
    pub reload_request: &'a mut bool,
    /// phux-foz.7 / ADR-0040: the driver's decoded `phux.agent/v1` records
    /// (`AgentMetaIndex::records`), kept live by the per-pane metadata
    /// subscriptions. The `agent-fleet` action projects them into the
    /// dashboard rows.
    pub agent_meta: &'a HashMap<TerminalId, crate::agent_meta::AgentRecord>,
    /// phux-foz.7 / phux-p4vp: the driver's pane-cwd index + memoized
    /// branch cache. The fleet rows resolve each pane's branch through it
    /// (mut only for the memo).
    pub vcs: &'a mut crate::attach::pane_state::VcsIndex,
}

/// An active divider drag (ADR-0048).
///
/// Press on a divider cell records the controlling split (`node_path`) and
/// its `axis`; while held, each button-motion event sets that split's
/// ratio so the divider tracks the pointer; release drops it. The grab is
/// keyed by split identity, not by cursor cell, so a fast drag that
/// outruns the divider still re-tunes the right split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::attach) struct DragGrab {
    /// Path to the grabbed [`crate::layout::LayoutNode::Split`].
    pub node_path: crate::layout::NodePath,
    /// The grabbed split's axis (drives x vs y of the pointer).
    pub axis: SplitDir,
}
