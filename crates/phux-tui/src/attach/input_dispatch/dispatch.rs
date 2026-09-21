//! The event dispatcher: parser events to wire frames, resolver
//! intercepts, mouse routing, and the pane/overlay geometry helpers.

//! Input dispatcher: translates parser-emitted events into wire frames
//! or layout-action effects.
//!
//! Owns the resolver-intercept path (prefix chord → `ResolvedAction` →
//! mutate the active window of the `Workspace`), the predict overlay's
//! keystroke feed, and the parked-spawn bookkeeping (`PendingSplit` /
//! `PendingWindow`) that bridges a local `split-pane` / `new-window`
//! chord to its remote `SPAWN_RESOURCE` reply.

use std::collections::HashMap;

use libghostty_vt::terminal::{Mode, Point, PointCoordinate, PointSpace, ScrollViewport};
use phux_protocol::ResourceId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::wire::frame::{FrameKind, Scope};

use crate::attach::actions::{self, PendingSplit};
use crate::attach::connection::Connection;
use crate::attach::focus::FocusHistory;
use crate::attach::input::make_named_key;
use crate::attach::input_replay::{InputReplayJournal, mint_input_operation_id};
use crate::attach::outcome::AttachError;
use crate::attach::paint::{SidebarReservation, content_rect};
use crate::attach::pane_state::{
    PaneSlot, clear_attention_on_input, published_replica, published_terminal,
    reanchor_predict_to_pane,
};
use crate::layout::Workspace;
use crate::predict::{Overlay, PredictionState};
use crate::render::chrome::sidebar::{SidebarHit, hit_test};
use crate::render::overlay::{ContextMenu, OverlayOutcome, OverlayState, ScreenSelectionPoint};
use phux_client::layout_ops::{DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, layout_key};

use super::args::switch_session_args;
use super::chrome_drag;
use super::ctx::{DispatchCtx, DividerGrab, DragGrab, WindowStrip};
use super::effects::encode_layout_or_log;
use super::effects::{ChordOutcome, apply_action_effects, consume_chord};
use super::run_action::run_action;

mod mouse;
#[cfg(test)]
pub(super) use mouse::{bar_click_action, sidebar_click_action};
pub(super) use mouse::{drag_resize, open_context_menu, quantize_cell};

fn edits_workspace(action: &str) -> bool {
    matches!(
        action,
        "split-pane"
            | "move-pane"
            | "new-window"
            | "kill-pane"
            | "kill-window"
            | "rename-window"
            | "move-window"
            | "resize-pane"
            | "plugin-pane"
    )
}
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
    focused_resource: &'a mut Option<ResourceId>,
    detach_pending: &'a mut bool,
    predict: &'a mut PredictionState,
    panes: &'a mut HashMap<ResourceId, PaneSlot>,
    ctx: &'a mut DispatchCtx<'c>,
}

