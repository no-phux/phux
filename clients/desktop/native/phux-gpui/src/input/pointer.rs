use super::{InputError, TerminalInput, engine_error, input_ready, keys, queued};
use crate::terminal::geometry::{self, Geometry, SurfacePoint};
use gpui::{
    Bounds, Context, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, ScrollEvent, TouchPhase, Window, px,
};
use phux_client_runtime::{
    Client,
    control::ControlPlane,
    engine::{MouseMode, Scroll, SelectionGestureEvent},
    publication::GridFrame,
};
use phux_protocol::input::mouse::{MouseAction, MouseButton as WireButton, MouseEvent};

mod release;
mod selection;
mod wheel;

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(super) use selection::tick;

impl TerminalInput {
    fn repeated_motion(&mut self, position: Point<Pixels>) -> bool {
        let cell = self.cell_at(position);
        if repeats_cell(self.last_motion_cell, cell) {
            return true;
        }
        self.last_motion_cell = cell;
        false
    }

    /// The painted grid, in the same geometry the painter hit-tests.
    fn geometry(&self) -> Option<Geometry> {
        let metrics = self.metrics?;
        Some(Geometry {
            bounds: metrics.bounds,
            cell_width: metrics.cell_width,
            cell_height: metrics.line_height,
            baseline: px(0.),
            scale: metrics.scale,
            cols: self.dimensions.0,
            rows: self.dimensions.1,
        })
    }

    /// The grid cell under `position`, or `None` when the point misses it.
    fn cell_at(&self, position: Point<Pixels>) -> Option<(u16, u16)> {
        self.geometry()?.hit(self.unshifted(position))
    }

    /// `position` in the grid's whole-row coordinates, undoing the painter's
    /// sub-row scroll shift.
    fn unshifted(&self, position: Point<Pixels>) -> Point<Pixels> {
        let shift = self.metrics.map_or(px(0.), |metrics| metrics.shift);
        Point::new(position.x, position.y - shift)
    }

    fn hit(&self, position: Point<Pixels>) -> bool {
        self.cell_at(position).is_some()
    }

    fn surface(&self, position: Point<Pixels>, clamped: bool) -> Result<SurfacePoint, InputError> {
        geometry::surface_at(
            self.geometry().ok_or(InputError::StalePresentation)?,
            self.unshifted(position),
            clamped,
        )
        .ok_or(InputError::StalePresentation)
    }

    /// Capture the painted link under a Command-press. Opening waits for
    /// [`Self::release_link`].
    pub fn press_link(&mut self, position: Point<Pixels>) -> bool {
        self.painted_links.cancel();
        self.cell_at(position)
            .is_some_and(|(row, col)| self.painted_links.press(row, col))
    }

    /// The URL to open when this release is still on the pressed link.
    /// Releasing the Command key, or missing the grid, cancels the gesture.
    pub fn release_link(&mut self, position: Point<Pixels>, modifier: bool) -> Option<String> {
        match self.cell_at(position).filter(|_| modifier) {
            Some((row, col)) => self.painted_links.release(row, col),
            None => {
                self.painted_links.cancel();
                None
            }
        }
    }

    /// Where the pointer rests and whether Command is held, so a link under
    /// it can read as clickable. Returns whether that changed what shows.
    pub fn hover(&mut self, position: Option<Point<Pixels>>, command: bool) -> bool {
        let before = self.hovered_link();
        self.hover = position.map(|position| (position, command));
        self.hovered_link() != before
    }

    /// Command went down or up with the pointer where it was.
    pub fn hover_command(&mut self, command: bool) -> bool {
        let position = self.hover.map(|(position, _)| position);
        self.hover(position, command)
    }

    /// The painted link Command would open, as `(row, start, end)` columns:
    /// only while Command is held over it and no drag is captured.
    pub fn hovered_link(&self) -> Option<(u16, u16, u16)> {
        let (position, command) = self.hover?;
        if !command || self.capturing() {
            return None;
        }
        let (row, col) = self.cell_at(position)?;
        let (start, end) = self.painted_links.span(row, col)?;
        Some((row, start, end))
    }

    pub fn cancel_link(&mut self) {
        self.painted_links.cancel();
    }

