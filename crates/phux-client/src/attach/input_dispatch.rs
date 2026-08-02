//! Input dispatcher: translates parser-emitted events into wire frames
//! or layout-action effects.
//!
//! Owns the resolver-intercept path (prefix chord → `ResolvedAction` →
//! mutate the active window of the `Workspace`), the predict overlay's
//! keystroke feed, and the parked-spawn bookkeeping (`PendingSplit` /
//! `PendingWindow`) that bridges a local `split-pane` / `new-window`
//! chord to its remote `SPAWN_TERMINAL` reply.

use std::collections::{HashMap, HashSet};

use libghostty_vt::terminal::{Mode, ScrollViewport};
use phux_protocol::TerminalId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::key::{ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::wire::frame::{
    Command, FrameKind, InputMode, SESSION_NAME_KEY, Scope, TerminalSignal,
};

use super::actions::{self, ActionError, PendingSplit, PendingWindow};
use super::connection::Connection;
use super::driver::{AttachError, AttentionNavigation, DEFAULT_GROUP_ID, PaneSlot, layout_key};
use super::focus::FocusHistory;
use super::input::make_named_key;
use super::paint::{SidebarReservation, content_rect};
use super::plugin_actions::{PluginActionEntry, PluginRunResult};
use super::plugin_panes::{HostedPlacement, PluginPaneEntry};
use crate::layout::{Direction, SplitDir, Workspace};
use crate::predict::{Overlay, PredictionState};
use crate::render::Theme;
use crate::render::overlay::{
    ContextMenu, HelpOverlay, OverlayOutcome, OverlayState, PromptOverlay, SelectItem, SelectList,
};

/// Mutable context the input-dispatch path needs to update on a chord
/// that resolves to a layout action (phux-4li.5). Bundles the items
/// that would otherwise inflate `dispatch_input_events`'s argument
/// list past clippy's threshold.
pub(super) struct DispatchCtx<'a> {
    /// Connection-owned engine replicas used for terminal queries and local scrolling.
    pub engine_kernel: &'a mut super::driver::AttachKernel,
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
    /// pane forwarding) and the `show-help` action pushes onto it.
    pub overlays: &'a mut OverlayState,
    /// phux-5ke.4: snapshot of the on-disk keybindings, captured at
    /// driver start. The help overlay reads this to render the modal
    /// body. `None` when config load failed (overlay still pushes but
    /// shows "no bindings configured").
    pub keybindings: Option<&'a phux_config::KeybindingsCfg>,
    /// phux-ahv.4: chrome + overlay color theme, resolved from
    /// `[theme]` config at driver start. Overlays snapshot it at
    /// construction (`show-help`, `rename-window`) so their painted
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
    /// phux-foz.9: the window index of each sidebar agents-section row, in
    /// display order — the same list the strip painter rendered from
    /// ([`crate::render::chrome::sidebar::SidebarPainter::agent_windows`]).
    /// `hit_test` needs it to resolve a click on an agent row to the
    /// window holding that agent's pane. Empty when the section is empty
    /// (or in fixtures that don't exercise the sidebar).
    pub sidebar_agents: &'a [usize],
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
    pub vcs: &'a mut super::driver::VcsIndex,
}

/// An active divider drag (ADR-0048).
///
/// Press on a divider cell records the controlling split (`node_path`) and
/// its `axis`; while held, each button-motion event sets that split's
/// ratio so the divider tracks the pointer; release drops it. The grab is
/// keyed by split identity, not by cursor cell, so a fast drag that
/// outruns the divider still re-tunes the right split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DragGrab {
    /// Path to the grabbed [`crate::layout::LayoutNode::Split`].
    pub node_path: crate::layout::NodePath,
    /// The grabbed split's axis (drives x vs y of the pointer).
    pub axis: SplitDir,
}

/// Translate a batch of parser events into wire frames and ship them.
///
/// Detach actions short-circuit into a single `FrameKind::Detach` and
/// flip `detach_pending`. Pre-attach events (no `focused_pane` yet) are
/// dropped with a debug log — the wire spec has no "pre-attach buffer"
/// notion.
///
/// phux-4li.5: when a `KeyEvent` matches a configured keybind, the
/// chord is consumed by the dispatcher and the corresponding layout
/// action runs (focus move / resize / etc.). The key is NOT forwarded
/// to the focused pane in that case — same convention as tmux's
/// `prefix` table.
// arg list bundles transport + render + predict context; follow-up to
// refactor into a context struct.
#[allow(clippy::too_many_arguments, reason = "see comment above")]
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
#[allow(
    clippy::too_many_lines,
    reason = "phux-4li.6 added the mouse-routing branch alongside resolver + predict + key forwarding; splitting would require carrying the connection + many mut locals through helpers"
)]
#[allow(
    clippy::cognitive_complexity,
    reason = "branch density rises with each input-event kind we route; same shape as the action-dispatch arm"
)]
pub(super) async fn dispatch_input_events<W: super::RenderSink>(
    out: &mut W,
    conn: &mut Connection,
    events: Vec<InputEvent>,
    focused_pane: &mut Option<TerminalId>,
    detach_pending: &mut bool,
    predict: &mut PredictionState,
    overlay: &Overlay,
    panes: &mut HashMap<TerminalId, PaneSlot>,
    ctx: &mut DispatchCtx<'_>,
) -> Result<bool, AttachError> {
    let mut predicted_any = false;
    let mut layout_changed = false;
    for ev in events {
        // phux-foz.2: the which-key popup is transparent to input. It is
        // dismissed by — and never consumes — the next event: a key press
        // pops it and then executes exactly as if the popup were absent
        // (the resolver still holds the pending prefix, so the chord
        // completes normally), except Esc, which pops it AND cancels the
        // pending prefix without reaching the pane. Mouse input pops it
        // and cancels the prefix too (a click is not a chord
        // continuation), then routes normally. Non-press key events and
        // paste/focus bypass the popup entirely (it stays up; they flow
        // to the pane) — the popup must never eat or delay real input.
        if ctx.overlays.top_is_passthrough() {
            use phux_protocol::input::key::{KeyAction, PhysicalKey};
            match &ev {
                InputEvent::Key(key_event) if matches!(key_event.action, KeyAction::Press) => {
                    ctx.overlays.dismiss();
                    layout_changed = true;
                    if key_event.key == PhysicalKey::Escape {
                        if let Some(resolver) = ctx.resolver.as_deref_mut() {
                            resolver.reset();
                        }
                        tracing::debug!("which-key: Esc cancelled the pending prefix");
                        continue;
                    }
                    // Fall through: the key executes as if no popup existed.
                }
                InputEvent::Mouse(_) => {
                    ctx.overlays.dismiss();
                    layout_changed = true;
                    if let Some(resolver) = ctx.resolver.as_deref_mut() {
                        resolver.reset();
                    }
                    // Fall through to normal mouse routing.
                }
                _ => {}
            }
        }
        // phux-5ke.4: while any overlay is active the stack captures all
        // input. Key events flow to `OverlayState::handle_key`, which
        // routes them to the *top* overlay (which may dismiss, popping
        // back to whatever is beneath it); mouse / paste / focus events
        // are dropped so they don't reach the pane underneath.
        //
        // The keybind resolver is bypassed entirely while an overlay is
        // up: the overlay owns every keystroke, exactly as tmux's command
        // prompt and menus consume the prefix key as literal input rather
        // than firing prefix bindings. This keeps a prefix chord (e.g. the
        // leader `C-a`) from being swallowed by the resolver before it can
        // reach the overlay — a name typed into the rename prompt that
        // starts with the leader key must land verbatim. Detach while a
        // modal is open is reachable by dismissing first (Esc), then
        // chording. The resolver is reset on entry so a partial chord begun
        // before the overlay opened cannot leak into post-dismiss input.
        //
        // phux-foz.2: a passthrough popup (which-key) is excluded — the
        // block above already dismissed it for presses/mouse, and events
        // it deliberately ignores (key release/repeat, paste, focus) must
        // flow to the pane, not be captured (and must NOT reset the
        // resolver, which is holding the pending prefix the popup shows).
        if ctx.overlays.is_active() && !ctx.overlays.top_is_passthrough() {
            if let InputEvent::Key(ref key_event) = ev {
                if let Some(resolver) = ctx.resolver.as_deref_mut() {
                    resolver.reset();
                }
                let was_active = ctx.overlays.is_active();
                // phux-ahv.1: an overlay may commit an action (e.g. the
                // rename prompt returning `rename-window { name }`); run
                // it through the same path as a keybinding.
                match ctx.overlays.handle_key(key_event) {
                    OverlayOutcome::RunAction(resolved) => {
                        let effects = run_action(&resolved, ctx, focused_pane.as_ref(), panes);
                        if apply_action_effects(
                            effects,
                            out,
                            conn,
                            ctx,
                            focused_pane,
                            detach_pending,
                            predict,
                            panes,
                        )
                        .await?
                        {
                            layout_changed = true;
                        }
                    }
                    OverlayOutcome::Copy(req) => {
                        // Copy-mode commit: resolve the selection against the
                        // focused pane's own engine and write it to the host
                        // clipboard via OSC 52. Client-local per ADR-0030 —
                        // no wire traffic.
                        if let Some(fid) = focused_pane.as_ref()
                            && let Some(terminal) =
                                super::driver::published_terminal(ctx.engine_kernel, fid)
                        {
                            super::copy::copy_to_host_clipboard(out, terminal, req)?;
                        }
                    }
                    OverlayOutcome::ScrollViewport(delta) => {
                        if scroll_focused_pane_viewport(
                            ctx.engine_kernel,
                            panes,
                            focused_pane.as_ref(),
                            delta,
                        ) {
                            layout_changed = true;
                        }
                    }
                    OverlayOutcome::None => {
                        // Overlay consumed the key but nothing else to do.
                    }
                }
                // On dismiss, repaint everything: the overlay scribbled
                // over pane cells and we need a coherent base for the
                // next TERMINAL_OUTPUT.
                if was_active && !ctx.overlays.is_active() {
                    layout_changed = true;
                }
            } else if let InputEvent::Mouse(ref mouse) = ev {
                // Copy-mode tracks pane-local cells but the parser emits
                // outer-viewport coordinates; translate into the focused
                // pane's frame so a drag over a non-origin pane highlights
                // the cells actually under the pointer. Modal overlays (the
                // only other mouse consumers) keep viewport coords.
                let routed = if ctx.overlays.copy_selection().is_some() {
                    let rect = focused_pane_rect(ctx, focused_pane.as_ref());
                    let mut m = *mouse;
                    m.x = (m.x - f64::from(rect.x)).max(0.0);
                    m.y = (m.y - f64::from(rect.y)).max(0.0);
                    m
                } else {
                    *mouse
                };
                let was_active = ctx.overlays.is_active();
                match ctx.overlays.handle_mouse(&routed) {
                    OverlayOutcome::Copy(req) => {
                        if let Some(fid) = focused_pane.as_ref()
                            && let Some(terminal) =
                                super::driver::published_terminal(ctx.engine_kernel, fid)
                        {
                            super::copy::copy_to_host_clipboard(out, terminal, req)?;
                        }
                        layout_changed = true;
                    }
                    OverlayOutcome::ScrollViewport(delta) => {
                        if scroll_focused_pane_viewport(
                            ctx.engine_kernel,
                            panes,
                            focused_pane.as_ref(),
                            delta,
                        ) {
                            layout_changed = true;
                        }
                    }
                    OverlayOutcome::RunAction(resolved) => {
                        let effects = run_action(&resolved, ctx, focused_pane.as_ref(), panes);
                        if apply_action_effects(
                            effects,
                            out,
                            conn,
                            ctx,
                            focused_pane,
                            detach_pending,
                            predict,
                            panes,
                        )
                        .await?
                        {
                            layout_changed = true;
                        }
                    }
                    OverlayOutcome::None => {}
                }
                // phux-wrnm: a pointer dismissal (clicking outside a context
                // menu) leaves the overlay's cells on screen with nothing
                // scheduled to erase them — the key path has always
                // repainted on dismiss; the mouse path never did, because
                // until now no overlay could be dismissed by a click.
                if was_active && !ctx.overlays.is_active() {
                    layout_changed = true;
                }
            }
            continue;
        }
        // phux-4li.5: resolver intercept. Run BEFORE the predict layer
        // so a chord that resolves to e.g. `focus-direction` doesn't
        // leave a stale ghost overlay on the previous focused pane.
        if let InputEvent::Key(ref key_event) = ev
            && let Some(outcome) = consume_chord(ctx, key_event)
        {
            match outcome {
                ChordOutcome::Partial => {
                    // Still waiting on the next chord in a multi-chord
                    // sequence; absorb the byte and move on.
                    continue;
                }
                ChordOutcome::Resolved(resolved) => {
                    let effects = run_action(&resolved, ctx, focused_pane.as_ref(), panes);
                    if apply_action_effects(
                        effects,
                        out,
                        conn,
                        ctx,
                        focused_pane,
                        detach_pending,
                        predict,
                        panes,
                    )
                    .await?
                    {
                        layout_changed = true;
                    }
                    continue;
                }
            }
        }
        // phux-4li.6 / ADR-0048: INPUT_MOUSE routing + click-to-focus +
        // divider drag-to-resize. The parser emits mouse coordinates in
        // outer-viewport cells (treated as 1-px-per-cell f64 per SPEC
        // §9.2.1); we hit-test against the multi-pane composition's
        // `Rect`s. A press on a divider cell *grabs* the split that
        // divider controls; button-motion while grabbed re-tunes the
        // split's ratio so the divider tracks the cursor; release drops
        // the grab. A click in a pane forwards the event (with pane-local
        // coords) to that pane — so an inner TUI that turned mouse
        // tracking on still receives every pointer event over its own
        // cells (the divider cells are the only ones whose meaning the
        // client claims).
        if let InputEvent::Mouse(ref mouse) = ev {
            use super::multi_pane::{RouteDecision, route_mouse_event};
            // ADR-0048: a release ALWAYS ends any in-flight drag first,
            // regardless of where it lands — the cursor may have left the
            // divider cell mid-drag. The commit broadcasts the final
            // layout via SET_METADATA, the same persistence path the
            // keyboard resize uses, so other attached clients converge. A
            // release with no active drag falls through to normal routing
            // (an inner app may want it).
            if matches!(mouse.action, MouseAction::Release) && ctx.drag.is_some() {
                *ctx.drag = None;
                if let Some(session) = ctx.focused_session
                    && let Some(bytes) = encode_layout_or_log(ctx.workspace)
                {
                    let request_id = *ctx.next_request_id;
                    *ctx.next_request_id = ctx.next_request_id.wrapping_add(1);
                    conn.send(&FrameKind::SetMetadata {
                        request_id,
                        scope: Scope::Group(DEFAULT_GROUP_ID),
                        key: layout_key(session),
                        value: bytes,
                    })
                    .await?;
                }
                tracing::debug!("divider drag: released, broadcast layout");
                continue;
            }
            // While a divider is grabbed, motion re-tunes that split and
            // nothing reaches a pane. Press/other actions fall through.
            if let Some(grab) = ctx.drag.clone()
                && matches!(mouse.action, MouseAction::Motion)
            {
                if drag_resize(ctx, mouse, &grab) {
                    layout_changed = true;
                }
                continue;
            }
            // phux-npb3 hardening (PR #142 review, recorded in ADR-0048):
            // while a divider drag is active, ONLY a release ends it and
            // ONLY motion re-tunes it — both handled above. Anything else
            // (notably a second Press from a chorded button, a wheel tick,
            // or a re-encoded press glitch) is consumed here so it cannot
            // fall through to normal routing mid-drag, where it would
            // forward to a pane, move focus, or grab a second divider while
            // the first grab is still live.
            if ctx.drag.is_some() {
                tracing::trace!(
                    action = ?mouse.action,
                    button = ?mouse.button,
                    "dropping mouse event during divider drag"
                );
                continue;
            }

            // phux-fce4: the sidebar strip claims every pointer event over
            // its own cells BEFORE pane routing — its rows are hit targets,
            // not pane content. A left press resolves against the strip's
            // row model (`sidebar::hit_test`) and dispatches the mapped
            // action through the same `run_action` path a keybinding or
            // palette row uses: a window block commits `select-window`, an
            // agents-section row (phux-foz.9) `select-window` for the
            // window holding that agent's pane, the `+ new` affordance
            // `new-window`, `= menu` the command palette (the
            // session/plugin menu), and the bottom-corner collapse chevron
            // `toggle-sidebar`. Everything else over the strip (motion,
            // non-left presses, headers, blank rows, the separator column)
            // is consumed and dropped so it can never leak into a pane
            // whose rect does not contain it anyway.
            if let Some(res) = ctx.sidebar {
                let strip = super::paint::sidebar_rect(ctx.viewport, res);
                let (cell_x, cell_y) = (quantize_cell(mouse.x), quantize_cell(mouse.y));
                if strip_contains(strip, cell_x, cell_y) {
                    let hit = sidebar_click_action(
                        strip,
                        ctx.workspace.windows.len(),
                        ctx.sidebar_agents,
                        cell_x,
                        cell_y,
                    );
                    if matches!(mouse.action, MouseAction::Press)
                        && mouse.button == MouseButton::Left
                        && let Some(resolved) = hit
                    {
                        tracing::debug!(action = %resolved.action, "sidebar: click dispatched");
                        let effects = run_action(&resolved, ctx, focused_pane.as_ref(), panes);
                        if apply_action_effects(
                            effects,
                            out,
                            conn,
                            ctx,
                            focused_pane,
                            detach_pending,
                            predict,
                            panes,
                        )
                        .await?
                        {
                            layout_changed = true;
                        }
                    } else if matches!(mouse.action, MouseAction::Press)
                        && mouse.button == MouseButton::Right
                    {
                        // phux-wrnm: a right press on a window block (or an
                        // agents-section row, which resolves to the window
                        // holding that agent) selects that window first —
                        // acting on what you pointed at is the whole promise
                        // of a context menu — and then opens the window menu
                        // for it. Every other cell of the strip is session
                        // chrome and gets the session menu, so a right-click
                        // anywhere on the sidebar does something useful.
                        let window_row = hit.filter(|r| r.action == "select-window");
                        let is_window = window_row.is_some();
                        if let Some(resolved) = window_row {
                            let effects = run_action(&resolved, ctx, focused_pane.as_ref(), panes);
                            if apply_action_effects(
                                effects,
                                out,
                                conn,
                                ctx,
                                focused_pane,
                                detach_pending,
                                predict,
                                panes,
                            )
                            .await?
                            {
                                layout_changed = true;
                            }
                        }
                        let spec = if is_window {
                            super::context_menu::window_menu(
                                ctx.keybindings,
                                &active_window_name(ctx),
                            )
                        } else {
                            super::context_menu::session_menu(ctx.keybindings, ctx.session_name)
                        };
                        open_context_menu(ctx, spec, (cell_x, cell_y));
                    }
                    continue;
                }
            }
            // phux-foz.12: the status-bar row is chrome, not pane content —
            // `content_rect` already excludes it, so every pointer event
            // here used to fall through to a Miss and get dropped. Claim
            // the row explicitly instead: a left press on a window tab
            // (resolved against the painter's cached strip, so the hit
            // targets are exactly the cells on screen) dispatches
            // `select-window { index }` through the same `run_action`
            // path the sidebar affordances and keybindings use. phux-qtw8:
            // the sidebar strip is full-height and claims its columns on
            // THIS row too — but it hit-tests first (above), so by here the
            // event is in the bar's own inset span and `window_hit_at`
            // (which indexes off the origin it painted at) resolves it.
            // Pane content is untouched — everything else on the row
            // (non-tab cells, motion, wheel, non-left buttons) is consumed
            // and dropped, matching the pre-claim behavior bit for bit.
            if let Some(pos) = ctx.bar {
                let bar_row = match pos {
                    crate::render::chrome::status_bar::Position::Bottom => {
                        ctx.viewport.1.saturating_sub(1)
                    }
                    crate::render::chrome::status_bar::Position::Top => 0,
                };
                let (cell_x, cell_y) = (quantize_cell(mouse.x), quantize_cell(mouse.y));
                if ctx.viewport.1 > 0 && cell_y == bar_row {
                    let hit = bar_click_action(ctx.status_bar, cell_x);
                    if matches!(mouse.action, MouseAction::Press)
                        && mouse.button == MouseButton::Left
                        && let Some(resolved) = hit
                    {
                        tracing::debug!(action = %resolved.action, "status bar: tab click dispatched");
                        let effects = run_action(&resolved, ctx, focused_pane.as_ref(), panes);
                        if apply_action_effects(
                            effects,
                            out,
                            conn,
                            ctx,
                            focused_pane,
                            detach_pending,
                            predict,
                            panes,
                        )
                        .await?
                        {
                            layout_changed = true;
                        }
                    } else if matches!(mouse.action, MouseAction::Press)
                        && mouse.button == MouseButton::Right
                    {
                        // phux-wrnm: right press on a tab selects that window
                        // (same as a left click) and opens its window menu;
                        // elsewhere on the bar — the session name, the
                        // widgets, the blank padding — the session menu. The
                        // menu is clamped into the content rect, so a
                        // bottom-docked bar opens it upward, over the panes.
                        let is_window = hit.is_some();
                        if let Some(resolved) = hit {
                            let effects = run_action(&resolved, ctx, focused_pane.as_ref(), panes);
                            if apply_action_effects(
                                effects,
                                out,
                                conn,
                                ctx,
                                focused_pane,
                                detach_pending,
                                predict,
                                panes,
                            )
                            .await?
                            {
                                layout_changed = true;
                            }
                        }
                        let spec = if is_window {
                            super::context_menu::window_menu(
                                ctx.keybindings,
                                &active_window_name(ctx),
                            )
                        } else {
                            super::context_menu::session_menu(ctx.keybindings, ctx.session_name)
                        };
                        open_context_menu(ctx, spec, (cell_x, cell_y));
                    }
                    continue;
                }
            }
            // Hit-test against the SAME inset content rect the renderer tiles
            // into — status-bar row and sidebar columns folded off the outer
            // viewport. Routing against the full viewport instead disagrees with
            // what is painted: a click near a divider lands one row off (the
            // status bar) and, with a sidebar docked, one strip-width off in x,
            // so it focuses/forwards to the wrong pane. Clicks in the reserved
            // chrome miss every pane rect and become a Miss (dropped).
            let content = content_rect(ctx.viewport, ctx.bar, ctx.sidebar);
            // phux-jow6: hit-test against the RENDER layout, not the real
            // tiled tree. When a pane is zoomed (phux-x2hm) the render layout
            // is a single full-content leaf, so any click lands on the
            // visible zoomed pane instead of whichever hidden tiled pane sits
            // under the cursor. Compute the decision in a scope that drops the
            // borrowing `Cow` before the click-to-focus `active_window_mut()`
            // below needs the workspace mutably.
            let decision = {
                let Some(render_ls) = ctx.workspace.render_window(ctx.zoomed.as_ref()) else {
                    tracing::debug!("dropping mouse event: no active window");
                    continue;
                };
                route_mouse_event(&render_ls, content, ctx.viewport, mouse)
            };
            match decision {
                RouteDecision::Pane {
                    target,
                    pane_x,
                    pane_y,
                    focus_changed,
                } => {
                    if focus_changed {
                        if let Some(ls) = ctx.workspace.active_window_mut() {
                            ls.focus = Some(target.clone());
                        }
                        apply_focus_transition(
                            &mut ctx.focus_history,
                            focused_pane,
                            target.clone(),
                        );
                        // Re-anchor predict to the clicked pane: drop the
                        // old pane's queue AND reset the cursor + viewport
                        // to the new pane, so a keystroke before the next
                        // reconcile echoes at the right place rather than
                        // the old pane's (mid-screen) coordinates (phux-7ry0).
                        super::driver::reanchor_predict_to_pane(predict, panes, &target);
                        // Heavy-edge chrome moves with focus; repaint
                        // dividers + all leaves so the focused pane's
                        // surrounding edges render heavy.
                        layout_changed = true;
                    }
                    // phux-npb3: a pane opted out via `set-pane mouse off`
                    // receives no client-synthesized mouse at all — no
                    // INPUT_MOUSE forward, no local wheel viewport scroll.
                    // Click-to-focus above still applies: it is chrome-level
                    // (the pane never sees it) and it is also the path that
                    // makes the driver drop outer capture once the opted-out
                    // pane is focused, restoring the host's raw handling.
                    if ctx.mouse_optout.contains(&target) {
                        tracing::trace!(
                            terminal = ?target,
                            "dropping mouse event: pane opted out (set-pane mouse off)"
                        );
                        continue;
                    }
                    let mut routed = *mouse;
                    routed.x = pane_x;
                    routed.y = pane_y;
                    if let Some(delta) = wheel_scroll_delta(&routed)
                        && let Some(terminal) =
                            super::driver::published_terminal(ctx.engine_kernel, &target)
                        && !terminal_wants_mouse_tracking(terminal)
                    {
                        // xterm "alternate scroll" (DECSET 1007, on by
                        // default in libghostty): the alt screen has no
                        // scrollback, so the viewport scroll below would be
                        // a silent no-op there and the wheel would go dead
                        // in any full-screen app that doesn't track the
                        // mouse (pagers, vim with mouse off). Convert each
                        // wheel notch into arrow-key presses instead — the
                        // same translation tmux and ghostty perform. Apps
                        // opt out with `?1007l` (phux-yyex).
                        if terminal_in_alt_screen(terminal) && terminal_alt_scroll(terminal) {
                            let arrow = make_named_key(
                                if delta < 0 {
                                    PhysicalKey::ArrowUp
                                } else {
                                    PhysicalKey::ArrowDown
                                },
                                ModSet::empty(),
                            );
                            for _ in 0..delta.unsigned_abs() {
                                conn.send(&FrameKind::InputKey {
                                    terminal_id: target.clone(),
                                    event: arrow.clone(),
                                })
                                .await?;
                            }
                            continue;
                        }
                        let scrolled = ctx
                            .engine_kernel
                            .published_engine_mut(&target)
                            .is_some_and(|replica| {
                                replica
                                    .scroll_viewport(ScrollViewport::Delta(delta))
                                    .is_ok()
                            });
                        if !scrolled {
                            continue;
                        }
                        if delta < 0
                            && let Some(slot) = panes.get_mut(&target)
                        {
                            slot.viewport_scrolled = true;
                        }
                        layout_changed = true;
                        continue;
                    }
                    // phux-wrnm (ADR-0058): a right press on a pane whose app
                    // has NOT enabled mouse tracking opens the pane context
                    // menu at the pointer. The gate is the same boundary
                    // drag-to-copy respects: an inner program that asked for
                    // the mouse (vim, htop, a TUI with its own right-click
                    // menu) keeps every button, and the keyboard-bindable
                    // `context-menu` action is the way in for those panes.
                    // Click-to-focus above has already run, so the menu acts
                    // on the pane you pointed at, not the one you left.
                    if matches!(mouse.action, MouseAction::Press)
                        && mouse.button == MouseButton::Right
                        && super::driver::published_terminal(ctx.engine_kernel, &target)
                            .is_some_and(|terminal| !terminal_wants_mouse_tracking(terminal))
                    {
                        let zoomed = ctx.zoomed.as_ref() == Some(&target);
                        let spec = super::context_menu::pane_menu(ctx.keybindings, zoomed);
                        open_context_menu(
                            ctx,
                            spec,
                            (quantize_cell(mouse.x), quantize_cell(mouse.y)),
                        );
                        continue;
                    }
                    // Drag-to-copy (tmux convention): a left press on a pane
                    // whose app has NOT enabled mouse tracking starts a
                    // copy-mode selection anchored at the click. Motion and
                    // release then route through the overlay branch above —
                    // release copies to the host clipboard (OSC 52) and
                    // dismisses; a click without drag just dismisses. Apps
                    // that DO track the mouse (vim, htop) keep receiving
                    // their events untouched.
                    if matches!(mouse.action, MouseAction::Press)
                        && mouse.button == MouseButton::Left
                        && super::driver::published_terminal(ctx.engine_kernel, &target)
                            .is_some_and(|terminal| !terminal_wants_mouse_tracking(terminal))
                    {
                        let rect = focused_pane_rect(ctx, focused_pane.as_ref());
                        ctx.overlays
                            .push(Box::new(crate::render::overlay::CopyModeOverlay::new(
                                0, 0, rect.w, rect.h,
                            )));
                        // Seed anchor + cursor from the (pane-local) press.
                        let _ = ctx.overlays.handle_mouse(&routed);
                        continue;
                    }
                    conn.send(&FrameKind::InputMouse {
                        terminal_id: target,
                        event: scale_to_surface_pixels(routed, ctx.cell_px),
                    })
                    .await?;
                    continue;
                }
                RouteDecision::Divider { node_path, axis } => {
                    // ADR-0048: a LEFT-button press on a divider starts a drag
                    // and immediately snaps the split to the press position (so
                    // a click-without-motion still nudges, matching the
                    // intuitive "grab here"). Scroll-wheel and right/middle
                    // presses encode as Press too, but landing on a 1-cell
                    // divider must not snap the split — those, and stray
                    // grab-less motions, are dropped (the divider gap has no
                    // pane to forward to).
                    if matches!(mouse.action, MouseAction::Press)
                        && mouse.button == MouseButton::Left
                    {
                        let grab = DragGrab { node_path, axis };
                        if drag_resize(ctx, mouse, &grab) {
                            layout_changed = true;
                        }
                        *ctx.drag = Some(grab);
                        tracing::debug!("divider drag: grabbed");
                    } else {
                        tracing::trace!(x = mouse.x, y = mouse.y, "dropping mouse on divider");
                    }
                    continue;
                }
                RouteDecision::Miss => {
                    tracing::trace!(x = mouse.x, y = mouse.y, "dropping mouse: no target");
                    continue;
                }
                RouteDecision::NoFocus => {
                    tracing::debug!("dropping mouse event before ATTACHED");
                    continue;
                }
            }
        }

        // A key press headed for the pane snaps a scrolled viewport back to
        // the live screen (tmux behavior). Without this, a wheel scroll into
        // scrollback pins the viewport there forever and the pane looks
        // frozen — new output (e.g. the shell prompt after a TUI app exits)
        // lands below the visible rows and never paints. Runs BEFORE the
        // predict peek so grid reads see the active area.
        if let InputEvent::Key(ref key_event) = ev
            && matches!(
                key_event.action,
                phux_protocol::input::key::KeyAction::Press
            )
            && snap_scrolled_viewport(
                ctx.engine_kernel,
                panes,
                ctx.workspace.active_window().and_then(|w| w.focus.as_ref()),
            )
        {
            layout_changed = true;
        }
        // Predictive echo only fires for key events; mouse / paste / focus
        // intentionally bypass the prediction layer (they target the
        // server's input model, not the visual grid). The branch is
        // skipped entirely when the config flag is off — `predict_key`
        // returns `Disabled` and no overlay paint is scheduled.
        //
        // Arrows over a known cell on the current line (phux-9gw.1.3)
        // need a grid peek to know the width of the grapheme they step
        // over; we hand `read_grapheme_at` to the predict layer so it
        // can refuse the prediction when the cell is blank.
        //
        // phux-4li.6: peek the focused pane's grid via the active
        // window's focus. The driver also mirrors that id into its
        // `focused_pane` local (server-frame handlers rely on it);
        // either reads the same TerminalId here.
        //
        // phux-51n6.1: proactive full-screen-app gate. When the focused
        // pane is on the alternate screen (vim/nvim, a pager, an agent
        // TUI), a printable key is a command the shell never echoes, so a
        // speculative insert would paint a ghost the server contradicts.
        // Predictive echo does nothing for app mode anyway — skip it here
        // rather than rely on the reactive auto-back-off to clean up after
        // the ghosts. The keystroke still travels upstream normally below.
        if let InputEvent::Key(key_event) = &ev
            && predict.is_enabled()
            && let Some(fid) = ctx.workspace.active_window().and_then(|w| w.focus.as_ref())
            && let Some(terminal) =
                super::driver::published_terminal(ctx.engine_kernel, fid)
            && !terminal_in_alt_screen(terminal)
            && let Some(slot) = panes.get_mut(fid)
        {
            use crate::predict::PredictionOutcome;
            let outcome = predict.predict_key_with_grid(key_event, |r, c| {
                slot.renderer
                    .read_grapheme_at(terminal, r, c)
                    .ok()
                    .flatten()
            });
            if matches!(outcome, PredictionOutcome::Predicted) {
                predicted_any = true;
            }
        }
        // phux-4li.6: INPUT_KEY / INPUT_FOCUS / INPUT_PASTE all target
        // the client's focused pane (per ADR-0019 decision 6). Focus
        // is canonically the active window's focus; the driver-side
        // `focused_pane` mirror stays in sync for the render path.
        // When focus is unset (pre-ATTACHED), drop the event with a
        // debug log instead of panicking — wave-A's "always Some
        // post-ATTACHED" invariant is enforced by the seed in
        // `handle_server_frame`, but a stray input race during
        // bootstrap shouldn't take the loop down.
        let Some(pane) = ctx.workspace.active_window().and_then(|w| w.focus.as_ref()) else {
            tracing::debug!("dropping input received before ATTACHED");
            continue;
        };
        // phux-foz.1: forwarding key/paste input to a pane answers (or at
        // least engages) its pending agent question, so clear its asked
        // attention flag. Focus/mouse events don't clear — merely looking
        // at a pane is not answering it. A real transition schedules the
        // chrome repaint via `layout_changed`.
        if matches!(ev, InputEvent::Key(_) | InputEvent::Paste(_))
            && super::driver::clear_attention_on_input(panes, pane)
        {
            layout_changed = true;
        }
        let frame = ev.into_frame(pane.clone());
        conn.send(&frame).await?;
    }
    // Paint the prediction overlay once per dispatch batch so a burst of
    // keystrokes produces a single positioned write run, not one per
    // event. The overlay is a no-op on an empty queue. Predictions are
    // pane-local; shift them by the focused pane's render origin so a
    // non-top-left pane echoes over its own cells (phux-7ry0).
    if predicted_any {
        let origin = ctx
            .workspace
            .active_window()
            .and_then(|w| w.focus.as_ref())
            .and_then(|fid| panes.get(fid))
            .map_or((0, 0), |s| s.renderer.last_origin());
        let _ = overlay.render(predict, origin, out);
    }
    // Hand the layout-mutation signal back to `main_loop`, which holds
    // the status-bar painter and session name needed for a proper full
    // frame. We never paint from here.
    Ok(layout_changed)
}