/// Translate a batch of parser events into wire frames and ship them.
///
/// Detach actions short-circuit into a single `FrameKind::Detach` and
/// flip `detach_pending`. Pre-attach events (no `focused_resource` yet) are
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
    focused_resource: &mut Option<ResourceId>,
    detach_pending: &mut bool,
    predict: &mut PredictionState,
    overlay: &Overlay,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    ctx: &mut DispatchCtx<'_>,
) -> Result<bool, AttachError> {
    let layout_changed = {
        let mut env = EventEnv {
            out,
            conn,
            focused_resource,
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
        layout_changed
    };
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
        if matches!(ev, InputEvent::Focus(FocusEvent::Lost)) {
            change.layout_changed |= self.abandon_chrome_drag().await?;
        }
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
        if !self.ctx.layout_read_complete && edits_workspace(&resolved.action) {
            tracing::debug!(action = %resolved.action, "waiting for initial shared layout read");
            return Ok(false);
        }
        let effects = run_action(
            resolved,
            self.ctx,
            self.focused_resource.as_ref(),
            self.panes,
        );
        apply_action_effects(
            effects,
            self.out,
            self.conn,
            self.ctx,
            self.focused_resource,
            self.detach_pending,
            self.predict,
            self.panes,
        )
        .await
    }

    /// phux-5ke.4: while any overlay is active the stack captures all
    /// input. Key events flow to `OverlayState::handle_key`, which
    /// routes them to the *top* overlay (which may dismiss, popping
    /// back to whatever is beneath it). Mouse and paste events stay within
    /// the overlay too; focus events are dropped rather than reaching the pane.
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
            InputEvent::Paste(paste) => {
                if let Some(resolver) = self.ctx.resolver.as_deref_mut() {
                    resolver.reset();
                }
                if let Ok(text) = std::str::from_utf8(&paste.data) {
                    self.ctx.overlays.handle_paste(text);
                }
                false
            }
            // Focus events are consumed without reaching the pane underneath.
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
        self.release_abandoned_listing();
        let ran = self.apply_overlay_outcome(outcome).await?;
        // On dismiss, repaint everything: the overlay scribbled
        // over pane cells and we need a coherent base for the
        // next RESOURCE_OUTPUT.
        let dismissed = was_active && !self.ctx.overlays.is_active();
        Ok(ran || dismissed)
    }

    /// Escape on the `go-to-directory` placeholder cancels its listing:
    /// once no stacked overlay awaits the pending request, forget it, so the
    /// late reply is dropped as stale instead of opening a picker.
    fn release_abandoned_listing(&mut self) {
        let pending = self
            .ctx
            .pending_directory
            .as_ref()
            .map(|pending| pending.request_id);
        if pending.is_some_and(|id| !self.ctx.overlays.awaits(id)) {
            *self.ctx.pending_directory = None;
        }
    }

    /// Feed one mouse event to the top overlay and run whatever it commits.
    async fn handle_overlay_mouse(&mut self, mouse: &MouseEvent) -> Result<bool, AttachError> {
        // Copy-mode tracks pane-local cells but the parser emits
        // outer-viewport coordinates; translate into the focused
        // pane's frame so a drag over a non-origin pane highlights
        // the cells actually under the pointer. Modal overlays (the
        // only other mouse consumers) keep viewport coords.
        let routed = if self.ctx.overlays.copy_selection().is_some() {
            let rect = focused_pane_rect(self.ctx, self.focused_resource.as_ref());
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
                if let Some(fid) = self.focused_resource.as_ref()
                    && let Some(terminal) = published_terminal(self.ctx.engine_kernel, fid)
                {
                    crate::attach::copy::copy_to_host_clipboard(self.out, terminal, req)?;
                }
                Ok(false)
            }
            OverlayOutcome::ScrollViewport(delta) => Ok(scroll_focused_pane_viewport(
                self.ctx.engine_kernel,
                self.panes,
                self.focused_resource.as_ref(),
                delta,
            )),
            // ADR-0101: the settings page wrote the file. The driver owns
            // the settings this batch is still borrowing, so hand the
            // reload up exactly as the `reload-config` action does.
            OverlayOutcome::ReloadConfig => {
                *self.ctx.reload_request = true;
                Ok(false)
            }
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
    /// `focused_resource` local (server-frame handlers rely on it);
    /// either reads the same `ResourceId` here.
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
    /// `focused_resource` mirror stays in sync for the render path.
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
        // ADR-0124: a retained pane's process exited; there is nothing to
        // type into, and the server would refuse it anyway.
        if crate::attach::pane_state::pane_exited(self.panes, &pane) {
            tracing::debug!(terminal = ?pane, "dropping input: the pane's process exited");
            return Ok(false);
        }
        // phux-foz.1: forwarding key/paste input to a pane answers (or at
        // least engages) its pending agent question, so clear its asked
        // attention flag. Focus/mouse events don't clear — merely looking
        // at a pane is not answering it. A real transition schedules the
        // chrome repaint via the returned flag.
        let layout_changed = matches!(ev, InputEvent::Key(_) | InputEvent::Paste(_))
            && clear_attention_on_input(self.panes, &pane);
        let acknowledged = matches!(ev, InputEvent::Paste(_));
        self.send_terminal_input(pane, ev, acknowledged).await?;
        Ok(layout_changed)
    }

    /// Keep later input behind an acknowledged operation for this Terminal.
    /// Outside that short ordering window, latency-sensitive atoms retain the
    /// ordinary fire-and-forget path.
    async fn send_terminal_input(
        &mut self,
        pane: ResourceId,
        event: InputEvent,
        acknowledged: bool,
    ) -> Result<(), AttachError> {
        let Some(journal) = self.ctx.input_replay else {
            return self.conn.send(&event.into_frame(pane)).await;
        };
        let should_queue = pane.host().is_none()
            && ((journal.borrow().active() && acknowledged)
                || journal.borrow().must_order_after(&pane));
        if !should_queue {
            return self.conn.send(&event.into_frame(pane)).await;
        }

        let (reports, frames) = {
            let mut journal = journal.borrow_mut();
            if let Err(report) = journal.submit(mint_input_operation_id(), pane, vec![event]) {
                tracing::warn!(line = %report.notice_line(), "acknowledged input refused locally");
                return Ok(());
            }
            journal.next_frames(&mut *self.ctx.next_request_id)
        };
        journal.borrow_mut().defer_reports(reports);
        send_replay_frames(self.conn, journal, &frames).await
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
        let focused = self
            .ctx
            .workspace
            .active_window()
            .and_then(|w| w.focus.as_ref());
        let origin = focused
            .and_then(|fid| self.panes.get(fid))
            .map_or((0, 0), |s| s.renderer.last_origin());
        let _ = overlay.render(self.predict, origin, self.out);
        // phux-esge: the guesses now sit over the focused pane's cells; its
        // front buffer must not keep claiming what was there before them.
        if let Some(slot) = focused.and_then(|fid| self.panes.get_mut(fid)) {
            crate::attach::pane_state::invalidate_predicted_rows(slot, self.predict);
        }
    }
}

