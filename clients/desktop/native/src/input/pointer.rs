use super::{InputError, TerminalInput, engine_error, input_ready, keys, queued};
use crate::terminal::geometry::{self, Geometry, SurfacePoint};
use gpui::{
    App, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollWheelEvent, Window, px,
};
use phux_client_runtime::{
    control::ControlPlane,
    engine::{MouseMode, Scroll, SelectionGestureEvent},
};
use phux_protocol::input::mouse::{MouseAction, MouseButton as WireButton, MouseEvent};

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
        self.geometry()?.hit(position)
    }

    fn hit(&self, position: Point<Pixels>) -> bool {
        self.cell_at(position).is_some()
    }

    fn surface(&self, position: Point<Pixels>, clamped: bool) -> Result<SurfacePoint, InputError> {
        geometry::surface_at(
            self.geometry().ok_or(InputError::StalePresentation)?,
            position,
            clamped,
        )
        .ok_or(InputError::StalePresentation)
    }

    /// The link under a Command-click, read from the frame last painted.
    /// A newer publication stays invisible until it is painted.
    pub fn link_at(&self, position: Point<Pixels>) -> Option<String> {
        let (row, col) = self.cell_at(position)?;
        self.painted_links.link_at(row, col)
    }

    /// Capture the painted link under a Command-press. Opening waits for
    /// [`Self::release_link`].
    pub fn press_link(&mut self, position: Point<Pixels>) -> bool {
        let Some((row, col)) = self.cell_at(position) else {
            self.painted_links.cancel();
            return false;
        };
        self.painted_links.press(row, col)
    }

    /// The URL to open when this release is still on the pressed link.
    /// Releasing the Command key, or missing the grid, cancels the gesture.
    pub fn release_link(&mut self, position: Point<Pixels>, modifier: bool) -> Option<String> {
        if !self.painted_links.is_held() {
            return None;
        }
        if !modifier {
            self.painted_links.cancel();
            return None;
        }
        let Some((row, col)) = self.cell_at(position) else {
            self.painted_links.cancel();
            return None;
        };
        self.painted_links.release(row, col)
    }

    pub fn cancel_link(&mut self) {
        self.painted_links.cancel();
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
        if self.gesture != 0 {
            self.current(window, cx)?;
            let result = self.select(2, 1, event.position, event.modifiers);
            self.gesture = 0;
            result?;
            return Ok(true);
        }
        // Take the captured application button before admission. `current`
        // rejects a temporary frame or geometry gap, and dropping the release
        // first would leave that button down.
        let Some(button) = self.reported_button.take() else {
            self.current(window, cx)?;
            return Ok(false);
        };
        if !self.focused(window, cx) {
            return Err(InputError::WrongFocus);
        }
        self.send_captured_release(button, event.position, event.modifiers)?;
        Ok(true)
    }

    fn send_captured_release(
        &self,
        button: WireButton,
        position: Point<Pixels>,
        modifiers: Modifiers,
    ) -> Result<(), InputError> {
        let surface = self.surface(position, true)?;
        let client = self.client()?;
        client.with_control(|control| {
            if let Some(error) = refuse_captured_release(self.release_facts(control)) {
                return Err(error);
            }
            queued(control.send_mouse(
                &self.terminal,
                mouse_at(MouseAction::Release, button, modifiers, surface),
            ))
        })
    }

    fn release_facts(&self, control: &ControlPlane) -> ReleaseFacts {
        let validation = match self.validate(control, true) {
            Ok(()) => ReleaseValidation::Ready,
            Err(InputError::StalePresentation) => ReleaseValidation::Stale,
            Err(InputError::NotReady) => ReleaseValidation::NotReady,
            Err(_) => ReleaseValidation::Other,
        };
        let epoch_matches = self
            .identity
            .is_some_and(|identity| control.connection_epoch() == identity.epoch);
        let frame = control
            .publication()
            .acquire_view(self.view)
            .map(|frame| ReleaseFrame {
                terminal_matches: frame.terminal_id == self.terminal,
                replica_matches: self.identity.is_some_and(|identity| {
                    (frame.stream_id, frame.bootstrap_id) == (identity.stream, identity.bootstrap)
                }) && control.engine().is_some_and(|engine| {
                    engine.view_replica_info(self.view).is_ok_and(|info| {
                        (info.stream_id, info.bootstrap_id) == (frame.stream_id, frame.bootstrap_id)
                    })
                }),
            });
        ReleaseFacts {
            validation,
            epoch_matches,
            frame,
            input_ready: input_ready(control, &self.terminal),
        }
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
            queued(control.send_mouse(&self.terminal, mouse_at(action, button, modifiers, surface)))
        })
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
            if !reporting(control, &self.terminal, modifiers)? {
                return Ok(false);
            }
            if !input_ready(control, &self.terminal) {
                return Err(InputError::NotReady);
            }
            queued(
                control.send_mouse(&self.terminal, mouse_at(action, button, modifiers, surface)),
            )?;
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
        // Clamped: a drag that leaves the pane stays on the edge cell. Pixels
        // are the integer grid `cell_px` reports, not the pane's logical size.
        let surface = self.surface(position, true)?;
        let result = self.with_current(false, |control| {
            let frame = self.live_frame(control)?;
            let event = SelectionGestureEvent {
                phase,
                clicks: clicks.clamp(1, 3),
                handle: self.gesture,
                column: surface.col,
                row: u32::from(surface.row),
                rectangle: modifiers.alt,
                x: surface.x,
                y: surface.y,
                columns: u32::from(frame.cols),
                cell_width: surface.cell_width,
                screen_height: u32::from(frame.rows)
                    .saturating_mul(surface.cell_height)
                    .max(1),
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
                self.report_scroll(control, lines, event)?;
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
    ) -> Result<(), InputError> {
        if !input_ready(control, &self.terminal) {
            return Err(InputError::NotReady);
        }
        let button = if lines > 0 {
            WireButton::Four
        } else {
            WireButton::Five
        };
        let surface = self.surface(event.position, false)?;
        for _ in 0..lines.unsigned_abs().min(100) {
            queued(control.send_mouse(
                &self.terminal,
                mouse_at(MouseAction::Press, button, event.modifiers, surface),
            ))?;
        }
        Ok(())
    }
}

/// Why `validate` rejected or accepted a captured application-mouse release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReleaseValidation {
    Ready,
    Stale,
    NotReady,
    Other,
}