fn wheel_scroll_delta(mouse: &MouseEvent) -> Option<isize> {
    if mouse.action != MouseAction::Press {
        return None;
    }
    match mouse.button {
        MouseButton::Four => Some(-3),
        MouseButton::Five => Some(3),
        _ => None,
    }
}

/// Scale a pane-local CELL-coordinate mouse event to the Terminal-local
/// surface-space PIXELS the wire carries (SPEC input.md §3.1: cell-quantized
/// clients emit `cell_index x cell_size`). The dispatcher hit-tests and
/// routes in cells; this runs at the `INPUT_MOUSE` send boundary only, so
/// every local consumer (overlays, wheel branch, drag) keeps cell units.
/// Axes are clamped to 1px so a degenerate geometry can never zero out the
/// position (phux-yyex).
fn scale_to_surface_pixels(mut mouse: MouseEvent, cell_px: (u16, u16)) -> MouseEvent {
    mouse.x *= f64::from(cell_px.0.max(1));
    mouse.y *= f64::from(cell_px.1.max(1));
    mouse
}
fn terminal_wants_mouse_tracking(terminal: &libghostty_vt::Terminal<'_, '_>) -> bool {
    [
        Mode::X10_MOUSE,
        Mode::NORMAL_MOUSE,
        Mode::BUTTON_MOUSE,
        Mode::ANY_MOUSE,
    ]
    .into_iter()
    .any(|mode| terminal.mode(mode).unwrap_or(false))
}

/// Whether the pane's mirror has DECSET 1007 (xterm "alternate scroll")
/// active. libghostty defaults it ON — matching ghostty — so wheel-to-arrow
/// translation works out of the box for alt-screen apps without mouse
fn terminal_alt_scroll(terminal: &libghostty_vt::Terminal<'_, '_>) -> bool {
    terminal.mode(Mode::ALT_SCROLL).unwrap_or(false)
}

/// Whether the pane's mirror is on the alternate screen buffer — the
/// proactive "full-screen app mode" gate for predictive echo (phux-51n6.1).
///
/// A pane running vim/nvim, `less`, `htop`, a pager, or an agent TUI (Claude
/// Code, codex) switches to the alternate screen via DEC private mode `?1049h`
/// (or the legacy `?1047h` / `?47h`). On the alt screen a printable keystroke
/// is a *command*, not text the shell echoes — so a speculative local insert
/// would paint a ghost the server never confirms. Predictive echo is inert in
/// app mode anyway (the latency win is a shell-prompt phenomenon), so the
/// correct behaviour is to predict nothing there rather than lean on the
/// reactive [`PredictionState`] auto-back-off to clean up after up to
/// `BACKOFF_THRESHOLD` mispredicted ghosts. libghostty tracks each variant
/// independently and reports it via `terminal.mode()` (verified against a
/// `?1049h`/`?1047h` probe), the same query path the mouse-tracking and
fn terminal_in_alt_screen(terminal: &libghostty_vt::Terminal<'_, '_>) -> bool {
    [
        Mode::ALT_SCREEN_SAVE,
        Mode::ALT_SCREEN,
        Mode::ALT_SCREEN_LEGACY,
    ]
    .into_iter()
    .any(|mode| terminal.mode(mode).unwrap_or(false))
}
fn scroll_focused_pane_viewport(
    kernel: &mut super::driver::AttachKernel,
    panes: &mut HashMap<TerminalId, PaneSlot>,
    focused_pane: Option<&TerminalId>,
    delta: isize,
) -> bool {
    if delta == 0 {
        return false;
    }
    let Some(fid) = focused_pane else {
        return false;
    };
    let Some(slot) = panes.get_mut(fid) else {
        return false;
    };
    let Some(replica) = kernel.published_engine_mut(fid) else {
        return false;
    };
    if replica
        .scroll_viewport(ScrollViewport::Delta(delta))
        .is_err()
    {
        return false;
    }
    if delta < 0 {
        slot.viewport_scrolled = true;
    }
    true
}

/// Snap `focused_pane`'s viewport back to the live screen if a wheel /
/// copy-mode scroll left it pinned in scrollback. Returns `true` iff the
fn snap_scrolled_viewport(
    kernel: &mut super::driver::AttachKernel,
    panes: &mut HashMap<TerminalId, PaneSlot>,
    focused_pane: Option<&TerminalId>,
) -> bool {
    let Some((fid, slot)) =
        focused_pane.and_then(|fid| panes.get_mut(fid).map(|slot| (fid, slot)))
    else {
        return false;
    };
    if !slot.viewport_scrolled {
        return false;
    }
    let Some(replica) = kernel.published_engine_mut(fid) else {
        return false;
    };
    if replica.scroll_viewport(ScrollViewport::Bottom).is_err() {
        return false;
    }
    slot.viewport_scrolled = false;
    true
}

fn focused_pane_rect(
    ctx: &DispatchCtx<'_>,
    focused_pane: Option<&TerminalId>,
) -> crate::layout::Rect {
    focused_pane_rect_for(
        ctx.workspace,
        ctx.zoomed.as_ref(),
        focused_pane,
        ctx.viewport,
        ctx.bar,
        ctx.sidebar,
    )
}

pub(super) fn focused_pane_rect_for(
    workspace: &Workspace,
    zoomed: Option<&TerminalId>,
    focused_pane: Option<&TerminalId>,
    viewport: (u16, u16),
    bar: Option<crate::render::chrome::status_bar::Position>,
    sidebar: Option<SidebarReservation>,
) -> crate::layout::Rect {
    let content = content_rect(viewport, bar, sidebar);
    let Some(fid) = focused_pane else {
        return content;
    };
    workspace
        .render_window(zoomed)
        .and_then(|layout| {
            crate::multi_pane::compute_layout_in(&layout, content, viewport)
                .rects
                .get(fid)
                .copied()
        })
        .unwrap_or(content)
}

/// Apply one drag step: re-tune the grabbed split so its divider tracks
/// `mouse`, returning `true` iff the layout changed (the caller repaints).
///
/// A pure mutation of the active window — no wire I/O (the `SET_METADATA`
/// broadcast happens once on release). Reuses [`actions::apply_divider_resize`]
/// so the drag, the keybind resize, and the persisted layout all run the
/// same `MIN_PANE_CELL` floor + `clamp_ratio` math. The pointer is
/// quantised to an outer-viewport cell exactly as the hit-test does.
/// `Ok(None)` from the resize (min-cell floor hit, or a stale grab whose
/// split the layout no longer has) leaves the layout untouched: the drag
/// stalls at the floor rather than collapsing a pane.
fn drag_resize(ctx: &mut DispatchCtx<'_>, mouse: &MouseEvent, grab: &DragGrab) -> bool {
    // Snapshot the geometry that feeds the resize before borrowing the
    // workspace mutably for the active window.
    let viewport = ctx.viewport;
    let bar = ctx.bar;
    let sidebar = ctx.sidebar;
    let Some(ls) = ctx.workspace.active_window_mut() else {
        return false;
    };
    let pointer = (quantize_cell(mouse.x), quantize_cell(mouse.y));
    match actions::apply_divider_resize(
        ls,
        &grab.node_path,
        grab.axis,
        pointer,
        viewport,
        bar,
        sidebar,
    ) {
        Ok(Some(new_state)) => {
            *ls = new_state;
            true
        }
        // Min-cell floor or stale grab — keep the divider where it is.
        Ok(None) | Err(_) => false,
    }
}

/// phux-fce4: whether an outer-viewport cell lies within the sidebar
/// strip's rect (separator column included — the strip consumes it even
/// though it is not a hit target).
const fn strip_contains(rect: crate::layout::Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.w)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.h)
}

/// phux-fce4: map a left press on the sidebar strip to the action it
/// commits, or `None` when it lands on a header, blank row, or the
/// separator.
///
/// The mapping goes through [`ResolvedAction`] so a sidebar click runs
/// exactly what a keybinding, palette row, or overlay commit would — one
/// dispatch path, no bespoke click semantics:
///
/// * a window block (name or branch row) commits `select-window { index }`;
/// * an agents-section row (phux-foz.9) commits `select-window` for the
///   window holding that agent's pane (`agent_windows` carries the
///   row-to-window mapping);
/// * `+ new` commits `new-window` (the strip lists windows, so its create
///   affordance creates one);
/// * `= menu` commits `command-palette` — the menu covering window,
///   session (`new-session` included), and plugin actions via the action
///   registry;
/// * the collapse chevron in the bottom corner (phux-foz.9) commits
///   `toggle-sidebar`.
fn sidebar_click_action(
    strip: crate::layout::Rect,
    window_count: usize,
    agent_windows: &[usize],
    x: u16,
    y: u16,
) -> Option<phux_config::keybind::ResolvedAction> {
    use crate::render::chrome::sidebar::{SidebarHit, hit_test};
    let (action, args) = match hit_test(strip, window_count, agent_windows, x, y)? {
        SidebarHit::Window(i) => {
            let mut args = std::collections::BTreeMap::new();
            args.insert(
                "index".to_owned(),
                toml::Value::Integer(i64::try_from(i).ok()?),
            );
            ("select-window", args)
        }
        SidebarHit::NewWindow => ("new-window", std::collections::BTreeMap::new()),
        SidebarHit::Menu => ("command-palette", std::collections::BTreeMap::new()),
        SidebarHit::Collapse => ("toggle-sidebar", std::collections::BTreeMap::new()),
    };
    Some(phux_config::keybind::ResolvedAction {
        action: action.to_owned(),
        args,
    })
}

/// phux-foz.12: map a left press on the status-bar row to the action it
/// commits, or `None` when it lands on a non-tab cell (separator, another
/// widget, blank padding) or no painter/strip is available.
///
/// Same shape as [`sidebar_click_action`]: the mapping goes through
/// [`phux_config::keybind::ResolvedAction`] so a tab click runs exactly
/// what a keybinding, palette row, or sidebar click would — one dispatch
/// path, no bespoke click semantics. A window tab commits
/// `select-window { index }`; the hit test itself lives with the painter
/// ([`crate::render::chrome::status_bar::StatusBarPainter::window_hit_at`])
/// so paint and click targets derive from the same composed strip.
fn bar_click_action(
    painter: Option<&crate::render::chrome::status_bar::StatusBarPainter>,
    x: u16,
) -> Option<phux_config::keybind::ResolvedAction> {
    let index = painter?.window_hit_at(x)?;
    let mut args = std::collections::BTreeMap::new();
    args.insert(
        "index".to_owned(),
        toml::Value::Integer(i64::try_from(index).ok()?),
    );
    Some(phux_config::keybind::ResolvedAction {
        action: "select-window".to_owned(),
        args,
    })
}

/// phux-wrnm: push `spec` as a context menu anchored at the viewport cell
/// `anchor` (ADR-0058).
///
/// The menu is clamped inside the pane content rect — the same rect the
/// panes tile into and centered modals are placed against — so it can
/// never occlude the sidebar strip or the status-bar row, including when
/// the click that opened it landed on that chrome.
fn open_context_menu(
    ctx: &mut DispatchCtx<'_>,
    spec: super::context_menu::MenuSpec,
    anchor: (u16, u16),
) {
    let area = content_rect(ctx.viewport, ctx.bar, ctx.sidebar);
    tracing::debug!(
        title = %spec.title,
        rows = spec.rows.len(),
        anchor_x = anchor.0,
        anchor_y = anchor.1,
        "context menu: opened",
    );
    ctx.overlays.push(Box::new(ContextMenu::new(
        spec.title, spec.rows, anchor, area, ctx.theme,
    )));
}

/// The active window's name, or an empty string when the workspace has no
/// windows yet. Used as the window menu's title.
fn active_window_name(ctx: &DispatchCtx<'_>) -> String {
    ctx.workspace
        .windows
        .get(ctx.workspace.active)
        .map_or_else(String::new, |w| w.name.clone())
}

/// Quantise an f64 pointer position (1-px-per-cell per SPEC §9.2.1) to an
/// outer-viewport cell, saturating into `u16` like the mouse hit-test.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "cell-quantised SGR/X10 input; saturate to keep malformed peers from breaking routing"
)]
fn quantize_cell(p: f64) -> u16 {
    if p.is_nan() || p < 0.0 {
        0
    } else if p >= f64::from(u16::MAX) {
        u16::MAX
    } else {
        p as u16
    }
}

/// Apply a client-local focus change through the single MRU transition path.
fn apply_focus_transition(
    history: &mut FocusHistory,
    focused_pane: &mut Option<TerminalId>,
    target: TerminalId,
) {
    history.transition(focused_pane, Some(target));
}

/// Apply the side-effects of a resolved action: layout-mutation repaint
/// signal, focus move, prediction reset, `SET_METADATA` broadcast, bell,
/// detach, parked spawn (split / new-window), and kill-frame sequences.
///
/// Shared by the keybinding path and the overlay-commit path (phux-ahv.1)
/// so a rename committed from the prompt broadcasts and repaints exactly
/// like a keybinding would. Returns `true` if the layout changed (the
/// caller repaints).
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
#[allow(
    clippy::too_many_arguments,
    reason = "shares the dispatch loop's transport + render + predict context; phux-7ry0 added the focused-pane map for the predict re-anchor"
)]
#[allow(
    clippy::cognitive_complexity,
    reason = "flat per-effect dispatch — one guarded block per ActionEffects field (zoom, focus, metadata, bell, detach, spawn, kill); splitting would thread the same transport + render context through helpers"
)]
#[allow(
    clippy::too_many_lines,
    reason = "same flat per-effect dispatch; phux-r82.5 added the plugin-run spawn block"
)]
async fn apply_action_effects<W: super::RenderSink>(
    effects: ActionEffects,
    out: &mut W,
    conn: &mut Connection,
    ctx: &mut DispatchCtx<'_>,
    focused_pane: &mut Option<TerminalId>,
    detach_pending: &mut bool,
    predict: &mut PredictionState,
    panes: &HashMap<TerminalId, PaneSlot>,
) -> Result<bool, AttachError> {
    let layout_changed = effects.layout_mutated;
    if effects.toggle_zoom {
        // phux-x2hm: flip pane-zoom. Un-zoom if zoomed; otherwise zoom the
        // focused pane. `run_action` already gated single-pane windows.
        *ctx.zoomed = if ctx.zoomed.is_some() {
            None
        } else {
            focused_pane.clone()
        };
    }
    if effects.toggle_sidebar {
        // phux-4h5a: flip the window-sidebar on/off state. The driver re-folds
        // `sidebar_enabled` into the per-frame reservation after dispatch, so
        // the `layout_mutated` repaint tiles into the new content rect.
        *ctx.sidebar_enabled = !*ctx.sidebar_enabled;
    }
    if let Some(target) = effects.set_focus {
        apply_focus_transition(&mut ctx.focus_history, focused_pane, target);
        // Focus moved (keybinding pane navigation) — re-anchor predict to
        // the new pane: reset its cursor + viewport and drop the old pane's
        // queue, so a keystroke before the next reconcile echoes at the
        // right place rather than the old pane's (mid-screen) coordinates
        // (phux-7ry0). Subsumes the plain `clear_predict` drop below.
        if let Some(fid) = focused_pane.as_ref() {
            super::driver::reanchor_predict_to_pane(predict, panes, fid);
        }
    } else if effects.clear_predict {
        predict.clear();
    }
    if effects.set_metadata
        && let Some(session) = ctx.focused_session
    {
        // Encoding can fail only on an empty workspace (we just produced
        // it — shouldn't happen), but propagate cleanly if it ever does.
        // phux-jy4t: keyed per session so a split here persists to THIS
        // session's layout, not a key every session shares.
        if let Some(bytes) = encode_layout_or_log(ctx.workspace) {
            let request_id = *ctx.next_request_id;
            *ctx.next_request_id = ctx.next_request_id.wrapping_add(1);
            conn.send(&FrameKind::SetMetadata {
                request_id,
                scope: Scope::Group(DEFAULT_GROUP_ID),
                key: layout_key(session),
                value: bytes,
            })
            .await?;
        }
    }
    if effects.bell {
        let _ = actions::write_bell(out);
    }
    if effects.detach && !*detach_pending {
        conn.send(&FrameKind::Detach).await?;
        *detach_pending = true;
    }
    // Parked split — send the SPAWN_TERMINAL and remember the intent.
    if let Some((request_id, pending, frame)) = effects.spawn_terminal {
        ctx.pending_splits.insert(request_id, pending);
        conn.send(&frame).await?;
    }
    // Parked new-window — same SPAWN flow; the reply opens a window.
    if let Some((request_id, pending, frame)) = effects.spawn_window {
        ctx.pending_windows.insert(request_id, pending);
        conn.send(&frame).await?;
    }
    // kill-pane / kill-window keystroke sequences; the TERMINAL_CLOSED
    // fold-out happens when each shell exits. Park the targets FIRST
    // (phux-i0e8.2.2): once the frames are on the wire the close can
    // race back, and an unmarked close would notice-spam the user about
    // a death they ordered.
    ctx.expected_closes.extend(effects.expected_closes);
    for frame in effects.kill_frames {
        conn.send(&frame).await?;
    }
    // ADR-0033: supervisory COMMAND frames (take/give the wheel, signal the
    // pane). Fire-and-forward — the server's COMMAND_RESULT + TerminalControl
    // broadcast drive the chrome; the input loop does not block on the reply.
    for frame in effects.command_frames {
        conn.send(&frame).await?;
    }
    // phux-r82.5: a plugin action runs as a spawned child-process task —
    // fire-and-forget from the input loop's perspective. The driver's
    // `select!` picks up the completion report and toasts failures. All
    // client-local (config + exec); nothing goes on the wire (ADR-0017).
    if let Some((plugin_id, action_id)) = effects.run_plugin {
        if let Some(tx) = ctx.plugin_tx {
            super::plugin_actions::spawn_plugin_action(tx.clone(), plugin_id, action_id);
        } else {
            tracing::warn!(
                plugin = %plugin_id,
                action = %action_id,
                "plugin-action dispatched with no plugin runtime channel; dropping",
            );
        }
    }
    // phux-foz.5: hand a `reload-config` up to the driver, which owns the
    // config-derived state (resolver, theme, keybindings snapshot, status
    // bar) this batch is still borrowing.
    if effects.reload_config {
        *ctx.reload_request = true;
    }
    // phux-eb0 / new-session: an in-process re-attach request. Hand the
    // target up to the driver via `ctx.switch_request`; `main_loop` reads
    // it after this dispatch batch and returns a `SwitchTo` exit so the
    // outer loop tears down the current session and re-attaches.
    //
    // Switching to the CURRENT session without a window/pane target is a
    // silent no-op. The session picker includes that row for orientation;
    // committing it has already dismissed the overlay, so no reattach is
    // needed. `new-session` is never a no-op — naming an existing session
    // just attaches to it.
    if let Some(target) = effects.reattach {
        match target {
            ReattachTarget::Existing {
                name,
                window: None,
                pane: None,
            } if &name == ctx.session_name => {
                tracing::debug!(target_session = %name, "switch-session to current session; no-op");
            }
            ReattachTarget::Existing { name, window, pane } => {
                tracing::info!(target_session = %name, target_window = ?window, target_pane = ?pane, "switch-session requested");
                *ctx.switch_request = Some(ReattachTarget::Existing { name, window, pane });
            }
            ReattachTarget::Create(name) => {
                tracing::info!(session = %name, "new-session requested");
                *ctx.switch_request = Some(ReattachTarget::Create(name));
            }
        }
    }
    // rename-session: since the v0.3.0 "Option B" re-tier (ADR-0019 /
    // ADR-0027) removed the RENAME_SESSION verb, a rename is a SET_METADATA
    // write of the conventional SESSION_NAME_KEY (Scope::Global, value
    // `current\0new`); the server intercepts it and applies the registry
    // rename. We optimistically reflect the new name locally — the server is
    // authoritative, and the next ATTACHED snapshot overwrites `session_name`
    // (also how other attached clients learn the rename; a live
    // SESSION_RENAMED push is out of scope for this pass). A no-op rename
    // (new == current) is dropped: nothing to send, nothing to repaint.
    let renamed = if let Some(new_name) = effects.rename_session.filter(|n| n != &*ctx.session_name)
    {
        let request_id = *ctx.next_request_id;
        *ctx.next_request_id = ctx.next_request_id.wrapping_add(1);
        let mut value = ctx.session_name.as_bytes().to_vec();
        value.push(0);
        value.extend_from_slice(new_name.as_bytes());
        conn.send(&FrameKind::SetMetadata {
            request_id,
            scope: Scope::Global,
            key: SESSION_NAME_KEY.to_owned(),
            value,
        })
        .await?;
        tracing::info!(new_name = %new_name, "rename-session sent; optimistically updating local name");
        *ctx.session_name = new_name;
        true
    } else {
        false
    };
    // A rename repaints the status bar (it carries the session name) the
    // same way a layout mutation does; fold it into the caller's repaint
    // signal so the new name shows immediately.
    Ok(layout_changed || renamed)
}

/// Result of feeding a key event through the resolver.
enum ChordOutcome {
    /// Chord extended a partial sequence; absorb and wait.
    Partial,
    /// Chord completed a binding; effects follow.
    Resolved(phux_config::keybind::ResolvedAction),
}

/// Convert a `KeyEvent` into a `KeyChord` and feed the resolver. Returns
/// `None` when the resolver is disabled (no config) or the chord
/// doesn't match any binding — caller forwards normally in that case.
///
/// Release / repeat events are NOT fed to the resolver — chord matching
/// is press-only, matching the convention of `phux-config::keybind`'s
/// tests and tmux's prefix table. Repeats of held keys (e.g. arrow keys
/// scrolling) would otherwise re-fire actions per-tick.
fn consume_chord(
    ctx: &mut DispatchCtx<'_>,
    key_event: &phux_protocol::input::key::KeyEvent,
) -> Option<ChordOutcome> {
    use phux_protocol::input::key::KeyAction;
    let resolver = ctx.resolver.as_deref_mut()?;
    if !matches!(key_event.action, KeyAction::Press) {
        return None;
    }
    let chord = phux_config::keybind::KeyChord {
        modifiers: key_event.mods,
        key: key_event.key,
    };
    match resolver.feed(chord) {
        phux_config::keybind::Feed::NoMatch => None,
        phux_config::keybind::Feed::Partial => {
            // Mid-chord: the user is partway through a multi-chord binding.
            // Debug (not info) — chord progress is finer-grained than the
            // resolved-action lifecycle event in `run_action`.
            tracing::debug!("chord: partial match, awaiting next chord");
            Some(ChordOutcome::Partial)
        }
        phux_config::keybind::Feed::Resolved(r) => {
            tracing::debug!(action = %r.action, "chord: resolved to action");
            Some(ChordOutcome::Resolved(r))
        }
    }
}

/// Side-effects a resolved action wants from the driver.
#[derive(Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "action dispatcher returns independent side-effect flags to keep async I/O outside run_action"
)]
struct ActionEffects {
    /// `true` ⇒ the active window was mutated in-place; driver repaints.
    layout_mutated: bool,
    /// phux-x2hm: `true` ⇒ flip the driver's pane-zoom state (zoom the
    /// focused pane to fill the window, or un-zoom). `apply_action_effects`
    /// owns the actual toggle since the `zoomed` state lives in the driver.
    toggle_zoom: bool,
    /// phux-4h5a: `true` ⇒ flip the driver's window-sidebar on/off state.
    /// `apply_action_effects` owns the toggle since `sidebar_enabled` lives in
    /// the driver; it also sets `layout_mutated` so the panes reflow into (or
    /// out of) the sidebar's reserved columns on the same-iteration repaint.
    toggle_sidebar: bool,
    /// `Some(new_focus)` ⇒ swap the driver's `focused_pane` (input
    /// routing follows). The action helper already updated the active
    /// window's focus; this carries the new id so the driver
    /// doesn't have to re-read it.
    set_focus: Option<TerminalId>,
    /// `true` ⇒ emit `SET_METADATA` carrying the new layout envelope.
    set_metadata: bool,
    /// `true` ⇒ emit a terminal bell (BEL `\x07`).
    bell: bool,
    /// phux-4li.16: `true` ⇒ the active window changed; the driver must
    /// drop the prediction queue (anchored to the old window's focused
    /// pane) so a stale ghost echo doesn't paint into the new window
    /// before the next `TERMINAL_OUTPUT` reconciles.
    clear_predict: bool,
    /// `true` ⇒ emit `DETACH` and wait for `DETACHED`.
    detach: bool,
    /// phux-4li.12: a `split-pane` action emitted a `SPAWN_TERMINAL`
    /// and parked a [`PendingSplit`] keyed by `request_id`. The async
    /// caller sends the frame, then inserts the parked entry into the
    /// driver-wide `pending_splits` map.
    spawn_terminal: Option<(u32, PendingSplit, FrameKind)>,
    /// phux-4li.15: a `new-window` action emitted a `SPAWN_TERMINAL` and
    /// parked a [`PendingWindow`] keyed by `request_id`. The async caller
    /// sends the frame and inserts the parked entry into the driver-wide
    /// `pending_windows` map; the reply opens a new window on the
    /// spawned pane.
    spawn_window: Option<(u32, PendingWindow, FrameKind)>,
    /// phux-4li.12: a `kill-pane` action ships a sequence of frames to
    /// the focused Terminal (the "soft-kill via shell-exit" — see
    /// `run_action`). The async caller sends them in order; the
    /// resulting `TERMINAL_CLOSED` from the server folds the pane out
    /// of the layout in [`crate::attach::server_frame::handle_server_frame`].
    kill_frames: Vec<FrameKind>,
    /// phux-i0e8.2.2: the Terminals `kill_frames` targets. The async
    /// caller parks them in `DispatchCtx::expected_closes` so the
    /// eventual `TERMINAL_CLOSED` is recognized as client-initiated and
    /// its pane-exit notice suppressed.
    expected_closes: Vec<TerminalId>,
    /// ADR-0033: supervisory commands (`ACQUIRE_INPUT` / `RELEASE_INPUT` /
    /// `SIGNAL_TERMINAL`) the `take-input` / `give-input` / `signal-terminal`
    /// actions built for the focused pane. The async caller sends each as a
    /// `COMMAND` frame in order; the server's `TerminalControl` broadcast (which
    /// we subscribed to at attach) drives the chrome update on the way back.
    command_frames: Vec<FrameKind>,
    /// phux-4li.20 / phux-eb0 / new-session: an in-process re-attach the
    /// driver should perform after this batch — either switch to an
    /// existing session or create a new one. [`apply_action_effects`]
    /// hands it up via `DispatchCtx::switch_request`; the driver's
    /// `main_loop` returns a `SwitchTo` exit and the outer loop detaches
    /// and re-attaches on the same connection. An `Existing` request
    /// matching the current session without a window/pane target is a silent
    /// no-op (the session picker uses that row to dismiss in place).
    reattach: Option<ReattachTarget>,
    /// rename-session: a committed rename. Carries the new name. The async
    /// caller ([`apply_action_effects`]) sends a `RENAME_SESSION` command
    /// for the *current* session over the existing connection and
    /// optimistically updates the client's own cached `session_name` +
    /// repaints its status bar. The server is authoritative: the next
    /// `ATTACHED` snapshot reconciles the name (and is how other attached
    /// clients learn of it — a live `SESSION_RENAMED` push is out of scope
    /// for this pass). A refusal (unknown session / name taken) arrives as a
    /// `COMMAND_RESULT { Error }`; this pass logs it and lets the next
    /// snapshot correct the optimistic name rather than blocking the input
    /// loop on the reply.
    rename_session: Option<String>,
    /// phux-r82.5: a `plugin-action` dispatch carrying
    /// `(plugin_id, action_id)`. The async caller
    /// ([`apply_action_effects`]) spawns the child-process run via
    /// [`super::plugin_actions::spawn_plugin_action`] so the input loop
    /// never blocks on the plugin; completion lands on the driver's
    /// plugin-events channel (failure output toasts there).
    run_plugin: Option<(String, String)>,
    /// phux-foz.5: `true` ⇒ the user asked for a live config reload
    /// (`reload-config`, via palette or a bound chord). Carried up to the
    /// driver via `DispatchCtx::reload_request`; the driver re-runs the
    /// layered loader after this batch and swaps its config-derived
    /// state atomically (old config kept on any failure).
    reload_config: bool,
}

