//! Pane pointer gestures: focus, terminal mouse delivery, alternate scrolling,
//! context menus and screen-anchored drag/word/line/rectangle selection.

use std::time::Instant;

use crate::render::overlay::{SelectionGrab, SelectionMode};

use super::{is_left_press, is_right_press, open_context_menu, quantize_cell};
use super::super::{
    AttachError, EventEnv, InputEvent, ModSet, MouseEvent, PhysicalKey, Point, PointCoordinate,
    PointSpace, ResourceId, ScreenSelectionPoint, StageOutcome, focused_pane_rect, make_named_key,
    published_terminal, replica_scroll_moved, scale_to_surface_pixels,
    terminal_in_alt_screen, terminal_wants_mouse_tracking, wheel_scroll_delta,
};

#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
impl<W: crate::attach::RenderSink> EventEnv<'_, '_, W> {
    /// Route a press inside a pane: click to focus, then the gestures the
    /// client claims (wheel, menu, drag-to-copy), else `INPUT_MOUSE`.
    pub(super) async fn route_mouse_to_pane(
        &mut self,
        mouse: &MouseEvent,
        target: ResourceId,
        pane_xy: (f64, f64),
        focus_changed: bool,
    ) -> Result<StageOutcome, AttachError> {
        // Heavy-edge chrome moves with focus; repaint
        // dividers + all leaves so the focused pane's
        // surrounding edges render heavy.
        let layout_changed = focus_changed;
        if is_left_press(mouse) {
            self.ctx.hold_typing(false);
        }
        if focus_changed {
            self.focus_pane_from_click(&target);
        }
        // An opted-out pane gets no client-synthesized mouse; click-to-focus
        // above still applies (it is how outer capture drops).
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
        // ADR-0124: scrolling and copying a retained pane still work above;
        // a mouse report has no process left to read it.
        if crate::attach::pane_state::pane_exited(self.panes, &target)
            || crate::attach::pane_state::pane_satellite_down(self.panes, &target)
        {
            return Ok(StageOutcome::consumed(layout_changed));
        }
        self.send_terminal_input(
            target,
            InputEvent::Mouse(scale_to_surface_pixels(routed, self.ctx.cell_px)),
            false,
        )
        .await?;
        Ok(StageOutcome::consumed(layout_changed))
    }

    /// Handle a wheel notch over `target`. `None` when the event is not a
    /// wheel notch the client claims (it forwards to the pane instead);
    /// `Some(layout_changed)` when the client consumed it.
    pub(super) async fn scroll_pane_wheel(
        &mut self,
        target: &ResourceId,
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
        // Alt-screen panes have no local scrollback: arrows under DECSET
        // 1007 (the default), else forward to the app.
        if modes.alt_screen {
            if modes.alt_scroll {
                self.send_wheel_as_arrows(target, delta).await?;
                return Ok(Some(false));
            }
            return Ok(None);
        }
        if self.scroll_pane_viewport(target, delta) {
            return Ok(Some(true));
        }
        // Local scroll did not move the viewport (already at the
        // edge, empty history, or an alt-screen replica whose
        // mode bits we missed). Forward so the inner app sees it.
        Ok(None)
    }

    /// Emit one arrow-key press per wheel notch — the alternate-scroll
    /// translation.
    async fn send_wheel_as_arrows(
        &mut self,
        target: &ResourceId,
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
            self.send_terminal_input(target.clone(), InputEvent::Key(arrow.clone()), false)
                .await?;
        }
        Ok(())
    }

    /// Scroll `target`'s mirror by `delta`; true only if the viewport moved
    /// (libghostty reports `Ok` for no-op scrolls, which must forward).
    fn scroll_pane_viewport(&mut self, target: &ResourceId, delta: isize) -> bool {
        let scrolled = self
            .ctx
            .engine_kernel
            .published_engine_mut(target)
            .is_some_and(|replica| replica_scroll_moved(replica, delta));
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

    /// ADR-0058: a right press on a pane whose app does not track the mouse
    /// opens the pane menu at the (un-routed) pointer.
    fn open_pane_context_menu(&mut self, mouse: &MouseEvent, target: &ResourceId) -> bool {
        if !is_right_press(mouse) || !pane_ignores_mouse(self.ctx.engine_kernel, target) {
            return false;
        }
        let zoomed = self.ctx.zoomed.as_ref() == Some(target);
        let spec = crate::attach::context_menu::pane_menu(Some(self.ctx.keybindings), zoomed);
        open_context_menu(
            self.ctx,
            spec,
            (quantize_cell(mouse.x), quantize_cell(mouse.y)),
        );
        true
    }

    /// Drag-to-copy (tmux): a left press on a pane that does not track the
    /// mouse, or any Ctrl-left press, starts a selection. Double-click copies
    /// a word, triple a line; Alt-drag selects a rectangle.
    fn begin_drag_to_copy(&mut self, routed: &MouseEvent, target: &ResourceId) -> bool {
        let force_copy = routed.mods.contains(ModSet::CTRL);
        if !is_left_press(routed)
            || (!force_copy && !pane_ignores_mouse(self.ctx.engine_kernel, target))
        {
            return false;
        }
        let rect = focused_pane_rect(self.ctx, self.focused_resource.as_ref());
        let mouse_col = quantize_cell(routed.x).min(rect.w.saturating_sub(1));
        let mouse_row = quantize_cell(routed.y).min(rect.h.saturating_sub(1));
        let anchor = published_terminal(self.ctx.engine_kernel, target)
            .and_then(|terminal| {
                let cell = terminal
                    .grid_ref(Point::Viewport(PointCoordinate {
                        x: mouse_col,
                        y: u32::from(mouse_row),
                    }))
                    .ok()?;
                terminal
                    .point_from_grid_ref(&cell, PointSpace::Screen)
                    .ok()?
            })
            .map(|point| ScreenSelectionPoint {
                col: point.x,
                row: point.y,
            });
        let count = self
            .ctx
            .overlays
            .count_click(target, mouse_col, mouse_row, Instant::now());
        let mut overlay =
            crate::render::overlay::CopyModeOverlay::new(mouse_row, mouse_col, rect.w, rect.h);
        if let Some(anchor) = anchor {
            overlay.set_mouse_anchor_screen(anchor);
        }
        if routed.mods.contains(ModSet::ALT) {
            overlay.mode = SelectionMode::Rect;
        }
        overlay.set_click_grab(match count {
            2 => Some(SelectionGrab::Word),
            3 => Some(SelectionGrab::Line),
            _ => None,
        });
        self.ctx.overlays.push(Box::new(overlay));
        // Seed anchor + cursor from the (pane-local) press.
        let _ = self.ctx.overlays.handle_mouse(routed);
        true
    }
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
    target: &ResourceId,
) -> Option<PaneScrollModes> {
    let terminal = published_terminal(kernel, target)?;
    Some(PaneScrollModes {
        wants_mouse_tracking: terminal_wants_mouse_tracking(terminal),
        alt_screen: terminal_in_alt_screen(terminal),
        alt_scroll: terminal
            .mode(libghostty_vt::mode::Mode::ALT_SCROLL)
            .unwrap_or(false),
    })
}

/// Whether `target`'s app has not enabled mouse tracking (then the client
/// may claim its buttons).
fn pane_ignores_mouse(
    kernel: &crate::attach::pane_state::AttachKernel,
    target: &ResourceId,
) -> bool {
    published_terminal(kernel, target)
        .is_some_and(|terminal| !terminal_wants_mouse_tracking(terminal))
}