    pub fn mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<bool, InputError> {
        self.admit_focused(window, cx, false)?;
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
        if event.button == MouseButton::Middle {
            return Ok(self.paste_primary(cx));
        }
        if event.button != MouseButton::Left {
            return Ok(false);
        }
        self.select(0, event.click_count as u32, event.position, event.modifiers)?;
        cx.notify();
        Ok(true)
    }

    pub fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<bool, InputError> {
        if self.gesture != 0 {
            return self.drag_selection(event.position, event.modifiers, window, cx);
        }
        if let Some(button) = self.reported_button {
            return self.drag_reported(button, event, window, cx);
        }
        // Cell-quantized clients send one position per cell (input spec §3.1).
        // A move that stays in that cell must not take the control lock.
        if self.repeated_motion(event.position) {
            return Ok(true);
        }
        // A Command-click is local until release. Motion must not report the
        // mouse or start a selection while that gesture is held.
        if self.painted_links.is_held() {
            return Ok(true);
        }
        self.admit_focused(window, cx, false)?;
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

    /// Motion while a program holds a captured button, inside the pane or
    /// past its edge clamped to the nearest cell: one report per cell.
    fn drag_reported(
        &mut self,
        button: WireButton,
        event: &MouseMoveEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<bool, InputError> {
        let cell = self
            .surface(event.position, true)
            .ok()
            .map(|surface| (surface.row, surface.col));
        if repeats_cell(self.last_motion_cell, cell) {
            return Ok(true);
        }
        self.last_motion_cell = cell;
        self.admit_focused(window, cx, false)?;
        self.send_captured_mouse(MouseAction::Motion, button, event.position, event.modifiers)?;
        Ok(true)
    }

    pub fn mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<bool, InputError> {
        if self.gesture != 0 {
            self.autoscroll = None;
            self.drag_cell = None;
            self.admit_focused(window, cx, false)?;
            let result = self.select(2, 1, event.position, event.modifiers);
            self.gesture = 0;
            result?;
            self.publish_primary(cx);
            cx.notify();
            return Ok(true);
        }
        // Take the captured application button before admission. `current`
        // rejects a temporary frame or geometry gap, and dropping the release
        // first would leave that button down.
        let Some(button) = self.reported_button.take() else {
            self.admit_focused(window, cx, false)?;
            return Ok(false);
        };
        if !self.focused(window, cx) {
            return Err(InputError::WrongFocus);
        }
        self.send_captured_release(button, event.position, event.modifiers)?;
        Ok(true)
    }

    fn send_captured_mouse(
        &self,
        action: MouseAction,
        button: WireButton,
        position: Point<Pixels>,
        modifiers: Modifiers,
    ) -> Result<(), InputError> {
        let surface = self.surface(position, true)?;
        self.with_current(true, |control| {
            self.send_mouse(control, action, button, modifiers, surface)
        })
    }

    fn send_mouse(
        &self,
        control: &mut ControlPlane,
        action: MouseAction,
        button: WireButton,
        modifiers: Modifiers,
        surface: SurfacePoint,
    ) -> Result<(), InputError> {
        queued(control.send_mouse(
            &self.terminal,
            MouseEvent {
                action,
                button,
                mods: keys::modifiers(modifiers),
                x: surface.x,
                y: surface.y,
            },
        ))
    }

    fn report_mouse(
        &self,
        action: MouseAction,
        button: WireButton,
        position: Point<Pixels>,
        modifiers: Modifiers,
    ) -> Result<bool, InputError> {
        let surface = self.surface(position, false)?;
        self.with_current(false, |control| {
            if modifiers.shift || !reporting(control, &self.terminal)? {
                return Ok(false);
            }
            if !input_ready(control, &self.terminal) {
                return Err(InputError::NotReady);
            }
            self.send_mouse(control, action, button, modifiers, surface)?;
            Ok(true)
        })
    }
}

/// A motion event the last one already covered. Leaving the grid is not a repeat.
fn repeats_cell(last: Option<(u16, u16)>, next: Option<(u16, u16)>) -> bool {
    next.is_some() && next == last
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
) -> Result<bool, InputError> {
    let mode = control
        .engine()
        .ok_or(InputError::NotReady)?
        .mouse_mode(terminal)
        .map_err(engine_error)?;
    Ok(mode != MouseMode::None)
}