/// An in-process re-attach request raised by a dispatched action.
///
/// Produced by `switch-session` / `new-session` (phux-eb0) and carried up
/// to the driver via `DispatchCtx::switch_request`; `main_loop` returns a
/// `SwitchTo` exit and the outer loop detaches and re-attaches on the same
/// connection without dropping the transport or leaving raw mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReattachTarget {
    /// Switch to an existing session by name (`switch-session`).
    Existing {
        /// Target session name.
        name: String,
        /// phux-foz.8: window index to select once the target session's
        /// persisted layout loads — the one-step cross-session window
        /// pick. `None` keeps the session's own remembered focus. The
        /// index addresses the target's L3 workspace (the same order its
        /// own window picker shows); if the layout changed under us and
        /// the index is out of range, the switch still lands and the
        /// select is a logged no-op.
        window: Option<usize>,
        /// phux-jpqd: DFS leaf ordinal within `window` to focus once the
        /// target's layout loads — the one-step cross-session **pane**
        /// pick the agent-fleet dashboard's foreign rows carry. `None`
        /// keeps the window's own restored focus. Applied only after
        /// `window` resolves in range; an out-of-range ordinal degrades to
        /// a logged no-op, same as `window`.
        pane: Option<usize>,
    },
    /// Create — or attach to, if it already exists — a session by name
    /// (`new-session`).
    Create(String),
}

/// Canonical names of every action `run_action` handles.
///
/// The list itself lives in [`phux_config::vocab`] (phux-i0e8.3.1) so
/// `phux config check` can validate against it; this re-export keeps the
/// dispatcher-side path working. The command-palette registry
/// ([`super::action_registry::REGISTRY`]) is checked against this list by
/// a unit test so the two cannot drift: adding a `run_action` arm without
/// adding it to the vocab (and to the registry) fails CI. Keep the vocab
/// list in sync with the `match resolved.action.as_str()` arms below —
/// they are the same set by construction, and the test enforces it.
pub use phux_config::vocab::ACTION_NAMES;

/// Dispatch a resolved action against the driver's context.
///
/// Returns the [`ActionEffects`] the caller needs to apply. The function
/// is sync: it never touches the connection — frame I/O happens in the
/// caller (`dispatch_input_events`) so a hypothetical async wire-send
/// failure doesn't leave layout state half-mutated.
#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "per-action arms accrete one-by-one; splitting into per-action helpers would obscure the central dispatch table"
)]
fn run_action(
    resolved: &phux_config::keybind::ResolvedAction,
    ctx: &mut DispatchCtx<'_>,
    focused: Option<&TerminalId>,
    // phux-foz.7: read-only view of the live pane slots. The `agent-fleet`
    // arm snapshots each pane's asked flag / OSC title / cwd from it;
    // every other arm ignores it. Threaded as a parameter (not a ctx
    // field) because the driver also passes `panes` mutably alongside the
    // ctx into `dispatch_input_events`.
    panes: &HashMap<TerminalId, PaneSlot>,
) -> ActionEffects {
    // One event per resolved action the user triggered. Info level: a
    // keybinding firing is a user-lifecycle event a trace reader wants under
    // the default filter, and it is human-paced (not per-frame), so it costs
    // nothing meaningful on the hot path. The action name is the key field;
    // any render-triggering effect is captured by the resulting repaint /
    // frame spans downstream.
    tracing::info!(action = %resolved.action, "input: running resolved action");
    let mut effects = ActionEffects::default();
    match resolved.action.as_str() {
        "split-pane" => {
            // phux-4li.12: SPAWN_TERMINAL → server allocates the new
            // Terminal under DEFAULT_GROUP_ID and replies with
            // TERMINAL_SPAWNED { request_id, result: Ok(new_id) }. The
            // layout mutation happens in the reply handler — see
            // `handle_server_frame`'s TerminalSpawned arm and
            // `apply_spawned_ok`. We park a `PendingSplit` keyed by
            // request id so the reply knows which leaf to split.
            let Some(dir) = split_dir_arg(resolved) else {
                tracing::warn!(
                    args = ?resolved.args,
                    "split-pane missing/bad `direction` arg (expected horizontal|vertical)",
                );
                effects.bell = true;
                return effects;
            };
            let Some(focused_id) = focused.cloned() else {
                tracing::warn!("split-pane: no focused pane to split against; dropping action");
                effects.bell = true;
                return effects;
            };
            let request_id = *ctx.next_request_id;
            *ctx.next_request_id = ctx.next_request_id.wrapping_add(1);
            // CWD inheritance is phux-4li.1; until then we let the
            // server pick (typically $HOME). `command = None` invokes
            // the server's default shell; `env = None` inherits the
            // server's environment as-is.
            let frame = FrameKind::SpawnTerminal {
                request_id,
                group: DEFAULT_GROUP_ID,
                command: None,
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
            };
            effects.spawn_terminal = Some((
                request_id,
                PendingSplit {
                    focused_at_request: focused_id,
                    dir,
                    zoom_on_spawn: false,
                },
                frame,
            ));
        }
        "kill-pane" => {
            // phux-4li.12: soft-kill — write `exit\n` as a sequence of
            // INPUT_KEY events to the focused Terminal. When the shell
            // processes those keystrokes it exits, the PTY closes, and
            // the server broadcasts TERMINAL_CLOSED which we then fold
            // out of the layout in `handle_server_frame`.
            //
            // Caveat: this is softer than tmux's `kill-pane`, which
            // sends SIGKILL to the entire process group. If the
            // focused pane has an unresponsive foreground process
            // (e.g. a stuck `cat` blocked on a non-existent FIFO) the
            // keystrokes go nowhere. A future ticket may add an
            // explicit KILL_TERMINAL wire frame; for v0.1 this gets
            // the daily-drive flow working end-to-end.
            let Some(focused_id) = focused.cloned() else {
                tracing::warn!("kill-pane: no focused pane to kill; dropping action");
                effects.bell = true;
                return effects;
            };
            effects.kill_frames = soft_kill_input_frames(&focused_id);
            // phux-i0e8.2.2: mark the close as ours so the resulting
            // TERMINAL_CLOSED does not raise a pane-exit notice.
            effects.expected_closes = vec![focused_id];
        }
        "take-input" => {
            // ADR-0033: seize the focused pane's input lease so only this
            // client's keystrokes reach the PTY. `Seize` preempts any holder;
            // the server broadcasts `TerminalControl` so the badge updates.
            let Some(focused_id) = focused.cloned() else {
                tracing::warn!("take-input: no focused pane; dropping action");
                effects.bell = true;
                return effects;
            };
            let request_id = *ctx.next_request_id;
            *ctx.next_request_id = ctx.next_request_id.wrapping_add(1);
            effects.command_frames.push(FrameKind::Command {
                request_id,
                command: Command::AcquireInput {
                    terminal_id: focused_id,
                    mode: InputMode::Seize,
                    ttl_ms: 0,
                },
            });
        }
        "give-input" => {
            // ADR-0033: release the focused pane's input lease back to open
            // input. A no-op server-side if we do not hold it.
            let Some(focused_id) = focused.cloned() else {
                tracing::warn!("give-input: no focused pane; dropping action");
                effects.bell = true;
                return effects;
            };
            let request_id = *ctx.next_request_id;
            *ctx.next_request_id = ctx.next_request_id.wrapping_add(1);
            effects.command_frames.push(FrameKind::Command {
                request_id,
                command: Command::ReleaseInput {
                    terminal_id: focused_id,
                },
            });
        }
        "signal-terminal" => {
            // ADR-0033: deliver a POSIX signal to the focused pane's process
            // group. `freeze`/`resume` is the reversible brake; distinct from
            // `kill-pane`, which removes the pane.
            let Some(signal) = signal_arg(resolved) else {
                tracing::warn!(
                    args = ?resolved.args,
                    "signal-terminal missing/bad `signal` arg (interrupt|freeze|resume|terminate|kill)",
                );
                effects.bell = true;
                return effects;
            };
            let Some(focused_id) = focused.cloned() else {
                tracing::warn!("signal-terminal: no focused pane; dropping action");
                effects.bell = true;
                return effects;
            };
            let request_id = *ctx.next_request_id;
            *ctx.next_request_id = ctx.next_request_id.wrapping_add(1);
            effects.command_frames.push(FrameKind::Command {
                request_id,
                command: Command::SignalTerminal {
                    terminal_id: focused_id,
                    signal,
                },
            });
        }
        "set-pane" => {
            // phux-npb3 (ADR-0048 decision 3 follow-up): flip the focused
            // pane's per-pane mouse opt-out. `mouse = "off"` opts the pane
            // out of client mouse handling (no synthesized INPUT_MOUSE; the
            // driver drops outer capture while the pane is focused, so the
            // host terminal's raw mouse handling returns for it alone);
            // `"on"` opts back in; `"toggle"` flips. Entirely client-local —
            // nothing crosses the wire.
            let Some(mode) = mouse_arg(resolved) else {
                tracing::warn!(
                    args = ?resolved.args,
                    "set-pane missing/bad `mouse` arg (expected on|off|toggle or a bool)",
                );
                effects.bell = true;
                return effects;
            };
            let Some(focused_id) = focused.cloned() else {
                tracing::warn!("set-pane: no focused pane; dropping action");
                effects.bell = true;
                return effects;
            };
            let opt_out = match mode {
                PaneMouseArg::Off => true,
                PaneMouseArg::On => false,
                PaneMouseArg::Toggle => !ctx.mouse_optout.contains(&focused_id),
            };
            if opt_out {
                ctx.mouse_optout.insert(focused_id.clone());
            } else {
                ctx.mouse_optout.remove(&focused_id);
            }
            tracing::info!(
                terminal = ?focused_id,
                mouse = !opt_out,
                "set-pane: per-pane mouse opt-out updated"
            );
            // No repaint needed: the opt-out has no chrome today, and the
            // driver re-syncs the outer capture DECSET from this set at the
            // top of every loop iteration.
        }
        "new-window" => {
            // phux-4li.15: open a new window. Spawn a fresh Terminal
            // (same SPAWN as a split) and park a `PendingWindow`; the
            // reply (`handle_server_frame`'s TerminalSpawned arm) adds a
            // window seeded on the spawned pane and makes it active. The
            // new pane is a bare leaf — the server files it under the
            // default Group; the TUI groups it into a window itself
            // (windows are a client convention, ADR-0017).
            let request_id = *ctx.next_request_id;
            *ctx.next_request_id = ctx.next_request_id.wrapping_add(1);
            let name = ctx.workspace.default_window_name();
            let frame = FrameKind::SpawnTerminal {
                request_id,
                group: DEFAULT_GROUP_ID,
                command: None,
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
            };
            effects.spawn_window = Some((request_id, PendingWindow { name }, frame));
        }
        "kill-window" => {
            // phux-4li.15: soft-kill every pane in the active window, the
            // same `exit\n` mechanism as `kill-pane`. As each
            // TERMINAL_CLOSED lands, `handle_server_frame` folds the pane
            // out; when the window's tree empties it is pruned and the
            // new layout broadcast. No synchronous window removal here.
            let leaves = ctx
                .workspace
                .active_window()
                .and_then(|ls| ls.tree.as_ref().map(crate::layout::leaves))
                .unwrap_or_default();
            if leaves.is_empty() {
                tracing::warn!("kill-window: no active window to kill; dropping action");
                effects.bell = true;
                return effects;
            }
            effects.kill_frames = leaves.iter().flat_map(soft_kill_input_frames).collect();
            // phux-i0e8.2.2: every pane in the window dies at our request;
            // none of those closes is news.
            effects.expected_closes = leaves;
        }
        "next-window" => {
            switch_window(ctx, &mut effects, Workspace::next);
        }
        "previous-window" => {
            switch_window(ctx, &mut effects, Workspace::prev);
        }
        "select-window" => {
            let Some(index) = index_arg(resolved) else {
                tracing::warn!(args = ?resolved.args, "select-window missing/bad `index` arg");
                effects.bell = true;
                return effects;
            };
            switch_window(ctx, &mut effects, |w| {
                w.select(index);
            });
        }
        "rename-window" => {
            if ctx.workspace.active_window().is_none() {
                tracing::warn!("rename-window: no active window; dropping action");
                effects.bell = true;
                return effects;
            }
            if let Some(name) = name_arg(resolved) {
                // Explicit `name` renames immediately. A rename is shared
                // window state, so (unlike focus/switch) it broadcasts.
                ctx.workspace.rename_active(name);
                effects.layout_mutated = true;
                effects.set_metadata = true;
            } else {
                // No name ⇒ open the interactive prompt pre-filled with
                // the active window's current name. On commit it re-runs
                // `rename-window` with the typed name (phux-ahv.1).
                let current = ctx
                    .workspace
                    .windows
                    .get(ctx.workspace.active)
                    .map(|w| w.name.clone())
                    .unwrap_or_default();
                ctx.overlays
                    .push(Box::new(PromptOverlay::rename_window(&current, ctx.theme)));
                effects.layout_mutated = true;
            }
        }
        "rename-session" => {
            // Rename the session this client is attached to. With an explicit
            // `name` it renames directly; with no name it opens a prompt
            // pre-filled with the current session name, which commits
            // `rename-session { name }` back through this same path (the
            // rename-window precedent). The actual `RENAME_SESSION` send +
            // optimistic local-name update happen in `apply_action_effects`
            // (the connection is async, run_action is sync — the `detach`
            // model).
            if let Some(name) = name_arg(resolved) {
                effects.rename_session = Some(name);
            } else {
                ctx.overlays.push(Box::new(PromptOverlay::rename_session(
                    ctx.session_name,
                    ctx.theme,
                )));
                effects.layout_mutated = true;
            }
        }
        "focus-direction" => {
            if let Some(dir) = direction_arg(resolved) {
                if let Some(ls) = ctx.workspace.active_window_mut()
                    && let Some(new_state) = actions::apply_focus(ls, dir)
                {
                    let new_focus = new_state.focus.clone();
                    *ls = new_state;
                    effects.layout_mutated = true;
                    effects.set_focus = new_focus;
                }
                // No-neighbour case: silently drop (tmux convention —
                // bumping into the layout edge isn't a bell).
            } else {
                tracing::warn!(args = ?resolved.args, "focus-direction missing/bad `direction` arg");
                effects.bell = true;
            }
        }
        "resize-pane" => {
            if let (Some(dir), Some(amount)) = (direction_arg(resolved), amount_arg(resolved)) {
                let Some(ls) = ctx.workspace.active_window_mut() else {
                    effects.bell = true;
                    return effects;
                };
                match actions::apply_resize(ls, dir, amount, ctx.viewport, ctx.sidebar) {
                    Ok(Some(new_state)) => {
                        *ls = new_state;
                        effects.layout_mutated = true;
                        effects.set_metadata = true;
                    }
                    Ok(None) | Err(ActionError::NoResizableBoundary) => {
                        // Underflow guard tripped or no matching axis —
                        // bell-no-op (ADR-0019 decision 5).
                        effects.bell = true;
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "resize-pane failed");
                        effects.bell = true;
                    }
                }
            } else {
                tracing::warn!(args = ?resolved.args, "resize-pane missing args");
                effects.bell = true;
            }
        }
        "reload-config" => {
            // phux-foz.5: explicit live config reload. The actual re-read
            // + swap happens in the driver after this batch (see
            // `DispatchCtx::reload_request`): the resolver that just
            // resolved this chord, the theme, and the keybindings
            // snapshot are all borrowed by `ctx` right now — they are
            // exactly the state the reload replaces.
            effects.reload_config = true;
        }
        "show-help" => {
            // phux-5ke.4: push the help overlay. The chord is consumed —
            // the bound key never reaches the pane (and it wouldn't make
            // sense to: F1 / `?` are typically reserved for the active
            // application, but the resolver matched, so the user's
            // intent was "open phux help"). Idempotent: pushing while
            // already active just replaces (debug-logged).
            let overlay = ctx.keybindings.map_or_else(
                || HelpOverlay::from_config(&phux_config::KeybindingsCfg::default(), ctx.theme),
                |kb| HelpOverlay::from_config(kb, ctx.theme),
            );
            ctx.overlays.push(Box::new(overlay));
        }
        "copy-mode" => {
            // phux-wave-a-copy-mode: enter selection/copy mode. Arrow keys move
            // the cursor without extending the selection unless Shift is held;
            // mouse drag can select and copy in one gesture.
            let pane_rect = focused_pane_rect(ctx, focused);
            let overlay = Box::new(crate::render::overlay::CopyModeOverlay::new(
                0,
                0,
                pane_rect.w,
                pane_rect.h,
            ));
            ctx.overlays.push(overlay);
        }
        "command-palette" => {
            // phux-ahv.8: push the command palette. It lists every action
            // the registry knows about, annotated with its currently-bound
            // chord from the live keybindings snapshot. Choosing a row
            // commits that action's `ResolvedAction`, which flows back
            // through this same `run_action` — keybinds and the palette
            // share one dispatch path (the architectural invariant).
            let items = super::action_registry::palette_items(
                ctx.keybindings,
                ctx.plugin_actions,
                ctx.plugin_panes,
            );
            ctx.overlays.push(Box::new(SelectList::new(
                "command palette",
                items,
                ctx.theme,
            )));
        }
        "context-menu" => {
            // phux-wrnm (ADR-0058): the keyboard route to the pane menu, and
            // the only route for a pane whose app owns the mouse. Anchored
            // just inside the focused pane's top-left corner so it opens over
            // the pane it acts on, wherever that pane sits in the layout.
            let rect = focused_pane_rect(ctx, focused);
            let anchor = (rect.x.saturating_add(2), rect.y.saturating_add(1));
            let zoomed = ctx.zoomed.is_some();
            let spec = super::context_menu::pane_menu(ctx.keybindings, zoomed);
            open_context_menu(ctx, spec, anchor);
        }
        "window-picker" => {
            // phux-4li.19 / nav: push the `<leader> w` grouped window
            // picker. Sessions are section headers; under the current
            // session each window (`index:name`, pane count) commits
            // `select-window { index }` (the same per-client switch the
            // numeric prefix bindings use). Other sessions list their own
            // windows as one-step `switch-session { name, window }` rows
            // when their persisted layout is cached (phux-foz.8), falling
            // back to a single "switch to session" row otherwise. With no
            // rows at all it bells.
            let items = window_picker_items(
                ctx.workspace,
                ctx.sessions,
                ctx.foreign_layouts,
                ctx.focused_session,
            );
            if items.iter().all(SelectItem::is_header) {
                effects.bell = true;
                return effects;
            }
            ctx.overlays
                .push(Box::new(SelectList::new("windows", items, ctx.theme)));
        }
        "session-picker" => {
            // phux-4li.20: push the session picker. The current session is
            // first and marked in its secondary text so the list is a full
            // inventory and opens with useful orientation. Committing that
            // row dismisses the picker as a silent no-op; peer rows commit
            // `switch-session { name }`. A trailing "+ New session" row
            // keeps creation reachable even when no sessions are cached.
            let mut items = session_picker_items(ctx.sessions, ctx.focused_session);
            items.push(new_session_item());
            ctx.overlays
                .push(Box::new(SelectList::new("sessions", items, ctx.theme)));
        }
        "agent-fleet" => {
            // phux-foz.7: push the agent-fleet dashboard — every pane of
            // the attached session grouped under session headers, with its
            // ADR-0040 agent record (name/kind + state glyph), ADR-0035
            // asked/attention highlight, and branch/cwd. Current-session
            // rows commit `focus-pane { window, pane }` through the single
            // dispatch path.
            //
            // phux-jpqd: a FOREIGN session with a cached persisted layout
            // (`foreign_layouts`) lists one row per pane committing a
            // one-step `switch-session { name, window, pane }`, its agent
            // glyph/state drawn from `foreign_agents` — no attach hop to see
            // a peer's panes. A foreign session with no cached layout still
            // falls back to a single `switch-session { name }` row.
            // Constructed with the fleet live key so the driver refreshes
            // the rows in place as agent events land while it is open. With
            // nothing to list it bells.
            let meta = super::fleet::collect_pane_meta(panes, ctx.vcs);
            let items = super::fleet::fleet_items(
                ctx.workspace,
                ctx.sessions,
                ctx.focused_session,
                ctx.agent_meta,
                &meta,
                ctx.foreign_layouts,
                ctx.foreign_agents,
            );
            if items.iter().all(SelectItem::is_header) {
                effects.bell = true;
                return effects;
            }
            ctx.overlays.push(Box::new(
                SelectList::new("agent fleet", items, ctx.theme)
                    .with_live_key(super::fleet::FLEET_LIVE_KEY),
            ));
        }
        "next-attention" => {
            // phux-oih5.16 / ADR-0049: advisory, client-local navigation over
            // asking panes. Flatten windows in display order and each tree in
            // DFS leaf order; choose the first asking pane strictly after the
            // current pane, wrapping once. No attention means a bell-no-op and
            // does not arm a return origin.
            let ordered = ordered_workspace_panes(ctx.workspace);
            let current = focused.and_then(|id| ordered.iter().position(|(_, pane)| pane == id));
            let target = ordered
                .iter()
                .enumerate()
                .filter(|(_, (_, id))| panes.get(id).is_some_and(|slot| slot.attention))
                .find(|(index, _)| current.is_none_or(|current| *index > current))
                .or_else(|| {
                    ordered
                        .iter()
                        .enumerate()
                        .find(|(_, (_, id))| panes.get(id).is_some_and(|slot| slot.attention))
                })
                .map(|(_, (window, id))| (*window, id.clone()));
            let Some((window, target)) = target else {
                effects.bell = true;
                return effects;
            };

            ctx.attention_navigation.save_origin_once(focused);
            focus_terminal(ctx.workspace, window, target.clone());
            effects.layout_mutated = true;
            effects.set_focus = Some(target);
        }
        "return-from-attention" => {
            // Consume first: a pane that disappeared while we were cycling is
            // a safe bell-no-op, not a sticky origin that can later resolve to
            // a different pane. TerminalId is stable across window reordering,
            // so a surviving origin is found in its current window/DFS slot.
            let Some(origin) = ctx.attention_navigation.take_origin() else {
                effects.bell = true;
                return effects;
            };
            let Some((window, _)) = ordered_workspace_panes(ctx.workspace)
                .into_iter()
                .find(|(_, id)| id == &origin)
            else {
                effects.bell = true;
                return effects;
            };
            focus_terminal(ctx.workspace, window, origin.clone());
            effects.layout_mutated = true;
            effects.set_focus = Some(origin);
        }
        "focus-pane" => {
            // phux-foz.7: focus a specific pane addressed as
            // (window index, DFS leaf ordinal) — the commit the fleet
            // dashboard's current-session rows carry. Per-client, like
            // `select-window` (no broadcast): switch to the window, then
            // move its client-local focus onto the target leaf. Stale
            // coordinates (the layout changed since the rows were built)
            // bell rather than focusing the wrong pane.
            let (Some(win), Some(ord)) =
                (usize_arg(resolved, "window"), usize_arg(resolved, "pane"))
            else {
                tracing::warn!(
                    args = ?resolved.args,
                    "focus-pane missing/bad `window`/`pane` args",
                );
                effects.bell = true;
                return effects;
            };
            let target = ctx
                .workspace
                .windows
                .get(win)
                .and_then(|w| w.state.tree.as_ref())
                .map(crate::layout::leaves)
                .and_then(|leaves| leaves.get(ord).cloned());
            let Some(target) = target else {
                tracing::warn!(
                    window = win,
                    pane = ord,
                    "focus-pane: no such pane (layout changed?)",
                );
                effects.bell = true;
                return effects;
            };
            switch_window(ctx, &mut effects, |w| {
                w.select(win);
            });
            if let Some(ls) = ctx.workspace.active_window_mut() {
                ls.focus = Some(target.clone());
            }
            effects.layout_mutated = true;
            effects.set_focus = Some(target);
        }
        "switch-session" => {
            // phux-4li.20 / phux-eb0: re-target this client to another
            // session. The effect carries the target up to
            // `apply_action_effects`, which routes it to the driver's
            // outer re-attach loop (in-process re-attach on the same
            // connection). A bad/absent `name` arg bells.
            //
            // phux-foz.8: an optional `window = N` arg makes it the
            // one-step cross-session window pick — after the re-attach
            // loads the target's persisted layout, the driver selects
            // window `N`. The grouped window picker's foreign-session
            // rows commit this form.
            //
            // phux-jpqd: an additional optional `pane = P` arg extends it
            // to a one-step cross-session PANE pick — after selecting the
            // window, the driver focuses its DFS leaf ordinal `P`. The
            // agent-fleet dashboard's foreign pane rows commit this form.
            if let Some(name) = name_arg(resolved) {
                let window = usize_arg(resolved, "window");
                let pane = usize_arg(resolved, "pane");
                effects.reattach = Some(ReattachTarget::Existing { name, window, pane });
            } else {
                tracing::warn!(
                    args = ?resolved.args,
                    "switch-session missing/bad `name` arg",
                );
                effects.bell = true;
            }
        }
        "new-session" => {
            // Create a fresh session (or attach to one already named) and
            // switch this client to it in-process. An explicit `name`
            // creates it directly; with no name we open a prompt to type
            // one, which commits `new-session { name }` back through this
            // same path. Either way the re-attach uses CreateIfMissing.
            match name_arg(resolved) {
                Some(name) => effects.reattach = Some(ReattachTarget::Create(name)),
                None => ctx
                    .overlays
                    .push(Box::new(PromptOverlay::new_session(ctx.theme))),
            }
        }
        "detach" => {
            effects.detach = true;
        }
        "plugin-action" => {
            // phux-r82.5: run a plugin manifest action through the same
            // child-process runtime `phux config run PLUGIN ACTION` uses.
            // Sync dispatch only records the intent; the async caller
            // (`apply_action_effects`) spawns the run off the input loop so
            // a slow plugin never freezes the TUI. Completion arrives on
            // the driver's plugin-events channel; failures toast.
            let (Some(plugin), Some(action)) =
                (str_arg(resolved, "plugin"), str_arg(resolved, "action"))
            else {
                tracing::warn!(
                    args = ?resolved.args,
                    "plugin-action missing/bad `plugin`/`action` args",
                );
                effects.bell = true;
                return effects;
            };
            effects.run_plugin = Some((plugin, action));
        }
        "plugin-pane" => {
            // phux-r82.7: open a plugin manifest `[[panes]]` entry as a
            // real server-side Terminal running the pane's argv. Routes
            // through the SAME SPAWN_TERMINAL machinery `split-pane` /
            // `new-window` use (ADR-0017: no plugin-privileged wire
            // surface) — the manifest supplies the command, the plugin
            // root the cwd, and PHUX_PLUGIN_* the additive env. Placement
            // picks the parked intent: `split`/`zoomed` park a
            // PendingSplit (zoomed also zooms the new pane when the
            // reply lands), `tab` parks a PendingWindow named after the
            // pane title. `overlay` entries never reach the snapshot
            // (deferred), so an unknown (plugin, pane) pair here also
            // covers a disabled plugin or an overlay declaration bound
            // directly in user config.
            let (Some(plugin), Some(pane)) =
                (str_arg(resolved, "plugin"), str_arg(resolved, "pane"))
            else {
                tracing::warn!(
                    args = ?resolved.args,
                    "plugin-pane missing/bad `plugin`/`pane` args",
                );
                effects.bell = true;
                return effects;
            };
            let Some(entry) = ctx
                .plugin_panes
                .iter()
                .find(|e| e.plugin_id == plugin && e.pane_id == pane)
            else {
                tracing::warn!(
                    plugin = %plugin,
                    pane = %pane,
                    "plugin-pane names no hostable pane (unknown, disabled, or overlay-deferred); dropping",
                );
                effects.bell = true;
                return effects;
            };
            let request_id = *ctx.next_request_id;
            *ctx.next_request_id = ctx.next_request_id.wrapping_add(1);
            let frame = entry.spawn_frame(request_id);
            match entry.placement {
                HostedPlacement::Split | HostedPlacement::Zoomed => {
                    let Some(focused_id) = focused.cloned() else {
                        tracing::warn!(
                            plugin = %plugin,
                            pane = %pane,
                            "plugin-pane split/zoomed placement needs a focused pane; dropping",
                        );
                        effects.bell = true;
                        return effects;
                    };
                    effects.spawn_terminal = Some((
                        request_id,
                        PendingSplit {
                            focused_at_request: focused_id,
                            // Side-by-side, matching the palette's
                            // `split-pane` default (vertical divider).
                            dir: SplitDir::Horizontal,
                            zoom_on_spawn: entry.placement == HostedPlacement::Zoomed,
                        },
                        frame,
                    ));
                }
                HostedPlacement::Tab => {
                    effects.spawn_window = Some((
                        request_id,
                        PendingWindow {
                            name: entry.title.clone(),
                        },
                        frame,
                    ));
                }
            }
        }
        "next-pane" => {
            if let Some(ls) = ctx.workspace.active_window_mut()
                && let Some(new_state) = actions::apply_next_pane(ls)
            {
                let new_focus = new_state.focus.clone();
                *ls = new_state;
                effects.layout_mutated = true;
                effects.set_focus = new_focus;
            }
        }
        "previous-pane" => {
            if let Some(ls) = ctx.workspace.active_window_mut()
                && let Some(new_state) = actions::apply_previous_pane(ls)
            {
                let new_focus = new_state.focus.clone();
                *ls = new_state;
                effects.layout_mutated = true;
                effects.set_focus = new_focus;
            }
        }
        "last-pane" => {
            // One-entry MRU jump-back. The target may be in another window;
            // locate it by stable TerminalId, switch the client-local active
            // window, and restore that window's local focus. Applying the
            // resulting focus change records the pane we jumped from as the
            // next MRU, so repeated invocations toggle between two panes.
            let Some(target) = ctx.focus_history.target(focused, ctx.workspace) else {
                effects.bell = true;
                return effects;
            };
            let owner = ctx.workspace.windows.iter().position(|window| {
                window
                    .state
                    .tree
                    .as_ref()
                    .is_some_and(|tree| crate::layout::leaves(tree).contains(&target))
            });
            let Some(window) = owner else {
                tracing::debug!(terminal = ?target, "last-pane MRU target is no longer live");
                effects.bell = true;
                return effects;
            };
            ctx.workspace.active = window;
            ctx.workspace.windows[window].state.focus = Some(target.clone());
            effects.layout_mutated = true;
            effects.clear_predict = true;
            effects.set_focus = Some(target);
        }
        "toggle-zoom" => {
            // phux-x2hm: zoom needs more than one pane (a single-pane window
            // bells, like tmux). When already zoomed the REAL tree still has
            // >1 leaf, so this same check permits un-zooming. The driver owns
            // the `zoomed` state; we just signal intent + request a repaint.
            let multi = ctx
                .workspace
                .active_window()
                .and_then(|ls| ls.tree.as_ref())
                .is_some_and(|t| crate::layout::leaves(t).len() > 1);
            if multi {
                effects.toggle_zoom = true;
                effects.layout_mutated = true;
            } else {
                effects.bell = true;
            }
        }
        "toggle-sidebar" => {
            // phux-4h5a: show/hide the window sidebar. Unconditional (unlike
            // zoom, which needs >1 pane) — the strip lists windows and is
            // meaningful even single-pane. The driver owns `sidebar_enabled`;
            // we signal intent + a repaint so the panes reflow into/out of the
            // reserved columns.
            // phux-4h5a P4 follow-up: a `focus-window`-by-index action (the
            // keyboard companion to clicking a strip row) is deferred; the
            // existing `select-window` jumps by tab position, but a strip-row
            // index action that pairs with mouse click-to-focus is not yet
            // wired.
            effects.toggle_sidebar = true;
            effects.layout_mutated = true;
        }
        other => {
            tracing::debug!(action = other, "unhandled resolved action");
        }
    }
    effects
}

