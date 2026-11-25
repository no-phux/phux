//! Mouse routing for the dispatcher: chrome hit-tests (sidebar, status bar,
//! dividers) and the pane gestures the client claims (click-to-focus, wheel,
//! context menu, drag-to-copy).

use super::{
    AttachError, EventEnv, InputEvent, MouseAction, MouseButton, MouseEvent, StageOutcome,
    chrome_drag, content_rect, scale_to_surface_pixels, wheel_scroll_delta,
};

mod chrome;
mod pane;

pub(in crate::attach::input_dispatch) use chrome::{
    bar_click_action, drag_resize, open_context_menu,
};

#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
impl<W: crate::attach::RenderSink> EventEnv<'_, '_, W> {
    /// ADR-0048 pointer stage: chrome drags and clicks, then the pane
    /// composition; a pane press focuses and forwards. Claims every event.
    pub(super) async fn route_mouse_input(
        &mut self,
        ev: &InputEvent,
    ) -> Result<StageOutcome, AttachError> {
        use crate::attach::multi_pane::{RouteDecision, route_mouse_event};
        let InputEvent::Mouse(mouse) = ev else {
            return Ok(StageOutcome::PASS);
        };
        if let Some(outcome) = self.step_chrome_drag(mouse).await? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.route_floating_mouse(mouse).await? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.route_sidebar_click(mouse).await? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.route_status_bar_click(mouse).await? {
            return Ok(outcome);
        }
        // Hit-test the same inset content rect the renderer tiles into (bar
        // and sidebar folded off), or clicks land a row/strip off.
        let content = content_rect(self.ctx.viewport, self.ctx.bar, self.ctx.sidebar);
        // Hit-test the render layout, so a zoomed pane receives the click.
        let (decision, title) = {
            let Some(render_ls) = self.ctx.workspace.render_group(self.ctx.zoomed.as_ref()) else {
                tracing::debug!("dropping mouse event: no active window");
                return Ok(StageOutcome::CONSUMED);
            };
            let decision = route_mouse_event(&render_ls, content, self.ctx.viewport, mouse);
            // A title sits in the rule above its pane, never inside one.
            let title = (is_left_press(mouse) && !matches!(decision, RouteDecision::Pane { .. }))
                .then(|| {
                    chrome_drag::pane_title_at(self.ctx, self.panes, &render_ls, content, mouse)
                })
                .flatten();
            (decision, title)
        };
        if let Some(pane) = title {
            chrome_drag::begin_pane_drag(self.ctx, pane);
            return Ok(StageOutcome::CONSUMED);
        }
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

    /// ADR-0147: the open floating overlay owns the pointer: its interior
    /// gets events pane-local, and a press outside dismisses it.
    async fn route_floating_mouse(
        &mut self,
        mouse: &MouseEvent,
    ) -> Result<Option<StageOutcome>, AttachError> {
        use crate::attach::floating::{floating_box, floating_pane, rect_contains};
        let Some(id) = floating_pane(self.panes).cloned() else {
            return Ok(None);
        };
        let frame = floating_box(content_rect(
            self.ctx.viewport,
            self.ctx.bar,
            self.ctx.sidebar,
        ));
        let (cell_x, cell_y) = (quantize_cell(mouse.x), quantize_cell(mouse.y));
        if rect_contains(frame.inner, cell_x, cell_y) {
            let mut routed = *mouse;
            routed.x -= f64::from(frame.inner.x);
            routed.y -= f64::from(frame.inner.y);
            if let Some(scrolled) = self.scroll_pane_wheel(&id, &routed).await? {
                return Ok(Some(StageOutcome::consumed(scrolled)));
            }
            if !crate::attach::pane_state::pane_exited(self.panes, &id) {
                self.send_terminal_input(
                    id,
                    InputEvent::Mouse(scale_to_surface_pixels(routed, self.ctx.cell_px)),
                    false,
                )
                .await?;
            }
            return Ok(Some(StageOutcome::CONSUMED));
        }
        let outside_press = matches!(mouse.action, MouseAction::Press)
            && !rect_contains(frame.outer, cell_x, cell_y)
            && wheel_scroll_delta(mouse).is_none();
        if !outside_press {
            return Ok(Some(StageOutcome::CONSUMED));
        }
        let dismissed = self.dismiss_floating().await?;
        Ok(Some(StageOutcome::consumed(dismissed)))
    }

}

/// A primary-button press: the gesture the client's own chrome claims
/// (sidebar rows, status-bar tabs, divider grabs, drag-to-copy).
pub(super) fn is_left_press(mouse: &MouseEvent) -> bool {
    matches!(mouse.action, MouseAction::Press) && mouse.button == MouseButton::Left
}

/// A secondary-button press: the context-menu gesture (phux-wrnm,
/// ADR-0058).
pub(super) fn is_right_press(mouse: &MouseEvent) -> bool {
    matches!(mouse.action, MouseAction::Press) && mouse.button == MouseButton::Right
}


/// Quantise an f64 pointer position (1 px per cell) to a cell,
/// saturating into `u16`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "cell-quantised SGR/X10 input; saturate to keep malformed peers from breaking routing"
)]
pub(in crate::attach::input_dispatch) fn quantize_cell(p: f64) -> u16 {
    if p.is_nan() || p < 0.0 {
        0
    } else if p >= f64::from(u16::MAX) {
        u16::MAX
    } else {
        p as u16
    }
}
