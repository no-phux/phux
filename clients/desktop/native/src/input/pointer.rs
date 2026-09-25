use super::{
    InputError, InputMetrics, TerminalInput, engine_error, gpui, input_ready, keys, queued,
};
use gpui::{
    App, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollWheelEvent, Window,
};
use phux_client_runtime::{
    control::ControlPlane,
    engine::{MouseMode, Scroll, SelectionGestureEvent},
};
use phux_protocol::input::mouse::{MouseAction, MouseButton as WireButton, MouseEvent};

impl InputMetrics {
    fn local(&self, point: Point<Pixels>) -> (f64, f64) {
        (
            f32::from(point.x - self.bounds.origin.x).max(0.0).into(),
            f32::from(point.y - self.bounds.origin.y).max(0.0).into(),
        )
    }
}

impl TerminalInput {
    fn hit(&self, position: Point<Pixels>) -> bool {
        let Some(metrics) = self.metrics else {
            return false;
        };
        if !metrics.bounds.contains(&position) {
            return false;
        }
        let (x, y) = metrics.local(position);
        x < f64::from(f32::from(metrics.cell_width)) * f64::from(self.dimensions.0)
            && y < f64::from(f32::from(metrics.line_height)) * f64::from(self.dimensions.1)
    }

    pub fn mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &Window,
        cx: &App,
    ) -> Result<bool, InputError> {
        self.current(window, cx)?;
        if !self.hit(event.position) {
            return Ok(false);
        }
        self.cancel();
        if self.report_mouse(
            MouseAction::Press,
            button(event.button),
            event.position,
            event.modifiers,
        )? {
            self.reported_button = Some(button(event.button));
            return Ok(true);
        }
        if event.button != MouseButton::Left {
            return Ok(false);
        }
        self.select(0, event.click_count as u32, event.position, event.modifiers)?;
        Ok(true)
    }

    pub fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &Window,
        cx: &App,
    ) -> Result<bool, InputError> {
        self.current(window, cx)?;
        if self.gesture != 0 {
            self.select(1, 1, event.position, event.modifiers)?;
            return Ok(true);
        }
        if let Some(button) = self.reported_button {
            self.send_captured_mouse(MouseAction::Motion, button, event.position, event.modifiers)?;
            return Ok(true);
        }
        if !self.hit(event.position) {
            return Ok(false);
        }
        self.report_mouse(
            MouseAction::Motion,
            event.pressed_button.map_or(WireButton::Unknown, button),
            event.position,
            event.modifiers,
        )
    }

    pub fn mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &Window,
        cx: &App,
    ) -> Result<bool, InputError> {
        self.current(window, cx)?;
        if self.gesture != 0 {
            let result = self.select(2, 1, event.position, event.modifiers);
            self.gesture = 0;
            result?;
            return Ok(true);
        }
        let Some(button) = self.reported_button.take() else {
            return Ok(false);
        };
        self.send_captured_mouse(
            MouseAction::Release,
            button,
            event.position,
            event.modifiers,
        )?;
        Ok(true)
    }

    fn send_captured_mouse(
        &self,
        action: MouseAction,
        button: WireButton,
        position: Point<Pixels>,
        modifiers: Modifiers,
    ) -> Result<(), InputError> {
        let (x, y) = self
            .metrics
            .ok_or(InputError::StalePresentation)?
            .local(position);
        self.with_current(true, |control| {
            queued(control.send_mouse(
                &self.terminal,
                MouseEvent {
                    action,
                    button,
                    mods: keys::modifiers(modifiers),
                    x,
                    y,
                },
            ))
        })
    }

    fn report_mouse(
        &self,
        action: MouseAction,
        button: WireButton,
        position: Point<Pixels>,
        modifiers: Modifiers,
    ) -> Result<bool, InputError> {
        let (x, y) = self
            .metrics
            .ok_or(InputError::StalePresentation)?
            .local(position);
        self.with_current(false, |control| {
            if !reporting(control, &self.terminal, modifiers)? {
                return Ok(false);
            }
            if !input_ready(control, &self.terminal) {
                return Err(InputError::NotReady);
            }
            queued(control.send_mouse(
                &self.terminal,
                MouseEvent {
                    action,
                    button,
                    mods: keys::modifiers(modifiers),
                    x,
                    y,
                },
            ))?;
            Ok(true)
        })
    }

    fn select(
        &mut self,
        phase: u32,
        clicks: u32,
        position: Point<Pixels>,
        modifiers: Modifiers,
    ) -> Result<(), InputError> {
        let metrics = self.metrics.ok_or(InputError::StalePresentation)?;
        let (x, y) = metrics.local(position);
        let width = f32::from(metrics.cell_width).max(1.0);
        let height = f32::from(metrics.line_height).max(1.0);
        let result = self.with_current(false, |control| {
            let frame = self.live_frame(control)?;
            let event = SelectionGestureEvent {
                phase,
                clicks: clicks.clamp(1, 3),
                handle: self.gesture,
                column: ((x / f64::from(width)) as u16).min(frame.cols.saturating_sub(1)),
                row: ((y / f64::from(height)) as u32).min(u32::from(frame.rows.saturating_sub(1))),
                rectangle: modifiers.alt,
                x,
                y,
                columns: frame.cols.into(),
                cell_width: width.round() as u32,
                screen_height: f32::from(metrics.bounds.size.height).max(1.0) as u32,
                padding_left: 0,
            };
            control
                .engine()
                .ok_or(InputError::NotReady)?
                .view_selection_gesture(self.view, event)
                .map_err(engine_error)
        })?;
        self.gesture = result.handle;
        Ok(())
    }

    pub fn scroll(
        &mut self,
        event: &ScrollWheelEvent,
        window: &Window,
        cx: &App,
    ) -> Result<bool, InputError> {
        self.current(window, cx)?;
        let metrics = self.metrics.ok_or(InputError::StalePresentation)?;
        if !self.hit(event.position) {
            return Ok(false);
        }
        let delta = event.delta.pixel_delta(metrics.line_height);
        self.scroll_remainder += f32::from(delta.y) / f32::from(metrics.line_height).max(1.0);
        let lines = self.scroll_remainder.trunc() as i64;
        self.scroll_remainder -= lines as f32;
        if lines == 0 {
            return Ok(true);
        }
        self.with_current(false, |control| {
            if reporting(control, &self.terminal, event.modifiers)? {
                self.report_scroll(control, lines, event, metrics)?;
            } else {
                control
                    .scroll_view(self.view, Scroll::Delta(-lines))
                    .map_err(engine_error)?;
            }
            Ok(true)
        })
    }

    fn report_scroll(
        &self,
        control: &mut ControlPlane,
        lines: i64,
        event: &ScrollWheelEvent,
        metrics: InputMetrics,
    ) -> Result<(), InputError> {
        if !input_ready(control, &self.terminal) {
            return Err(InputError::NotReady);
        }
        let button = if lines > 0 {
            WireButton::Four
        } else {
            WireButton::Five
        };
        let (x, y) = metrics.local(event.position);
        for _ in 0..lines.unsigned_abs().min(100) {
            queued(control.send_mouse(
                &self.terminal,
                MouseEvent {
                    action: MouseAction::Press,
                    button,
                    mods: keys::modifiers(event.modifiers),
                    x,
                    y,
                },
            ))?;
        }
        Ok(())
    }
}

fn button(button: MouseButton) -> WireButton {
    match button {
        MouseButton::Left => WireButton::Left,
        MouseButton::Right => WireButton::Right,
        MouseButton::Middle => WireButton::Middle,
        _ => WireButton::Unknown,
    }
}

fn reporting(
    control: &ControlPlane,
    terminal: &phux_protocol::ResourceId,
    modifiers: Modifiers,
) -> Result<bool, InputError> {
    if modifiers.shift {
        return Ok(false);
    }
    let mode = control
        .engine()
        .ok_or(InputError::NotReady)?
        .mouse_mode(terminal)
        .map_err(engine_error)?;
    Ok(mode != MouseMode::None)
}