/// Flatten a workspace deterministically: window order, then DFS leaf order.
fn ordered_workspace_panes(workspace: &Workspace) -> Vec<(usize, TerminalId)> {
    workspace
        .windows
        .iter()
        .enumerate()
        .flat_map(|(window, state)| {
            state
                .state
                .tree
                .as_ref()
                .map(crate::layout::leaves)
                .unwrap_or_default()
                .into_iter()
                .map(move |id| (window, id))
        })
        .collect()
}

/// Apply a resolved local focus target without producing shared-layout state.
fn focus_terminal(workspace: &mut Workspace, window: usize, target: TerminalId) {
    workspace.select(window);
    if let Some(state) = workspace.active_window_mut() {
        state.focus = Some(target);
    }
}

/// Pull a `Direction` out of a [`phux_config::keybind::ResolvedAction`]'s `direction = "..."`
/// arg.
fn direction_arg(resolved: &phux_config::keybind::ResolvedAction) -> Option<Direction> {
    let s = resolved.args.get("direction")?.as_str()?;
    match s {
        "up" => Some(Direction::Up),
        "down" => Some(Direction::Down),
        "left" => Some(Direction::Left),
        "right" => Some(Direction::Right),
        // `split-pane direction=horizontal|vertical` uses a different
        // axis vocabulary; this helper is only for focus/resize.
        _ => None,
    }
}

/// Pull an `amount = N` arg out of a [`phux_config::keybind::ResolvedAction`]. TOML integers
/// decode as `i64`; we clamp to `i16` (the [`actions::apply_resize`]
/// signature). Out-of-range values are silently clamped — a `resize-pane
/// amount = 99999` user binding gets a 32767-cell amount, which the
/// underflow guard inside `apply_resize` then rejects.
#[allow(clippy::cast_possible_truncation)]
fn amount_arg(resolved: &phux_config::keybind::ResolvedAction) -> Option<i16> {
    let v = resolved.args.get("amount")?.as_integer()?;
    Some(v.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16)
}

/// Pull a window index out of a [`phux_config::keybind::ResolvedAction`]'s `index = N` arg.
/// Negative or non-integer values yield `None` (the caller bells).
fn index_arg(resolved: &phux_config::keybind::ResolvedAction) -> Option<usize> {
    usize_arg(resolved, "index")
}

/// Pull a non-negative integer arg (`key = N`) out of a
/// [`phux_config::keybind::ResolvedAction`] (phux-foz.7: `window` / `pane`
/// on `focus-pane`). Negative or non-integer values yield `None` (the
/// caller bells).
fn usize_arg(resolved: &phux_config::keybind::ResolvedAction, key: &str) -> Option<usize> {
    let v = resolved.args.get(key)?.as_integer()?;
    usize::try_from(v).ok()
}

/// Pull a window name out of a [`phux_config::keybind::ResolvedAction`]'s `name = "..."` arg.
fn name_arg(resolved: &phux_config::keybind::ResolvedAction) -> Option<String> {
    resolved.args.get("name")?.as_str().map(ToOwned::to_owned)
}

/// Pull an arbitrary string arg out of a
/// [`phux_config::keybind::ResolvedAction`] (phux-r82.5: `plugin` /
/// `action` on `plugin-action`).
fn str_arg(resolved: &phux_config::keybind::ResolvedAction, key: &str) -> Option<String> {
    resolved.args.get(key)?.as_str().map(ToOwned::to_owned)
}

/// The `mouse` argument of `set-pane` (phux-npb3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneMouseArg {
    /// Opt the pane back in to client mouse handling.
    On,
    /// Opt the pane out (`set-pane mouse off`, ADR-0048's escape hatch).
    Off,
    /// Flip the pane's current state (the palette default).
    Toggle,
}

/// Pull the `mouse = ...` arg out of a `set-pane` action. Accepts the
/// documented strings (`"on"` / `"off"` / `"toggle"`) and, for TOML
/// ergonomics in keybinding tables, plain booleans (`mouse = false` ≡
/// `"off"`). Anything else yields `None` (the caller bells).
fn mouse_arg(resolved: &phux_config::keybind::ResolvedAction) -> Option<PaneMouseArg> {
    match resolved.args.get("mouse")? {
        toml::Value::String(s) => match s.as_str() {
            "on" => Some(PaneMouseArg::On),
            "off" => Some(PaneMouseArg::Off),
            "toggle" => Some(PaneMouseArg::Toggle),
            _ => None,
        },
        toml::Value::Boolean(b) => Some(if *b {
            PaneMouseArg::On
        } else {
            PaneMouseArg::Off
        }),
        _ => None,
    }
}

/// Build the `<leader> w` grouped window picker's rows (phux-4li.19 / nav).
///
/// The picker is hierarchical: one [`SelectItem::header`] per session, with
/// that session's windows nested (indented) beneath it. Sessions are
/// ordered with the **current** session first (so the windows you can act
/// on directly lead), then the rest by name for a stable layout.
///
/// - Under the **current** session, each window row is `index:name` with
///   the pane count as the dimmed secondary; it commits
///   `select-window { index }` — the same per-client window switch the
///   numeric prefix bindings use, routed through the single dispatch path.
/// - Under **other** sessions with a cached persisted layout
///   (`foreign_layouts`, fetched by the driver at attach — phux-foz.8),
///   each window renders the same `index:name` row committing
///   `switch-session { name, window = index }`: one step re-attaches to
///   that session AND selects the window once its layout loads.
/// - A foreign session with **no** cached layout (nothing persisted yet,
///   the GET reply hasn't landed, or the session appeared after attach)
///   falls back to a single "switch to this session" row committing
///   `switch-session { name }` — its own picker then lists its windows.
///
/// Headers are non-selectable; a session with no rows beneath it (the
/// current session with zero windows) still contributes its header, and
/// the caller bells when *only* headers result.
fn window_picker_items(
    workspace: &Workspace,
    sessions: &[phux_protocol::wire::info::SessionInfo],
    foreign_layouts: &HashMap<phux_protocol::ids::SessionId, Workspace>,
    focused: Option<phux_protocol::ids::SessionId>,
) -> Vec<SelectItem> {
    // Order sessions: current first, then the rest alphabetically by name
    // for a deterministic layout.
    let mut ordered: Vec<&phux_protocol::wire::info::SessionInfo> = sessions.iter().collect();
    ordered.sort_by(|a, b| {
        let a_cur = Some(a.id) == focused;
        let b_cur = Some(b.id) == focused;
        b_cur.cmp(&a_cur).then_with(|| a.name.cmp(&b.name))
    });

    let mut items = Vec::new();
    for session in ordered {
        let is_current = Some(session.id) == focused;
        let header = if is_current {
            format!("{} (current)", session.name)
        } else {
            session.name.clone()
        };
        items.push(SelectItem::header(header));

        if is_current {
            items.extend(current_session_window_rows(workspace));
        } else if let Some(foreign) = foreign_layouts
            .get(&session.id)
            .filter(|ws| !ws.windows.is_empty())
        {
            // phux-foz.8: the one-step rows. Same `index:name` + pane-count
            // shape as the current session's rows, but committing
            // `switch-session { name, window }` so a single Enter lands in
            // that window of that session.
            items.extend(foreign_session_window_rows(&session.name, foreign));
        } else {
            // No cached layout for this foreign session; offer a switch.
            let windows = if session.window_count == 1 {
                "1 window".to_owned()
            } else {
                format!("{} windows", session.window_count)
            };
            let mut args = std::collections::BTreeMap::new();
            args.insert("name".to_owned(), toml::Value::String(session.name.clone()));
            items.push(
                SelectItem::new(
                    "switch to this session",
                    phux_config::keybind::ResolvedAction {
                        action: "switch-session".to_owned(),
                        args,
                    },
                )
                .secondary(windows)
                .indented(),
            );
        }
    }

    // No sessions cached yet (pre-snapshot): fall back to a flat list of
    // the current workspace's windows so the picker is still useful.
    if items.is_empty() {
        items.extend(current_session_window_rows(workspace));
    }
    items
}

/// The indented, selectable window rows for the locally-attached session,
/// drawn from the client's [`Workspace`]. Each commits
/// `select-window { index }`.
fn current_session_window_rows(workspace: &Workspace) -> Vec<SelectItem> {
    workspace
        .windows
        .iter()
        .enumerate()
        .map(|(index, window)| {
            let panes = window
                .state
                .tree
                .as_ref()
                .map_or(0, |tree| crate::layout::leaves(tree).len());
            let label = format!("{index}:{}", window.name);
            let secondary = if panes == 1 {
                "1 pane".to_owned()
            } else {
                format!("{panes} panes")
            };
            let mut args = std::collections::BTreeMap::new();
            // Window counts never approach i64::MAX; the lossless path is
            // the only one that can fire in practice.
            let idx_i64 = i64::try_from(index).unwrap_or(i64::MAX);
            args.insert("index".to_owned(), toml::Value::Integer(idx_i64));
            SelectItem::new(
                label,
                phux_config::keybind::ResolvedAction {
                    action: "select-window".to_owned(),
                    args,
                },
            )
            .secondary(secondary)
            .indented()
        })
        .collect()
}

/// phux-foz.8: the indented one-step jump rows for a **foreign** session,
/// drawn from its cached persisted [`Workspace`] (`DispatchCtx::
/// foreign_layouts`). Same `index:name` + pane-count shape as
/// [`current_session_window_rows`], but each row commits
/// `switch-session { name, window = index }` — the combined
/// re-attach-and-select the driver resolves after the target's layout
/// loads.
fn foreign_session_window_rows(session_name: &str, workspace: &Workspace) -> Vec<SelectItem> {
    workspace
        .windows
        .iter()
        .enumerate()
        .map(|(index, window)| {
            let panes = window
                .state
                .tree
                .as_ref()
                .map_or(0, |tree| crate::layout::leaves(tree).len());
            let label = format!("{index}:{}", window.name);
            let secondary = if panes == 1 {
                "1 pane".to_owned()
            } else {
                format!("{panes} panes")
            };
            let mut args = std::collections::BTreeMap::new();
            args.insert(
                "name".to_owned(),
                toml::Value::String(session_name.to_owned()),
            );
            // Window counts never approach i64::MAX; the lossless path is
            // the only one that can fire in practice.
            let idx_i64 = i64::try_from(index).unwrap_or(i64::MAX);
            args.insert("window".to_owned(), toml::Value::Integer(idx_i64));
            SelectItem::new(
                label,
                phux_config::keybind::ResolvedAction {
                    action: "switch-session".to_owned(),
                    args,
                },
            )
            .secondary(secondary)
            .indented()
        })
        .collect()
}

/// Build the session picker's rows from the client's cached
/// session graph (phux-4li.20).
///
/// One row per session, with `focused` first and marked `current`. Each row's
/// label is the session name with a window/attached-client summary as the
/// dimmed secondary. Choosing it commits `switch-session { name }`; the
/// current row dismisses as a silent no-op and peer rows reattach through the
/// same dispatch path.
fn session_picker_items(
    sessions: &[phux_protocol::wire::info::SessionInfo],
    focused: Option<phux_protocol::ids::SessionId>,
) -> Vec<SelectItem> {
    let mut ordered: Vec<_> = sessions.iter().collect();
    ordered.sort_by(|a, b| {
        let a_current = Some(a.id) == focused;
        let b_current = Some(b.id) == focused;
        b_current.cmp(&a_current).then_with(|| a.name.cmp(&b.name))
    });

    ordered
        .into_iter()
        .map(|s| {
            let windows = if s.window_count == 1 {
                "1 window".to_owned()
            } else {
                format!("{} windows", s.window_count)
            };
            let mut details = vec![windows];
            if Some(s.id) == focused {
                details.push("current".to_owned());
            }
            if s.attached_client_count != 0 {
                details.push(format!("{} attached", s.attached_client_count));
            }
            let mut args = std::collections::BTreeMap::new();
            args.insert("name".to_owned(), toml::Value::String(s.name.clone()));
            SelectItem::new(
                s.name.clone(),
                phux_config::keybind::ResolvedAction {
                    action: "switch-session".to_owned(),
                    args,
                },
            )
            .secondary(details.join(", "))
        })
        .collect()
}

/// The trailing "+ New session" row for the session picker. Committing it
/// runs the bare `new-session` action, which opens the name prompt — so a
/// new session is always reachable from `<leader> a`, even when this is
/// the only session.
fn new_session_item() -> SelectItem {
    SelectItem::new(
        "+ New session…".to_owned(),
        phux_config::keybind::ResolvedAction {
            action: "new-session".to_owned(),
            args: std::collections::BTreeMap::new(),
        },
    )
    .secondary("create".to_owned())
}

/// Apply a window-switch `mutate` to the workspace and, **only if the
/// active window actually changed**, record the follow-up: repaint the
/// new composition, drop the prediction queue, and move focus to the new
/// active window's focused leaf. A no-op switch (single window, wrap to
/// self, or an out-of-range `select`) leaves `effects` untouched.
///
/// Window selection is per-client like focus (ADR-0019 decision 6), so
/// this emits no `SET_METADATA` — siblings keep their own active window.
fn switch_window(
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
    mutate: impl FnOnce(&mut Workspace),
) {
    let before = ctx.workspace.active;
    mutate(ctx.workspace);
    if ctx.workspace.active == before {
        return;
    }
    effects.layout_mutated = true;
    effects.clear_predict = true;
    effects.set_focus = ctx.workspace.active_window().and_then(|w| w.focus.clone());
}

/// Encode the workspace for `SET_METADATA`, logging encode failures.
/// Returns `None` on failure — caller should not emit a frame in that case.
pub(super) fn encode_layout_or_log(workspace: &Workspace) -> Option<Vec<u8>> {
    match workspace.encode_cbor() {
        Ok(bytes) => Some(bytes),
        Err(err) => {
            tracing::warn!(error = %err, "layout CBOR encode failed; SET_METADATA skipped");
            None
        }
    }
}

/// Allow `SplitDir` to be parsed from a `direction = "horizontal|vertical"`
/// arg on a `split-pane` action. Lives here (not in `actions.rs`) so the
/// pure helper module stays free of `ResolvedAction` parsing.
///
/// The `direction` string names the DIVIDER orientation (the tmux mental
/// model the default config documents): `vertical` = a vertical divider,
/// i.e. side-by-side panes, which geometrically is a `SplitDir::Horizontal`
/// (split along the width — see `multi_pane::pane_rects`). `horizontal` = a
/// horizontal divider, i.e. stacked panes = `SplitDir::Vertical`. The
/// names are deliberately crossed here: the user-facing word describes the
/// divider; the internal enum describes the split axis.
fn split_dir_arg(resolved: &phux_config::keybind::ResolvedAction) -> Option<SplitDir> {
    let s = resolved.args.get("direction")?.as_str()?;
    match s {
        "horizontal" => Some(SplitDir::Vertical),
        "vertical" => Some(SplitDir::Horizontal),
        _ => None,
    }
}

/// ADR-0033: parse the `signal` arg of a `signal-terminal` action into a
/// [`TerminalSignal`]. Recognises `interrupt` / `freeze` / `resume` /
/// `terminate` / `kill`; returns `None` for a missing or unknown value (the
/// arm bells and drops the action).
fn signal_arg(resolved: &phux_config::keybind::ResolvedAction) -> Option<TerminalSignal> {
    match resolved.args.get("signal")?.as_str()? {
        "interrupt" => Some(TerminalSignal::Interrupt),
        "freeze" => Some(TerminalSignal::Freeze),
        "resume" => Some(TerminalSignal::Resume),
        "terminate" => Some(TerminalSignal::Terminate),
        "kill" => Some(TerminalSignal::Kill),
        _ => None,
    }
}

