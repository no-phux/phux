//! The event dispatcher: parser events to wire frames, resolver
//! intercepts, mouse routing, and the pane/overlay geometry helpers.

use std::collections::HashMap;

use libghostty_vt::terminal::{Mode, Point, PointCoordinate, PointSpace, ScrollViewport};
use phux_protocol::ResourceId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::wire::frame::FrameKind;

use crate::attach::actions::{self, PendingSplit};
use crate::attach::connection::Connection;
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

use super::args::switch_session_args;
use super::chrome_drag;
use super::ctx::{DispatchCtx, DividerGrab, DragGrab, WindowStrip};
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

/// The arguments of [`dispatch_input_events`], bundled once per batch so each
/// stage takes one parameter.
struct EventEnv<'a, 'c, W: crate::attach::RenderSink> {
    out: &'a mut W,
    conn: &'a mut Connection,
    focused_resource: &'a mut Option<ResourceId>,
    predict: &'a mut PredictionState,
    panes: &'a mut HashMap<ResourceId, PaneSlot>,
    ctx: &'a mut DispatchCtx<'c>,
}

/// Translate a batch of parser events into wire frames and ship them. Each
/// event walks the stages in `EventEnv::dispatch_event`; a chord that
/// resolves runs its action and is not forwarded (tmux's prefix table).
/// Predictions paint once per batch.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub(in crate::attach) async fn dispatch_input_events<W: crate::attach::RenderSink>(
    out: &mut W,
    conn: &mut Connection,
    events: &mut Vec<InputEvent>,
    focused_resource: &mut Option<ResourceId>,
    predict: &mut PredictionState,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    ctx: &mut DispatchCtx<'_>,
) -> Result<bool, AttachError> {
    let layout_changed = {
        let mut env = EventEnv {
            out,
            conn,
            focused_resource,
            predict,
            panes,
            ctx,
        };
        let mut predicted_any = false;
        let mut layout_changed = false;
        // Drained rather than consumed: the driver owns `events` for the life of
        // the attach and reuses its allocation for every batch.
        for ev in events.drain(..) {
            let change = env.dispatch_event(ev).await?;
            layout_changed |= change.layout_changed;
            predicted_any |= change.predicted;
        }
        // Paint the prediction overlay once per dispatch batch so a burst of
        // keystrokes produces a single positioned write run, not one per
        // event. The overlay is a no-op on an empty queue.
        if predicted_any {
            env.paint_predictions();
        }
        layout_changed
    };
    // Hand the layout-mutation signal back to `main_loop`, which holds
    // the status-bar painter and session name needed for a proper full
    // frame. We never paint from here.
    Ok(layout_changed)
}

