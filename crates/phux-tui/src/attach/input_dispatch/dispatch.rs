//! The event dispatcher: parser events to wire frames, resolver
//! intercepts, mouse routing, and the pane/overlay geometry helpers.

use std::collections::HashMap;

use libghostty_vt::terminal::{Mode, Point, PointCoordinate, PointSpace, ScrollViewport};
use phux_protocol::ResourceId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::PasteEvent;
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
use crate::layout::ArrangementDoc;
use crate::predict::{PredictionState, render_overlay};
use crate::render::chrome::sidebar::{SidebarHit, hit_test};
use crate::render::overlay::{
    ContextMenu, CopySearchRequest, CopySearchResult, OverlayOutcome, OverlayState,
    ScreenSelectionPoint,
};

use super::args::switch_session_args;
use super::chrome_drag;
use super::ctx::{DispatchCtx, DividerGrab, DragGrab, WindowStrip};
use super::deferred::DeferredArrangementEdit;
use super::effects::{ChordOutcome, apply_action_effects, consume_chord};
use super::run_action::run_action;

mod mouse;
#[cfg(test)]
pub(super) use mouse::{bar_click_action, sidebar_click_action};
pub(super) use mouse::{drag_resize, open_context_menu, quantize_cell};

mod forwarding;
mod geometry;
mod overlay;
mod resolved;

pub(in crate::attach) use geometry::focused_pane_rect_for;
use geometry::replica_scroll_moved;
pub(in crate::attach) use geometry::sync_overlays_to_focused_pane;
pub(super) use geometry::{
    copy_search, focused_pane_rect, predicted_split_size, scroll_focused_pane_viewport,
    set_spawn_initial_size, snap_scrolled_viewport, spawn_initial_size,
};

/// Hidden-view edits. Unlike `edits_workspace`, these are queued and
/// applied once the stored layout is known, then saved.
fn defers_for_layout_read(action: &str, ctx: &DispatchCtx<'_>) -> bool {
    matches!(action, "close-view" | "reopen-views")
        && (!ctx.layout_read_complete || ctx.unloaded_arrangement_read.is_some())
}

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
        if let InputEvent::Paste(paste) = &ev
            && self.hold_unsafe_paste(paste)
        {
            change.layout_changed = true;
            return Ok(change);
        }
        change.predicted = self.feed_predict(&ev);
        change.layout_changed |= self.forward_to_focused_pane(ev).await?;
        Ok(change)
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
/// docs/protocol.md#input), at the send boundary only; axes clamp to at least 1px.
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