/// phux-4li.12: build the `INPUT_KEY` frame sequence that types `exit\n`
/// into the targeted Terminal. The shell processes those bytes, exits,
/// the PTY closes, and the server emits `TERMINAL_CLOSED` which the
/// driver folds out of the layout. See the `kill-pane` arm of
/// [`run_action`] for the soft-kill caveat.
fn soft_kill_input_frames(target: &TerminalId) -> Vec<FrameKind> {
    use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

    fn ascii_letter(ch: char, key: PhysicalKey) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: Some(ch.to_string()),
            unshifted_codepoint: Some(u32::from(ch)),
        }
    }
    const fn named(key: PhysicalKey) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        }
    }

    let events = [
        ascii_letter('e', PhysicalKey::E),
        ascii_letter('x', PhysicalKey::X),
        ascii_letter('i', PhysicalKey::I),
        ascii_letter('t', PhysicalKey::T),
        named(PhysicalKey::Enter),
    ];
    events
        .into_iter()
        .map(|event| FrameKind::InputKey {
            terminal_id: target.clone(),
            event,
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Ceiling for draining a scripted peer connection whose writer has
    /// already been dropped.
    ///
    /// Not load-bearing: the drain ends on the peer's EOF, and the
    /// assertions are on the frames collected — never on how fast they
    /// arrived. The timeout only stops a peer that never hangs up from
    /// wedging the binary. The 5s it replaces was generous on an idle laptop
    /// and a measurement of the scheduler on a saturated one (phux-br1f).
    const PEER_DRAIN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

    fn tid(id: u32) -> TerminalId {
        TerminalId::local(id)
    }

    fn test_engine_kernel() -> super::super::driver::AttachKernel {
        phux_client_core::session::SessionKernel::new(
            phux_client_core::engine::ghostty::GhosttyAdapter::new(
                phux_protocol::BootstrapLimits::default(),
            ),
            phux_protocol::BootstrapProfile::SynthesizedVtRaw,
        )
    }

    #[test]
    fn soft_kill_input_frames_emits_exit_newline_sequence() {
        let frames = soft_kill_input_frames(&tid(7));
        assert_eq!(frames.len(), 5, "expected e/x/i/t/Enter");
        // Each frame is INPUT_KEY targeting tid(7).
        for f in &frames {
            match f {
                FrameKind::InputKey { terminal_id, .. } => {
                    assert_eq!(terminal_id, &tid(7));
                }
                other => panic!("expected InputKey, got {other:?}"),
            }
        }
        // First four are printable letters with text="e".."t".
        let expected_text = ["e", "x", "i", "t"];
        for (i, want) in expected_text.iter().enumerate() {
            match &frames[i] {
                FrameKind::InputKey { event, .. } => {
                    assert_eq!(
                        event.text.as_deref(),
                        Some(*want),
                        "frame {i}: text mismatch",
                    );
                }
                _ => unreachable!(),
            }
        }
        // Last frame is Enter (no text).
        match &frames[4] {
            FrameKind::InputKey { event, .. } => {
                assert_eq!(event.key, phux_protocol::input::key::PhysicalKey::Enter);
                assert_eq!(event.text, None);
            }
            _ => unreachable!(),
        }
    }


    #[test]
    fn split_dir_arg_parses_horizontal_and_vertical() {
        use phux_config::keybind::ResolvedAction;
        // `direction` names the divider orientation, not the split axis:
        // "horizontal" divider ⇒ stacked panes ⇒ SplitDir::Vertical;
        // "vertical" divider ⇒ side-by-side panes ⇒ SplitDir::Horizontal.
        let mut h = ResolvedAction {
            action: "split-pane".to_owned(),
            args: std::collections::BTreeMap::new(),
        };
        h.args.insert(
            "direction".to_owned(),
            toml::Value::String("horizontal".into()),
        );
        assert_eq!(split_dir_arg(&h), Some(SplitDir::Vertical));

        let mut v = ResolvedAction {
            action: "split-pane".to_owned(),
            args: std::collections::BTreeMap::new(),
        };
        v.args.insert(
            "direction".to_owned(),
            toml::Value::String("vertical".into()),
        );
        assert_eq!(split_dir_arg(&v), Some(SplitDir::Horizontal));

        let mut bogus = ResolvedAction {
            action: "split-pane".to_owned(),
            args: std::collections::BTreeMap::new(),
        };
        bogus.args.insert(
            "direction".to_owned(),
            toml::Value::String("diagonal".into()),
        );
        assert_eq!(split_dir_arg(&bogus), None);
    }

    #[test]
    fn focused_pane_rect_tracks_rendered_pane_bounds() {
        use crate::layout::{LayoutNode, LayoutState, Rect, WindowState, split_at};

        let tree = split_at(
            &LayoutNode::Leaf(tid(1)),
            &tid(1),
            &tid(2),
            SplitDir::Horizontal,
            0.5,
        )
        .unwrap();
        let workspace = Workspace {
            windows: vec![WindowState {
                name: "1".to_owned(),
                state: LayoutState {
                    tree: Some(tree),
                    focus: Some(tid(2)),
                },
            }],
            active: 0,
        };

        let split_rect = focused_pane_rect_for(
            &workspace,
            None,
            Some(&tid(2)),
            (80, 24),
            Some(crate::render::chrome::status_bar::Position::Bottom),
            None,
        );
        assert_eq!(split_rect.y, 0);
        assert_eq!(split_rect.h, 23, "status bar row is not copy-mode content");
        assert_eq!(split_rect.x + split_rect.w, 80);
        assert!(
            split_rect.w < 80,
            "split pane must not inherit the outer viewport width"
        );
        assert_ne!(
            split_rect,
            Rect {
                x: 0,
                y: 0,
                w: 80,
                h: 23
            }
        );

        let zoomed = tid(2);
        let zoomed_rect = focused_pane_rect_for(
            &workspace,
            Some(&zoomed),
            Some(&tid(2)),
            (80, 24),
            Some(crate::render::chrome::status_bar::Position::Bottom),
            None,
        );
        assert_eq!(
            zoomed_rect,
            Rect {
                x: 0,
                y: 0,
                w: 80,
                h: 23
            }
        );
    }

    /// Build a [`ResolvedAction`] with no args.
    fn bare_action(name: &str) -> phux_config::keybind::ResolvedAction {
        phux_config::keybind::ResolvedAction {
            action: name.to_owned(),
            args: BTreeMap::new(),
        }
    }

    /// Run `action` against `workspace`, returning the resulting effects.
    fn run(
        action: &phux_config::keybind::ResolvedAction,
        workspace: &mut Workspace,
    ) -> ActionEffects {
        run_with_last(action, workspace, None)
    }

    fn run_with_last(
        action: &phux_config::keybind::ResolvedAction,
        workspace: &mut Workspace,
        last_focused: Option<TerminalId>,
    ) -> ActionEffects {
        let mut next_request_id = 100;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let mut overlays = OverlayState::new();
        let theme = Theme::default();
        let mut switch_request = None;
        let mut session_name = String::new();
        let mut zoomed = None;
        let mut sidebar_enabled = false;
        let mut drag: Option<DragGrab> = None;
        let mut reload_request = false;
        let mut mouse_optout: std::collections::HashSet<TerminalId> =
            std::collections::HashSet::new();
        let fleet_agent_meta = HashMap::new();
        let mut fleet_vcs = crate::attach::driver::VcsIndex::default();
        let mut engine_kernel = test_engine_kernel();
        let mut ctx = DispatchCtx {
            engine_kernel: &mut engine_kernel,
            resolver: None,
            focus_history: last_focused
                .map_or_else(FocusHistory::default, FocusHistory::with_previous),
            workspace,
            viewport: (80, 24),
            cell_px: (1, 1),
            next_request_id: &mut next_request_id,
            pending_splits: &mut pending_splits,
            pending_windows: &mut pending_windows,
            expected_closes: &mut HashSet::new(),
            overlays: &mut overlays,
            keybindings: None,
            theme: &theme,
            sessions: &[],
            foreign_layouts: &HashMap::new(),
            foreign_agents: &HashMap::new(),
            focused_session: None,
            session_name: &mut session_name,
            switch_request: &mut switch_request,
            zoomed: &mut zoomed,
            sidebar: None,
            sidebar_enabled: &mut sidebar_enabled,
            sidebar_agents: &[],
            bar: None,
            status_bar: None,
            drag: &mut drag,
            mouse_optout: &mut mouse_optout,
            attention_navigation: &mut AttentionNavigation::default(),
            plugin_actions: &[],
            plugin_panes: &[],
            plugin_tx: None,
            reload_request: &mut reload_request,
            agent_meta: &fleet_agent_meta,
            vcs: &mut fleet_vcs,
        };
        let focused = ctx.workspace.active_window().and_then(|w| w.focus.clone());
        run_action(action, &mut ctx, focused.as_ref(), &HashMap::new())
    }

    #[test]
    fn reload_config_action_raises_the_reload_effect() {
        // phux-foz.5: the arm only raises the effect — the driver owns
        // the actual re-read + swap (the ctx borrows the state to
        // replace). No layout mutation, no bell, no frames.
        let mut workspace = Workspace::single(tid(1));
        let effects = run(&bare_action("reload-config"), &mut workspace);
        assert!(effects.reload_config, "reload-config must raise the effect");
        assert!(!effects.layout_mutated);
        assert!(!effects.bell);
        assert!(effects.kill_frames.is_empty());
    }

    #[test]
    fn new_window_parks_pending_and_emits_spawn() {
        let mut workspace = Workspace::single(tid(1)); // window "1"
        let effects = run(&bare_action("new-window"), &mut workspace);
        let (_req, pending, frame) = effects
            .spawn_window
            .expect("new-window should park a PendingWindow + SPAWN");
        // Default name skips the in-use "1".
        assert_eq!(pending.name, "2");
        assert!(matches!(frame, FrameKind::SpawnTerminal { .. }));
        // No synchronous workspace mutation — the window opens on reply.
        assert_eq!(workspace.windows.len(), 1);
    }

    #[test]
    fn kill_window_emits_one_soft_kill_sequence_per_leaf() {
        use crate::layout::{LayoutNode, LayoutState, SplitDir, WindowState, split_at};
        // Active window with three leaves: ((1|2)/3).
        let tree = split_at(
            &LayoutNode::Leaf(tid(1)),
            &tid(1),
            &tid(2),
            SplitDir::Horizontal,
            0.5,
        )
        .unwrap();
        let tree = split_at(&tree, &tid(2), &tid(3), SplitDir::Vertical, 0.5).unwrap();
        let mut workspace = Workspace {
            windows: vec![WindowState {
                name: "1".to_owned(),
                state: LayoutState {
                    tree: Some(tree),
                    focus: Some(tid(1)),
                },
            }],
            active: 0,
        };
        let effects = run(&bare_action("kill-window"), &mut workspace);
        // 3 leaves x 5 frames (e/x/i/t/Enter) each.
        assert_eq!(effects.kill_frames.len(), 15);
        // phux-i0e8.2.2: every targeted leaf is marked as an expected
        // close so the resulting TERMINAL_CLOSEDs stay notice-silent.
        assert_eq!(effects.expected_closes, vec![tid(1), tid(2), tid(3)]);
        // No synchronous removal — TerminalClosed folds + prunes.
        assert_eq!(workspace.windows.len(), 1);
    }

    /// phux-i0e8.2.2: `kill-pane` marks its own target as an expected
    /// close alongside the soft-kill frames.
    #[test]
    fn kill_pane_marks_the_focused_pane_as_expected_close() {
        let mut workspace = Workspace::single(tid(7));
        let effects = run(&bare_action("kill-pane"), &mut workspace);
        assert!(!effects.kill_frames.is_empty());
        assert_eq!(effects.expected_closes, vec![tid(7)]);
    }

    #[test]
    fn next_window_switches_active_clears_predict_no_metadata() {
        let mut workspace = Workspace::single(tid(1));
        workspace.add_window("2".to_owned(), tid(2));
        workspace.select(0);
        let effects = run(&bare_action("next-window"), &mut workspace);
        assert_eq!(workspace.active, 1);
        assert!(effects.layout_mutated);
        assert!(effects.clear_predict);
        assert!(!effects.set_metadata, "window switch is per-client");
        assert_eq!(effects.set_focus, Some(tid(2)));
    }

    #[test]
    fn last_pane_dispatch_jumps_across_windows_and_toggles() {
        let mut workspace = Workspace::single(tid(1));
        workspace.add_window("2".to_owned(), tid(2));
        workspace.select(0);

        let action = bare_action("last-pane");
        let effects = run_with_last(&action, &mut workspace, Some(tid(2)));
        assert_eq!(workspace.active, 1);
        assert_eq!(
            workspace.active_window().and_then(|w| w.focus.clone()),
            Some(tid(2))
        );
        assert_eq!(effects.set_focus, Some(tid(2)));
        assert!(effects.clear_predict);
        assert!(!effects.set_metadata, "focus MRU is client-local");
        let mut focused = Some(tid(1));
        let mut history = FocusHistory::with_previous(tid(2));
        apply_focus_transition(
            &mut history,
            &mut focused,
            effects.set_focus.expect("dispatch target"),
        );
        assert_eq!(history.previous(), Some(&tid(1)));

        // Feed the recorded pane back through dispatch + the same apply path:
        // repeated last-pane genuinely toggles and repairs the MRU to pane 2.
        let effects = run_with_last(&action, &mut workspace, Some(tid(1)));
        assert_eq!(workspace.active, 0);
        apply_focus_transition(
            &mut history,
            &mut focused,
            effects.set_focus.expect("toggle target"),
        );
        assert_eq!(focused, Some(tid(1)));
        assert_eq!(history.previous(), Some(&tid(2)));
    }

    #[test]
    fn last_pane_without_history_bells_without_mutation() {
        let mut workspace = Workspace::single(tid(1));
        let effects = run(&bare_action("last-pane"), &mut workspace);
        assert!(effects.bell);
        assert!(!effects.layout_mutated);
        assert!(effects.set_focus.is_none());
    }

    #[test]
    fn next_window_single_window_is_noop() {
        let mut workspace = Workspace::single(tid(1));
        let effects = run(&bare_action("next-window"), &mut workspace);
        assert_eq!(workspace.active, 0);
        assert!(!effects.layout_mutated);
        assert!(!effects.clear_predict);
    }

    #[test]
    fn select_window_jumps_to_index() {
        let mut workspace = Workspace::single(tid(1));
        workspace.add_window("2".to_owned(), tid(2));
        workspace.add_window("3".to_owned(), tid(3)); // active = 2
        let mut action = bare_action("select-window");
        action
            .args
            .insert("index".to_owned(), toml::Value::Integer(0));
        let effects = run(&action, &mut workspace);
        assert_eq!(workspace.active, 0);
        assert!(effects.layout_mutated);
        assert_eq!(effects.set_focus, Some(tid(1)));
    }

    #[test]
    fn select_window_out_of_range_is_noop() {
        let mut workspace = Workspace::single(tid(1)); // only index 0
        let mut action = bare_action("select-window");
        action
            .args
            .insert("index".to_owned(), toml::Value::Integer(5));
        let effects = run(&action, &mut workspace);
        assert_eq!(workspace.active, 0);
        assert!(!effects.layout_mutated);
    }

    #[test]
    fn select_window_missing_index_bells() {
        let mut workspace = Workspace::single(tid(1));
        let effects = run(&bare_action("select-window"), &mut workspace);
        assert!(effects.bell);
        assert!(!effects.layout_mutated);
    }

    /// phux-x2hm: a multi-pane window can zoom — `toggle-zoom` requests the
    /// driver-side flip (`toggle_zoom`) plus a repaint (`layout_mutated`),
    /// without mutating the real tree or bell-ing.
    #[test]
    fn toggle_zoom_on_multi_pane_window_requests_toggle() {
        use crate::layout::{LayoutState, WindowState, split_at};
        let tree = split_at(
            &crate::layout::LayoutNode::Leaf(tid(1)),
            &tid(1),
            &tid(2),
            crate::layout::SplitDir::Horizontal,
            0.5,
        )
        .unwrap();
        let mut workspace = Workspace {
            windows: vec![WindowState {
                name: "1".to_owned(),
                state: LayoutState {
                    tree: Some(tree),
                    focus: Some(tid(1)),
                },
            }],
            active: 0,
        };
        let effects = run(&bare_action("toggle-zoom"), &mut workspace);
        assert!(effects.toggle_zoom, "multi-pane window may zoom");
        assert!(effects.layout_mutated, "zoom toggles drive a repaint");
        assert!(!effects.bell);
    }

    /// phux-x2hm: a single-pane window has nothing to zoom — `toggle-zoom`
    /// bells (tmux parity) and does NOT request a toggle or repaint.
    #[test]
    fn toggle_zoom_on_single_pane_window_bells() {
        let mut workspace = Workspace::single(tid(1));
        let effects = run(&bare_action("toggle-zoom"), &mut workspace);
        assert!(effects.bell, "single-pane window cannot zoom");
        assert!(!effects.toggle_zoom);
        assert!(!effects.layout_mutated);
    }

    /// A two-pane Horizontal split with focus on the left leaf, root
    /// ratio 0.5 — the fixture the `resize-pane` dispatch tests mutate.
    fn two_pane_workspace() -> Workspace {
        use crate::layout::{LayoutState, WindowState, split_at};
        let tree = split_at(
            &crate::layout::LayoutNode::Leaf(tid(1)),
            &tid(1),
            &tid(2),
            SplitDir::Horizontal,
            0.5,
        )
        .unwrap();
        Workspace {
            windows: vec![WindowState {
                name: "1".to_owned(),
                state: LayoutState {
                    tree: Some(tree),
                    focus: Some(tid(1)),
                },
            }],
            active: 0,
        }
    }

    /// The root split's ratio, for asserting a resize actually moved it.
    fn root_ratio(workspace: &Workspace) -> f32 {
        match workspace.active_window().unwrap().tree.as_ref().unwrap() {
            crate::layout::LayoutNode::Split { ratio, .. } => *ratio,
            other => panic!("expected root Split, got {other:?}"),
        }
    }

    /// phux-foz.3: `resize-pane { direction, amount }` dispatches through
    /// `run_action` — the ratio moves by amount/axis-cells, the layout
    /// repaints, and the mutation broadcasts via `SET_METADATA` (unlike
    /// per-client focus moves).
    #[test]
    fn resize_pane_dispatch_moves_ratio_and_broadcasts() {
        let mut workspace = two_pane_workspace();
        let before = root_ratio(&workspace);
        let mut action = bare_action("resize-pane");
        action
            .args
            .insert("direction".to_owned(), toml::Value::String("right".into()));
        action
            .args
            .insert("amount".to_owned(), toml::Value::Integer(8));
        let effects = run(&action, &mut workspace);
        assert!(!effects.bell);
        assert!(effects.layout_mutated, "resize repaints the layout");
        assert!(
            effects.set_metadata,
            "a layout mutation broadcasts to other clients"
        );
        let after = root_ratio(&workspace);
        // Growing the focused (left) pane rightward by 8 of 80 cols.
        assert!(
            (after - before - 0.1).abs() < 1e-4,
            "ratio moved {before} -> {after}, wanted +0.1"
        );
    }

    /// phux-foz.3: a `resize-pane` missing its args bells and mutates
    /// nothing (ADR-0019 decision 5 bell-no-op contract).
    #[test]
    fn resize_pane_dispatch_missing_args_bells() {
        let mut workspace = two_pane_workspace();
        let before = root_ratio(&workspace);
        let effects = run(&bare_action("resize-pane"), &mut workspace);
        assert!(effects.bell);
        assert!(!effects.layout_mutated);
        assert!(!effects.set_metadata);
        assert!((root_ratio(&workspace) - before).abs() < f32::EPSILON);
    }

    /// phux-foz.3: a resize that would squeeze a pane below the 2-cell
    /// floor (ADR-0019 decision 5) bells and leaves the ratio unchanged.
    #[test]
    fn resize_pane_dispatch_min_cell_floor_bells() {
        let mut workspace = two_pane_workspace();
        let before = root_ratio(&workspace);
        let mut action = bare_action("resize-pane");
        action
            .args
            .insert("direction".to_owned(), toml::Value::String("right".into()));
        action
            .args
            .insert("amount".to_owned(), toml::Value::Integer(80));
        let effects = run(&action, &mut workspace);
        assert!(effects.bell, "floor violation is a bell-no-op");
        assert!(!effects.layout_mutated);
        assert!((root_ratio(&workspace) - before).abs() < f32::EPSILON);
    }

    /// phux-4h5a: `toggle-sidebar` requests the driver-side flip
    /// (`toggle_sidebar`) plus a repaint (`layout_mutated`), unconditionally —
    /// even single-pane, since the strip lists windows. It never bells and
    /// mutates no tree.
    #[test]
    fn toggle_sidebar_requests_flip_and_repaint() {
        let mut workspace = Workspace::single(tid(1));
        let effects = run(&bare_action("toggle-sidebar"), &mut workspace);
        assert!(effects.toggle_sidebar, "toggle-sidebar requests the flip");
        assert!(
            effects.layout_mutated,
            "sidebar toggle drives a reflow repaint"
        );
        assert!(!effects.bell);
        assert!(!effects.toggle_zoom);
    }

    /// phux-4h5a: `apply_action_effects` flips the driver-owned
    /// `sidebar_enabled` when `toggle_sidebar` is set — off→on and back on a
    /// second toggle.
    #[allow(
        clippy::too_many_lines,
        reason = "two hand-built DispatchCtx values exercise the full toggle round trip"
    )]
    #[tokio::test]
    async fn apply_effects_flips_sidebar_enabled_state() {
        let mut workspace = Workspace::single(tid(1));
        let mut next_request_id = 1;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let mut overlays = OverlayState::new();
        let theme = Theme::default();
        let mut switch_request = None;
        let mut session_name = String::new();
        let mut zoomed = None;
        let mut sidebar_enabled = false;
        let mut drag: Option<DragGrab> = None;
        let mut reload_request = false;
        let mut mouse_optout: std::collections::HashSet<TerminalId> =
            std::collections::HashSet::new();
        let fleet_agent_meta = HashMap::new();
        let mut fleet_vcs = crate::attach::driver::VcsIndex::default();
        let mut engine_kernel = test_engine_kernel();
        let mut ctx = DispatchCtx {
            engine_kernel: &mut engine_kernel,
            resolver: None,
            focus_history: FocusHistory::default(),
            workspace: &mut workspace,
            viewport: (80, 24),
            cell_px: (1, 1),
            next_request_id: &mut next_request_id,
            pending_splits: &mut pending_splits,
            pending_windows: &mut pending_windows,
            expected_closes: &mut HashSet::new(),
            overlays: &mut overlays,
            keybindings: None,
            theme: &theme,
            sessions: &[],
            foreign_layouts: &HashMap::new(),
            foreign_agents: &HashMap::new(),
            focused_session: None,
            session_name: &mut session_name,
            switch_request: &mut switch_request,
            zoomed: &mut zoomed,
            sidebar: None,
            sidebar_enabled: &mut sidebar_enabled,
            sidebar_agents: &[],
            bar: None,
            status_bar: None,
            drag: &mut drag,
            mouse_optout: &mut mouse_optout,
            attention_navigation: &mut AttentionNavigation::default(),
            plugin_actions: &[],
            plugin_panes: &[],
            plugin_tx: None,
            reload_request: &mut reload_request,
            agent_meta: &fleet_agent_meta,
            vcs: &mut fleet_vcs,
        };
        let effects = run_action(
            &bare_action("toggle-sidebar"),
            &mut ctx,
            None,
            &HashMap::new(),
        );
        let mut out: Vec<u8> = Vec::new();
        let (a, _b) = tokio::net::UnixStream::pair().expect("uds pair");
        let mut conn = Connection::from_stream(a);
        let mut focused_pane = None;
        let mut detach_pending = false;
        let mut predict =
            PredictionState::new(crate::predict::PredictiveConfig::disabled(), 80, 24);
        let panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        apply_action_effects(
            effects,
            &mut out,
            &mut conn,
            &mut ctx,
            &mut focused_pane,
            &mut detach_pending,
            &mut predict,
            &panes,
        )
        .await
        .expect("apply effects");
        assert!(sidebar_enabled, "first toggle enables the sidebar");

        // A second toggle disables it again.
        let mut reload_request = false;
        let fleet_agent_meta = HashMap::new();
        let mut fleet_vcs = crate::attach::driver::VcsIndex::default();
        let mut engine_kernel = test_engine_kernel();
        let mut ctx = DispatchCtx {
            engine_kernel: &mut engine_kernel,
            resolver: None,
            focus_history: FocusHistory::default(),
            workspace: &mut workspace,
            viewport: (80, 24),
            cell_px: (1, 1),
            next_request_id: &mut next_request_id,
            pending_splits: &mut pending_splits,
            pending_windows: &mut pending_windows,
            expected_closes: &mut HashSet::new(),
            overlays: &mut overlays,
            keybindings: None,
            theme: &theme,
            sessions: &[],
            foreign_layouts: &HashMap::new(),
            foreign_agents: &HashMap::new(),
            focused_session: None,
            session_name: &mut session_name,
            switch_request: &mut switch_request,
            zoomed: &mut zoomed,
            sidebar: None,
            sidebar_enabled: &mut sidebar_enabled,
            sidebar_agents: &[],
            bar: None,
            status_bar: None,
            drag: &mut drag,
            mouse_optout: &mut mouse_optout,
            attention_navigation: &mut AttentionNavigation::default(),
            plugin_actions: &[],
            plugin_panes: &[],
            plugin_tx: None,
            reload_request: &mut reload_request,
            agent_meta: &fleet_agent_meta,
            vcs: &mut fleet_vcs,
        };
        let effects = run_action(
            &bare_action("toggle-sidebar"),
            &mut ctx,
            None,
            &HashMap::new(),
        );
        apply_action_effects(
            effects,
            &mut out,
            &mut conn,
            &mut ctx,
            &mut focused_pane,
            &mut detach_pending,
            &mut predict,
            &panes,
        )
        .await
        .expect("apply effects");
        assert!(!sidebar_enabled, "second toggle disables the sidebar");
    }

    #[test]
    fn rename_window_with_name_arg_renames_and_broadcasts() {
        let mut workspace = Workspace::single(tid(1)); // window "1"
        let mut action = bare_action("rename-window");
        action
            .args
            .insert("name".to_owned(), toml::Value::String("build".into()));
        let effects = run(&action, &mut workspace);
        assert_eq!(workspace.windows[0].name, "build");
        assert!(effects.layout_mutated);
        assert!(effects.set_metadata, "rename is shared window state");
    }

    /// Like [`run`], but returns the `OverlayState` so a test can assert
    /// an action pushed an overlay.
    fn run_capturing(
        action: &phux_config::keybind::ResolvedAction,
        workspace: &mut Workspace,
    ) -> (ActionEffects, OverlayState) {
        run_capturing_with_sessions(action, workspace, &[], None)
    }

    /// Like [`run_capturing`], but seeds the dispatcher's cached session
    /// graph so `session-picker` tests can drive the picker.
    fn run_capturing_with_sessions(
        action: &phux_config::keybind::ResolvedAction,
        workspace: &mut Workspace,
        sessions: &[phux_protocol::wire::info::SessionInfo],
        focused_session: Option<phux_protocol::ids::SessionId>,
    ) -> (ActionEffects, OverlayState) {
        let mut next_request_id = 100;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let mut overlays = OverlayState::new();
        let theme = Theme::default();
        let mut switch_request = None;
        let mut session_name = String::new();
        let mut zoomed = None;
        let mut sidebar_enabled = false;
        let mut drag: Option<DragGrab> = None;
        let mut mouse_optout: std::collections::HashSet<TerminalId> =
            std::collections::HashSet::new();
        let effects = {
            let mut reload_request = false;
            let fleet_agent_meta = HashMap::new();
            let mut fleet_vcs = crate::attach::driver::VcsIndex::default();
            let mut engine_kernel = test_engine_kernel();
            let mut ctx = DispatchCtx {
                engine_kernel: &mut engine_kernel,
                resolver: None,
                focus_history: FocusHistory::default(),
                workspace,
                viewport: (80, 24),
                cell_px: (1, 1),
                next_request_id: &mut next_request_id,
                pending_splits: &mut pending_splits,
                pending_windows: &mut pending_windows,
                expected_closes: &mut HashSet::new(),
                overlays: &mut overlays,
                keybindings: None,
                theme: &theme,
                sessions,
                foreign_layouts: &HashMap::new(),
                foreign_agents: &HashMap::new(),
                focused_session,
                session_name: &mut session_name,
                switch_request: &mut switch_request,
                zoomed: &mut zoomed,
                sidebar: None,
                sidebar_enabled: &mut sidebar_enabled,
                sidebar_agents: &[],
                bar: None,
                status_bar: None,
                drag: &mut drag,
                mouse_optout: &mut mouse_optout,
                attention_navigation: &mut AttentionNavigation::default(),
                plugin_actions: &[],
                plugin_panes: &[],
                plugin_tx: None,
                reload_request: &mut reload_request,
                agent_meta: &fleet_agent_meta,
                vcs: &mut fleet_vcs,
            };
            let focused = ctx.workspace.active_window().and_then(|w| w.focus.clone());
            run_action(action, &mut ctx, focused.as_ref(), &HashMap::new())
        };
        (effects, overlays)
    }

    #[test]
    fn rename_window_no_arg_opens_prompt() {
        let mut workspace = Workspace::single(tid(1)); // window "1"
        let (effects, overlays) = run_capturing(&bare_action("rename-window"), &mut workspace);
        assert!(
            overlays.is_active(),
            "no-arg rename should open the prompt overlay"
        );
        assert!(effects.layout_mutated);
        // Not renamed yet — that happens when the prompt commits.
        assert_eq!(workspace.windows[0].name, "1");
        assert!(!effects.set_metadata, "no broadcast until commit");
    }

    #[test]
    fn kill_window_on_empty_workspace_bells() {
        let mut workspace = Workspace::default();
        let effects = run(&bare_action("kill-window"), &mut workspace);
        assert!(effects.bell);
        assert!(effects.kill_frames.is_empty());
    }

    #[test]
    fn palette_committed_action_routes_through_run_action() {
        // A palette row's ResolvedAction, fed back through run_action,
        // produces the same effect a keybind would. Use `detach` — a row
        // whose effect is unambiguous.
        let cfg = phux_config::parse_str(
            phux_config::DEFAULT_CONFIG_TOML,
            std::path::Path::new("default.toml"),
        )
        .expect("default config parses");
        let items = crate::attach::action_registry::palette_items(Some(&cfg.keybindings), &[], &[]);
        let detach = items
            .iter()
            .find(|i| i.action.action == "detach")
            .expect("detach in palette");
        let mut workspace = Workspace::default();
        let effects = run(&detach.action, &mut workspace);
        assert!(effects.detach, "committing the detach palette row detaches");
    }

    #[test]
    fn plugin_action_records_run_intent_for_the_async_caller() {
        // phux-r82.5: the sync dispatcher never execs the plugin itself —
        // it records (plugin, action) and the async caller spawns the
        // child-process run so the input loop can't freeze on a plugin.
        let mut args = BTreeMap::new();
        args.insert(
            "plugin".to_owned(),
            toml::Value::String("com.example.tools".to_owned()),
        );
        args.insert(
            "action".to_owned(),
            toml::Value::String("summarize".to_owned()),
        );
        let action = phux_config::keybind::ResolvedAction {
            action: "plugin-action".to_owned(),
            args,
        };
        let mut workspace = Workspace::single(tid(1));
        let effects = run(&action, &mut workspace);
        assert_eq!(
            effects.run_plugin,
            Some(("com.example.tools".to_owned(), "summarize".to_owned()))
        );
        assert!(!effects.bell);
        assert!(!effects.layout_mutated, "no repaint for a spawned run");
    }

    #[test]
    fn plugin_action_missing_args_bells() {
        let mut workspace = Workspace::single(tid(1));
        let effects = run(&bare_action("plugin-action"), &mut workspace);
        assert!(effects.bell, "missing plugin/action args must bell");
        assert!(effects.run_plugin.is_none());
    }

    // ---------- phux-r82.7: plugin-pane placement routing ----------

    /// Build the `plugin-pane { plugin, pane }` dispatcher action.
    fn plugin_pane_action(plugin: &str, pane: &str) -> phux_config::keybind::ResolvedAction {
        let mut args = BTreeMap::new();
        args.insert("plugin".to_owned(), toml::Value::String(plugin.to_owned()));
        args.insert("pane".to_owned(), toml::Value::String(pane.to_owned()));
        phux_config::keybind::ResolvedAction {
            action: "plugin-pane".to_owned(),
            args,
        }
    }

    /// A hostable pane snapshot entry with the given placement.
    fn pane_entry(placement: HostedPlacement) -> PluginPaneEntry {
        PluginPaneEntry {
            plugin_id: "com.example.board".to_owned(),
            plugin_name: "Board".to_owned(),
            pane_id: "board".to_owned(),
            title: "Agent Board".to_owned(),
            placement,
            command: vec!["agent-board".to_owned(), "--watch".to_owned()],
            plugin_root: std::path::PathBuf::from("/plugins/board"),
        }
    }

    /// Like [`run`], but with a plugin-pane snapshot installed.
    fn run_with_panes(
        action: &phux_config::keybind::ResolvedAction,
        workspace: &mut Workspace,
        panes: &[PluginPaneEntry],
    ) -> ActionEffects {
        let mut next_request_id = 100;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let mut overlays = OverlayState::new();
        let theme = Theme::default();
        let mut switch_request = None;
        let mut session_name = String::new();
        let mut zoomed = None;
        let mut sidebar_enabled = false;
        let mut drag: Option<DragGrab> = None;
        let mut mouse_optout = std::collections::HashSet::new();
        let mut reload_request = false;
        let fleet_agent_meta = HashMap::new();
        let mut fleet_vcs = crate::attach::driver::VcsIndex::default();
        let mut engine_kernel = test_engine_kernel();
        let mut ctx = DispatchCtx {
            engine_kernel: &mut engine_kernel,
            resolver: None,
            focus_history: FocusHistory::default(),
            workspace,
            viewport: (80, 24),
            cell_px: (1, 1),
            next_request_id: &mut next_request_id,
            pending_splits: &mut pending_splits,
            pending_windows: &mut pending_windows,
            expected_closes: &mut HashSet::new(),
            overlays: &mut overlays,
            keybindings: None,
            theme: &theme,
            sessions: &[],
            foreign_layouts: &HashMap::new(),
            foreign_agents: &HashMap::new(),
            focused_session: None,
            session_name: &mut session_name,
            switch_request: &mut switch_request,
            zoomed: &mut zoomed,
            sidebar: None,
            sidebar_enabled: &mut sidebar_enabled,
            sidebar_agents: &[],
            bar: None,
            status_bar: None,
            drag: &mut drag,
            mouse_optout: &mut mouse_optout,
            attention_navigation: &mut AttentionNavigation::default(),
            plugin_actions: &[],
            plugin_panes: panes,
            plugin_tx: None,
            reload_request: &mut reload_request,
            agent_meta: &fleet_agent_meta,
            vcs: &mut fleet_vcs,
        };
        let focused = ctx.workspace.active_window().and_then(|w| w.focus.clone());
        run_action(action, &mut ctx, focused.as_ref(), &HashMap::new())
    }

    /// The spawn frame's plugin-relevant fields, destructured for
    /// assertions.
    struct SpawnParts {
        command: Option<Vec<String>>,
        cwd: Option<String>,
        env: Option<Vec<(String, String)>>,
    }

    fn spawn_frame_parts(frame: &FrameKind) -> SpawnParts {
        let FrameKind::SpawnTerminal {
            command, cwd, env, ..
        } = frame
        else {
            panic!("expected SpawnTerminal, got {frame:?}");
        };
        SpawnParts {
            command: command.clone(),
            cwd: cwd.clone(),
            env: env.clone(),
        }
    }

    #[test]
    fn plugin_pane_split_placement_parks_pending_split_with_argv_and_env() {
        let mut workspace = Workspace::single(tid(1));
        let effects = run_with_panes(
            &plugin_pane_action("com.example.board", "board"),
            &mut workspace,
            &[pane_entry(HostedPlacement::Split)],
        );
        let (_req, pending, frame) = effects
            .spawn_terminal
            .expect("split placement parks a PendingSplit + SPAWN");
        assert_eq!(pending.focused_at_request, tid(1));
        assert!(!pending.zoom_on_spawn, "plain split must not zoom");
        assert!(effects.spawn_window.is_none());
        let SpawnParts { command, cwd, env } = spawn_frame_parts(&frame);
        assert_eq!(
            command,
            Some(vec!["agent-board".to_owned(), "--watch".to_owned()]),
            "spawn runs the manifest argv, not the default shell",
        );
        assert_eq!(cwd.as_deref(), Some("/plugins/board"));
        let env = env.expect("identity env injected");
        assert!(env.contains(&("PHUX_PLUGIN_ID".to_owned(), "com.example.board".to_owned())));
        assert!(env.contains(&("PHUX_PLUGIN_PANE_ID".to_owned(), "board".to_owned())));
        assert!(env.contains(&("PHUX_PLUGIN_ROOT".to_owned(), "/plugins/board".to_owned())));
    }

    #[test]
    fn plugin_pane_zoomed_placement_requests_zoom_on_spawn() {
        let mut workspace = Workspace::single(tid(1));
        let effects = run_with_panes(
            &plugin_pane_action("com.example.board", "board"),
            &mut workspace,
            &[pane_entry(HostedPlacement::Zoomed)],
        );
        let (_req, pending, _frame) = effects
            .spawn_terminal
            .expect("zoomed placement parks a PendingSplit + SPAWN");
        assert!(pending.zoom_on_spawn, "zoomed placement zooms on reply");
    }

    #[test]
    fn plugin_pane_tab_placement_parks_pending_window_named_after_title() {
        let mut workspace = Workspace::single(tid(1));
        let effects = run_with_panes(
            &plugin_pane_action("com.example.board", "board"),
            &mut workspace,
            &[pane_entry(HostedPlacement::Tab)],
        );
        let (_req, pending, frame) = effects
            .spawn_window
            .expect("tab placement parks a PendingWindow + SPAWN");
        assert_eq!(pending.name, "Agent Board");
        assert!(effects.spawn_terminal.is_none());
        let SpawnParts { command, .. } = spawn_frame_parts(&frame);
        assert_eq!(
            command,
            Some(vec!["agent-board".to_owned(), "--watch".to_owned()])
        );
    }

    #[test]
    fn plugin_pane_unknown_entry_bells() {
        // Covers a disabled plugin, a typo'd id, or an overlay declaration
        // (never snapshotted) reached via a user-config binding.
        let mut workspace = Workspace::single(tid(1));
        let effects = run_with_panes(
            &plugin_pane_action("com.example.absent", "board"),
            &mut workspace,
            &[pane_entry(HostedPlacement::Split)],
        );
        assert!(effects.bell);
        assert!(effects.spawn_terminal.is_none());
        assert!(effects.spawn_window.is_none());
    }

    #[test]
    fn plugin_pane_split_without_focused_pane_bells() {
        let mut workspace = Workspace::default(); // empty: no focus
        let effects = run_with_panes(
            &plugin_pane_action("com.example.board", "board"),
            &mut workspace,
            &[pane_entry(HostedPlacement::Split)],
        );
        assert!(effects.bell);
        assert!(effects.spawn_terminal.is_none());
    }

    #[test]
    fn command_palette_action_pushes_overlay() {
        let mut workspace = Workspace::single(tid(1));
        let (effects, overlays) = run_capturing(&bare_action("command-palette"), &mut workspace);
        assert!(
            overlays.is_active(),
            "command-palette should push the palette overlay"
        );
        assert_eq!(overlays.depth(), 1);
        // No layout mutation — opening the palette doesn't touch windows.
        assert!(!effects.layout_mutated);
        assert!(!effects.bell);
    }

    #[test]
    fn window_picker_action_pushes_overlay_with_windows() {
        let mut workspace = Workspace::single(tid(1));
        workspace.add_window("2".to_owned(), tid(2));
        let (effects, overlays) = run_capturing(&bare_action("window-picker"), &mut workspace);
        assert!(overlays.is_active(), "window-picker should push an overlay");
        assert!(!effects.bell);
    }

    #[test]
    fn window_picker_on_empty_workspace_bells() {
        let mut workspace = Workspace::default();
        let (effects, overlays) = run_capturing(&bare_action("window-picker"), &mut workspace);
        assert!(!overlays.is_active(), "no windows ⇒ no overlay");
        assert!(effects.bell);
    }

    #[test]
    fn current_session_window_rows_label_index_name_and_pane_count() {
        let mut workspace = Workspace::single(tid(1)); // window "1", 1 pane
        workspace.add_window("editor".to_owned(), tid(2));
        let items = current_session_window_rows(&workspace);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].label, "0:1");
        assert_eq!(items[0].secondary.as_deref(), Some("1 pane"));
        assert!(items[0].indented, "window rows nest under their session");
        assert_eq!(items[1].label, "1:editor");
        // Each row commits select-window with its index.
        assert_eq!(items[1].action.action, "select-window");
        assert_eq!(
            items[1].action.args.get("index"),
            Some(&toml::Value::Integer(1))
        );
    }

    #[test]
    fn window_picker_groups_windows_under_their_session() {
        let mut workspace = Workspace::single(tid(1));
        workspace.add_window("editor".to_owned(), tid(2));
        let sessions = [sinfo(1, "work"), sinfo(2, "scratch")];
        let items = window_picker_items(
            &workspace,
            &sessions,
            &HashMap::new(),
            Some(phux_protocol::ids::SessionId::new(1)),
        );
        // Current session ("work") leads, as a header marked "(current)".
        assert!(items[0].is_header());
        assert_eq!(items[0].label, "work (current)");
        // Its windows nest directly beneath, selectable + indented.
        assert!(!items[1].is_header() && items[1].indented);
        assert_eq!(items[1].action.action, "select-window");
        assert_eq!(items[2].action.action, "select-window");
        // The foreign session is a header followed by a switch-session row.
        let scratch = items
            .iter()
            .position(|i| i.is_header() && i.label == "scratch")
            .expect("scratch header present");
        assert_eq!(items[scratch + 1].action.action, "switch-session");
        assert_eq!(
            items[scratch + 1].action.args.get("name"),
            Some(&toml::Value::String("scratch".to_owned())),
        );
        // No cached layout for "scratch" ⇒ no `window` arg (fallback row,
        // plain switch).
        assert!(!items[scratch + 1].action.args.contains_key("window"));
    }

    /// phux-foz.8: with a peer session's persisted layout cached, the
    /// picker lists that session's windows as one-step rows committing
    /// `switch-session { name, window }` — same `index:name` + pane-count
    /// shape as the current session's rows.
    #[test]
    fn window_picker_lists_foreign_windows_one_step_when_layout_cached() {
        let mut workspace = Workspace::single(tid(1));
        workspace.add_window("editor".to_owned(), tid(2));
        let sessions = [sinfo(1, "work"), sinfo(2, "scratch")];
        // scratch's persisted workspace: two windows, "build" and "logs".
        let mut scratch_ws = Workspace::single(tid(10));
        scratch_ws.rename_active("build".to_owned());
        scratch_ws.add_window("logs".to_owned(), tid(11));
        let mut foreign = HashMap::new();
        foreign.insert(phux_protocol::ids::SessionId::new(2), scratch_ws);

        let items = window_picker_items(
            &workspace,
            &sessions,
            &foreign,
            Some(phux_protocol::ids::SessionId::new(1)),
        );
        let scratch = items
            .iter()
            .position(|i| i.is_header() && i.label == "scratch")
            .expect("scratch header present");
        // Two one-step window rows, indented under the header.
        let row0 = &items[scratch + 1];
        let row1 = &items[scratch + 2];
        assert_eq!(row0.label, "0:build");
        assert_eq!(row0.secondary.as_deref(), Some("1 pane"));
        assert!(row0.indented);
        assert_eq!(row0.action.action, "switch-session");
        assert_eq!(
            row0.action.args.get("name"),
            Some(&toml::Value::String("scratch".to_owned())),
        );
        assert_eq!(
            row0.action.args.get("window"),
            Some(&toml::Value::Integer(0)),
        );
        assert_eq!(row1.label, "1:logs");
        assert_eq!(
            row1.action.args.get("window"),
            Some(&toml::Value::Integer(1)),
        );
        // No fallback "switch to this session" row when windows list.
        assert!(
            items.iter().all(|i| i.label != "switch to this session"),
            "one-step rows replace the fallback row"
        );
    }

    /// phux-foz.8: an empty cached workspace (decoded but windowless) is
    /// not useful — the picker falls back to the plain switch row.
    #[test]
    fn window_picker_empty_foreign_layout_falls_back_to_switch_row() {
        let workspace = Workspace::single(tid(1));
        let sessions = [sinfo(1, "work"), sinfo(2, "scratch")];
        let mut foreign = HashMap::new();
        foreign.insert(phux_protocol::ids::SessionId::new(2), Workspace::default());
        let items = window_picker_items(
            &workspace,
            &sessions,
            &foreign,
            Some(phux_protocol::ids::SessionId::new(1)),
        );
        let scratch = items
            .iter()
            .position(|i| i.is_header() && i.label == "scratch")
            .expect("scratch header present");
        assert_eq!(items[scratch + 1].label, "switch to this session");
        assert!(!items[scratch + 1].action.args.contains_key("window"));
    }

    /// phux-foz.8: committing a one-step picker row through `run_action`
    /// yields the combined reattach target — session name AND window index
    /// — that the driver resolves after the re-attach.
    #[test]
    fn one_step_picker_row_commits_switch_session_with_window() {
        let mut workspace = Workspace::single(tid(1));
        let mut scratch_ws = Workspace::single(tid(10));
        scratch_ws.add_window("logs".to_owned(), tid(11));
        let rows = foreign_session_window_rows("scratch", &scratch_ws);
        assert_eq!(rows.len(), 2);
        let effects = run(&rows[1].action, &mut workspace);
        assert_eq!(
            effects.reattach,
            Some(ReattachTarget::Existing {
                name: "scratch".to_owned(),
                window: Some(1),
                pane: None,
            }),
            "the one-step row carries the target window through dispatch"
        );
        // The switch is a re-attach, not a local window change.
        assert_eq!(workspace.active, 0);
    }

    /// phux-foz.8: a `switch-session` with a bad `window` arg (negative /
    /// non-integer) degrades to a plain switch rather than belling — the
    /// `name` is still valid and honoring it is strictly more useful.
    #[test]
    fn switch_session_bad_window_arg_degrades_to_plain_switch() {
        let mut workspace = Workspace::single(tid(1));
        let mut args = BTreeMap::new();
        args.insert("name".to_owned(), toml::Value::String("scratch".to_owned()));
        args.insert("window".to_owned(), toml::Value::Integer(-3));
        let action = phux_config::keybind::ResolvedAction {
            action: "switch-session".to_owned(),
            args,
        };
        let effects = run(&action, &mut workspace);
        assert_eq!(
            effects.reattach,
            Some(ReattachTarget::Existing {
                name: "scratch".to_owned(),
                window: None,
                pane: None,
            }),
        );
        assert!(!effects.bell);
    }

    /// phux-jpqd: a `switch-session { name, window, pane }` — the commit the
    /// agent-fleet dashboard's foreign pane rows carry — parses into the
    /// combined one-step cross-session pane target.
    #[test]
    fn switch_session_with_pane_arg_carries_one_step_pane_target() {
        let mut workspace = Workspace::single(tid(1));
        let mut args = BTreeMap::new();
        args.insert("name".to_owned(), toml::Value::String("scratch".to_owned()));
        args.insert("window".to_owned(), toml::Value::Integer(1));
        args.insert("pane".to_owned(), toml::Value::Integer(2));
        let action = phux_config::keybind::ResolvedAction {
            action: "switch-session".to_owned(),
            args,
        };
        let effects = run(&action, &mut workspace);
        assert_eq!(
            effects.reattach,
            Some(ReattachTarget::Existing {
                name: "scratch".to_owned(),
                window: Some(1),
                pane: Some(2),
            }),
        );
        assert!(!effects.bell);
        // The switch is a re-attach, not a local change.
        assert_eq!(workspace.active, 0);
    }

    #[test]
    fn window_picker_commit_routes_select_window_through_run_action() {
        // The architectural invariant: a picker selection commits a
        // select-window ResolvedAction that, when fed back through
        // run_action, performs the same per-client switch a numeric prefix
        // binding does.
        let mut workspace = Workspace::single(tid(1));
        workspace.add_window("2".to_owned(), tid(2));
        workspace.select(0); // active = 0
        let items = current_session_window_rows(&workspace);
        // Commit the picker row for window index 1.
        let effects = run(&items[1].action, &mut workspace);
        assert_eq!(
            workspace.active, 1,
            "select-window switched the active window"
        );
        assert!(effects.layout_mutated);
        assert_eq!(effects.set_focus, Some(tid(2)));
        assert!(!effects.set_metadata, "window switch is per-client");
    }

    // ---------- phux-oih5.16: client-local attention navigation ----------

    /// Run an attention action with caller-owned pane flags and excursion
    /// state, so successive dispatches exercise origin preservation.
    fn run_attention(
        action: &str,
        workspace: &mut Workspace,
        panes: &HashMap<TerminalId, PaneSlot>,
        navigation: &mut AttentionNavigation,
    ) -> ActionEffects {
        let mut next_request_id = 100;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let mut overlays = OverlayState::new();
        let theme = Theme::default();
        let mut switch_request = None;
        let mut session_name = String::new();
        let mut zoomed = None;
        let mut sidebar_enabled = false;
        let mut drag = None;
        let mut reload_request = false;
        let mut mouse_optout = std::collections::HashSet::new();
        let agent_meta = HashMap::new();
        let mut vcs = crate::attach::driver::VcsIndex::default();
        let focus_history = FocusHistory::default();
        let focused = workspace.active_window().and_then(|w| w.focus.clone());
        let mut engine_kernel = test_engine_kernel();
        let mut ctx = DispatchCtx {
            engine_kernel: &mut engine_kernel,
            resolver: None,
            workspace,
            viewport: (80, 24),
            cell_px: (1, 1),
            next_request_id: &mut next_request_id,
            pending_splits: &mut pending_splits,
            pending_windows: &mut pending_windows,
            expected_closes: &mut HashSet::new(),
            overlays: &mut overlays,
            keybindings: None,
            theme: &theme,
            sessions: &[],
            foreign_layouts: &HashMap::new(),
            foreign_agents: &HashMap::new(),
            focused_session: None,
            session_name: &mut session_name,
            switch_request: &mut switch_request,
            zoomed: &mut zoomed,
            sidebar: None,
            sidebar_enabled: &mut sidebar_enabled,
            sidebar_agents: &[],
            bar: None,
            status_bar: None,
            drag: &mut drag,
            mouse_optout: &mut mouse_optout,
            attention_navigation: navigation,
            focus_history,
            plugin_actions: &[],
            plugin_panes: &[],
            plugin_tx: None,
            reload_request: &mut reload_request,
            agent_meta: &agent_meta,
            vcs: &mut vcs,
        };
        run_action(&bare_action(action), &mut ctx, focused.as_ref(), panes)
    }

    fn asking_panes(ids: &[u32]) -> HashMap<TerminalId, PaneSlot> {
        ids.iter()
            .map(|id| {
                let terminal = tid(*id);
                let mut slot = PaneSlot::new_with_size(20, 4).expect("pane slot");
                slot.attention = true;
                (terminal, slot)
            })
            .collect()
    }

    #[test]
    fn next_attention_with_no_attention_is_a_local_bell_noop() {
        let mut workspace = fleet_workspace();
        workspace.select(0);
        let before = workspace.clone();
        let mut navigation = AttentionNavigation::default();
        let effects = run_attention(
            "next-attention",
            &mut workspace,
            &HashMap::new(),
            &mut navigation,
        );
        assert!(effects.bell);
        assert!(!effects.layout_mutated);
        assert!(!effects.set_metadata);
        assert_eq!(workspace, before);
        assert!(
            navigation.take_origin().is_none(),
            "no jump must not save an origin"
        );
    }

    #[test]
    fn next_attention_cycles_window_then_dfs_with_wrap_and_one_origin() {
        // fleet_workspace is window 0 DFS [1,2], then window 1 DFS [3].
        let mut workspace = fleet_workspace();
        workspace.select(0);
        let panes = asking_panes(&[2, 3]);
        let mut navigation = AttentionNavigation::default();

        let first = run_attention("next-attention", &mut workspace, &panes, &mut navigation);
        assert_eq!(workspace.active, 0);
        assert_eq!(workspace.windows[0].state.focus, Some(tid(2)));
        assert_eq!(first.set_focus, Some(tid(2)));
        assert!(!first.set_metadata, "attention focus is never shared");

        let cross = run_attention("next-attention", &mut workspace, &panes, &mut navigation);
        assert_eq!(
            workspace.active, 1,
            "cycle crosses windows in display order"
        );
        assert_eq!(cross.set_focus, Some(tid(3)));
        assert!(!cross.set_metadata);

        let wrapped = run_attention("next-attention", &mut workspace, &panes, &mut navigation);
        assert_eq!(workspace.active, 0, "last asking pane wraps to the first");
        assert_eq!(wrapped.set_focus, Some(tid(2)));
        assert!(!wrapped.set_metadata);

        let returned = run_attention(
            "return-from-attention",
            &mut workspace,
            &panes,
            &mut navigation,
        );
        assert_eq!(
            returned.set_focus,
            Some(tid(1)),
            "cycling kept the first origin"
        );
        assert_eq!(workspace.windows[0].state.focus, Some(tid(1)));
        assert!(!returned.set_metadata);

        let consumed = run_attention(
            "return-from-attention",
            &mut workspace,
            &panes,
            &mut navigation,
        );
        assert!(consumed.bell, "return consumes the single saved origin");
        assert!(!consumed.layout_mutated);
        assert!(!consumed.set_metadata);
    }

    #[test]
    fn return_from_attention_consumes_a_stale_origin_safely() {
        let mut workspace = fleet_workspace();
        workspace.select(0);
        let panes = asking_panes(&[2]);
        let mut navigation = AttentionNavigation::default();
        let jumped = run_attention("next-attention", &mut workspace, &panes, &mut navigation);
        assert_eq!(jumped.set_focus, Some(tid(2)));

        // The original pane closes while the user is examining the question.
        workspace.windows[0].state = crate::layout::LayoutState::single(tid(2));
        let before = workspace.clone();
        let stale = run_attention(
            "return-from-attention",
            &mut workspace,
            &panes,
            &mut navigation,
        );
        assert!(stale.bell);
        assert!(!stale.layout_mutated);
        assert!(stale.set_focus.is_none());
        assert!(!stale.set_metadata);
        assert_eq!(
            workspace, before,
            "stale return must not focus another pane"
        );

        let consumed = run_attention(
            "return-from-attention",
            &mut workspace,
            &panes,
            &mut navigation,
        );
        assert!(
            consumed.bell,
            "stale origin is consumed on the first return"
        );
        assert!(!consumed.layout_mutated);
    }

    // ---------- phux-foz.7: agent-fleet dashboard + focus-pane ----------

    #[test]
    fn agent_fleet_action_pushes_overlay() {
        let mut workspace = Workspace::single(tid(1));
        let (effects, overlays) = run_capturing(&bare_action("agent-fleet"), &mut workspace);
        assert!(overlays.is_active(), "agent-fleet should push the overlay");
        assert_eq!(overlays.depth(), 1);
        assert!(!effects.bell);
    }

    #[test]
    fn agent_fleet_on_empty_workspace_bells() {
        let mut workspace = Workspace::default();
        let (effects, overlays) = run_capturing(&bare_action("agent-fleet"), &mut workspace);
        assert!(!overlays.is_active(), "nothing to list => no overlay");
        assert!(effects.bell);
    }

    #[test]
    fn agent_fleet_overlay_accepts_live_fleet_refresh() {
        // The pushed overlay is constructed with the fleet live key, so the
        // driver's push-based refresh (rows rebuilt when an agent event
        // lands) reaches it in place.
        let mut workspace = Workspace::single(tid(1));
        let (_effects, mut overlays) = run_capturing(&bare_action("agent-fleet"), &mut workspace);
        let fresh = crate::attach::fleet::fleet_items(
            &workspace,
            &[],
            None,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert!(
            overlays.refresh_items(crate::attach::fleet::FLEET_LIVE_KEY, &fresh),
            "the fleet overlay must accept a matching live refresh"
        );
    }

    #[test]
    fn command_palette_ignores_fleet_refresh() {
        // Static overlays (the palette, the pickers) must never swap their
        // rows for fleet data.
        let mut workspace = Workspace::single(tid(1));
        let (_effects, mut overlays) =
            run_capturing(&bare_action("command-palette"), &mut workspace);
        assert!(
            !overlays.refresh_items(crate::attach::fleet::FLEET_LIVE_KEY, &[]),
            "a static overlay must ignore the fleet refresh"
        );
    }

    /// Window 0 split into panes 1|2, window 1 a single pane 3.
    fn fleet_workspace() -> Workspace {
        use crate::layout::{LayoutNode, LayoutState, SplitDir, WindowState, split_at};
        let tree = split_at(
            &LayoutNode::Leaf(tid(1)),
            &tid(1),
            &tid(2),
            SplitDir::Horizontal,
            0.5,
        )
        .unwrap();
        Workspace {
            windows: vec![
                WindowState {
                    name: "main".to_owned(),
                    state: LayoutState {
                        tree: Some(tree),
                        focus: Some(tid(1)),
                    },
                },
                WindowState {
                    name: "logs".to_owned(),
                    state: LayoutState::single(tid(3)),
                },
            ],
            active: 1,
        }
    }

    #[test]
    fn focus_pane_switches_window_and_focuses_leaf() {
        let mut workspace = fleet_workspace(); // active = window 1
        let mut action = bare_action("focus-pane");
        action
            .args
            .insert("window".to_owned(), toml::Value::Integer(0));
        action
            .args
            .insert("pane".to_owned(), toml::Value::Integer(1));
        let effects = run(&action, &mut workspace);
        assert_eq!(workspace.active, 0, "switched to the target window");
        assert_eq!(
            workspace.windows[0].state.focus,
            Some(tid(2)),
            "focus landed on the second DFS leaf"
        );
        assert_eq!(effects.set_focus, Some(tid(2)));
        assert!(effects.layout_mutated);
        assert!(!effects.set_metadata, "focus is per-client, no broadcast");
        assert!(!effects.bell);
    }

    #[test]
    fn focus_pane_within_active_window_moves_focus_only() {
        let mut workspace = fleet_workspace();
        workspace.select(0); // active = 0, focus = tid(1)
        let mut action = bare_action("focus-pane");
        action
            .args
            .insert("window".to_owned(), toml::Value::Integer(0));
        action
            .args
            .insert("pane".to_owned(), toml::Value::Integer(1));
        let effects = run(&action, &mut workspace);
        assert_eq!(workspace.active, 0);
        assert_eq!(workspace.windows[0].state.focus, Some(tid(2)));
        assert_eq!(effects.set_focus, Some(tid(2)));
    }

    #[test]
    fn focus_pane_missing_args_bells() {
        let mut workspace = fleet_workspace();
        let effects = run(&bare_action("focus-pane"), &mut workspace);
        assert!(effects.bell);
        assert!(effects.set_focus.is_none());
    }

    #[test]
    fn focus_pane_stale_coordinates_bell_without_mutation() {
        // The fleet rows may outlive a layout change; a stale (window, pane)
        // address must bell rather than focus the wrong pane.
        let mut workspace = fleet_workspace();
        let mut action = bare_action("focus-pane");
        action
            .args
            .insert("window".to_owned(), toml::Value::Integer(0));
        action
            .args
            .insert("pane".to_owned(), toml::Value::Integer(9));
        let effects = run(&action, &mut workspace);
        assert!(effects.bell);
        assert_eq!(workspace.active, 1, "no window switch on a stale address");
        assert!(effects.set_focus.is_none());
    }

    #[test]
    fn fleet_commit_routes_focus_pane_through_run_action() {
        // The architectural invariant: a fleet row's committed ResolvedAction,
        // fed back through run_action, performs the same per-client focus a
        // keybinding path would.
        let mut workspace = fleet_workspace();
        let items = crate::attach::fleet::fleet_items(
            &workspace,
            &[],
            None,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        // Row 1 is window 0's second pane (tid 2).
        let effects = run(&items[1].action.clone(), &mut workspace);
        assert_eq!(workspace.active, 0);
        assert_eq!(effects.set_focus, Some(tid(2)));
    }

    fn sinfo(id: u32, name: &str) -> phux_protocol::wire::info::SessionInfo {
        phux_protocol::wire::info::SessionInfo::new(phux_protocol::ids::SessionId::new(id), name)
            .with_window_count(1)
    }

    #[test]
    fn session_picker_items_include_focused_first_and_commit_switch_session() {
        let sessions = [sinfo(1, "work"), sinfo(2, "scratch"), sinfo(3, "logs")];
        let items = session_picker_items(&sessions, Some(phux_protocol::ids::SessionId::new(1)));
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].label, "work");
        assert_eq!(items[0].secondary.as_deref(), Some("1 window, current"));
        assert_eq!(items[1].label, "logs");
        assert_eq!(items[2].label, "scratch");
        // Each row commits switch-session with the session name.
        assert_eq!(items[0].action.action, "switch-session");
        assert_eq!(
            items[0].action.args.get("name"),
            Some(&toml::Value::String("work".to_owned()))
        );
        assert_eq!(items[1].secondary.as_deref(), Some("1 window"));
    }

    #[test]
    fn session_picker_action_pushes_overlay_with_peer_sessions() {
        let mut workspace = Workspace::single(tid(1));
        let sessions = [sinfo(1, "work"), sinfo(2, "scratch")];
        let (effects, overlays) = run_capturing_with_sessions(
            &bare_action("session-picker"),
            &mut workspace,
            &sessions,
            Some(phux_protocol::ids::SessionId::new(1)),
        );
        assert!(
            overlays.is_active(),
            "session-picker should push an overlay"
        );
        assert!(!effects.bell);
    }

    #[test]
    fn session_picker_with_only_current_session_still_opens_for_new() {
        // Even when the client's own session is the only one, the picker
        // opens so the user can create a new session via the "+ New
        // session" row — it no longer bells into a dead end.
        let mut workspace = Workspace::single(tid(1));
        let sessions = [sinfo(1, "work")];
        let (effects, overlays) = run_capturing_with_sessions(
            &bare_action("session-picker"),
            &mut workspace,
            &sessions,
            Some(phux_protocol::ids::SessionId::new(1)),
        );
        assert!(overlays.is_active(), "picker opens to offer + New session");
        assert!(!effects.bell);
    }

    #[test]
    fn session_picker_with_no_sessions_still_opens_for_new() {
        // Before the first ATTACHED snapshot lands the cache is empty; the
        // picker still opens with the "+ New session" row.
        let mut workspace = Workspace::single(tid(1));
        let (effects, overlays) =
            run_capturing_with_sessions(&bare_action("session-picker"), &mut workspace, &[], None);
        assert!(overlays.is_active());
        assert!(!effects.bell);
    }

    #[test]
    fn session_picker_commit_routes_switch_session_through_run_action() {
        // The architectural invariant: a picker row commits a
        // switch-session ResolvedAction that, fed back through run_action,
        // yields the reattach effect keyed by the chosen name.
        let mut workspace = Workspace::single(tid(1));
        let sessions = [sinfo(1, "work"), sinfo(2, "scratch")];
        let items = session_picker_items(&sessions, Some(phux_protocol::ids::SessionId::new(1)));
        let scratch = items
            .iter()
            .find(|item| item.label == "scratch")
            .expect("peer session row");
        let effects = run(&scratch.action, &mut workspace);
        assert_eq!(
            effects.reattach,
            Some(ReattachTarget::Existing {
                name: "scratch".to_owned(),
                window: None,
                pane: None,
            }),
            "committing the picker row requests a switch to that session"
        );
    }

    #[test]
    fn switch_session_missing_name_bells() {
        let mut workspace = Workspace::single(tid(1));
        let effects = run(&bare_action("switch-session"), &mut workspace);
        assert!(effects.reattach.is_none());
        assert!(effects.bell, "a switch-session with no name arg bells");
    }

    #[test]
    fn new_session_with_name_requests_create_reattach() {
        let mut workspace = Workspace::single(tid(1));
        let mut args = BTreeMap::new();
        args.insert("name".to_owned(), toml::Value::String("scratch".to_owned()));
        let action = phux_config::keybind::ResolvedAction {
            action: "new-session".to_owned(),
            args,
        };
        let effects = run(&action, &mut workspace);
        assert_eq!(
            effects.reattach,
            Some(ReattachTarget::Create("scratch".to_owned())),
            "new-session with a name requests a create-and-switch"
        );
    }

    #[test]
    fn new_session_without_name_opens_prompt() {
        let mut workspace = Workspace::single(tid(1));
        let (effects, overlays) = run_capturing(&bare_action("new-session"), &mut workspace);
        assert!(
            overlays.is_active(),
            "new-session with no name opens the name prompt"
        );
        assert!(
            effects.reattach.is_none(),
            "the prompt commit drives the re-attach later"
        );
    }

    #[test]
    fn detach_action_requests_detach_effect() {
        let mut workspace = Workspace::default();
        let mut next_request_id = 1;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let mut overlays = OverlayState::new();
        let theme = Theme::default();
        let mut switch_request = None;
        let mut session_name = String::new();
        let mut zoomed = None;
        let mut sidebar_enabled = false;
        let mut drag: Option<DragGrab> = None;
        let mut reload_request = false;
        let mut mouse_optout: std::collections::HashSet<TerminalId> =
            std::collections::HashSet::new();
        let fleet_agent_meta = HashMap::new();
        let mut fleet_vcs = crate::attach::driver::VcsIndex::default();
        let mut engine_kernel = test_engine_kernel();
        let mut ctx = DispatchCtx {
            engine_kernel: &mut engine_kernel,
            resolver: None,
            focus_history: FocusHistory::default(),
            workspace: &mut workspace,
            viewport: (80, 24),
            cell_px: (1, 1),
            next_request_id: &mut next_request_id,
            pending_splits: &mut pending_splits,
            pending_windows: &mut pending_windows,
            expected_closes: &mut HashSet::new(),
            overlays: &mut overlays,
            keybindings: None,
            theme: &theme,
            sessions: &[],
            foreign_layouts: &HashMap::new(),
            foreign_agents: &HashMap::new(),
            focused_session: None,
            session_name: &mut session_name,
            switch_request: &mut switch_request,
            zoomed: &mut zoomed,
            sidebar: None,
            sidebar_enabled: &mut sidebar_enabled,
            sidebar_agents: &[],
            bar: None,
            status_bar: None,
            drag: &mut drag,
            mouse_optout: &mut mouse_optout,
            attention_navigation: &mut AttentionNavigation::default(),
            plugin_actions: &[],
            plugin_panes: &[],
            plugin_tx: None,
            reload_request: &mut reload_request,
            agent_meta: &fleet_agent_meta,
            vcs: &mut fleet_vcs,
        };
        let action = phux_config::keybind::ResolvedAction {
            action: "detach".to_owned(),
            args: BTreeMap::new(),
        };

        let effects = run_action(&action, &mut ctx, None, &HashMap::new());

        assert!(effects.detach);
        assert!(!effects.layout_mutated);
    }

    #[test]
    fn rename_session_with_name_arg_requests_rename_effect() {
        // An explicit `name` produces the rename-session effect carrying the
        // new name; no prompt is opened. The send + local-name update happen
        // in `apply_action_effects` (async), so run_action only sets the
        // effect.
        let mut workspace = Workspace::single(tid(1));
        let mut args = BTreeMap::new();
        args.insert("name".to_owned(), toml::Value::String("notes".to_owned()));
        let action = phux_config::keybind::ResolvedAction {
            action: "rename-session".to_owned(),
            args,
        };
        let effects = run(&action, &mut workspace);
        assert_eq!(
            effects.rename_session.as_deref(),
            Some("notes"),
            "rename-session with a name requests the rename effect",
        );
    }

    #[test]
    fn rename_session_without_name_opens_prompt_prefilled() {
        // No `name` arg opens the prompt pre-filled with the current session
        // name; the rename itself is deferred to the prompt commit.
        let mut workspace = Workspace::single(tid(1));
        let mut next_request_id = 100;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let mut overlays = OverlayState::new();
        let theme = Theme::default();
        let mut switch_request = None;
        let mut session_name = "work".to_owned();
        let mut zoomed = None;
        let mut sidebar_enabled = false;
        let mut drag: Option<DragGrab> = None;
        let mut mouse_optout: std::collections::HashSet<TerminalId> =
            std::collections::HashSet::new();
        let effects = {
            let mut reload_request = false;
            let fleet_agent_meta = HashMap::new();
            let mut fleet_vcs = crate::attach::driver::VcsIndex::default();
            let mut engine_kernel = test_engine_kernel();
            let mut ctx = DispatchCtx {
                engine_kernel: &mut engine_kernel,
                resolver: None,
                focus_history: FocusHistory::default(),
                workspace: &mut workspace,
                viewport: (80, 24),
                cell_px: (1, 1),
                next_request_id: &mut next_request_id,
                pending_splits: &mut pending_splits,
                pending_windows: &mut pending_windows,
                expected_closes: &mut HashSet::new(),
                overlays: &mut overlays,
                keybindings: None,
                theme: &theme,
                sessions: &[],
                foreign_layouts: &HashMap::new(),
                foreign_agents: &HashMap::new(),
                focused_session: None,
                session_name: &mut session_name,
                switch_request: &mut switch_request,
                zoomed: &mut zoomed,
                sidebar: None,
                sidebar_enabled: &mut sidebar_enabled,
                sidebar_agents: &[],
                bar: None,
                status_bar: None,
                drag: &mut drag,
                mouse_optout: &mut mouse_optout,
                attention_navigation: &mut AttentionNavigation::default(),
                plugin_actions: &[],
                plugin_panes: &[],
                plugin_tx: None,
                reload_request: &mut reload_request,
                agent_meta: &fleet_agent_meta,
                vcs: &mut fleet_vcs,
            };
            run_action(
                &bare_action("rename-session"),
                &mut ctx,
                None,
                &HashMap::new(),
            )
        };
        assert!(
            overlays.is_active(),
            "no-arg rename-session opens the name prompt",
        );
        assert!(
            effects.rename_session.is_none(),
            "the prompt commit drives the rename later",
        );
    }

    #[test]
    fn rename_session_prompt_commits_rename_session_action() {
        // The prompt the bare action opens must commit a
        // `rename-session { name }` ResolvedAction, so feeding it back
        // through run_action yields the rename effect (the same single
        // dispatch path rename-window uses).
        use crate::render::overlay::{OverlayCommand, RenderOverlay};
        use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

        let mut prompt = PromptOverlay::rename_session("work", &Theme::default());
        let press = |key: PhysicalKey, text: Option<&str>| KeyEvent {
            action: KeyAction::Press,
            key,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: text.map(ToOwned::to_owned),
            unshifted_codepoint: None,
        };
        // Clear the prefilled "work" and type "notes".
        for _ in 0..4 {
            let _ = prompt.handle_key(&press(PhysicalKey::Backspace, None));
        }
        for ch in ['n', 'o', 't', 'e', 's'] {
            let _ = prompt.handle_key(&press(PhysicalKey::A, Some(&ch.to_string())));
        }
        let OverlayCommand::Commit(resolved) = prompt.handle_key(&press(PhysicalKey::Enter, None))
        else {
            panic!("Enter on a non-empty prompt should commit");
        };
        assert_eq!(resolved.action, "rename-session");

        let mut workspace = Workspace::single(tid(1));
        let effects = run(&resolved, &mut workspace);
        assert_eq!(
            effects.rename_session.as_deref(),
            Some("notes"),
            "the committed prompt action yields the rename effect with the typed name",
        );
    }

    // -- overlay input routing (regression: prefix key swallowed) ---------
    // The `RecordingOverlay` test double lives in `crate::render::overlay`
    // because implementing `RenderOverlay::render` names ratatui types, which
    // the boundary guard confines to `render/`.

    /// Regression (wave-hunt/client-tui): while an overlay is active the
    /// keybind resolver must be bypassed, so the leader prefix key (and any
    /// mid-chord key) reaches the overlay as literal input instead of being
    /// swallowed by the resolver.
    ///
    /// Pre-fix: feeding `C-a` (the default leader) while an overlay was up
    /// returned `ChordOutcome::Partial`, hit `continue`, and the key never
    /// reached the overlay — *and* left the resolver mid-chord so it
    /// intercepted the next key too. A user typing a name into the rename
    /// prompt that contained the leader chord lost characters.
    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the fixture's DispatchCtx grows a line per composed feature (wave-2 + wave-2.5); the scenario itself is one flow"
    )]
    async fn overlay_active_prefix_key_reaches_overlay_not_resolver() {
        use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

        let cfg = phux_config::parse_str(
            phux_config::DEFAULT_CONFIG_TOML,
            std::path::Path::new("default.toml"),
        )
        .expect("default config parses");
        let mut resolver =
            phux_config::keybind::Resolver::new(&cfg.keybindings).expect("resolver builds");

        // Record what the overlay receives.
        let keys = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut overlays = OverlayState::new();
        overlays.push(Box::new(crate::render::overlay::RecordingOverlay {
            keys: keys.clone(),
        }));

        let (a, _b) = tokio::net::UnixStream::pair().expect("uds pair");
        let mut conn = Connection::from_stream(a);
        let mut out: Vec<u8> = Vec::new();
        let mut workspace = Workspace::single(tid(1));
        let mut focused_pane = Some(tid(1));
        let mut detach_pending = false;
        let mut predict =
            PredictionState::new(crate::predict::PredictiveConfig::disabled(), 80, 24);
        let overlay = Overlay;
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        let mut next_request_id = 1;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let theme = Theme::default();
        let mut switch_request = None;
        let mut session_name = String::new();

        // The default leader is `C-a`. Feed it, then a printable key.
        let leader = KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::A,
            mods: ModSet::CTRL,
            consumed_mods: ModSet::CTRL,
            composing: false,
            text: None,
            unshifted_codepoint: Some(u32::from(b'a')),
        };
        let letter = KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::X,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: Some("x".to_owned()),
            unshifted_codepoint: Some(u32::from(b'x')),
        };

        let mut zoomed = None;
        let mut sidebar_enabled = false;
        let mut drag: Option<DragGrab> = None;
        let mut reload_request = false;
        let mut mouse_optout: std::collections::HashSet<TerminalId> =
            std::collections::HashSet::new();
        let fleet_agent_meta = HashMap::new();
        let mut fleet_vcs = crate::attach::driver::VcsIndex::default();
        let mut engine_kernel = test_engine_kernel();
        let mut ctx = DispatchCtx {
            engine_kernel: &mut engine_kernel,
            resolver: Some(&mut resolver),
            focus_history: FocusHistory::default(),
            workspace: &mut workspace,
            viewport: (80, 24),
            cell_px: (1, 1),
            next_request_id: &mut next_request_id,
            pending_splits: &mut pending_splits,
            pending_windows: &mut pending_windows,
            expected_closes: &mut HashSet::new(),
            overlays: &mut overlays,
            keybindings: None,
            theme: &theme,
            sessions: &[],
            foreign_layouts: &HashMap::new(),
            foreign_agents: &HashMap::new(),
            focused_session: None,
            session_name: &mut session_name,
            switch_request: &mut switch_request,
            zoomed: &mut zoomed,
            sidebar: None,
            sidebar_enabled: &mut sidebar_enabled,
            sidebar_agents: &[],
            bar: None,
            status_bar: None,
            drag: &mut drag,
            mouse_optout: &mut mouse_optout,
            attention_navigation: &mut AttentionNavigation::default(),
            plugin_actions: &[],
            plugin_panes: &[],
            plugin_tx: None,
            reload_request: &mut reload_request,
            agent_meta: &fleet_agent_meta,
            vcs: &mut fleet_vcs,
        };
        dispatch_input_events(
            &mut out,
            &mut conn,
            vec![InputEvent::Key(leader), InputEvent::Key(letter)],
            &mut focused_pane,
            &mut detach_pending,
            &mut predict,
            &overlay,
            &mut panes,
            &mut ctx,
        )
        .await
        .expect("dispatch");

        let received = keys.borrow();
        assert_eq!(
            received.len(),
            2,
            "both the leader chord and the following key must reach the overlay; got {received:?}",
        );
        assert_eq!(received[0].key, PhysicalKey::A);
        assert!(received[0].mods.contains(ModSet::CTRL));
        assert_eq!(received[1].key, PhysicalKey::X);
    }

    // -- which-key popup passthrough (phux-foz.2) --------------------------

    /// Drive `dispatch_input_events` with the given events against a
    /// resolver already pending at the prefix and the which-key popup on
    /// the overlay stack. Returns `(overlays_active_after, detach_pending,
    /// resolver_pending_after)`.
    #[allow(
        clippy::future_not_send,
        reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
    )]
    async fn dispatch_with_which_key_popup(events: Vec<InputEvent>) -> (bool, bool, bool) {
        let cfg = phux_config::parse_str(
            phux_config::DEFAULT_CONFIG_TOML,
            std::path::Path::new("default.toml"),
        )
        .expect("default config parses");
        let mut resolver =
            phux_config::keybind::Resolver::new(&cfg.keybindings).expect("resolver builds");
        // Walk to the pending-prefix state the popup describes.
        let prefix =
            phux_config::keybind::parse_chord(&cfg.keybindings.prefix).expect("prefix parses");
        assert_eq!(resolver.feed(prefix), phux_config::keybind::Feed::Partial);
        assert!(resolver.pending_at_prefix());

        let theme = Theme::default();
        let mut overlays = OverlayState::new();
        overlays.push(Box::new(
            crate::render::overlay::WhichKeyOverlay::from_config(&cfg.keybindings, &theme),
        ));
        assert!(overlays.top_is_passthrough());

        let (a, _b) = tokio::net::UnixStream::pair().expect("uds pair");
        let mut conn = Connection::from_stream(a);
        let mut out: Vec<u8> = Vec::new();
        let mut workspace = Workspace::single(tid(1));
        let mut focused_pane = Some(tid(1));
        let mut detach_pending = false;
        let mut predict =
            PredictionState::new(crate::predict::PredictiveConfig::disabled(), 80, 24);
        let overlay = Overlay;
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        let mut next_request_id = 1;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let mut switch_request = None;
        let mut session_name = String::new();
        let mut zoomed = None;
        let mut sidebar_enabled = false;
        let mut drag: Option<DragGrab> = None;
        let mut reload_request = false;
        let mut mouse_optout: std::collections::HashSet<TerminalId> =
            std::collections::HashSet::new();
        let fleet_agent_meta = HashMap::new();
        let mut fleet_vcs = crate::attach::driver::VcsIndex::default();
        let mut engine_kernel = test_engine_kernel();
        let mut ctx = DispatchCtx {
            engine_kernel: &mut engine_kernel,
            resolver: Some(&mut resolver),
            focus_history: FocusHistory::default(),
            workspace: &mut workspace,
            viewport: (80, 24),
            cell_px: (1, 1),
            next_request_id: &mut next_request_id,
            pending_splits: &mut pending_splits,
            pending_windows: &mut pending_windows,
            expected_closes: &mut HashSet::new(),
            overlays: &mut overlays,
            keybindings: Some(&cfg.keybindings),
            theme: &theme,
            sessions: &[],
            foreign_layouts: &HashMap::new(),
            foreign_agents: &HashMap::new(),
            focused_session: None,
            session_name: &mut session_name,
            switch_request: &mut switch_request,
            zoomed: &mut zoomed,
            sidebar: None,
            sidebar_enabled: &mut sidebar_enabled,
            sidebar_agents: &[],
            bar: None,
            status_bar: None,
            drag: &mut drag,
            mouse_optout: &mut mouse_optout,
            attention_navigation: &mut AttentionNavigation::default(),
            plugin_actions: &[],
            plugin_panes: &[],
            plugin_tx: None,
            reload_request: &mut reload_request,
            agent_meta: &fleet_agent_meta,
            vcs: &mut fleet_vcs,
        };
        dispatch_input_events(
            &mut out,
            &mut conn,
            events,
            &mut focused_pane,
            &mut detach_pending,
            &mut predict,
            &overlay,
            &mut panes,
            &mut ctx,
        )
        .await
        .expect("dispatch");
        (overlays.is_active(), detach_pending, resolver.is_pending())
    }

    fn press(key: phux_protocol::input::key::PhysicalKey, text: Option<&str>) -> InputEvent {
        use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet};
        InputEvent::Key(KeyEvent {
            action: KeyAction::Press,
            key,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: text.map(ToOwned::to_owned),
            unshifted_codepoint: text.and_then(|t| t.chars().next()).map(u32::from),
        })
    }

    /// phux-foz.2 requirement 3 (execute path): with the which-key popup
    /// up and the prefix pending, the next key must dismiss the popup AND
    /// execute its prefix-table binding exactly as if the popup had never
    /// appeared — the popup eats nothing.
    #[tokio::test]
    async fn which_key_popup_next_chord_dismisses_and_executes() {
        use phux_protocol::input::key::PhysicalKey;
        // Default prefix table binds `d` = detach.
        let (overlay_active, detach_pending, resolver_pending) =
            dispatch_with_which_key_popup(vec![press(PhysicalKey::D, Some("d"))]).await;
        assert!(!overlay_active, "the chord must dismiss the popup");
        assert!(
            detach_pending,
            "the chord must still execute its binding (C-a d = detach)"
        );
        assert!(!resolver_pending, "the chord resolved; nothing pending");
    }

    /// phux-foz.2 requirement 3 (cancel path): Esc dismisses the popup
    /// and cancels the pending prefix — the binding does NOT run, and a
    /// following prefix-table key is a plain keystroke for the pane.
    #[tokio::test]
    async fn which_key_popup_esc_cancels_the_prefix() {
        use phux_protocol::input::key::PhysicalKey;
        let (overlay_active, detach_pending, resolver_pending) =
            dispatch_with_which_key_popup(vec![
                press(PhysicalKey::Escape, None),
                // With the prefix cancelled, `d` must NOT resolve to detach.
                press(PhysicalKey::D, Some("d")),
            ])
            .await;
        assert!(!overlay_active, "Esc must dismiss the popup");
        assert!(!resolver_pending, "Esc must cancel the pending prefix");
        assert!(
            !detach_pending,
            "after Esc, `d` is a plain pane keystroke, not `C-a d`"
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "full copy-mode page-up/page-down round trip needs a complete DispatchCtx fixture, which grows a line per composed feature (phux-foz.9 sidebar_agents, phux-foz.12 status-bar lend)"
    )]
    #[tokio::test]
    async fn copy_mode_page_scroll_mutates_focused_terminal_viewport() {
        use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

        fn visible_prefix(
            kernel: &super::super::driver::AttachKernel,
            panes: &mut HashMap<TerminalId, PaneSlot>,
            id: &TerminalId,
            row: u16,
        ) -> String {
            let slot = panes.get_mut(id).expect("pane");
            let terminal =
                super::super::driver::published_terminal(kernel, id).expect("published terminal");
            (0..6)
                .filter_map(|col| {
                    slot.renderer
                        .read_grapheme_string_at(terminal, row, col)
                        .expect("read cell")
                })
                .collect()
        }

        let (a, _b) = tokio::net::UnixStream::pair().expect("uds pair");
        let mut conn = Connection::from_stream(a);
        let mut out: Vec<u8> = Vec::new();
        let mut workspace = Workspace::single(tid(1));
        let mut focused_pane = Some(tid(1));
        let mut detach_pending = false;
        let mut predict = PredictionState::new(crate::predict::PredictiveConfig::disabled(), 8, 4);
        let overlay = Overlay;
        let mut replay = Vec::new();
        for n in 0..10 {
            replay.extend_from_slice(format!("line{n:02}\r\n").as_bytes());
        }
        let (mut engine_kernel, _, mut panes) =
            super::super::driver::published_test_state(&[(&tid(1), 8, 4, &replay)]);

        let before = visible_prefix(&engine_kernel, &mut panes, &tid(1), 0);

        let mut overlays = OverlayState::new();
        overlays.push(Box::new(crate::render::overlay::CopyModeOverlay::new(
            0, 0, 8, 4,
        )));
        let mut next_request_id = 1;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let theme = Theme::default();
        let mut switch_request = None;
        let mut session_name = String::new();
        let mut zoomed = None;
        let mut sidebar_enabled = false;
        let mut drag: Option<DragGrab> = None;
        let mut reload_request = false;
        let mut mouse_optout: std::collections::HashSet<TerminalId> =
            std::collections::HashSet::new();
        let fleet_agent_meta = HashMap::new();
        let mut fleet_vcs = crate::attach::driver::VcsIndex::default();
        let mut ctx = DispatchCtx {
            engine_kernel: &mut engine_kernel,
            resolver: None,
            focus_history: FocusHistory::default(),
            workspace: &mut workspace,
            viewport: (8, 4),
            cell_px: (1, 1),
            next_request_id: &mut next_request_id,
            pending_splits: &mut pending_splits,
            pending_windows: &mut pending_windows,
            expected_closes: &mut HashSet::new(),
            overlays: &mut overlays,
            keybindings: None,
            theme: &theme,
            sessions: &[],
            foreign_layouts: &HashMap::new(),
            foreign_agents: &HashMap::new(),
            focused_session: None,
            session_name: &mut session_name,
            switch_request: &mut switch_request,
            zoomed: &mut zoomed,
            sidebar: None,
            sidebar_enabled: &mut sidebar_enabled,
            sidebar_agents: &[],
            bar: None,
            status_bar: None,
            drag: &mut drag,
            mouse_optout: &mut mouse_optout,
            attention_navigation: &mut AttentionNavigation::default(),
            plugin_actions: &[],
            plugin_panes: &[],
            plugin_tx: None,
            reload_request: &mut reload_request,
            agent_meta: &fleet_agent_meta,
            vcs: &mut fleet_vcs,
        };
        let page_up = KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::PageUp,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        };

        let changed = dispatch_input_events(
            &mut out,
            &mut conn,
            vec![InputEvent::Key(page_up)],
            &mut focused_pane,
            &mut detach_pending,
            &mut predict,
            &overlay,
            &mut panes,
            &mut ctx,
        )
        .await
        .expect("dispatch");

        let after = visible_prefix(&engine_kernel, &mut panes, &tid(1), 0);
        assert!(changed, "scrolling copy-mode should trigger a repaint");
        assert_ne!(
            before, after,
            "dispatch should apply copy-mode scroll to the focused pane viewport"
        );
    }

    // ---------- phux-fce4: sidebar hit targets ----------

    /// The pure click→action mapping: window blocks and agent rows commit
    /// `select-window { index }`, the footer rows `new-window` and
    /// `command-palette`, the collapse corner `toggle-sidebar`, and
    /// header/blank/separator cells nothing.
    #[test]
    fn sidebar_click_action_maps_rows_to_registry_actions() {
        // Left-docked 20-column strip over a 24-row viewport with a status
        // bar: rows 0..=22, footer on rows 21 (new) and 22 (menu). Row 0 is
        // the `spaces` header (phux-foz.9), so window 1's block sits on
        // rows 3-4.
        let strip = crate::layout::Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 23,
        };
        // Window 1's name row (y = 3) and branch row (y = 4) both select it.
        for y in [3, 4] {
            let resolved = sidebar_click_action(strip, 2, &[], 4, y).expect("window row hits");
            assert_eq!(resolved.action, "select-window");
            assert_eq!(index_arg(&resolved), Some(1));
        }
        // phux-foz.9: an agents-section row (gap y=5, header y=6, first
        // entry y=7) selects the window holding the agent's pane.
        let agent = sidebar_click_action(strip, 2, &[1], 4, 7).expect("agent row hits");
        assert_eq!(agent.action, "select-window");
        assert_eq!(index_arg(&agent), Some(1));
        assert!(
            sidebar_click_action(strip, 2, &[1], 4, 6).is_none(),
            "the agents header is inert"
        );
        let new = sidebar_click_action(strip, 2, &[], 4, 21).expect("new row hits");
        assert_eq!(new.action, "new-window");
        assert!(new.args.is_empty());
        let menu = sidebar_click_action(strip, 2, &[], 4, 22).expect("menu row hits");
        assert_eq!(menu.action, "command-palette");
        // phux-foz.9: the collapse chevron in the bottom corner.
        let collapse = sidebar_click_action(strip, 2, &[], 19, 22).expect("collapse corner hits");
        assert_eq!(collapse.action, "toggle-sidebar");
        assert!(collapse.args.is_empty());
        // Header row, blank padding row, and the separator column (outside
        // the chevron corner) commit nothing.
        assert!(sidebar_click_action(strip, 2, &[], 4, 0).is_none());
        assert!(sidebar_click_action(strip, 2, &[], 4, 10).is_none());
        assert!(sidebar_click_action(strip, 2, &[], 19, 0).is_none());
    }

    /// Every action a sidebar click can commit must be a dispatched action
    /// name — the same lockstep the palette registry test enforces.
    #[test]
    fn sidebar_click_actions_are_dispatched_names() {
        let strip = crate::layout::Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 23,
        };
        for y in 0..strip.h {
            for x in [2u16, 19] {
                if let Some(resolved) = sidebar_click_action(strip, 3, &[0, 2], x, y) {
                    assert!(
                        ACTION_NAMES.contains(&resolved.action.as_str()),
                        "sidebar committed `{}`, which run_action does not dispatch",
                        resolved.action,
                    );
                }
            }
        }
    }

    fn left_press_at(x: u16, y: u16) -> InputEvent {
        use phux_protocol::input::key::ModSet;
        InputEvent::Mouse(MouseEvent {
            action: MouseAction::Press,
            button: MouseButton::Left,
            mods: ModSet::empty(),
            x: f64::from(x),
            y: f64::from(y),
        })
    }

    /// phux-wrnm: the same press with the right button.
    fn right_press_at(x: u16, y: u16) -> InputEvent {
        use phux_protocol::input::key::ModSet;
        InputEvent::Mouse(MouseEvent {
            action: MouseAction::Press,
            button: MouseButton::Right,
            mods: ModSet::empty(),
            x: f64::from(x),
            y: f64::from(y),
        })
    }

    /// Drive `dispatch_input_events` with a left-docked sidebar reservation
    /// and one mouse event; returns `(active_window, overlay_active,
    /// pending_window_count)` so the callers can assert each affordance's
    /// end-to-end effect.
    #[allow(
        clippy::future_not_send,
        reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
    )]
    async fn dispatch_sidebar_click(ev: InputEvent) -> (usize, bool, usize) {
        let (a, _b) = tokio::net::UnixStream::pair().expect("uds pair");
        let mut conn = Connection::from_stream(a);
        let mut out: Vec<u8> = Vec::new();
        let mut workspace = Workspace::single(tid(1));
        workspace.add_window("two".to_owned(), tid(2));
        workspace.select(0);
        let mut focused_pane = Some(tid(1));
        let mut detach_pending = false;
        let mut predict =
            PredictionState::new(crate::predict::PredictiveConfig::disabled(), 80, 24);
        let overlay = Overlay;
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        let mut next_request_id = 1;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let mut overlays = OverlayState::new();
        let theme = Theme::default();
        let mut switch_request = None;
        let mut session_name = String::new();
        let mut zoomed = None;
        let mut sidebar_enabled = true;
        let mut drag: Option<DragGrab> = None;
        let mut mouse_optout: std::collections::HashSet<TerminalId> =
            std::collections::HashSet::new();
        let mut reload_request = false;
        let fleet_agent_meta = HashMap::new();
        let mut fleet_vcs = crate::attach::driver::VcsIndex::default();
        let mut engine_kernel = test_engine_kernel();
        let mut ctx = DispatchCtx {
            engine_kernel: &mut engine_kernel,
            resolver: None,
            focus_history: FocusHistory::default(),
            workspace: &mut workspace,
            viewport: (80, 24),
            cell_px: (1, 1),
            next_request_id: &mut next_request_id,
            pending_splits: &mut pending_splits,
            pending_windows: &mut pending_windows,
            expected_closes: &mut HashSet::new(),
            overlays: &mut overlays,
            keybindings: None,
            theme: &theme,
            sessions: &[],
            foreign_layouts: &HashMap::new(),
            foreign_agents: &HashMap::new(),
            focused_session: None,
            session_name: &mut session_name,
            switch_request: &mut switch_request,
            zoomed: &mut zoomed,
            sidebar: Some(SidebarReservation {
                edge: super::super::paint::SidebarEdge::Left,
                width: 20,
            }),
            sidebar_enabled: &mut sidebar_enabled,
            sidebar_agents: &[],
            bar: Some(crate::render::chrome::status_bar::Position::Bottom),
            status_bar: None,
            drag: &mut drag,
            mouse_optout: &mut mouse_optout,
            attention_navigation: &mut AttentionNavigation::default(),
            plugin_actions: &[],
            plugin_panes: &[],
            plugin_tx: None,
            reload_request: &mut reload_request,
            agent_meta: &fleet_agent_meta,
            vcs: &mut fleet_vcs,
        };
        dispatch_input_events(
            &mut out,
            &mut conn,
            vec![ev],
            &mut focused_pane,
            &mut detach_pending,
            &mut predict,
            &overlay,
            &mut panes,
            &mut ctx,
        )
        .await
        .expect("dispatch");
        (
            workspace.active,
            overlays.is_active(),
            pending_windows.len(),
        )
    }

    /// A left press on the second window's block switches to it — the
    /// mouse route runs the same `select-window` a keybinding would.
    #[tokio::test]
    async fn sidebar_click_on_window_block_selects_it() {
        // phux-qtw8: the strip is full-height (h = 24 in a 24-row viewport)
        // even with a bar docked. Row 0 is the spaces header (phux-foz.9), so
        // window 1's name row is y=3.
        let (active, overlay_active, pending) = dispatch_sidebar_click(left_press_at(3, 3)).await;
        assert_eq!(active, 1, "clicking window 1's block must select it");
        assert!(!overlay_active);
        assert_eq!(pending, 0);
    }

    /// A left press on `+ new` parks a `new-window` spawn (the reply opens
    /// the window), exactly like the `new-window` chord.
    #[tokio::test]
    async fn sidebar_click_on_new_parks_a_window_spawn() {
        // The footer is bottom-anchored: `+ new` is the strip's second-to-last
        // row, y = 22 of a full-height 24-row strip (phux-qtw8).
        let (active, overlay_active, pending) = dispatch_sidebar_click(left_press_at(3, 22)).await;
        assert_eq!(active, 0, "spawn is parked; no window switch yet");
        assert!(!overlay_active);
        assert_eq!(pending, 1, "new-window spawn must be parked");
    }

    /// A left press on `= menu` opens the command palette overlay — the
    /// session/plugin menu built from the action registry.
    ///
    /// phux-qtw8: `= menu` is the strip's last row, which is also the bar row —
    /// the strip owns its columns there, and the bar has yielded them. The
    /// strip hit-tests first, so the click reaches the footer, not the bar.
    #[tokio::test]
    async fn sidebar_click_on_menu_opens_the_command_palette() {
        let (active, overlay_active, pending) = dispatch_sidebar_click(left_press_at(3, 23)).await;
        assert_eq!(active, 0);
        assert!(overlay_active, "menu click must push the palette overlay");
        assert_eq!(pending, 0);
    }

    /// Pointer events over the strip never leak into pane routing: a press
    /// on a blank row is consumed, mutating nothing.
    #[tokio::test]
    async fn sidebar_consumes_clicks_on_blank_rows() {
        let (active, overlay_active, pending) = dispatch_sidebar_click(left_press_at(3, 10)).await;
        assert_eq!(active, 0);
        assert!(!overlay_active);
        assert_eq!(pending, 0);
    }

    /// phux-wrnm: a right press on a window block selects that window (a
    /// menu acts on what you pointed at) and then opens its window menu.
    #[tokio::test]
    async fn sidebar_right_press_on_a_window_block_selects_it_and_opens_its_menu() {
        let (active, overlay_active, pending) = dispatch_sidebar_click(right_press_at(3, 3)).await;
        assert_eq!(active, 1, "right-clicking window 1's block selects it");
        assert!(overlay_active, "and opens the window menu for it");
        assert_eq!(pending, 0);
    }

    /// phux-wrnm: every other cell of the strip is session chrome, so a
    /// right press there opens the session menu rather than doing nothing.
    #[tokio::test]
    async fn sidebar_right_press_on_blank_chrome_opens_the_session_menu() {
        let (active, overlay_active, pending) = dispatch_sidebar_click(right_press_at(3, 10)).await;
        assert_eq!(active, 0, "no window was pointed at, so none is selected");
        assert!(overlay_active, "the session menu opens");
        assert_eq!(pending, 0);
    }

    // ---------- phux-foz.12: status-bar window-tab hit targets ----------

    /// Build a status-bar painter with the `windows` widget in the left
    /// slot (the default config's layout), fed `bash`/`vim` tabs and
    /// painted once at `cols x rows` so its cached strip — the click
    /// hit-test source — is populated. The strip reads "0:bash 1:vim":
    /// window 0 on columns 0..=5, the separator on 6, window 1 on 7..=11.
    fn painted_windows_bar(
        position: crate::render::chrome::status_bar::Position,
        cols: u16,
        rows: u16,
    ) -> crate::render::chrome::status_bar::StatusBarPainter {
        use crate::render::chrome::status_bar::{StatusBarPainter, make_context};
        use phux_config::widget::{StatusBar, WidgetRegistry, WindowInfo};
        let cfg = phux_config::StatusCfg {
            left: vec![phux_config::Widget::Bare("windows".into())],
            ..Default::default()
        };
        let bar = StatusBar::build(&cfg, &WidgetRegistry::with_builtins()).expect("bar builds");
        let mut painter = StatusBarPainter::new(bar, position);
        painter.set_windows(vec![
            WindowInfo {
                name: "bash".to_owned(),
                active: true,
                zoomed: false,
                attention: false,
                branch: None,
            },
            WindowInfo {
                name: "vim".to_owned(),
                active: false,
                zoomed: false,
                attention: false,
                branch: None,
            },
        ]);
        let mut sink = Vec::new();
        painter
            .paint(
                &mut sink,
                crate::render::chrome::status_bar::BarInset::NONE,
                cols,
                rows,
                &make_context("", std::time::SystemTime::UNIX_EPOCH),
            )
            .expect("paint");
        painter
    }

    /// Drive `dispatch_input_events` with a two-window workspace, no
    /// sidebar, and a painted status bar at `position`; returns
    /// `(active_window, frames_the_peer_received)` so callers can assert
    /// both the select effect and that nothing leaked to a pane.
    #[allow(
        clippy::future_not_send,
        reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
    )]
    async fn dispatch_bar_click(
        ev: InputEvent,
        position: crate::render::chrome::status_bar::Position,
        with_painter: bool,
    ) -> (usize, Vec<FrameKind>, bool) {
        let painter = painted_windows_bar(position, 80, 24);
        let (a, b) = tokio::net::UnixStream::pair().expect("uds pair");
        let mut conn = Connection::from_stream(a);
        let mut peer = Connection::from_stream(b);
        let mut out: Vec<u8> = Vec::new();
        let mut workspace = Workspace::single(tid(1));
        workspace.add_window("two".to_owned(), tid(2));
        workspace.select(0);
        let mut focused_pane = Some(tid(1));
        let mut detach_pending = false;
        let mut predict =
            PredictionState::new(crate::predict::PredictiveConfig::disabled(), 80, 24);
        let overlay = Overlay;
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        let mut next_request_id = 1;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let mut overlays = OverlayState::new();
        let theme = Theme::default();
        let mut switch_request = None;
        let mut session_name = String::new();
        let mut zoomed = None;
        let mut sidebar_enabled = false;
        let mut drag: Option<DragGrab> = None;
        let mut mouse_optout: std::collections::HashSet<TerminalId> =
            std::collections::HashSet::new();
        let mut reload_request = false;
        let fleet_agent_meta = HashMap::new();
        let mut fleet_vcs = crate::attach::driver::VcsIndex::default();
        {
            let mut engine_kernel = test_engine_kernel();
            let mut ctx = DispatchCtx {
                engine_kernel: &mut engine_kernel,
                resolver: None,
                focus_history: FocusHistory::default(),
                workspace: &mut workspace,
                viewport: (80, 24),
                cell_px: (1, 1),
                next_request_id: &mut next_request_id,
                pending_splits: &mut pending_splits,
                pending_windows: &mut pending_windows,
                expected_closes: &mut HashSet::new(),
                overlays: &mut overlays,
                keybindings: None,
                theme: &theme,
                sessions: &[],
                foreign_layouts: &HashMap::new(),
                foreign_agents: &HashMap::new(),
                focused_session: None,
                session_name: &mut session_name,
                switch_request: &mut switch_request,
                zoomed: &mut zoomed,
                sidebar: None,
                sidebar_enabled: &mut sidebar_enabled,
                sidebar_agents: &[],
                bar: Some(position),
                status_bar: with_painter.then_some(&painter),
                drag: &mut drag,
                mouse_optout: &mut mouse_optout,
                attention_navigation: &mut AttentionNavigation::default(),
                plugin_actions: &[],
                plugin_panes: &[],
                plugin_tx: None,
                reload_request: &mut reload_request,
                agent_meta: &fleet_agent_meta,
                vcs: &mut fleet_vcs,
            };
            dispatch_input_events(
                &mut out,
                &mut conn,
                vec![ev],
                &mut focused_pane,
                &mut detach_pending,
                &mut predict,
                &overlay,
                &mut panes,
                &mut ctx,
            )
            .await
            .expect("dispatch");
        }
        // Same drain discipline as `dispatch_mouse_two_pane`: close the
        // writer so the peer's recv loop terminates on EOF.
        drop(conn);
        let mut received = Vec::new();
        loop {
            let next = tokio::time::timeout(PEER_DRAIN_DEADLINE, peer.recv())
                .await
                .expect("timed out draining the peer connection");
            match next {
                Ok(frame) => received.push(frame),
                Err(_) => break,
            }
        }
        (workspace.active, received, overlays.is_active())
    }

    /// A left press on window 1's tab in the BOTTOM bar (the user-reported
    /// dogfood case) selects it — the same `select-window` a keybinding or
    /// sidebar click runs — and forwards nothing to a pane.
    #[tokio::test]
    async fn bar_click_on_window_tab_selects_it() {
        use crate::render::chrome::status_bar::Position;
        // "0:bash 1:vim" — column 8 is inside window 1's tab; bottom bar
        // row of a 24-row viewport is y = 23.
        let (active, received, _) =
            dispatch_bar_click(left_press_at(8, 23), Position::Bottom, true).await;
        assert_eq!(active, 1, "clicking window 1's tab must select it");
        assert!(
            received.is_empty(),
            "a bar-row click must not reach a pane; got {received:?}"
        );
    }

    /// The same tab click works with the bar docked at the TOP (phux-foz.8):
    /// the claimed row is y = 0 and the pane content below is untouched.
    #[tokio::test]
    async fn bar_click_honors_top_placement() {
        use crate::render::chrome::status_bar::Position;
        let (active, received, _) =
            dispatch_bar_click(left_press_at(8, 0), Position::Top, true).await;
        assert_eq!(active, 1, "top-docked tab click must select window 1");
        assert!(received.is_empty());
    }

    /// A click on the bar row that misses every tab (separator, blank
    /// padding, another widget's cells) is consumed as chrome: no select,
    /// no forward, exactly the pre-claim no-op.
    #[tokio::test]
    async fn bar_click_on_non_tab_cell_is_a_noop() {
        use crate::render::chrome::status_bar::Position;
        // Column 6 is the tab separator; column 40 is blank padding.
        for x in [6, 40] {
            let (active, received, _) =
                dispatch_bar_click(left_press_at(x, 23), Position::Bottom, true).await;
            assert_eq!(active, 0, "col {x} must not select");
            assert!(
                received.is_empty(),
                "col {x} must not forward; got {received:?}"
            );
        }
    }

    /// With a TOP bar, a click on the bottom row is pane content — the bar
    /// claim must not intercept it (it forwards to the pane under it).
    #[tokio::test]
    async fn bar_claim_leaves_pane_content_alone() {
        use crate::render::chrome::status_bar::Position;
        let (active, received, _) =
            dispatch_bar_click(left_press_at(8, 23), Position::Top, true).await;
        assert_eq!(active, 0, "a pane click must not select a window");
        match received.as_slice() {
            [FrameKind::InputMouse { terminal_id, .. }] => assert_eq!(*terminal_id, tid(1)),
            other => panic!("expected the click to forward to the pane, got {other:?}"),
        }
    }

    /// phux-wrnm: a right press on a tab selects that window and opens its
    /// window menu — the tab-bar equivalent of right-clicking a browser tab.
    #[tokio::test]
    async fn bar_right_press_on_a_tab_selects_it_and_opens_the_window_menu() {
        use crate::render::chrome::status_bar::Position;
        let (active, received, overlay) =
            dispatch_bar_click(right_press_at(8, 23), Position::Bottom, true).await;
        assert_eq!(active, 1, "right-clicking a tab selects its window");
        assert!(overlay, "and opens that window's menu");
        assert!(received.is_empty(), "nothing reaches a pane: {received:?}");
    }

    /// phux-wrnm: off the tabs the bar is session chrome — the session name,
    /// the widgets, the padding — so a right press there opens the session
    /// menu instead of being swallowed.
    #[tokio::test]
    async fn bar_right_press_off_the_tabs_opens_the_session_menu() {
        use crate::render::chrome::status_bar::Position;
        // Column 40 is blank padding on the "0:bash 1:vim" strip.
        let (active, received, overlay) =
            dispatch_bar_click(right_press_at(40, 23), Position::Bottom, true).await;
        assert_eq!(active, 0, "no tab pointed at ⇒ no window switch");
        assert!(overlay, "the session menu opens");
        assert!(received.is_empty(), "nothing reaches a pane: {received:?}");
    }

    /// A bar reservation without a lent painter (headless paths, stale
    /// fixtures) still claims the row safely: consumed, no panic, no select.
    #[tokio::test]
    async fn bar_click_without_painter_is_consumed() {
        use crate::render::chrome::status_bar::Position;
        let (active, received, _) =
            dispatch_bar_click(left_press_at(8, 23), Position::Bottom, false).await;
        assert_eq!(active, 0);
        assert!(received.is_empty());
    }

    /// The pure click->action mapping mirrors `sidebar_click_action`: a tab
    /// column commits `select-window { index }`; non-tab columns and a
    /// missing painter commit nothing — and the committed name must be a
    /// dispatched action (the palette-registry lockstep).
    #[test]
    fn bar_click_action_maps_tab_columns_to_select_window() {
        use crate::render::chrome::status_bar::Position;
        let painter = painted_windows_bar(Position::Bottom, 80, 24);
        let resolved = bar_click_action(Some(&painter), 8).expect("tab column hits");
        assert_eq!(resolved.action, "select-window");
        assert_eq!(index_arg(&resolved), Some(1));
        assert!(
            ACTION_NAMES.contains(&resolved.action.as_str()),
            "bar committed `{}`, which run_action does not dispatch",
            resolved.action,
        );
        assert!(bar_click_action(Some(&painter), 6).is_none(), "separator");
        assert!(bar_click_action(Some(&painter), 40).is_none(), "padding");
        assert!(bar_click_action(None, 8).is_none(), "no painter");
    }

    // -- phux-npb3: per-pane mouse opt-out + drag double-press hardening ---
    // (reuses the `two_pane_workspace` fixture defined for the resize-pane
    // dispatch tests above.)

    /// Build a mouse event in outer-viewport cell coordinates.
    fn mev(action: MouseAction, button: MouseButton, x: f64, y: f64) -> MouseEvent {
        MouseEvent {
            action,
            button,
            mods: phux_protocol::input::key::ModSet::empty(),
            x,
            y,
        }
    }

    /// The divider column of [`two_pane_workspace`] at viewport 80x24 (no
    /// bar, no sidebar), found by hit-testing rather than hardcoding the
    /// rasterizer's rounding.
    fn two_pane_divider_x() -> u16 {
        use crate::multi_pane::{RouteDecision, route_mouse_event};
        let workspace = two_pane_workspace();
        let ls = workspace.active_window().expect("active window");
        let content = content_rect((80, 24), None, None);
        (0..80u16)
            .find(|&x| {
                matches!(
                    route_mouse_event(
                        ls,
                        content,
                        (80, 24),
                        &mev(MouseAction::Press, MouseButton::Left, f64::from(x), 5.0),
                    ),
                    RouteDecision::Divider { .. }
                )
            })
            .expect("a two-pane split has a divider column")
    }

    /// Drive `dispatch_input_events` with `events` against
    /// [`two_pane_workspace`] (viewport 80x24, no bar / sidebar), seeding
    /// the per-pane opt-out set with `seed_optout`. Returns every frame the
    /// peer end of the connection received plus the post-dispatch drag,
    /// focus, and opt-out state.
    #[allow(
        clippy::future_not_send,
        reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
    )]
    async fn dispatch_mouse_two_pane(
        events: Vec<InputEvent>,
        seed_optout: &[TerminalId],
    ) -> (
        Vec<FrameKind>,
        Option<DragGrab>,
        Option<TerminalId>,
        std::collections::HashSet<TerminalId>,
    ) {
        dispatch_mouse_two_pane_with(events, seed_optout, &[], (1, 1)).await
    }

    /// [`dispatch_mouse_two_pane`] with pane slots: each `(id, vt)` entry
    /// allocates a [`PaneSlot`] mirror and feeds it `vt` (mode seeds like
    /// `?1049h`), so the wheel branch's mode gates are exercised against
    /// real libghostty state. `cell_px` is the ctx cell geometry for the
    /// SPEC §3.1 cells→pixels send-boundary scaling.
    #[allow(
        clippy::future_not_send,
        reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
    )]
    async fn dispatch_mouse_two_pane_with(
        events: Vec<InputEvent>,
        seed_optout: &[TerminalId],
        seed_vt: &[(TerminalId, &[u8])],
        cell_px: (u16, u16),
    ) -> (
        Vec<FrameKind>,
        Option<DragGrab>,
        Option<TerminalId>,
        std::collections::HashSet<TerminalId>,
    ) {
        let mut overlays = OverlayState::new();
        let (frames, drag, focused, optout, _repaint) =
            dispatch_mouse_two_pane_into(&mut overlays, events, seed_optout, seed_vt, cell_px)
                .await;
        (frames, drag, focused, optout)
    }

    /// [`dispatch_mouse_two_pane_with`] against a caller-owned overlay
    /// stack, so a test can assert what the batch pushed (phux-wrnm: the
    /// right-click menus) as well as what it sent.
    #[allow(
        clippy::future_not_send,
        reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
    )]
    async fn dispatch_mouse_two_pane_into(
        overlays: &mut OverlayState,
        events: Vec<InputEvent>,
        seed_optout: &[TerminalId],
        seed_vt: &[(TerminalId, &[u8])],
        cell_px: (u16, u16),
    ) -> (
        Vec<FrameKind>,
        Option<DragGrab>,
        Option<TerminalId>,
        std::collections::HashSet<TerminalId>,
        bool,
    ) {
        let mut workspace = two_pane_workspace();
        let (a, b) = tokio::net::UnixStream::pair().expect("uds pair");
        let mut conn = Connection::from_stream(a);
        let mut peer = Connection::from_stream(b);
        let mut out: Vec<u8> = Vec::new();
        let mut focused_pane = Some(tid(1));
        let mut detach_pending = false;
        let mut predict =
            PredictionState::new(crate::predict::PredictiveConfig::disabled(), 80, 24);
        let overlay = Overlay;
        let entries: Vec<_> = seed_vt
            .iter()
            .map(|(id, bytes)| (id, 39, 24, *bytes))
            .collect();
        let (mut engine_kernel, _, mut panes) =
            super::super::driver::published_test_state(&entries);
        let mut next_request_id = 1;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let theme = Theme::default();
        let mut switch_request = None;
        let mut session_name = String::new();
        let mut zoomed = None;
        let mut sidebar_enabled = false;
        let mut drag: Option<DragGrab> = None;
        let mut mouse_optout: std::collections::HashSet<TerminalId> =
            seed_optout.iter().cloned().collect();
        let mut reload_request = false;
        let fleet_agent_meta = HashMap::new();
        let mut fleet_vcs = crate::attach::driver::VcsIndex::default();
        let repainted;
        {
            let mut ctx = DispatchCtx {
                engine_kernel: &mut engine_kernel,
                resolver: None,
                focus_history: FocusHistory::default(),
                workspace: &mut workspace,
                viewport: (80, 24),
                cell_px,
                next_request_id: &mut next_request_id,
                pending_splits: &mut pending_splits,
                pending_windows: &mut pending_windows,
                expected_closes: &mut HashSet::new(),
                overlays,
                keybindings: None,
                theme: &theme,
                sessions: &[],
                foreign_layouts: &HashMap::new(),
                foreign_agents: &HashMap::new(),
                focused_session: None,
                session_name: &mut session_name,
                switch_request: &mut switch_request,
                zoomed: &mut zoomed,
                sidebar: None,
                sidebar_enabled: &mut sidebar_enabled,
                sidebar_agents: &[],
                bar: None,
                status_bar: None,
                drag: &mut drag,
                mouse_optout: &mut mouse_optout,
                attention_navigation: &mut AttentionNavigation::default(),
                plugin_actions: &[],
                plugin_panes: &[],
                plugin_tx: None,
                reload_request: &mut reload_request,
                agent_meta: &fleet_agent_meta,
                vcs: &mut fleet_vcs,
            };
            repainted = dispatch_input_events(
                &mut out,
                &mut conn,
                events,
                &mut focused_pane,
                &mut detach_pending,
                &mut predict,
                &overlay,
                &mut panes,
                &mut ctx,
            )
            .await
            .expect("dispatch");
        }
        // Close the writer so the peer's drain terminates: once the buffered
        // frames are consumed, `recv` sees the EOF and returns Disconnected.
        // (`try_recv` is not used here — tokio's non-blocking read reports
        // WouldBlock until the reactor has observed readiness, which this
        // freshly-paired socket never awaited.)
        drop(conn);
        let mut received = Vec::new();
        loop {
            let next = tokio::time::timeout(PEER_DRAIN_DEADLINE, peer.recv())
                .await
                .expect("timed out draining the peer connection");
            match next {
                Ok(frame) => received.push(frame),
                Err(_) => break, // EOF after the writer dropped
            }
        }
        (received, drag, focused_pane, mouse_optout, repainted)
    }

    /// phux-npb3 hardening: a second Press arriving while a divider drag is
    /// active must be consumed — not fall through to normal routing, where
    /// it would move focus and forward an `INPUT_MOUSE` mid-drag.
    #[tokio::test]
    async fn second_press_during_divider_drag_is_consumed() {
        let dx = f64::from(two_pane_divider_x());
        let (received, drag, focused, _) = dispatch_mouse_two_pane(
            vec![
                InputEvent::Mouse(mev(MouseAction::Press, MouseButton::Left, dx, 5.0)),
                InputEvent::Mouse(mev(MouseAction::Press, MouseButton::Left, 70.0, 5.0)),
            ],
            &[],
        )
        .await;
        assert!(drag.is_some(), "the divider press grabs a drag");
        assert_eq!(
            focused,
            Some(tid(1)),
            "a press mid-drag must not move focus"
        );
        assert!(
            received.is_empty(),
            "a press mid-drag must not forward to a pane; got {received:?}"
        );
    }

    /// The double-press guard must not eat the release that ends the drag.
    #[tokio::test]
    async fn release_after_guarded_press_still_ends_drag() {
        let dx = f64::from(two_pane_divider_x());
        let (_received, drag, _focused, _) = dispatch_mouse_two_pane(
            vec![
                InputEvent::Mouse(mev(MouseAction::Press, MouseButton::Left, dx, 5.0)),
                InputEvent::Mouse(mev(MouseAction::Press, MouseButton::Right, 70.0, 5.0)),
                InputEvent::Mouse(mev(MouseAction::Release, MouseButton::Left, 70.0, 5.0)),
            ],
            &[],
        )
        .await;
        assert!(
            drag.is_none(),
            "the release must still end the drag after a guarded press"
        );
    }

    /// phux-npb3 routing: a press inside an opted-out pane still
    /// click-focuses it (chrome-level — that is also what makes the driver
    /// drop outer capture), but no `INPUT_MOUSE` is synthesized for it.
    #[tokio::test]
    async fn press_in_opted_out_pane_focuses_but_does_not_forward() {
        let (received, _, focused, _) = dispatch_mouse_two_pane(
            vec![InputEvent::Mouse(mev(
                MouseAction::Press,
                MouseButton::Left,
                70.0,
                5.0,
            ))],
            &[tid(2)],
        )
        .await;
        assert_eq!(
            focused,
            Some(tid(2)),
            "click-to-focus still applies to an opted-out pane"
        );
        assert!(
            received.is_empty(),
            "an opted-out pane must receive no INPUT_MOUSE; got {received:?}"
        );
    }

    /// The opt-out is per-pane: a sibling that did NOT opt out still gets
    /// its `INPUT_MOUSE` forwarded (with pane-local coordinates) while the
    /// other pane sits in the opt-out set.
    #[tokio::test]
    async fn press_in_opted_in_sibling_still_forwards() {
        let dx = two_pane_divider_x();
        let (received, _, focused, _) = dispatch_mouse_two_pane(
            vec![InputEvent::Mouse(mev(
                MouseAction::Press,
                MouseButton::Left,
                70.0,
                5.0,
            ))],
            &[tid(1)], // the OTHER pane is opted out
        )
        .await;
        assert_eq!(focused, Some(tid(2)));
        match received.as_slice() {
            [FrameKind::InputMouse { terminal_id, event }] => {
                assert_eq!(*terminal_id, tid(2));
                assert!(
                    event.x < f64::from(dx),
                    "forwarded coordinates are pane-local; got x = {}",
                    event.x
                );
            }
            other => panic!("expected exactly one INPUT_MOUSE, got {other:?}"),
        }
    }

    // -- phux-yyex: wheel routing per pane screen/mode state ---------------

    /// Wheel over an alt-screen pane without mouse tracking synthesizes
    /// arrow-key presses (xterm alternate scroll, DECSET 1007 — default ON
    /// in libghostty): the alt screen has no scrollback, so the local
    /// viewport scroll would be a silent no-op and the wheel would go dead.
    #[tokio::test]
    async fn wheel_in_alt_screen_pane_synthesizes_arrow_keys() {
        let (received, _, _, _) = dispatch_mouse_two_pane_with(
            vec![InputEvent::Mouse(mev(
                MouseAction::Press,
                MouseButton::Four,
                70.0,
                5.0,
            ))],
            &[],
            &[(tid(2), b"\x1b[?1049h")],
            (1, 1),
        )
        .await;
        assert_eq!(
            received.len(),
            3,
            "one wheel notch = 3 arrows: {received:?}"
        );
        for frame in &received {
            match frame {
                FrameKind::InputKey { terminal_id, event } => {
                    assert_eq!(*terminal_id, tid(2));
                    assert_eq!(event.key, PhysicalKey::ArrowUp);
                }
                other => panic!("expected INPUT_KEY, got {other:?}"),
            }
        }
    }

    /// Wheel-down in the same alt-screen pane maps to `ArrowDown`.
    #[tokio::test]
    async fn wheel_down_in_alt_screen_pane_synthesizes_arrow_down() {
        let (received, _, _, _) = dispatch_mouse_two_pane_with(
            vec![InputEvent::Mouse(mev(
                MouseAction::Press,
                MouseButton::Five,
                70.0,
                5.0,
            ))],
            &[],
            &[(tid(2), b"\x1b[?1049h")],
            (1, 1),
        )
        .await;
        assert_eq!(received.len(), 3);
        for frame in &received {
            match frame {
                FrameKind::InputKey { event, .. } => {
                    assert_eq!(event.key, PhysicalKey::ArrowDown);
                }
                other => panic!("expected INPUT_KEY, got {other:?}"),
            }
        }
    }

    /// An app that opted out of alternate scroll (`?1007l`) gets neither
    /// arrows nor a forwarded wheel — matching xterm, the wheel is inert on
    /// an alt screen that asked for silence.
    #[tokio::test]
    async fn wheel_with_alt_scroll_off_sends_nothing() {
        let (received, _, _, _) = dispatch_mouse_two_pane_with(
            vec![InputEvent::Mouse(mev(
                MouseAction::Press,
                MouseButton::Four,
                70.0,
                5.0,
            ))],
            &[],
            &[(tid(2), b"\x1b[?1049h\x1b[?1007l")],
            (1, 1),
        )
        .await;
        assert!(received.is_empty(), "expected no frames, got {received:?}");
    }

    /// Wheel over a primary-screen pane without mouse tracking is consumed
    /// by the local scrollback viewport — nothing crosses the wire.
    #[tokio::test]
    async fn wheel_in_primary_screen_pane_scrolls_locally() {
        let (received, _, _, _) = dispatch_mouse_two_pane_with(
            vec![InputEvent::Mouse(mev(
                MouseAction::Press,
                MouseButton::Four,
                70.0,
                5.0,
            ))],
            &[],
            &[(tid(2), b"")],
            (1, 1),
        )
        .await;
        assert!(received.is_empty(), "expected no frames, got {received:?}");
    }

    /// Wheel over a pane whose app tracks the mouse (Claude Code sets
    /// `?1000h ?1002h ?1003h ?1006h`) forwards the wheel as `INPUT_MOUSE` so
    /// the app scrolls itself.
    #[tokio::test]
    async fn wheel_in_mouse_tracking_pane_forwards_input_mouse() {
        let (received, _, _, _) = dispatch_mouse_two_pane_with(
            vec![InputEvent::Mouse(mev(
                MouseAction::Press,
                MouseButton::Four,
                70.0,
                5.0,
            ))],
            &[],
            &[(
                tid(2),
                b"\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h",
            )],
            (1, 1),
        )
        .await;
        match received.as_slice() {
            [FrameKind::InputMouse { terminal_id, event }] => {
                assert_eq!(*terminal_id, tid(2));
                assert_eq!(event.button, MouseButton::Four);
            }
            other => panic!("expected exactly one INPUT_MOUSE, got {other:?}"),
        }
    }

    // ---------- phux-wrnm: right-click context menus (ADR-0058) ----------

    /// A right press over a pane whose app has NOT asked for the mouse
    /// opens the pane menu and forwards nothing: the button belongs to the
    /// client here, exactly as the left button does for drag-to-copy.
    #[tokio::test]
    async fn right_press_on_a_pane_opens_the_pane_menu() {
        let mut overlays = OverlayState::new();
        let (received, _, _, _, _) = dispatch_mouse_two_pane_into(
            &mut overlays,
            vec![InputEvent::Mouse(mev(
                MouseAction::Press,
                MouseButton::Right,
                70.0,
                5.0,
            ))],
            &[],
            &[(tid(2), b"")],
            (1, 1),
        )
        .await;
        assert!(overlays.is_active(), "the pane menu must be pushed");
        assert!(
            overlays.wants_pointer_hover(),
            "a menu hover-tracks the pointer, so the driver raises ?1003h",
        );
        assert!(
            received.is_empty(),
            "the right press is consumed by the menu; got {received:?}",
        );
    }

    /// Click-to-focus runs first, so the menu acts on the pane you pointed
    /// at rather than the one that happened to hold focus.
    #[tokio::test]
    async fn right_press_focuses_the_pane_under_the_pointer_first() {
        let mut overlays = OverlayState::new();
        let (_, _, focused, _, _) = dispatch_mouse_two_pane_into(
            &mut overlays,
            vec![InputEvent::Mouse(mev(
                MouseAction::Press,
                MouseButton::Right,
                70.0,
                5.0,
            ))],
            &[],
            &[(tid(2), b"")],
            (1, 1),
        )
        .await;
        assert_eq!(focused, Some(tid(2)), "the right-clicked pane takes focus");
        assert!(overlays.is_active());
    }

    /// An inner program that turned mouse tracking on owns every button —
    /// including the right one, which many TUIs bind to their own menu. The
    /// client must forward rather than steal it (the keyboard `context-menu`
    /// action is the way in for those panes).
    #[tokio::test]
    async fn right_press_in_a_mouse_tracking_pane_forwards_instead_of_opening_a_menu() {
        let mut overlays = OverlayState::new();
        let (received, _, _, _, _) = dispatch_mouse_two_pane_into(
            &mut overlays,
            vec![InputEvent::Mouse(mev(
                MouseAction::Press,
                MouseButton::Right,
                70.0,
                5.0,
            ))],
            &[],
            &[(tid(2), b"\x1b[?1000h\x1b[?1006h")],
            (1, 1),
        )
        .await;
        assert!(
            !overlays.is_active(),
            "the app owns the mouse; no menu may cover it",
        );
        match received.as_slice() {
            [FrameKind::InputMouse { terminal_id, event }] => {
                assert_eq!(*terminal_id, tid(2));
                assert_eq!(event.button, MouseButton::Right);
            }
            other => panic!("expected the right press to be forwarded, got {other:?}"),
        }
    }

    /// Clicking away closes the menu — and schedules the repaint that
    /// erases it. Without the dismissal repaint the box stayed painted over
    /// the panes until unrelated output happened to redraw them.
    #[tokio::test]
    async fn clicking_outside_the_menu_closes_it_and_repaints() {
        let mut overlays = OverlayState::new();
        let (_, _, _, _, repainted) = dispatch_mouse_two_pane_into(
            &mut overlays,
            vec![
                // Open at the far right of pane 2, then click back over pane 1.
                InputEvent::Mouse(mev(MouseAction::Press, MouseButton::Right, 70.0, 5.0)),
                InputEvent::Mouse(mev(MouseAction::Press, MouseButton::Left, 2.0, 2.0)),
            ],
            &[],
            &[(tid(2), b"")],
            (1, 1),
        )
        .await;
        assert!(!overlays.is_active(), "the click outside dismissed it");
        assert!(repainted, "dismissal must schedule a repaint");
    }

    /// A pane that opted out via `set-pane mouse off` gets no menu either:
    /// the whole point of the opt-out is that the client stops claiming
    /// that pane's pointer events (phux-npb3).
    #[tokio::test]
    async fn right_press_on_an_opted_out_pane_opens_nothing() {
        let mut overlays = OverlayState::new();
        let (received, _, _, _, _) = dispatch_mouse_two_pane_into(
            &mut overlays,
            vec![InputEvent::Mouse(mev(
                MouseAction::Press,
                MouseButton::Right,
                70.0,
                5.0,
            ))],
            &[tid(2)],
            &[(tid(2), b"")],
            (1, 1),
        )
        .await;
        assert!(!overlays.is_active());
        assert!(received.is_empty(), "opted out ⇒ nothing forwarded either");
    }

    /// SPEC input.md §3.1: forwarded `INPUT_MOUSE` positions are surface-space
    /// pixels (`cell_index x cell_size`), scaled at the send boundary from
    /// the dispatcher's pane-local cell coordinates.
    #[tokio::test]
    async fn forwarded_input_mouse_scales_cells_to_surface_pixels() {
        let dx = two_pane_divider_x();
        let (received, _, _, _) = dispatch_mouse_two_pane_with(
            vec![InputEvent::Mouse(mev(
                MouseAction::Press,
                MouseButton::Left,
                70.0,
                5.0,
            ))],
            &[],
            &[(tid(2), b"\x1b[?1000h\x1b[?1006h")],
            (8, 16),
        )
        .await;
        // Pane 2's content starts one column right of the divider.
        let expected_x = (70.0 - f64::from(dx) - 1.0) * 8.0;
        let expected_y = 5.0 * 16.0;
        match received.as_slice() {
            [FrameKind::InputMouse { event, .. }] => {
                assert!((event.x - expected_x).abs() < f64::EPSILON);
                assert!((event.y - expected_y).abs() < f64::EPSILON);
            }
            other => panic!("expected exactly one INPUT_MOUSE, got {other:?}"),
        }
    }

    /// Run a `set-pane` action against `workspace` with a caller-owned
    /// opt-out set, returning the effects.
    fn run_set_pane(
        mouse: Option<toml::Value>,
        workspace: &mut Workspace,
        mouse_optout: &mut std::collections::HashSet<TerminalId>,
    ) -> ActionEffects {
        let mut next_request_id = 100;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let mut overlays = OverlayState::new();
        let theme = Theme::default();
        let mut switch_request = None;
        let mut session_name = String::new();
        let mut zoomed = None;
        let mut sidebar_enabled = false;
        let mut drag: Option<DragGrab> = None;
        let mut reload_request = false;
        let fleet_agent_meta = HashMap::new();
        let mut fleet_vcs = crate::attach::driver::VcsIndex::default();
        let mut engine_kernel = test_engine_kernel();
        let mut ctx = DispatchCtx {
            engine_kernel: &mut engine_kernel,
            resolver: None,
            focus_history: FocusHistory::default(),
            workspace,
            viewport: (80, 24),
            cell_px: (1, 1),
            next_request_id: &mut next_request_id,
            pending_splits: &mut pending_splits,
            pending_windows: &mut pending_windows,
            expected_closes: &mut HashSet::new(),
            overlays: &mut overlays,
            keybindings: None,
            theme: &theme,
            sessions: &[],
            foreign_layouts: &HashMap::new(),
            foreign_agents: &HashMap::new(),
            focused_session: None,
            session_name: &mut session_name,
            switch_request: &mut switch_request,
            zoomed: &mut zoomed,
            sidebar: None,
            sidebar_enabled: &mut sidebar_enabled,
            sidebar_agents: &[],
            bar: None,
            status_bar: None,
            drag: &mut drag,
            mouse_optout,
            attention_navigation: &mut AttentionNavigation::default(),
            plugin_actions: &[],
            plugin_panes: &[],
            plugin_tx: None,
            reload_request: &mut reload_request,
            agent_meta: &fleet_agent_meta,
            vcs: &mut fleet_vcs,
        };
        let mut action = bare_action("set-pane");
        if let Some(v) = mouse {
            action.args.insert("mouse".to_owned(), v);
        }
        let focused = ctx.workspace.active_window().and_then(|w| w.focus.clone());
        run_action(&action, &mut ctx, focused.as_ref(), &HashMap::new())
    }

    #[test]
    fn set_pane_mouse_off_then_on_updates_optout() {
        let mut workspace = Workspace::single(tid(1));
        let mut optout = std::collections::HashSet::new();
        let effects = run_set_pane(
            Some(toml::Value::String("off".to_owned())),
            &mut workspace,
            &mut optout,
        );
        assert!(!effects.bell);
        assert!(optout.contains(&tid(1)), "`mouse = off` opts the pane out");

        let effects = run_set_pane(
            Some(toml::Value::String("on".to_owned())),
            &mut workspace,
            &mut optout,
        );
        assert!(!effects.bell);
        assert!(
            !optout.contains(&tid(1)),
            "`mouse = on` opts the pane back in"
        );
    }

    #[test]
    fn set_pane_toggle_flips_state() {
        let mut workspace = Workspace::single(tid(1));
        let mut optout = std::collections::HashSet::new();
        let toggle = || toml::Value::String("toggle".to_owned());
        run_set_pane(Some(toggle()), &mut workspace, &mut optout);
        assert!(optout.contains(&tid(1)), "first toggle opts out");
        run_set_pane(Some(toggle()), &mut workspace, &mut optout);
        assert!(!optout.contains(&tid(1)), "second toggle opts back in");
    }

    #[test]
    fn set_pane_bool_arg_maps_to_on_off() {
        let mut workspace = Workspace::single(tid(1));
        let mut optout = std::collections::HashSet::new();
        run_set_pane(
            Some(toml::Value::Boolean(false)),
            &mut workspace,
            &mut optout,
        );
        assert!(optout.contains(&tid(1)), "`mouse = false` means off");
        run_set_pane(
            Some(toml::Value::Boolean(true)),
            &mut workspace,
            &mut optout,
        );
        assert!(!optout.contains(&tid(1)), "`mouse = true` means on");
    }

    #[test]
    fn set_pane_missing_or_bad_mouse_arg_bells() {
        let mut workspace = Workspace::single(tid(1));
        let mut optout = std::collections::HashSet::new();
        let effects = run_set_pane(None, &mut workspace, &mut optout);
        assert!(effects.bell, "missing `mouse` arg bells");
        let effects = run_set_pane(
            Some(toml::Value::String("sideways".to_owned())),
            &mut workspace,
            &mut optout,
        );
        assert!(effects.bell, "unknown `mouse` value bells");
        assert!(optout.is_empty());
    }

    #[test]
    fn set_pane_without_focused_pane_bells() {
        let mut workspace = Workspace::default();
        let mut optout = std::collections::HashSet::new();
        let effects = run_set_pane(
            Some(toml::Value::String("off".to_owned())),
            &mut workspace,
            &mut optout,
        );
        assert!(effects.bell, "no focused pane to set");
        assert!(optout.is_empty());
    }

    // ---- phux-51n6.1: predictive-echo full-screen-app (alt-screen) gate ----

    use crate::predict::{PredictionState, PredictiveConfig};

    /// A fresh shell-prompt pane (main screen) is not in app mode: the gate
    /// must let prediction through.
    #[test]
    fn alt_screen_gate_false_on_main_screen() {
        let terminal = libghostty_vt::Terminal::new(libghostty_vt::TerminalOptions {
            cols: 80,
            rows: 24,
            max_scrollback: 100,
        })
        .expect("terminal");
        assert!(
            !terminal_in_alt_screen(&terminal),
            "a fresh pane sits on the main screen — predict here"
        );
    }

    /// Entering the alternate screen (`?1049h`, as vim/nvim/less/agent TUIs
    /// do) trips the gate; leaving it (`?1049l`) clears it. The legacy
    /// `?1047h` variant is caught too.
    #[test]
    fn alt_screen_gate_tracks_dec_private_modes() {
        let mut terminal = libghostty_vt::Terminal::new(libghostty_vt::TerminalOptions {
            cols: 80,
            rows: 24,
            max_scrollback: 100,
        })
        .expect("terminal");
        terminal.vt_write(b"\x1b[?1049h");
        assert!(
            terminal_in_alt_screen(&terminal),
            "1049h (save-cursor alt screen) is app mode"
        );
        terminal.vt_write(b"\x1b[?1049l");
        assert!(
            !terminal_in_alt_screen(&terminal),
            "1049l returns to the main screen — predict again"
        );

        let mut legacy = libghostty_vt::Terminal::new(libghostty_vt::TerminalOptions {
            cols: 80,
            rows: 24,
            max_scrollback: 100,
        })
        .expect("terminal");
        legacy.vt_write(b"\x1b[?1047h");
        assert!(
            terminal_in_alt_screen(&legacy),
            "1047h (legacy alt screen) is app mode too"
        );
    }

    /// Drive the REAL [`dispatch_input_events`] with one printable keystroke
    /// against a focused pane, returning how many predictions were queued
    /// afterward. When `alt_screen` is set, the pane's mirror is switched to
    /// the alternate screen (`?1049h`) before dispatch, so the phux-51n6.1
    /// app-mode gate must suppress the prediction.
    ///
    /// This exercises the true dispatch-site condition
    /// (`predict.is_enabled() && ... && !terminal_in_alt_screen(slot)`) end to
    /// end rather than re-stating it inline — so a refactor that silently drops
    /// the `&& !terminal_in_alt_screen(slot)` clause turns the alt-screen case
    /// red instead of passing on a private copy of the predicate.
    #[allow(
        clippy::future_not_send,
        reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
    )]
    async fn predictions_after_key_dispatch(alt_screen: bool) -> usize {
        use phux_protocol::input::key::PhysicalKey;

        let theme = Theme::default();
        let mut overlays = OverlayState::new();
        let (a, _b) = tokio::net::UnixStream::pair().expect("uds pair");
        let mut conn = Connection::from_stream(a);
        let mut out: Vec<u8> = Vec::new();
        let mut workspace = Workspace::single(tid(1));
        let mut focused_pane = Some(tid(1));
        let mut detach_pending = false;
        // Enabled predictor, fresh (un-suspended) — a printable insert at the
        // origin cursor is predictable, so the only thing standing between the
        // keystroke and a queued ghost is the app-mode gate under test.
        let mut predict = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        let overlay = Overlay;
        // The focused replica carries the alt-screen signal the gate reads
        // via `terminal.mode()`. A fresh pane sits on the main screen (cooked
        // shell prompt); `?1049h` puts it in a full-screen app.
        let bootstrap: &[u8] = if alt_screen { b"\x1b[?1049h" } else { b"" };
        let (mut engine_kernel, _, mut panes) =
            super::super::driver::published_test_state(&[(&tid(1), 80, 24, bootstrap)]);

        let mut next_request_id = 1;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let mut switch_request = None;
        let mut session_name = String::new();
        let mut zoomed = None;
        let mut sidebar_enabled = false;
        let mut drag: Option<DragGrab> = None;
        let mut reload_request = false;
        let mut mouse_optout: std::collections::HashSet<TerminalId> =
            std::collections::HashSet::new();
        let fleet_agent_meta = HashMap::new();
        let mut fleet_vcs = crate::attach::driver::VcsIndex::default();
        let mut ctx = DispatchCtx {
            engine_kernel: &mut engine_kernel,
            // No resolver: every key forwards straight through to the pane,
            // past the predict layer — no keybinding interception to muddy
            // the gate assertion.
            resolver: None,
            focus_history: FocusHistory::default(),
            workspace: &mut workspace,
            viewport: (80, 24),
            cell_px: (1, 1),
            next_request_id: &mut next_request_id,
            pending_splits: &mut pending_splits,
            pending_windows: &mut pending_windows,
            expected_closes: &mut HashSet::new(),
            overlays: &mut overlays,
            keybindings: None,
            theme: &theme,
            sessions: &[],
            foreign_layouts: &HashMap::new(),
            foreign_agents: &HashMap::new(),
            focused_session: None,
            session_name: &mut session_name,
            switch_request: &mut switch_request,
            zoomed: &mut zoomed,
            sidebar: None,
            sidebar_enabled: &mut sidebar_enabled,
            sidebar_agents: &[],
            bar: None,
            status_bar: None,
            drag: &mut drag,
            mouse_optout: &mut mouse_optout,
            attention_navigation: &mut AttentionNavigation::default(),
            plugin_actions: &[],
            plugin_panes: &[],
            plugin_tx: None,
            reload_request: &mut reload_request,
            agent_meta: &fleet_agent_meta,
            vcs: &mut fleet_vcs,
        };

        dispatch_input_events(
            &mut out,
            &mut conn,
            vec![press(PhysicalKey::A, Some("a"))],
            &mut focused_pane,
            &mut detach_pending,
            &mut predict,
            &overlay,
            &mut panes,
            &mut ctx,
        )
        .await
        .expect("dispatch");

        predict.pending_len()
    }

    /// Cooked shell prompt (main screen): driving the real dispatch path with
    /// a printable key queues exactly one speculative ghost.
    #[tokio::test]
    async fn dispatch_predicts_key_at_cooked_prompt() {
        assert_eq!(
            predictions_after_key_dispatch(false).await,
            1,
            "main-screen prompt: the keystroke echoes speculatively"
        );
    }

    /// Full-screen app (alt screen via `?1049h`, as vim/nvim/less/an agent TUI
    /// do): the same real dispatch path queues nothing — the app-mode gate
    /// suppresses the prediction before `predict_key_with_grid` is reached.
    /// Dropping the `!terminal_in_alt_screen(slot)` clause fails this.
    #[tokio::test]
    async fn dispatch_gates_prediction_in_alt_screen_app() {
        assert_eq!(
            predictions_after_key_dispatch(true).await,
            0,
            "alt-screen app: the gate suppresses the ghost, no back-off needed"
        );
    }
}