#[allow(
    clippy::future_not_send,
    reason = "the attach loop and its RefCell replay journal are current-thread state"
)]
pub(super) async fn send_replay_frames(
    conn: &mut Connection,
    journal: &std::cell::RefCell<InputReplayJournal>,
    frames: &[FrameKind],
) -> Result<(), AttachError> {
    for (index, frame) in frames.iter().enumerate() {
        if let Err(error) = conn.send(frame).await {
            journal.borrow_mut().rollback_unsent(&frames[index + 1..]);
            return Err(error);
        }
    }
    Ok(())
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
    // Same source of truth the server encoder and the FFI bridge use.
    // The four DECSET bits (9/1000/1002/1003) are the common cases, but
    // `TrackingMode` is non-exhaustive — a Mode-list miss is what sent
    // grok/opencode wheels into local scroll (phux-2vnl).
    libghostty_vt::mouse::EncoderOptions::from_terminal(terminal)
        .is_ok_and(|options| options.tracking_mode != libghostty_vt::mouse::TrackingMode::None)
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

/// Apply `delta` and report whether the history viewport offset changed.
fn replica_scroll_moved(
    replica: &mut phux_client_core::engine::ghostty::GhosttyReplica,
    delta: isize,
) -> bool {
    let before = replica_viewport_offset(replica);
    if replica
        .scroll_viewport(ScrollViewport::Delta(delta))
        .is_err()
    {
        return false;
    }
    let after = replica_viewport_offset(replica);
    before.zip(after).is_some_and(|(a, b)| a != b)
}

fn replica_viewport_offset(
    replica: &phux_client_core::engine::ghostty::GhosttyReplica,
) -> Option<u64> {
    replica.terminal()?.scrollbar().ok().map(|bar| bar.offset)
}

pub(super) fn scroll_focused_pane_viewport(
    kernel: &mut crate::attach::pane_state::AttachKernel,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    focused_resource: Option<&ResourceId>,
    delta: isize,
) -> bool {
    if delta == 0 {
        return false;
    }
    let Some(fid) = focused_resource else {
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

/// Snap `focused_resource`'s viewport back to the live screen if a wheel /
/// copy-mode scroll left it pinned in scrollback. Returns `true` iff the
pub(super) fn snap_scrolled_viewport(
    kernel: &mut crate::attach::pane_state::AttachKernel,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    focused_resource: Option<&ResourceId>,
) -> bool {
    let Some((fid, slot)) =
        focused_resource.and_then(|fid| panes.get_mut(fid).map(|slot| (fid, slot)))
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
    focused_resource: Option<&ResourceId>,
) -> crate::layout::Rect {
    focused_pane_rect_for(
        ctx.workspace,
        ctx.zoomed.as_ref(),
        focused_resource,
        ctx.viewport,
        ctx.bar,
        ctx.sidebar,
    )
}

/// Resolve `SPAWN_RESOURCE.initial_size` for a spawn this client is about to
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

/// Stamp `size` onto an already-built `SPAWN_RESOURCE` frame — the plugin-pane
/// path builds the frame from its manifest entry before it knows which
/// placement (and therefore which tile) it is about to park.
pub(super) const fn set_spawn_initial_size(frame: &mut FrameKind, size: Option<(u16, u16)>) {
    if let FrameKind::SpawnResource { initial_size, .. } = frame {
        *initial_size = size;
    }
}

pub(in crate::attach) fn focused_pane_rect_for(
    workspace: &Workspace,
    zoomed: Option<&ResourceId>,
    focused_resource: Option<&ResourceId>,
    viewport: (u16, u16),
    bar: Option<crate::render::chrome::status_bar::Position>,
    sidebar: Option<SidebarReservation>,
) -> crate::layout::Rect {
    let content = content_rect(viewport, bar, sidebar);
    let Some(fid) = focused_resource else {
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
/// involved. The same flag also covers the ResourceSpawned/ResourceClosed
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
    zoomed: Option<&ResourceId>,
    focused_resource: Option<&ResourceId>,
    viewport: (u16, u16),
    bar: Option<crate::render::chrome::status_bar::Position>,
    sidebar: Option<SidebarReservation>,
) {
    if !overlays.is_active() {
        return;
    }
    let pane = focused_pane_rect_for(workspace, zoomed, focused_resource, viewport, bar, sidebar);
    overlays.on_viewport_resize(pane.w, pane.h);
}

/// Apply a client-local focus change through the single MRU transition path.
pub(super) fn apply_focus_transition(
    history: &mut FocusHistory,
    focused_resource: &mut Option<ResourceId>,
    target: ResourceId,
) {
    history.transition(focused_resource, Some(target));
}