/// Passthrough popups (which-key, guidance) never eat input: a key press
/// dismisses and then executes as if the popup were absent, except Esc,
/// which also cancels the pending prefix; mouse input dismisses and cancels
/// the prefix, then routes normally. Other events pass untouched.
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
    /// Walk one event through the stages (popup dismissal, overlay capture,
    /// chord, mouse) and forward what none claimed to the focused pane.
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

    /// A key press headed for the pane snaps a scrolled viewport back to the
    /// live screen (tmux), before the predict peek reads the grid.
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

    /// Run an action through the path every trigger shares; true iff the
    /// layout changed.
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
            self.predict,
            self.panes,
        )
        .await
    }

    /// While a (non-passthrough) overlay is up it captures all input: keys and
    /// mouse go to the top overlay, pastes fill its text, focus is dropped.
    /// The resolver is bypassed and reset, so a leader chord typed into a
    /// prompt lands verbatim and a half-chord cannot leak past dismissal.
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
                self.send_changed_path_query().await?;
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
        // An overlay may commit an action (e.g. the
        // rename prompt returning `rename-window { name }`); run
        // it through the same path as a keybinding.
        let outcome = self.ctx.overlays.handle_key(key_event);
        self.release_abandoned_listing();
        let ran = self.apply_overlay_outcome(outcome).await?;
        self.send_changed_path_query().await?;
        self.release_abandoned_path();
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

    /// Query the serving/satellite host as the search field changes. Every
    /// edit gets a fresh request id, so delayed results cannot replace newer
    /// results or resurrect a dismissed picker.
    async fn send_changed_path_query(&mut self) -> Result<(), AttachError> {
        let Some((root, query)) = self.ctx.overlays.path_search() else {
            return Ok(());
        };
        let (root, query) = (root.to_owned(), query.to_owned());
        let Some(pending) = self.ctx.pending_path.as_mut() else {
            return Ok(());
        };
        if pending.root == root && pending.query == query {
            return Ok(());
        }
        let request_id = *self.ctx.next_request_id;
        *self.ctx.next_request_id = request_id.wrapping_add(1);
        pending.request_id = request_id;
        pending.root.clone_from(&root);
        pending.query.clone_from(&query);
        self.conn
            .send(&FrameKind::PathQuery {
                request_id,
                root,
                recursive: !query.is_empty(),
                query,
                host: crate::attach::path_picker::host(pending),
            })
            .await
    }

    fn release_abandoned_path(&mut self) {
        if self.ctx.overlays.path_search().is_none() {
            *self.ctx.pending_path = None;
        }
    }

    /// Feed one mouse event to the top overlay and run whatever it commits.
    async fn handle_overlay_mouse(&mut self, mouse: &MouseEvent) -> Result<bool, AttachError> {
        // Copy mode tracks pane-local cells; modals keep viewport coords.
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
        // Clicking a menu away must repaint the cells it covered.
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
                // ADR-0030: resolve against the focused pane and copy via
                // OSC 52; client-local.
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

    /// Resolver intercept. Runs BEFORE the predict layer
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

    /// Feed a key to predictive echo (mouse/paste/focus bypass it), peeking
    /// the focused pane's grid so arrows can size the grapheme they cross.
    /// ADR-0090: predictions queue on both screens; on the alternate screen
    /// display waits for echo evidence.
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

    /// Forward key/focus/paste input to the focused pane (ADR-0019). Input
    /// before ATTACHED, or to an exited or unreachable pane, is dropped.
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
        // A down satellite keeps its slot and its last
        // snapshot, and takes no new input until the link returns.
        if crate::attach::pane_state::pane_satellite_down(self.panes, &pane) {
            tracing::debug!(terminal = ?pane, "dropping input: satellite unreachable");
            return Ok(false);
        }
        // Typing into a pane answers its pending question; looking does not.
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

    /// Paint queued predictions at the focused pane's origin when the
    /// ADR-0090 display policy allows.
    fn paint_predictions(&mut self) {
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
        let _ = Overlay.render(self.predict, origin, self.out);
        // The guesses now sit over the focused pane's cells; its
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
/// Send a replay batch in request order; on a failed write, the frames not
/// yet handed to the transport roll back to their previous definite state.
pub(in crate::attach) async fn send_replay_frames(
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

/// Scale pane-local cells to the surface pixels `INPUT_MOUSE` carries (SPEC
/// input.md §3.1), at the send boundary only; axes clamp to at least 1px.
pub(super) fn scale_to_surface_pixels(mut mouse: MouseEvent, cell_px: (u16, u16)) -> MouseEvent {
    mouse.x *= f64::from(cell_px.0.max(1));
    mouse.y *= f64::from(cell_px.1.max(1));
    mouse
}
pub(super) fn terminal_wants_mouse_tracking(terminal: &libghostty_vt::Terminal<'_, '_>) -> bool {
    // The encoder's own reading; `TrackingMode` is non-exhaustive, so a
    // hand-kept DECSET list would miss modes.
    libghostty_vt::mouse::EncoderOptions::from_terminal(terminal)
        .is_ok_and(|options| options.tracking_mode != libghostty_vt::mouse::TrackingMode::None)
}

/// Monotonic milliseconds for prediction stamps (ADR-0090). Here, not in
/// `phux-client-core`, because `Instant` is unavailable on wasm.
pub(in crate::attach) fn predict_now_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = *EPOCH.get_or_init(Instant::now);
    u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Whether the pane is on the alternate screen (`?1049h`, `?1047h`, `?47h`),
/// the signal for predictive echo's confirmation-gated display (ADR-0090).
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

/// `SPAWN_RESOURCE.initial_size`: the tile `predict` says the new leaf will
/// occupy, or `None` (field absent) when unsupported or degenerate.
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

/// Hand every overlay the focused pane's current rect. The choke point for
/// rect changes without SIGWINCH (a peer's layout, a spawn/close reflow);
/// copy mode is the overlay that cares.
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