/// The live publication, when one exists, compared with the painted identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReleaseFrame {
    terminal_matches: bool,
    replica_matches: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReleaseFacts {
    validation: ReleaseValidation,
    epoch_matches: bool,
    /// `None` when the view has no published frame.
    frame: Option<ReleaseFrame>,
    input_ready: bool,
}

/// `None` means send the release.
///
/// A geometry gap (the live frame is this replica, but its size is not the
/// painted one yet) and an unpublished frame still belong to the captured
/// button. A replaced connection or an unknown-delivery fence does not.
fn refuse_captured_release(facts: ReleaseFacts) -> Option<InputError> {
    match facts.validation {
        ReleaseValidation::Ready => None,
        ReleaseValidation::NotReady => Some(InputError::NotReady),
        ReleaseValidation::Other => Some(InputError::StalePresentation),
        ReleaseValidation::Stale => {
            let same_target = facts.epoch_matches
                && facts
                    .frame
                    .is_none_or(|frame| frame.terminal_matches && frame.replica_matches);
            if same_target && facts.input_ready {
                None
            } else if same_target {
                Some(InputError::NotReady)
            } else {
                Some(InputError::StalePresentation)
            }
        }
    }
}

/// A motion event the last one already covered. Leaving the grid is not a repeat.
fn repeats_cell(last: Option<(u16, u16)>, next: Option<(u16, u16)>) -> bool {
    next.is_some() && next == last
}

fn mouse_at(
    action: MouseAction,
    button: WireButton,
    modifiers: Modifiers,
    surface: SurfacePoint,
) -> MouseEvent {
    MouseEvent {
        action,
        button,
        mods: keys::modifiers(modifiers),
        x: surface.x,
        y: surface.y,
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

#[cfg(test)]
mod tests {
    use super::{
        InputError, ReleaseFacts, ReleaseFrame, ReleaseValidation, refuse_captured_release,
        repeats_cell,
    };

    #[test]
    fn motion_in_the_same_cell_is_one_event() {
        assert!(!repeats_cell(None, Some((0, 0))));
        assert!(repeats_cell(Some((0, 0)), Some((0, 0))));
        assert!(!repeats_cell(Some((0, 0)), Some((0, 1))));
        assert!(!repeats_cell(Some((1, 0)), Some((0, 0))));
        assert!(!repeats_cell(Some((0, 0)), None));
    }

    fn facts(
        validation: ReleaseValidation,
        epoch_matches: bool,
        frame: Option<ReleaseFrame>,
        input_ready: bool,
    ) -> ReleaseFacts {
        ReleaseFacts {
            validation,
            epoch_matches,
            frame,
            input_ready,
        }
    }

    #[test]
    fn a_captured_release_survives_a_geometry_or_frame_gap() {
        let same = ReleaseFrame {
            terminal_matches: true,
            replica_matches: true,
        };
        assert_eq!(
            refuse_captured_release(facts(ReleaseValidation::Stale, true, Some(same), true)),
            None,
            "painted size behind the live frame"
        );
        assert_eq!(
            refuse_captured_release(facts(ReleaseValidation::Stale, true, None, true)),
            None,
            "no published frame"
        );
        assert_eq!(
            refuse_captured_release(facts(ReleaseValidation::Ready, true, Some(same), true)),
            None
        );
    }

    #[test]
    fn a_captured_release_stops_for_replacement_or_a_fence() {
        let same = ReleaseFrame {
            terminal_matches: true,
            replica_matches: true,
        };
        let other_replica = ReleaseFrame {
            terminal_matches: true,
            replica_matches: false,
        };
        let other_terminal = ReleaseFrame {
            terminal_matches: false,
            replica_matches: true,
        };
        assert_eq!(
            refuse_captured_release(facts(ReleaseValidation::Stale, false, Some(same), true)),
            Some(InputError::StalePresentation)
        );
        assert_eq!(
            refuse_captured_release(facts(
                ReleaseValidation::Stale,
                true,
                Some(other_replica),
                true
            )),
            Some(InputError::StalePresentation)
        );
        assert_eq!(
            refuse_captured_release(facts(
                ReleaseValidation::Stale,
                true,
                Some(other_terminal),
                true
            )),
            Some(InputError::StalePresentation)
        );
        assert_eq!(
            refuse_captured_release(facts(ReleaseValidation::Stale, true, Some(same), false)),
            Some(InputError::NotReady)
        );
        assert_eq!(
            refuse_captured_release(facts(ReleaseValidation::Stale, true, None, false)),
            Some(InputError::NotReady)
        );
        assert_eq!(
            refuse_captured_release(facts(ReleaseValidation::NotReady, true, Some(same), false)),
            Some(InputError::NotReady)
        );
        assert_eq!(
            refuse_captured_release(facts(ReleaseValidation::Other, true, None, true)),
            Some(InputError::StalePresentation)
        );
    }
}
