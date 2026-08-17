//! The event dispatcher: parser events to wire frames, resolver
//! intercepts, mouse routing, and the pane/overlay geometry helpers.

//! Input dispatcher: translates parser-emitted events into wire frames
//! or layout-action effects.
//!
//! Owns the resolver-intercept path (prefix chord → `ResolvedAction` →
//! mutate the active window of the `Workspace`), the predict overlay's
//! keystroke feed, and the parked-spawn bookkeeping (`PendingSplit` /
//! `PendingWindow`) that bridges a local `split-pane` / `new-window`
//! chord to its remote `SPAWN_TERMINAL` reply.

use std::collections::HashMap;

use libghostty_vt::terminal::{Mode, ScrollViewport};
use phux_protocol::TerminalId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::key::{ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::wire::frame::{FrameKind, Scope};

use crate::attach::actions::{self, PendingSplit};
use crate::attach::connection::Connection;
use crate::attach::focus::FocusHistory;
use crate::attach::input::make_named_key;
use crate::attach::outcome::AttachError;
use crate::attach::paint::{SidebarReservation, content_rect};
use crate::attach::pane_state::{
    PaneSlot, clear_attention_on_input, published_replica, published_terminal,
    reanchor_predict_to_pane,
};
use crate::layout::Workspace;
use crate::layout_ops::{DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, layout_key};
use crate::predict::{Overlay, PredictionState};
use crate::render::overlay::{ContextMenu, OverlayOutcome, OverlayState};

use super::ctx::{DispatchCtx, DragGrab};
use super::effects::encode_layout_or_log;
use super::effects::{ChordOutcome, apply_action_effects, consume_chord};
use super::run_action::run_action;

/// A stage's verdict on one input event: whether the event is fully
/// handled (the batch loop advances to the next event) and whether the
/// stage mutated state the caller must repaint.
#[derive(Clone, Copy)]
struct StageOutcome {
    consumed: bool,
    layout_changed: bool,
}

impl StageOutcome {
    /// The stage did not claim the event and changed nothing; the next
    /// stage sees it.
    const PASS: Self = Self {
        consumed: false,
        layout_changed: false,
    };
    /// The stage claimed the event and changed nothing.
    const CONSUMED: Self = Self {
        consumed: true,
        layout_changed: false,
    };

    /// The stage claimed the event, possibly changing layout state.
    const fn consumed(layout_changed: bool) -> Self {
        Self {
            consumed: true,
            layout_changed,
        }
    }

    /// The stage let the event through, but changed layout state on the
    /// way (the which-key popup dismissal is the only such case).
    const fn passed(layout_changed: bool) -> Self {
        Self {
            consumed: false,
            layout_changed,
        }
    }
}

/// What one event contributed to the batch's accumulators.
#[derive(Clone, Copy, Default)]
struct EventChange {
    /// The event mutated state the caller must repaint.
    layout_changed: bool,
    /// The event queued a prediction, so the overlay wants a paint.
    predicted: bool,
}

/// Everything one dispatch batch threads through every per-event stage:
/// the render sink, the wire connection, the driver-owned focus / detach /
/// predict / pane mirrors, and the dispatch context.
///
/// This is the argument list of [`dispatch_input_events`] itself, bundled
/// once at the top of the batch so each stage takes one parameter instead
/// of eight. The public entry point keeps its flat signature — the driver
/// owns these pieces separately and lends them per call.
struct EventEnv<'a, 'c, W: crate::attach::RenderSink> {
    out: &'a mut W,
    conn: &'a mut Connection,
    focused_pane: &'a mut Option<TerminalId>,
    detach_pending: &'a mut bool,
    predict: &'a mut PredictionState,
    panes: &'a mut HashMap<TerminalId, PaneSlot>,
    ctx: &'a mut DispatchCtx<'c>,
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
///
/// Each event walks the stage pipeline in `EventEnv::dispatch_event`;
/// this is the batch frame around it — accumulate, then paint the
/// predictions once.
// arg list bundles transport + render + predict context; the driver owns
// each piece separately, so they arrive flat and are bundled into
// `EventEnv` for the stages below.
#[allow(clippy::too_many_arguments, reason = "see comment above")]
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub(in crate::attach) async fn dispatch_input_events<W: crate::attach::RenderSink>(
    out: &mut W,
    conn: &mut Connection,
    events: &mut Vec<InputEvent>,
    focused_pane: &mut Option<TerminalId>,
    detach_pending: &mut bool,
    predict: &mut PredictionState,
    overlay: &Overlay,
    panes: &mut HashMap<TerminalId, PaneSlot>,
    ctx: &mut DispatchCtx<'_>,
) -> Result<bool, AttachError> {
    let mut env = EventEnv {
        out,
        conn,
        focused_pane,
        detach_pending,
        predict,
        panes,
        ctx,
    };
    let mut predicted_any = false;
    let mut layout_changed = false;
    // Drained rather than consumed: the driver owns `events` for the life of
    // the attach and reuses its allocation for every batch (phux-l96p.4).
    for ev in events.drain(..) {
        let change = env.dispatch_event(ev).await?;
        layout_changed |= change.layout_changed;
        predicted_any |= change.predicted;
    }
    // Paint the prediction overlay once per dispatch batch so a burst of
    // keystrokes produces a single positioned write run, not one per
    // event. The overlay is a no-op on an empty queue.
    if predicted_any {
        env.paint_predictions(overlay);
    }
    // Hand the layout-mutation signal back to `main_loop`, which holds
    // the status-bar painter and session name needed for a proper full
    // frame. We never paint from here.
    Ok(layout_changed)
}

/// phux-foz.2: the which-key popup is transparent to input. It is
/// dismissed by — and never consumes — the next event: a key press
/// pops it and then executes exactly as if the popup were absent
/// (the resolver still holds the pending prefix, so the chord
/// completes normally), except Esc, which pops it AND cancels the
/// pending prefix without reaching the pane. Mouse input pops it
/// and cancels the prefix too (a click is not a chord
/// continuation), then routes normally. Non-press key events and
/// paste/focus bypass the popup entirely (it stays up; they flow
/// to the pane) — the popup must never eat or delay real input.
fn dismiss_passthrough_popup(ctx: &mut DispatchCtx<'_>, ev: &InputEvent) -> StageOutcome {
    use phux_protocol::input::key::KeyAction;
    if !ctx.overlays.top_is_passthrough() {
        return StageOutcome::PASS;
    }
    match ev {
        InputEvent::Key(key_event) if matches!(key_event.action, KeyAction::Press) => {
            let escape_cancels_prefix = ctx.overlays.passthrough_escape_cancels_prefix();
            ctx.overlays.dismiss();
            if key_event.key == PhysicalKey::Escape && escape_cancels_prefix {
                if let Some(resolver) = ctx.resolver.as_deref_mut() {
                    resolver.reset();
                }
                tracing::debug!("which-key: Esc cancelled the pending prefix");
                return StageOutcome::consumed(true);
            }
            // Fall through: the key executes as if no popup existed.
            StageOutcome::passed(true)
        }
        InputEvent::Mouse(_) => {
            ctx.overlays.dismiss();
            if let Some(resolver) = ctx.resolver.as_deref_mut() {
                resolver.reset();
            }
            // Fall through to normal mouse routing.
            StageOutcome::passed(true)
        }
        _ => StageOutcome::PASS,
    }
}

/// Whether `ev` is a key *press* — the gesture that snaps a scrolled
/// viewport back to the live screen.
const fn is_key_press(ev: &InputEvent) -> bool {
    matches!(
        ev,
        InputEvent::Key(key_event)
            if matches!(key_event.action, phux_protocol::input::key::KeyAction::Press)
    )
}

/// A primary-button press: the gesture the client's own chrome claims
/// (sidebar rows, status-bar tabs, divider grabs, drag-to-copy).
fn is_left_press(mouse: &MouseEvent) -> bool {
    matches!(mouse.action, MouseAction::Press) && mouse.button == MouseButton::Left
}

/// A secondary-button press: the context-menu gesture (phux-wrnm,
/// ADR-0058).
fn is_right_press(mouse: &MouseEvent) -> bool {
    matches!(mouse.action, MouseAction::Press) && mouse.button == MouseButton::Right
}

/// The three DEC private modes the wheel branch gates on, read in one
/// borrow of the pane's published mirror.
struct PaneScrollModes {
    wants_mouse_tracking: bool,
    alt_screen: bool,
    alt_scroll: bool,
}

/// Read [`PaneScrollModes`] off `target`'s published mirror, or `None`
/// when the pane has no mirror yet.
fn pane_scroll_modes(
    kernel: &crate::attach::pane_state::AttachKernel,
    target: &TerminalId,
) -> Option<PaneScrollModes> {
    let terminal = published_terminal(kernel, target)?;
    Some(PaneScrollModes {
        wants_mouse_tracking: terminal_wants_mouse_tracking(terminal),
        alt_screen: terminal_in_alt_screen(terminal),
        alt_scroll: terminal_alt_scroll(terminal),
    })
}

/// Whether `target`'s app has NOT enabled mouse tracking — the boundary
/// the pane context menu and drag-to-copy both respect: an inner program
/// that asked for the mouse (vim, htop, a TUI with its own right-click
/// menu) keeps every button.
fn pane_ignores_mouse(
    kernel: &crate::attach::pane_state::AttachKernel,
    target: &TerminalId,
) -> bool {
    published_terminal(kernel, target)
        .is_some_and(|terminal| !terminal_wants_mouse_tracking(terminal))
}

#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
impl<W: crate::attach::RenderSink> EventEnv<'_, '_, W> {
    /// Walk one event through the dispatch stages in the order they are
    /// defined below — which-key dismissal, overlay capture, resolver
    /// chord, mouse routing — and forward whatever none of them claimed
    /// to the focused pane. The first stage that claims the event wins.
    async fn dispatch_event(&mut self, ev: InputEvent) -> Result<EventChange, AttachError> {
        let mut change = EventChange::default();
        let popup = dismiss_passthrough_popup(self.ctx, &ev);
        change.layout_changed |= popup.layout_changed;
        if popup.consumed {
            return Ok(change);
        }
        let captured = self.capture_into_overlay(&ev).await?;
        change.layout_changed |= captured.layout_changed;
        if captured.consumed {
            return Ok(change);
        }
        let chord = self.intercept_chord(&ev).await?;
        change.layout_changed |= chord.layout_changed;
        if chord.consumed {
            return Ok(change);
        }
        let mouse = self.route_mouse_input(&ev).await?;
        change.layout_changed |= mouse.layout_changed;
        if mouse.consumed {
            return Ok(change);
        }
        if is_key_press(&ev) && self.snap_focused_viewport() {
            change.layout_changed = true;
        }
        change.predicted = self.feed_predict(&ev);
        change.layout_changed |= self.forward_to_focused_pane(ev).await?;
        Ok(change)
    }

    /// A key press headed for the pane snaps a scrolled viewport back to
    /// the live screen (tmux behavior). Without this, a wheel scroll into
    /// scrollback pins the viewport there forever and the pane looks
    /// frozen — new output (e.g. the shell prompt after a TUI app exits)
    /// lands below the visible rows and never paints. Runs BEFORE the
    /// predict peek so grid reads see the active area.
    fn snap_focused_viewport(&mut self) -> bool {
        snap_scrolled_viewport(
            self.ctx.engine_kernel,
            self.panes,
            self.ctx
                .workspace
                .active_window()
                .and_then(|w| w.focus.as_ref()),
        )
    }

    /// Run a [`ResolvedAction`](phux_config::keybind::ResolvedAction)
    /// through the single action path every trigger shares — keybinding,
    /// overlay commit, sidebar click, status-bar tab, context-menu row.
    /// Returns `true` iff the layout changed.
    async fn run_resolved(
        &mut self,
        resolved: &phux_config::keybind::ResolvedAction,
    ) -> Result<bool, AttachError> {
        let effects = run_action(resolved, self.ctx, self.focused_pane.as_ref(), self.panes);
        apply_action_effects(
            effects,
            self.out,
            self.conn,
            self.ctx,
            self.focused_pane,
            self.detach_pending,
            self.predict,
            self.panes,
        )
        .await
    }

    /// phux-5ke.4: while any overlay is active the stack captures all
    /// input. Key events flow to `OverlayState::handle_key`, which
    /// routes them to the *top* overlay (which may dismiss, popping
    /// back to whatever is beneath it); mouse / paste / focus events
    /// are dropped so they don't reach the pane underneath.
    ///
    /// The keybind resolver is bypassed entirely while an overlay is
    /// up: the overlay owns every keystroke, exactly as tmux's command
    /// prompt and menus consume the prefix key as literal input rather
    /// than firing prefix bindings. This keeps a prefix chord (e.g. the
    /// leader `C-a`) from being swallowed by the resolver before it can
    /// reach the overlay — a name typed into the rename prompt that
    /// starts with the leader key must land verbatim. Detach while a
    /// modal is open is reachable by dismissing first (Esc), then
    /// chording. The resolver is reset on entry so a partial chord begun
    /// before the overlay opened cannot leak into post-dismiss input.
    ///
    /// phux-foz.2: a passthrough popup (which-key) is excluded — the
    /// stage above already dismissed it for presses/mouse, and events
    /// it deliberately ignores (key release/repeat, paste, focus) must
    /// flow to the pane, not be captured (and must NOT reset the
    /// resolver, which is holding the pending prefix the popup shows).
    async fn capture_into_overlay(&mut self, ev: &InputEvent) -> Result<StageOutcome, AttachError> {
        if !self.ctx.overlays.is_active() || self.ctx.overlays.top_is_passthrough() {
            return Ok(StageOutcome::PASS);
        }
        let layout_changed = match ev {
            InputEvent::Key(key_event) => self.handle_overlay_key(key_event).await?,
            InputEvent::Mouse(mouse) => self.handle_overlay_mouse(mouse).await?,
            // Paste / focus are dropped so they don't reach the pane
            // underneath.
            _ => false,
        };
        Ok(StageOutcome::consumed(layout_changed))
    }

    /// Feed one key event to the top overlay and run whatever it commits.
    async fn handle_overlay_key(
        &mut self,
        key_event: &phux_protocol::input::key::KeyEvent,
    ) -> Result<bool, AttachError> {
        if let Some(resolver) = self.ctx.resolver.as_deref_mut() {
            resolver.reset();
        }
        let was_active = self.ctx.overlays.is_active();
        // phux-ahv.1: an overlay may commit an action (e.g. the
        // rename prompt returning `rename-window { name }`); run
        // it through the same path as a keybinding.
        let outcome = self.ctx.overlays.handle_key(key_event);
        let ran = self.apply_overlay_outcome(outcome).await?;
        // On dismiss, repaint everything: the overlay scribbled
        // over pane cells and we need a coherent base for the
        // next TERMINAL_OUTPUT.
        let dismissed = was_active && !self.ctx.overlays.is_active();
        Ok(ran || dismissed)
    }

    /// Feed one mouse event to the top overlay and run whatever it commits.
    async fn handle_overlay_mouse(&mut self, mouse: &MouseEvent) -> Result<bool, AttachError> {
        // Copy-mode tracks pane-local cells but the parser emits
        // outer-viewport coordinates; translate into the focused
        // pane's frame so a drag over a non-origin pane highlights
        // the cells actually under the pointer. Modal overlays (the
        // only other mouse consumers) keep viewport coords.
        let routed = if self.ctx.overlays.copy_selection().is_some() {
            let rect = focused_pane_rect(self.ctx, self.focused_pane.as_ref());
            let mut m = *mouse;
            m.x = (m.x - f64::from(rect.x)).max(0.0);
            m.y = (m.y - f64::from(rect.y)).max(0.0);
            m
        } else {
            *mouse
        };
        let was_active = self.ctx.overlays.is_active();
        let outcome = self.ctx.overlays.handle_mouse(&routed);
        // A pointer-driven copy commit repaints: the selection highlight
        // has to come back off the pane's cells.
        let copy_commit = matches!(outcome, OverlayOutcome::Copy(_));
        let ran = self.apply_overlay_outcome(outcome).await? || copy_commit;
        // phux-wrnm: a pointer dismissal (clicking outside a context
        // menu) leaves the overlay's cells on screen with nothing
        // scheduled to erase them — the key path has always
        // repainted on dismiss; the mouse path never did, because
        // until now no overlay could be dismissed by a click.
        let dismissed = was_active && !self.ctx.overlays.is_active();
        Ok(ran || dismissed)
    }

    /// Run one [`OverlayOutcome`] the overlay stack produced, returning
    /// `true` iff it changed layout state the caller must repaint.
    async fn apply_overlay_outcome(
        &mut self,
        outcome: OverlayOutcome,
    ) -> Result<bool, AttachError> {
        match outcome {
            OverlayOutcome::RunAction(resolved) => self.run_resolved(&resolved).await,
            OverlayOutcome::Copy(req) => {
                // Copy-mode commit: resolve the selection against the
                // focused pane's own engine and write it to the host
                // clipboard via OSC 52. Client-local per ADR-0030 —
                // no wire traffic.
                if let Some(fid) = self.focused_pane.as_ref()
                    && let Some(terminal) = published_terminal(self.ctx.engine_kernel, fid)
                {
                    crate::attach::copy::copy_to_host_clipboard(self.out, terminal, req)?;
                }
                Ok(false)
            }
            OverlayOutcome::ScrollViewport(delta) => Ok(scroll_focused_pane_viewport(
                self.ctx.engine_kernel,
                self.panes,
                self.focused_pane.as_ref(),
                delta,
            )),
            // Overlay consumed the event but nothing else to do.
            OverlayOutcome::None => Ok(false),
        }
    }

    /// phux-4li.5: resolver intercept. Runs BEFORE the predict layer
    /// so a chord that resolves to e.g. `focus-direction` doesn't
    /// leave a stale ghost overlay on the previous focused pane.
    async fn intercept_chord(&mut self, ev: &InputEvent) -> Result<StageOutcome, AttachError> {
        let InputEvent::Key(key_event) = ev else {
            return Ok(StageOutcome::PASS);
        };
        let Some(outcome) = consume_chord(self.ctx, key_event) else {
            return Ok(StageOutcome::PASS);
        };
        match outcome {
            // Still waiting on the next chord in a multi-chord
            // sequence; absorb the byte and move on.
            ChordOutcome::Partial => Ok(StageOutcome::CONSUMED),
            ChordOutcome::Resolved(resolved) => {
                let layout_changed = self.run_resolved(&resolved).await?;
                Ok(StageOutcome::consumed(layout_changed))
            }
        }
    }

    /// phux-4li.6 / ADR-0048: `INPUT_MOUSE` routing + click-to-focus +
    /// divider drag-to-resize. The parser emits mouse coordinates in
    /// outer-viewport cells (treated as 1-px-per-cell f64 per SPEC
    /// §9.2.1); we hit-test against the multi-pane composition's
    /// `Rect`s. A press on a divider cell *grabs* the split that
    /// divider controls; button-motion while grabbed re-tunes the
    /// split's ratio so the divider tracks the cursor; release drops
    /// the grab. A click in a pane forwards the event (with pane-local
    /// coords) to that pane — so an inner TUI that turned mouse
    /// tracking on still receives every pointer event over its own
    /// cells (the divider cells are the only ones whose meaning the
    /// client claims).
    ///
    /// Every mouse event that reaches this stage is claimed by it.
    async fn route_mouse_input(&mut self, ev: &InputEvent) -> Result<StageOutcome, AttachError> {
        use crate::attach::multi_pane::{RouteDecision, route_mouse_event};
        let InputEvent::Mouse(mouse) = ev else {
            return Ok(StageOutcome::PASS);
        };
        if let Some(outcome) = self.step_divider_drag(mouse).await? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.route_sidebar_click(mouse).await? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.route_status_bar_click(mouse).await? {
            return Ok(outcome);
        }
        // Hit-test against the SAME inset content rect the renderer tiles
        // into — status-bar row and sidebar columns folded off the outer
        // viewport. Routing against the full viewport instead disagrees with
        // what is painted: a click near a divider lands one row off (the
        // status bar) and, with a sidebar docked, one strip-width off in x,
        // so it focuses/forwards to the wrong pane. Clicks in the reserved
        // chrome miss every pane rect and become a Miss (dropped).
        let content = content_rect(self.ctx.viewport, self.ctx.bar, self.ctx.sidebar);
        // phux-jow6: hit-test against the RENDER layout, not the real
        // tiled tree. When a pane is zoomed (phux-x2hm) the render layout
        // is a single full-content leaf, so any click lands on the
        // visible zoomed pane instead of whichever hidden tiled pane sits
        // under the cursor. Compute the decision in a scope that drops the
        // borrowing `Cow` before the click-to-focus `active_window_mut()`
        // below needs the workspace mutably.
        let decision = {
            let Some(render_ls) = self.ctx.workspace.render_window(self.ctx.zoomed.as_ref()) else {
                tracing::debug!("dropping mouse event: no active window");
                return Ok(StageOutcome::CONSUMED);
            };
            route_mouse_event(&render_ls, content, self.ctx.viewport, mouse)
        };
        match decision {
            RouteDecision::Pane {
                target,
                pane_x,
                pane_y,
                focus_changed,
            } => {
                self.route_mouse_to_pane(mouse, target, (pane_x, pane_y), focus_changed)
                    .await
            }
            RouteDecision::Divider { node_path, axis } => Ok(StageOutcome::consumed(
                self.grab_divider(mouse, node_path, axis),
            )),
            RouteDecision::Miss => {
                tracing::trace!(x = mouse.x, y = mouse.y, "dropping mouse: no target");
                Ok(StageOutcome::CONSUMED)
            }
            RouteDecision::NoFocus => {
                tracing::debug!("dropping mouse event before ATTACHED");
                Ok(StageOutcome::CONSUMED)
            }
        }
    }

    /// Advance (or end) an in-flight divider drag. `None` when no drag is
    /// active and the event should route normally.
    async fn step_divider_drag(
        &mut self,
        mouse: &MouseEvent,
    ) -> Result<Option<StageOutcome>, AttachError> {
        // ADR-0048: a release ALWAYS ends any in-flight drag first,
        // regardless of where it lands — the cursor may have left the
        // divider cell mid-drag. The commit broadcasts the final
        // layout via SET_METADATA, the same persistence path the
        // keyboard resize uses, so other attached clients converge. A
        // release with no active drag falls through to normal routing
        // (an inner app may want it).
        if matches!(mouse.action, MouseAction::Release) && self.ctx.drag.is_some() {
            *self.ctx.drag = None;
            self.broadcast_dragged_layout().await?;
            tracing::debug!("divider drag: released, broadcast layout");
            return Ok(Some(StageOutcome::CONSUMED));
        }
        // While a divider is grabbed, motion re-tunes that split and
        // nothing reaches a pane. Press/other actions fall through.
        if let Some(grab) = self.ctx.drag.clone()
            && matches!(mouse.action, MouseAction::Motion)
        {
            return Ok(Some(StageOutcome::consumed(drag_resize(
                self.ctx, mouse, &grab,
            ))));
        }
        // phux-npb3 hardening (PR #142 review, recorded in ADR-0048):
        // while a divider drag is active, ONLY a release ends it and
        // ONLY motion re-tunes it — both handled above. Anything else
        // (notably a second Press from a chorded button, a wheel tick,
        // or a re-encoded press glitch) is consumed here so it cannot
        // fall through to normal routing mid-drag, where it would
        // forward to a pane, move focus, or grab a second divider while
        // the first grab is still live.
        if self.ctx.drag.is_some() {
            tracing::trace!(
                action = ?mouse.action,
                button = ?mouse.button,
                "dropping mouse event during divider drag"
            );
            return Ok(Some(StageOutcome::CONSUMED));
        }
        Ok(None)
    }

    /// Broadcast the layout a finished divider drag produced via
    /// `SET_METADATA`, so other attached clients converge on it.
    async fn broadcast_dragged_layout(&mut self) -> Result<(), AttachError> {
        if let Some(session) = self.ctx.focused_session
            && let Some(bytes) = encode_layout_or_log(self.ctx.workspace)
        {
            let request_id = *self.ctx.next_request_id;
            *self.ctx.next_request_id = self.ctx.next_request_id.wrapping_add(1);
            self.conn
                .send(&FrameKind::SetMetadata {
                    request_id,
                    scope: Scope::Group(DEFAULT_GROUP_ID),
                    key: layout_key(session),
                    value: bytes,
                })
                .await?;
        }
        Ok(())
    }

    /// phux-fce4: the sidebar strip claims every pointer event over
    /// its own cells BEFORE pane routing — its rows are hit targets,
    /// not pane content. A left press resolves against the strip's
    /// row model (`sidebar::hit_test`) and dispatches the mapped
    /// action through the same `run_action` path a keybinding or
    /// palette row uses: a window block commits `select-window`, an
    /// agents-section row (phux-foz.9) `select-window` for the
    /// window holding that agent's pane, the `+ new` affordance
    /// `new-window`, `= menu` the command palette (the
    /// session/plugin menu), and the bottom-corner collapse chevron
    /// `toggle-sidebar`. Everything else over the strip (motion,
    /// non-left presses, headers, blank rows, the separator column)
    /// is consumed and dropped so it can never leak into a pane
    /// whose rect does not contain it anyway.
    ///
    /// `None` when the pointer is not over the strip.
    async fn route_sidebar_click(
        &mut self,
        mouse: &MouseEvent,
    ) -> Result<Option<StageOutcome>, AttachError> {
        let Some(res) = self.ctx.sidebar else {
            return Ok(None);
        };
        let strip = crate::attach::paint::sidebar_rect(self.ctx.viewport, res);
        let (cell_x, cell_y) = (quantize_cell(mouse.x), quantize_cell(mouse.y));
        if !strip_contains(strip, cell_x, cell_y) {
            return Ok(None);
        }
        let hit = sidebar_click_action(strip, self.ctx.sidebar_targets, cell_x, cell_y);
        let mut layout_changed = false;
        if is_left_press(mouse) {
            if let Some(resolved) = hit {
                tracing::debug!(action = %resolved.action, "sidebar: click dispatched");
                layout_changed = self.run_resolved(&resolved).await?;
            }
        } else if is_right_press(mouse) {
            // phux-wrnm: a right press on a window block (or an
            // agents-section row, which resolves to the window
            // holding that agent) selects that window first —
            // acting on what you pointed at is the whole promise
            // of a context menu — and then opens the window menu
            // for it. Every other cell of the strip is session
            // chrome and gets the session menu, so a right-click
            // anywhere on the sidebar does something useful.
            let window_row = hit.filter(|r| r.action == "select-window");
            layout_changed = self
                .open_chrome_context_menu(window_row, (cell_x, cell_y))
                .await?;
        }
        Ok(Some(StageOutcome::consumed(layout_changed)))
    }

    /// phux-foz.12: the status-bar row is chrome, not pane content —
    /// `content_rect` already excludes it, so every pointer event
    /// here used to fall through to a Miss and get dropped. Claim
    /// the row explicitly instead: a left press on a window tab
    /// (resolved against the painter's cached strip, so the hit
    /// targets are exactly the cells on screen) dispatches
    /// `select-window { index }` through the same `run_action`
    /// path the sidebar affordances and keybindings use. phux-qtw8:
    /// the sidebar strip is full-height and claims its columns on
    /// THIS row too — but it hit-tests first (above), so by here the
    /// event is in the bar's own inset span and `window_hit_at`
    /// (which indexes off the origin it painted at) resolves it.
    /// Pane content is untouched — everything else on the row
    /// (non-tab cells, motion, wheel, non-left buttons) is consumed
    /// and dropped, matching the pre-claim behavior bit for bit.
    ///
    /// `None` when the pointer is not on the bar's row.
    async fn route_status_bar_click(
        &mut self,
        mouse: &MouseEvent,
    ) -> Result<Option<StageOutcome>, AttachError> {
        let Some(pos) = self.ctx.bar else {
            return Ok(None);
        };
        let bar_row = match pos {
            crate::render::chrome::status_bar::Position::Bottom => {
                self.ctx.viewport.1.saturating_sub(1)
            }
            crate::render::chrome::status_bar::Position::Top => 0,
        };
        let (cell_x, cell_y) = (quantize_cell(mouse.x), quantize_cell(mouse.y));
        if self.ctx.viewport.1 == 0 || cell_y != bar_row {
            return Ok(None);
        }
        let hit = bar_click_action(self.ctx.status_bar, cell_x);
        let mut layout_changed = false;
        if is_left_press(mouse) {
            if let Some(resolved) = hit {
                tracing::debug!(action = %resolved.action, "status bar: tab click dispatched");
                layout_changed = self.run_resolved(&resolved).await?;
            }
        } else if is_right_press(mouse) {
            // phux-wrnm: right press on a tab selects that window
            // (same as a left click) and opens its window menu;
            // elsewhere on the bar — the session name, the
            // widgets, the blank padding — the session menu. The
            // menu is clamped into the content rect, so a
            // bottom-docked bar opens it upward, over the panes.
            layout_changed = self.open_chrome_context_menu(hit, (cell_x, cell_y)).await?;
        }
        Ok(Some(StageOutcome::consumed(layout_changed)))
    }

    /// phux-wrnm: commit `window_row` (when the right press landed on a
    /// window target) and open the matching context menu at `anchor` —
    /// the window menu when it did, the session menu otherwise. Shared
    /// by the sidebar strip and the status-bar row.
    async fn open_chrome_context_menu(
        &mut self,
        window_row: Option<phux_config::keybind::ResolvedAction>,
        anchor: (u16, u16),
    ) -> Result<bool, AttachError> {
        let is_window = window_row.is_some();
        let layout_changed = match window_row {
            Some(resolved) => self.run_resolved(&resolved).await?,
            None => false,
        };
        let spec = if is_window {
            crate::attach::context_menu::window_menu(
                self.ctx.keybindings,
                &active_window_name(self.ctx),
            )
        } else {
            crate::attach::context_menu::session_menu(self.ctx.keybindings, self.ctx.session_name)
        };
        open_context_menu(self.ctx, spec, anchor);
        Ok(layout_changed)
    }

    /// Route a pointer event that landed inside a pane's rect: click to
    /// focus, then the pane-level gestures the client claims (wheel,
    /// context menu, drag-to-copy), and finally the `INPUT_MOUSE`
    /// forward to the pane itself.
    async fn route_mouse_to_pane(
        &mut self,
        mouse: &MouseEvent,
        target: TerminalId,
        pane_xy: (f64, f64),
        focus_changed: bool,
    ) -> Result<StageOutcome, AttachError> {
        // Heavy-edge chrome moves with focus; repaint
        // dividers + all leaves so the focused pane's
        // surrounding edges render heavy.
        let layout_changed = focus_changed;
        if focus_changed {
            self.focus_pane_from_click(&target);
        }
        // phux-npb3: a pane opted out via `set-pane mouse off`
        // receives no client-synthesized mouse at all — no
        // INPUT_MOUSE forward, no local wheel viewport scroll.
        // Click-to-focus above still applies: it is chrome-level
        // (the pane never sees it) and it is also the path that
        // makes the driver drop outer capture once the opted-out
        // pane is focused, restoring the host's raw handling.
        if self.ctx.mouse_optout.contains(&target) {
            tracing::trace!(
                terminal = ?target,
                "dropping mouse event: pane opted out (set-pane mouse off)"
            );
            return Ok(StageOutcome::consumed(layout_changed));
        }
        let mut routed = *mouse;
        routed.x = pane_xy.0;
        routed.y = pane_xy.1;
        if let Some(scrolled) = self.scroll_pane_wheel(&target, &routed).await? {
            return Ok(StageOutcome::consumed(layout_changed || scrolled));
        }
        if self.open_pane_context_menu(mouse, &target) {
            return Ok(StageOutcome::consumed(layout_changed));
        }
        if self.begin_drag_to_copy(&routed, &target) {
            return Ok(StageOutcome::consumed(layout_changed));
        }
        self.conn
            .send(&FrameKind::InputMouse {
                terminal_id: target,
                event: scale_to_surface_pixels(routed, self.ctx.cell_px),
            })
            .await?;
        Ok(StageOutcome::consumed(layout_changed))
    }

    /// Move client-local focus to the clicked pane.
    fn focus_pane_from_click(&mut self, target: &TerminalId) {
        if let Some(ls) = self.ctx.workspace.active_window_mut() {
            ls.focus = Some(target.clone());
        }
        apply_focus_transition(
            &mut self.ctx.focus_history,
            self.focused_pane,
            target.clone(),
        );
        // Re-anchor predict to the clicked pane: drop the
        // old pane's queue AND reset the cursor + viewport
        // to the new pane, so a keystroke before the next
        // reconcile echoes at the right place rather than
        // the old pane's (mid-screen) coordinates (phux-7ry0).
        reanchor_predict_to_pane(self.predict, self.panes, target);
    }

    /// Handle a wheel notch over `target`. `None` when the event is not a
    /// wheel notch the client claims (it forwards to the pane instead);
    /// `Some(layout_changed)` when the client consumed it.
    async fn scroll_pane_wheel(
        &mut self,
        target: &TerminalId,
        routed: &MouseEvent,
    ) -> Result<Option<bool>, AttachError> {
        let Some(delta) = wheel_scroll_delta(routed) else {
            return Ok(None);
        };
        let Some(modes) = pane_scroll_modes(self.ctx.engine_kernel, target) else {
            return Ok(None);
        };
        if modes.wants_mouse_tracking {
            return Ok(None);
        }
        // xterm "alternate scroll" (DECSET 1007, on by
        // default in libghostty): the alt screen has no
        // scrollback, so the viewport scroll below would be
        // a silent no-op there and the wheel would go dead
        // in any full-screen app that doesn't track the
        // mouse (pagers, vim with mouse off). Convert each
        // wheel notch into arrow-key presses instead — the
        // same translation tmux and ghostty perform. Apps
        // opt out with `?1007l` (phux-yyex).
        if modes.alt_screen && modes.alt_scroll {
            self.send_wheel_as_arrows(target, delta).await?;
            return Ok(Some(false));
        }
        Ok(Some(self.scroll_pane_viewport(target, delta)))
    }

    /// Emit one arrow-key press per wheel notch — the alternate-scroll
    /// translation.
    async fn send_wheel_as_arrows(
        &mut self,
        target: &TerminalId,
        delta: isize,
    ) -> Result<(), AttachError> {
        let arrow = make_named_key(
            if delta < 0 {
                PhysicalKey::ArrowUp
            } else {
                PhysicalKey::ArrowDown
            },
            ModSet::empty(),
        );
        for _ in 0..delta.unsigned_abs() {
            self.conn
                .send(&FrameKind::InputKey {
                    terminal_id: target.clone(),
                    event: arrow.clone(),
                })
                .await?;
        }
        Ok(())
    }

    /// Scroll `target`'s local mirror by `delta`, returning `true` iff the
    /// viewport actually moved (the caller repaints).
    fn scroll_pane_viewport(&mut self, target: &TerminalId, delta: isize) -> bool {
        let scrolled = self
            .ctx
            .engine_kernel
            .published_engine_mut(target)
            .is_some_and(|replica| {
                replica
                    .scroll_viewport(ScrollViewport::Delta(delta))
                    .is_ok()
            });
        if !scrolled {
            return false;
        }
        if delta < 0
            && let Some(slot) = self.panes.get_mut(target)
        {
            slot.viewport_scrolled = true;
        }
        true
    }

    /// phux-wrnm (ADR-0058): a right press on a pane whose app
    /// has NOT enabled mouse tracking opens the pane context
    /// menu at the pointer. The gate is the same boundary
    /// drag-to-copy respects: an inner program that asked for
    /// the mouse (vim, htop, a TUI with its own right-click
    /// menu) keeps every button, and the keyboard-bindable
    /// `context-menu` action is the way in for those panes.
    /// Click-to-focus above has already run, so the menu acts
    /// on the pane you pointed at, not the one you left.
    ///
    /// The menu is anchored in viewport cells, so this takes the
    /// un-routed event.
    fn open_pane_context_menu(&mut self, mouse: &MouseEvent, target: &TerminalId) -> bool {
        if !is_right_press(mouse) || !pane_ignores_mouse(self.ctx.engine_kernel, target) {
            return false;
        }
        let zoomed = self.ctx.zoomed.as_ref() == Some(target);
        let spec = crate::attach::context_menu::pane_menu(self.ctx.keybindings, zoomed);
        open_context_menu(
            self.ctx,
            spec,
            (quantize_cell(mouse.x), quantize_cell(mouse.y)),
        );
        true
    }

    /// Drag-to-copy (tmux convention): a left press on a pane
    /// whose app has NOT enabled mouse tracking starts a
    /// copy-mode selection anchored at the click. Motion and
    /// release then route through the overlay stage above —
    /// release copies to the host clipboard (OSC 52) and
    /// dismisses; a click without drag just dismisses. Apps
    /// that DO track the mouse (vim, htop) keep receiving
    /// their events untouched.
    fn begin_drag_to_copy(&mut self, routed: &MouseEvent, target: &TerminalId) -> bool {
        if !is_left_press(routed) || !pane_ignores_mouse(self.ctx.engine_kernel, target) {
            return false;
        }
        let rect = focused_pane_rect(self.ctx, self.focused_pane.as_ref());
        self.ctx
            .overlays
            .push(Box::new(crate::render::overlay::CopyModeOverlay::new(
                0, 0, rect.w, rect.h,
            )));
        // Seed anchor + cursor from the (pane-local) press.
        let _ = self.ctx.overlays.handle_mouse(routed);
        true
    }

    /// ADR-0048: a LEFT-button press on a divider starts a drag
    /// and immediately snaps the split to the press position (so
    /// a click-without-motion still nudges, matching the
    /// intuitive "grab here"). Scroll-wheel and right/middle
    /// presses encode as Press too, but landing on a 1-cell
    /// divider must not snap the split — those, and stray
    /// grab-less motions, are dropped (the divider gap has no
    /// pane to forward to).
    fn grab_divider(
        &mut self,
        mouse: &MouseEvent,
        node_path: crate::layout::NodePath,
        axis: crate::layout::SplitDir,
    ) -> bool {
        if !is_left_press(mouse) {
            tracing::trace!(x = mouse.x, y = mouse.y, "dropping mouse on divider");
            return false;
        }
        let grab = DragGrab { node_path, axis };
        let layout_changed = drag_resize(self.ctx, mouse, &grab);
        *self.ctx.drag = Some(grab);
        tracing::debug!("divider drag: grabbed");
        layout_changed
    }

    /// Predictive echo only fires for key events; mouse / paste / focus
    /// intentionally bypass the prediction layer (they target the
    /// server's input model, not the visual grid). The stage is
    /// skipped entirely when the config flag is off — `predict_key`
    /// returns `Disabled` and no overlay paint is scheduled.
    ///
    /// Arrows over a known cell on the current line (phux-9gw.1.3)
    /// need a grid peek to know the width of the grapheme they step
    /// over; we hand `read_grapheme_at` to the predict layer so it
    /// can refuse the prediction when the cell is blank.
    ///
    /// phux-4li.6: peek the focused pane's grid via the active
    /// window's focus. The driver also mirrors that id into its
    /// `focused_pane` local (server-frame handlers rely on it);
    /// either reads the same `TerminalId` here.
    ///
    /// ADR-0090: predictions queue on both screens; only *display* is
    /// policy. The predictor learns which screen the pane is on (a
    /// transition drops the queue and the echo evidence) and stamps
    /// each guess with a monotonic clock so the display TTL can expire
    /// an overlay the server never answered. On the alternate screen
    /// the overlay stays hidden until the app proves it echoes (vim
    /// insert mode, an agent TUI's prompt), so non-echoing apps (htop,
    /// less) behave exactly as under the retired binary gate
    /// (phux-51n6.1). The keystroke still travels upstream normally
    /// afterwards.
    fn feed_predict(&mut self, ev: &InputEvent) -> bool {
        use crate::predict::PredictionOutcome;
        let InputEvent::Key(key_event) = ev else {
            return false;
        };
        if !self.predict.is_enabled() {
            return false;
        }
        let Some(fid) = self
            .ctx
            .workspace
            .active_window()
            .and_then(|w| w.focus.as_ref())
        else {
            return false;
        };
        let Some(walk) = published_replica(self.ctx.engine_kernel, fid) else {
            return false;
        };
        let Some(slot) = self.panes.get_mut(fid) else {
            return false;
        };
        self.predict
            .set_alt_screen(terminal_in_alt_screen(walk.terminal));
        let outcome = self
            .predict
            .predict_key_with_grid_at(key_event, predict_now_ms(), |r, c| {
                slot.renderer.read_grapheme_at(walk, r, c).ok().flatten()
            });
        matches!(outcome, PredictionOutcome::Predicted)
    }

    /// phux-4li.6: `INPUT_KEY` / `INPUT_FOCUS` / `INPUT_PASTE` all target
    /// the client's focused pane (per ADR-0019 decision 6). Focus
    /// is canonically the active window's focus; the driver-side
    /// `focused_pane` mirror stays in sync for the render path.
    /// When focus is unset (pre-ATTACHED), drop the event with a
    /// debug log instead of panicking — wave-A's "always Some
    /// post-ATTACHED" invariant is enforced by the seed in
    /// `handle_server_frame`, but a stray input race during
    /// bootstrap shouldn't take the loop down.
    async fn forward_to_focused_pane(&mut self, ev: InputEvent) -> Result<bool, AttachError> {
        let Some(pane) = self
            .ctx
            .workspace
            .active_window()
            .and_then(|w| w.focus.as_ref())
            .cloned()
        else {
            tracing::debug!("dropping input received before ATTACHED");
            return Ok(false);
        };
        // phux-foz.1: forwarding key/paste input to a pane answers (or at
        // least engages) its pending agent question, so clear its asked
        // attention flag. Focus/mouse events don't clear — merely looking
        // at a pane is not answering it. A real transition schedules the
        // chrome repaint via the returned flag.
        let layout_changed = matches!(ev, InputEvent::Key(_) | InputEvent::Paste(_))
            && clear_attention_on_input(self.panes, &pane);
        // ADR-0053: on a remote reconnect lane, a bracketed paste — the one
        // composed, non-latency-sensitive batch this surface produces — goes
        // through the acknowledged `APPLY_INPUT` journal so it survives a
        // mid-flight reconnect under one idempotent operation id. Keystrokes
        // and mouse stay fire-and-forget by design (ADR-0053 point 8), and
        // the server's input-lane FIFO keeps a same-connection key from
        // overtaking the acknowledged batch. Everything the journal cannot
        // honestly carry — a satellite-routed pane (APPLY_INPUT is
        // local-only), a batch over the wire caps, an inactive journal — falls
        // back to today's fire-and-forget `INPUT_PASTE`, byte-identical.
        if matches!(ev, InputEvent::Paste(_))
            && pane.host().is_none()
            && let Some(journal) = self.ctx.input_replay
            && journal.borrow().active()
            && crate::agent_prompt::validate_batch(std::slice::from_ref(&ev)).is_ok()
        {
            // Scoped so the RefCell borrow provably ends before any await.
            let (reports, frame) = {
                let mut journal = journal.borrow_mut();
                journal.submit(pane.clone(), vec![ev]);
                journal.next_frame(&mut *self.ctx.next_request_id)
            };
            // A strand at submit time can only be an OLDER queued operation
            // crossing the retry horizon. Dispatch has no notice channel;
            // the trace line keeps the outcome from vanishing entirely.
            for report in reports {
                tracing::warn!(line = %report.notice_line(), "acknowledged paste stranded");
            }
            if let Some(frame) = frame {
                self.conn.send(&frame).await?;
            }
            return Ok(layout_changed);
        }
        self.conn.send(&ev.into_frame(pane)).await?;
        Ok(layout_changed)
    }

    /// Paint the queued predictions. Predictions are pane-local; shift
    /// them by the focused pane's render origin so a non-top-left pane
    /// echoes over its own cells (phux-7ry0). ADR-0090: the display
    /// policy gates the paint — on the alternate screen without echo
    /// evidence (or while tentative / past the TTL) the queue reconciles
    /// silently and nothing is painted.
    fn paint_predictions(&mut self, overlay: &Overlay) {
        if !self.predict.should_display(predict_now_ms()) {
            return;
        }
        let origin = self
            .ctx
            .workspace
            .active_window()
            .and_then(|w| w.focus.as_ref())
            .and_then(|fid| self.panes.get(fid))
            .map_or((0, 0), |s| s.renderer.last_origin());
        let _ = overlay.render(self.predict, origin, self.out);
    }
}

pub(super) fn wheel_scroll_delta(mouse: &MouseEvent) -> Option<isize> {
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
pub(super) fn scale_to_surface_pixels(mut mouse: MouseEvent, cell_px: (u16, u16)) -> MouseEvent {
    mouse.x *= f64::from(cell_px.0.max(1));
    mouse.y *= f64::from(cell_px.1.max(1));
    mouse
}
pub(super) fn terminal_wants_mouse_tracking(terminal: &libghostty_vt::Terminal<'_, '_>) -> bool {
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
pub(super) fn terminal_alt_scroll(terminal: &libghostty_vt::Terminal<'_, '_>) -> bool {
    terminal.mode(Mode::ALT_SCROLL).unwrap_or(false)
}

/// Monotonic milliseconds since the first call, for stamping predictions
/// and evaluating the ADR-0090 display policy. Process-local epoch: the
/// absolute value is meaningless, only differences matter, which is all
/// [`PredictionState::should_display`] needs. Lives here (not in
/// `phux-client-core`) because `std::time::Instant` is unavailable on the
/// wasm targets the core also serves.
pub(in crate::attach) fn predict_now_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = *EPOCH.get_or_init(Instant::now);
    u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Whether the pane's mirror is on the alternate screen buffer — the
/// screen-mode signal for predictive echo's confirmation-gated display
/// (ADR-0090).
///
/// A pane running vim/nvim, `less`, `htop`, a pager, or an agent TUI (Claude
/// Code, codex) switches to the alternate screen via DEC private mode `?1049h`
/// (or the legacy `?1047h` / `?47h`). The driver feeds this into
/// [`PredictionState::set_alt_screen`], which flips the display policy to
/// confirmation-gated: predictions still queue and reconcile there, but the
/// overlay stays hidden until the app proves it echoes. libghostty tracks
/// each variant independently and reports it via `terminal.mode()` (verified
/// against a `?1049h`/`?1047h` probe), the same query path the mouse-tracking
/// and synchronized-output gates use.
pub(in crate::attach) fn terminal_in_alt_screen(
    terminal: &libghostty_vt::Terminal<'_, '_>,
) -> bool {
    [
        Mode::ALT_SCREEN_SAVE,
        Mode::ALT_SCREEN,
        Mode::ALT_SCREEN_LEGACY,
    ]
    .into_iter()
    .any(|mode| terminal.mode(mode).unwrap_or(false))
}
pub(super) fn scroll_focused_pane_viewport(
    kernel: &mut crate::attach::pane_state::AttachKernel,
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
pub(super) fn snap_scrolled_viewport(
    kernel: &mut crate::attach::pane_state::AttachKernel,
    panes: &mut HashMap<TerminalId, PaneSlot>,
    focused_pane: Option<&TerminalId>,
) -> bool {
    let Some((fid, slot)) = focused_pane.and_then(|fid| panes.get_mut(fid).map(|slot| (fid, slot)))
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

pub(super) fn focused_pane_rect(
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

/// Resolve `SPAWN_TERMINAL.initial_size` for a spawn this client is about to
/// issue (phux-a5xj), by asking `predict` for the tile the new leaf will
/// occupy in the current content rect.
///
/// `None` — and therefore an absent wire field — whenever the server did not
/// advertise the capability, the content rect is degenerate, or `predict`
/// cannot answer. Every one of those falls back to the pre-field behavior:
/// the server spawns at its default and the reflow resize sizes the pane.
pub(super) fn spawn_initial_size(
    ctx: &DispatchCtx<'_>,
    predict: impl FnOnce(crate::layout::Rect) -> Option<(u16, u16)>,
) -> Option<(u16, u16)> {
    if !ctx.spawn_initial_size_supported {
        return None;
    }
    let content = content_rect(ctx.viewport, ctx.bar, ctx.sidebar);
    // A zero axis means there is nothing to render into; the server reads a
    // zero as "unknown" anyway, so do not spend a field on it.
    predict(content).filter(|&(cols, rows)| cols > 0 && rows > 0)
}

/// [`spawn_initial_size`] for a `split-pane`: tile the split this client is
/// about to ask for and read the new leaf's rect out of it.
pub(super) fn predicted_split_size(
    ctx: &DispatchCtx<'_>,
    pending: &PendingSplit,
) -> Option<(u16, u16)> {
    let active = ctx.workspace.active_window()?.clone();
    spawn_initial_size(ctx, |content| {
        actions::predicted_spawn_dims(&active, pending, content)
    })
}

/// Stamp `size` onto an already-built `SPAWN_TERMINAL` frame — the plugin-pane
/// path builds the frame from its manifest entry before it knows which
/// placement (and therefore which tile) it is about to park.
pub(super) const fn set_spawn_initial_size(frame: &mut FrameKind, size: Option<(u16, u16)>) {
    if let FrameKind::SpawnTerminal { initial_size, .. } = frame {
        *initial_size = size;
    }
}

pub(in crate::attach) fn focused_pane_rect_for(
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

/// phux-z6wt: single choke point for "the focused pane's rect may have
/// changed without a SIGWINCH firing" — recomputes it via
/// [`focused_pane_rect_for`] and fans it out to every surviving overlay
/// ([`OverlayState::on_viewport_resize`]).
///
/// PR #331 (phux-d26y) added that fan-out only on the SIGWINCH edge, but a
/// peer's layout broadcast (`FrameOutcome::layout_replaced` in
/// `server_frame.rs`) moves the focused pane's rect too, with no SIGWINCH
/// involved. The same flag also covers the TerminalSpawned/TerminalClosed
/// reflow path — every `reflow_panes: true` in `server_frame.rs` is emitted
/// alongside `layout_replaced: true` — so routing through `layout_replaced`
/// picks up both triggers via one call site instead of three. Toggling zoom
/// or the sidebar can move the rect too, but both are local keybindings
/// dispatched through this same module, which routes every key to the
/// active overlay while one is up (copy-mode included); they cannot fire
/// while an overlay needs this fan-out, so they are deliberately not wired
/// here.
///
/// Copy-mode is the only overlay this matters to today (see
/// [`crate::render::overlay::copy_mode`]); every other overlay's
/// `on_viewport_resize` is a no-op, and the `is_active` guard keeps the
/// steady-state (no overlay up) cost at one `Vec::is_empty`.
pub(in crate::attach) fn sync_overlays_to_focused_pane(
    overlays: &mut OverlayState,
    workspace: &Workspace,
    zoomed: Option<&TerminalId>,
    focused_pane: Option<&TerminalId>,
    viewport: (u16, u16),
    bar: Option<crate::render::chrome::status_bar::Position>,
    sidebar: Option<SidebarReservation>,
) {
    if !overlays.is_active() {
        return;
    }
    let pane = focused_pane_rect_for(workspace, zoomed, focused_pane, viewport, bar, sidebar);
    overlays.on_viewport_resize(pane.w, pane.h);
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
pub(super) fn drag_resize(ctx: &mut DispatchCtx<'_>, mouse: &MouseEvent, grab: &DragGrab) -> bool {
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
pub(super) const fn strip_contains(rect: crate::layout::Rect, x: u16, y: u16) -> bool {
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
/// * a zone-1 `needs you` row (phux-k0cw) commits `select-window` when the
///   agent is in this session, and `switch-session { name, window, pane }`
///   when it is in another one — the row resolves through `targets`, which
///   carries the NAME the frame was painted with rather than re-deriving it
///   from a queue that may have reordered since;
/// * a zone-3 roster row commits `switch-session { name }`;
/// * either zone's overflow row commits `agent-fleet` — the strip drops
///   rows, the dashboard is where they all still are;
/// * `+ new` commits `new-window` (the strip lists windows, so its create
///   affordance creates one);
/// * `= menu` commits `command-palette` — the menu covering window,
///   session (`new-session` included), and plugin actions via the action
///   registry;
/// * the collapse chevron in the bottom corner (phux-foz.9) commits
///   `toggle-sidebar`.
pub(super) fn sidebar_click_action(
    strip: crate::layout::Rect,
    targets: &crate::render::chrome::sidebar::SidebarTargets,
    x: u16,
    y: u16,
) -> Option<phux_config::keybind::ResolvedAction> {
    use crate::render::chrome::sidebar::{SidebarHit, SidebarTarget, hit_test};
    let select_window = |i: usize| {
        let mut args = std::collections::BTreeMap::new();
        args.insert(
            "index".to_owned(),
            toml::Value::Integer(i64::try_from(i).ok()?),
        );
        Some(("select-window", args))
    };
    let (action, args) = match hit_test(strip, targets.counts, x, y)? {
        SidebarHit::Window(i) => select_window(i)?,
        SidebarHit::NeedsYou(j) => match targets.needs_you.get(j)? {
            SidebarTarget::Window(i) => select_window(*i)?,
            SidebarTarget::Session { name, window, pane } => {
                let mut args = std::collections::BTreeMap::new();
                args.insert("name".to_owned(), toml::Value::String(name.clone()));
                args.insert(
                    "window".to_owned(),
                    toml::Value::Integer(i64::try_from(*window).ok()?),
                );
                args.insert(
                    "pane".to_owned(),
                    toml::Value::Integer(i64::try_from(*pane).ok()?),
                );
                ("switch-session", args)
            }
        },
        SidebarHit::Roster(j) => {
            let mut args = std::collections::BTreeMap::new();
            args.insert(
                "name".to_owned(),
                toml::Value::String(targets.roster.get(j)?.clone()),
            );
            ("switch-session", args)
        }
        SidebarHit::Fleet => ("agent-fleet", std::collections::BTreeMap::new()),
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
pub(super) fn bar_click_action(
    painter: Option<&crate::render::chrome::status_bar::StatusBarPainter>,
    x: u16,
) -> Option<phux_config::keybind::ResolvedAction> {
    match painter?.hit_at(x)? {
        phux_config::widget::CellHit::Window(index) => {
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
        // The `switch` chip opens the fleet dashboard — the same overlay
        // `prefix A` opens, through the same dispatch path. It is the
        // right target for a pointer because it is the *only* switcher
        // that answers all three questions at once (which sessions, which
        // windows, which agent needs me), and on the narrow terminal
        // where the chip is shown that is the whole point.
        phux_config::widget::CellHit::Switch => Some(phux_config::keybind::ResolvedAction {
            action: "agent-fleet".to_owned(),
            args: std::collections::BTreeMap::new(),
        }),
    }
}

/// phux-wrnm: push `spec` as a context menu anchored at the viewport cell
/// `anchor` (ADR-0058).
///
/// The menu is clamped inside the pane content rect — the same rect the
/// panes tile into and centered modals are placed against — so it can
/// never occlude the sidebar strip or the status-bar row, including when
/// the click that opened it landed on that chrome.
pub(super) fn open_context_menu(
    ctx: &mut DispatchCtx<'_>,
    spec: crate::attach::context_menu::MenuSpec,
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
pub(super) fn active_window_name(ctx: &DispatchCtx<'_>) -> String {
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
pub(super) fn quantize_cell(p: f64) -> u16 {
    if p.is_nan() || p < 0.0 {
        0
    } else if p >= f64::from(u16::MAX) {
        u16::MAX
    } else {
        p as u16
    }
}

/// Apply a client-local focus change through the single MRU transition path.
pub(super) fn apply_focus_transition(
    history: &mut FocusHistory,
    focused_pane: &mut Option<TerminalId>,
    target: TerminalId,
) {
    history.transition(focused_pane, Some(target));
}
